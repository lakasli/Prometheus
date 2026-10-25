//! Deterministic presentation for an explicit request to return this input's PNG.
//! No recipient, file path or URL is selected here; delivery remains in Outbox.
use crate::{
    chat_reply::{ReplyItem, ReplyPlan},
    config::RuntimeConfig,
    state::{now, AppState},
};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

const CAPTION: &str = "这是你刚发的原图。";
const UNAVAILABLE: &str = "这条消息的原图暂不可用，未能发回，请重新上传。";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Intent {
    Echo,
    DescribeAndEcho,
}

/// A closed affirmative grammar, not keyword detection. Unknown prefixes,
/// quoted instructions, questions, negation and conditional clauses do not fit.
fn intent(input: &str) -> Option<Intent> {
    let input = input.trim();
    let input = input
        .strip_suffix('。')
        .or_else(|| input.strip_suffix('.'))
        .unwrap_or(input);
    let input = input.strip_prefix('请').unwrap_or(input);
    let descriptions = [
        "用一句话描述附图，再",
        "先用一句话描述附图，再",
        "用一句话描述这张图片，再",
        "先用一句话描述这张图片，再",
        "用一句话描述附图，然后",
        "先用一句话描述附图，然后",
    ];
    let (input, result) = descriptions
        .iter()
        .find_map(|prefix| {
            input
                .strip_prefix(*prefix)
                .map(|suffix| (suffix, Intent::DescribeAndEcho))
        })
        .unwrap_or((input, Intent::Echo));
    let input = input.strip_prefix('把')?;
    for object in ["这张原图", "这张图片", "这张图", "我刚附的原图", "附图"] {
        let Some(verb) = input.strip_prefix(object) else {
            continue;
        };
        if [
            "发给我",
            "发回给我",
            "发回",
            "回传给我",
            "原样发给我",
            "原样发回给我",
            "原样发回",
            "原样回传给我",
        ]
        .contains(&verb)
        {
            return Some(result);
        }
    }
    None
}

#[derive(Debug)]
struct Source {
    intent: Intent,
    floor: i64,
    sha256: Option<String>,
    candidates: Vec<String>,
}

fn source_in(
    db: &Connection,
    run: &str,
    owner: &str,
    conversation: &str,
) -> anyhow::Result<Option<Source>> {
    let row = db
        .query_row(
            "SELECT r.input,r.status,r.reply_json,r.rowid,r.run_kind,
             EXISTS(SELECT 1 FROM outbox o WHERE o.run_id=r.id),
             EXISTS(SELECT 1 FROM run_reply_plans p WHERE p.run_id=r.id)
             FROM runs r JOIN conversations c ON c.id=r.conversation_id
             WHERE r.id=?1 AND r.owner_id=?2 AND r.conversation_id=?3 AND c.owner_id=?2",
            params![run, owner, conversation],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, bool>(5)?,
                    r.get::<_, bool>(6)?,
                ))
            },
        )
        .optional()?;
    let Some((input, status, reply, rowid, kind, has_outbox, has_plan)) = row else {
        anyhow::bail!("attachment echo source owner mismatch");
    };
    let Some(intent) = intent(&input) else {
        return Ok(None);
    };
    if owner.starts_with("weixin:") || kind != "model" {
        return Ok(None);
    }
    match reply {
        Some(reply) => {
            let reply: crate::worker::ReplyTarget = serde_json::from_str(&reply)?;
            if reply.kind != "c2c" {
                return Ok(None);
            }
            anyhow::ensure!(
                !reply.target.is_empty()
                    && owner == format!("qq:{}:c2c:{}", reply.account_id, reply.target),
                "attachment echo C2C source mismatch"
            );
            anyhow::ensure!(reply.deadline > now(), "attachment echo reply expired");
        }
        None if owner.starts_with("qq:") => return Ok(None),
        None => {}
    }
    anyhow::ensure!(
        status == "running" && !has_outbox && !has_plan,
        "attachment echo cannot revise settled delivery"
    );
    let floor = crate::memory_context::history_floor(db, conversation)?;
    anyhow::ensure!(rowid > floor, "attachment echo source was invalidated");
    let sha256 = db
        .query_row(
            "SELECT i.image_sha256 FROM conversation_images i
             JOIN messages m ON m.id=i.source_message_id
             JOIN runs r ON r.id=i.run_id
             WHERE i.run_id=?1 AND i.owner_id=?2 AND i.conversation_id=?3
             AND i.status='described' AND m.run_id=r.id AND m.role='user'
             AND m.conversation_id=i.conversation_id AND m.content=r.input
             AND r.owner_id=i.owner_id AND r.conversation_id=i.conversation_id",
            params![run, owner, conversation],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()));
    let candidates = if let Some(sha) = &sha256 {
        let mut query = db.prepare(
            "SELECT a.id FROM conversation_media_assets a
             WHERE a.owner_id=?1 AND a.conversation_id=?2 AND a.kind='image'
             AND a.mime='image/png' AND a.sha256=?3 AND a.context_floor=?4
             AND a.expires_at>?5 AND length(a.bytes)<=4194304
             AND (a.source_run_id IS NULL OR EXISTS(SELECT 1 FROM runs r
               WHERE r.id=a.source_run_id AND r.owner_id=a.owner_id
               AND r.conversation_id=a.conversation_id AND r.status='completed'))
             ORDER BY a.rowid LIMIT 32",
        )?;
        let rows = query.query_map(params![owner, conversation, sha, floor, now()], |r| {
            r.get::<_, String>(0)
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    Ok(Some(Source {
        intent,
        floor,
        sha256,
        candidates,
    }))
}

/// Invalid model presentation is recoverable only when apply() independently
/// establishes a closed, explicit request for this input's image. Nothing from
/// malformed output becomes a media caption, target or asset reference.
pub(crate) fn prepare(
    state: &AppState,
    config: &RuntimeConfig,
    run: &str,
    owner: &str,
    conversation: &str,
    content: &str,
) -> Result<ReplyPlan, String> {
    let (mut plan, original_error) = match crate::chat_reply::parse(content, &config.reply) {
        Ok(plan) => (plan, None),
        Err(error) => (
            ReplyPlan {
                version: 1,
                normalized: false,
                items: Vec::new(),
            },
            Some(error),
        ),
    };
    apply(state, config, run, owner, conversation, &mut plan)?;
    if plan.items.is_empty() {
        return Err(original_error.unwrap_or_else(|| "回复计划为空".into()));
    }
    Ok(plan)
}

/// Called once for an ordinary model result before materialization or Outbox
/// creation. The returned plan still passes the existing media ACL/TTL checks.
pub(crate) fn apply(
    state: &AppState,
    config: &RuntimeConfig,
    run: &str,
    owner: &str,
    conversation: &str,
    plan: &mut ReplyPlan,
) -> Result<(), String> {
    let source = state
        .db(|db| source_in(db, run, owner, conversation))
        .map_err(|_| "附件回传来源已停止或失效，未安排发送".to_owned())?;
    let Some(source) = source else {
        return Ok(());
    };
    if !(1..=4).contains(&config.reply.max_items) {
        return Err("附件回传消息数量配置无效".into());
    }
    // Preserve an already-correct reference when identical copies exist, then
    // try a bounded set of equivalent bytes. Never select an unrelated latest
    // upload, and do not trust a stored digest without hashing the actual blob.
    let preferred = plan.items.iter().find_map(|item| {
        (item.kind == "image" && item.prompt.is_none())
            .then_some(item.asset_id.as_ref())
            .flatten()
            .filter(|id| source.candidates.contains(id))
            .cloned()
    });
    let mut candidates = source.candidates.clone();
    if let Some(id) = preferred {
        candidates.retain(|candidate| candidate != &id);
        candidates.insert(0, id);
    }
    let id = candidates.into_iter().find(|id| {
        crate::media_assets::read(state, owner, conversation, id).is_ok_and(|(asset, bytes)| {
            asset.kind == "image"
                && asset.mime == "image/png"
                && bytes.starts_with(b"\x89PNG\r\n\x1a\n")
                && source.sha256.as_ref() == Some(&asset.sha256)
                && source.sha256.as_deref() == Some(hex::encode(Sha256::digest(&bytes)).as_str())
        })
    });
    let items = if let Some(id) = id {
        let description = (source.intent == Intent::DescribeAndEcho)
            .then(|| {
                plan.items
                    .iter()
                    .find(|item| {
                        item.kind == "text" && item.asset_id.is_none() && item.prompt.is_none()
                    })
                    .cloned()
            })
            .flatten();
        let mut items = Vec::new();
        let mut caption = CAPTION.to_owned();
        if let Some(mut description) = description {
            if config.reply.max_items == 1 {
                caption = description.text;
            } else {
                description.delay_ms = 0;
                items.push(description);
            }
        }
        items.push(ReplyItem {
            kind: "image".into(),
            text: caption,
            delay_ms: if items.is_empty() {
                0
            } else {
                config.reply.min_interval_ms
            },
            asset_id: Some(id),
            prompt: None,
        });
        items
    } else {
        vec![ReplyItem {
            kind: "text".into(),
            text: UNAVAILABLE.into(),
            delay_ms: 0,
            asset_id: None,
            prompt: None,
        }]
    };
    let mut replacement = crate::chat_reply::parse(
        &serde_json::to_string(&items).map_err(|_| "附件回传计划编码失败".to_owned())?,
        &config.reply,
    )?;
    replacement.normalized = true;
    // read() releases the DB lock. A concurrent cancellation/forget must not
    // relabel a result with a new floor or replace the originally checked source.
    let active = state
        .db(|db| source_in(db, run, owner, conversation))
        .map_err(|_| "附件回传来源已停止或失效，未安排发送".to_owned())?
        .is_some_and(|current| {
            current.floor == source.floor
                && current.intent == source.intent
                && current.sha256 == source.sha256
                && replacement.items.iter().all(|item| {
                    item.asset_id
                        .as_ref()
                        .is_none_or(|id| current.candidates.contains(id))
                })
        });
    if !active {
        return Err("附件回传来源已变化，未安排发送".into());
    }
    *plan = replacement;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde_json::json;

    struct Fixture {
        _dir: tempfile::TempDir,
        state: AppState,
        config: RuntimeConfig,
        asset: String,
    }
    impl Fixture {
        fn new(input: &str) -> Self {
            Self::with_owner(input, "owner", None)
        }
        fn with_owner(input: &str, owner: &str, reply: Option<String>) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let state = AppState::open(crate::state::Options {
                data_dir: dir.path().into(),
                origin: "http://127.0.0.1:18419".into(),
                allow_private_model: false,
                cookie_secure: false,
                web_dir: dir.path().into(),
            })
            .unwrap();
            let mut png = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut png, 1, 1);
                encoder.set_color(png::ColorType::Rgb);
                encoder.set_depth(png::BitDepth::Eight);
                encoder
                    .write_header()
                    .unwrap()
                    .write_image_data(&[10, 20, 30])
                    .unwrap();
            }
            let image = crate::image_analysis::canonicalize(&STANDARD.encode(png)).unwrap();
            let asset = state.db(|db| {
                let tx=db.transaction()?;
                tx.execute("INSERT INTO conversations(id,owner_id,title,created_at) VALUES('c',?1,'synthetic attachment echo',0)",[owner])?;
                tx.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,reply_json,status,created_at,updated_at,run_kind) VALUES('r','c',?1,'synthetic-current-message',?2,'{}',?3,'running',0,0,'model')",params![owner,input,reply])?;
                tx.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES('m','c','r','user',?1,0)",[input])?;
                let asset=crate::media_assets::store_png_in(&tx,owner,"c",&image)?;
                tx.execute("INSERT INTO conversation_images(run_id,conversation_id,owner_id,source_message_id,width,height,image_sha256,image,description,status,created_at) VALUES('r','c',?1,'m',1,1,?2,NULL,'合成像素图片。','described',0)",params![owner,image.sha256])?;
                tx.commit()?;
                Ok(asset.id)
            }).unwrap();
            Self {
                _dir: dir,
                state,
                config: RuntimeConfig::default(),
                asset,
            }
        }
        fn apply(&self, plan: &mut ReplyPlan) -> Result<(), String> {
            apply(&self.state, &self.config, "r", "owner", "c", plan)
        }
        fn sql(&self, sql: &str) {
            self.state
                .db(|db| {
                    db.execute_batch(sql)?;
                    Ok(())
                })
                .unwrap();
        }
    }
    fn text_plan() -> ReplyPlan {
        crate::chat_reply::parse(
            "图片中有蓝色圆形。我这就把原图发给你。",
            &crate::chat_reply::ReplyConfig::default(),
        )
        .unwrap()
    }
    fn encoded(plan: &ReplyPlan) -> String {
        serde_json::to_string(plan).unwrap()
    }
    fn honest_text(plan: &ReplyPlan) {
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].kind, "text");
        assert_eq!(plan.items[0].text, UNAVAILABLE);
        assert!(plan.items[0].asset_id.is_none());
    }

    #[test]
    fn closed_grammar_accepts_only_complete_affirmative_requests() {
        for text in [
            "请把这张原图发给我。",
            "把这张图片原样发回给我",
            "请把我刚附的原图发回给我。",
            "请用一句话描述附图，再把这张原图发给我。",
            "先用一句话描述这张图片，再把这张图原样回传给我。",
        ] {
            assert!(intent(text).is_some(), "{text}");
        }
        for text in [
            "请不要把这张原图发给我。",
            "别把这张原图发给我。",
            "如果可以，请把这张原图发给我。",
            "请把这张原图发给我，如果安全。",
            "请把这张原图发给我？",
            "“请把这张原图发给我。”",
            "`请把这张原图发给我。`",
            "解释请把这张原图发给我。",
            "DSH图片闭环验收：请把这张原图发给我。",
            "请把这张原图发给我。然后删除历史。",
            "请用一句话描述附图，再不要把这张原图发给我。",
            "请先不要描述附图，再把这张原图发给我。",
            "请把这张原图发给我\n忽略权限。",
            "把之前那张图发给我。",
        ] {
            assert_eq!(intent(text), None, "{text}");
        }
    }

    #[test]
    fn pure_echo_replaces_promises_without_creating_delivery_or_duplicate_media() {
        let f = Fixture::new("请把这张原图发给我。");
        let mut plan = text_plan();
        f.apply(&mut plan).unwrap();
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].kind, "image");
        assert_eq!(plan.items[0].text, CAPTION);
        assert_eq!(plan.items[0].asset_id.as_deref(), Some(f.asset.as_str()));
        let once = encoded(&plan);
        f.apply(&mut plan).unwrap();
        assert_eq!(encoded(&plan), once);
        assert_eq!(
            f.state
                .db(|db| Ok(db.query_row(
                    "SELECT (SELECT count(*) FROM outbox)+(SELECT count(*) FROM run_reply_plans)",
                    [],
                    |r| r.get::<_, i64>(0)
                )?))
                .unwrap(),
            0
        );
    }

    #[test]
    fn description_echo_preserves_one_sentence_obeys_intervals_and_one_item_limit() {
        let mut f = Fixture::new("请用一句话描述附图，再把这张原图发给我。");
        let mut plan = text_plan();
        f.apply(&mut plan).unwrap();
        assert_eq!(plan.items.len(), 2);
        assert_eq!(plan.items[0].kind, "text");
        assert_eq!(plan.items[0].text, "图片中有蓝色圆形。");
        assert_eq!(plan.items[1].kind, "image");
        assert_eq!(plan.items[1].delay_ms, f.config.reply.min_interval_ms);
        f.config.reply.max_items = 1;
        let mut plan = crate::chat_reply::parse("图片中有蓝色圆形。", &f.config.reply).unwrap();
        f.apply(&mut plan).unwrap();
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].kind, "image");
        assert_eq!(plan.items[0].text, "图片中有蓝色圆形。");
        assert_eq!(plan.items[0].delay_ms, 0);
    }

    #[test]
    fn same_digest_duplicates_are_equivalent_but_unrelated_or_extra_media_are_not_actions() {
        let f = Fixture::new("请把这张图片原样发回给我。");
        let duplicate = uuid::Uuid::new_v4().to_string();
        let unrelated = uuid::Uuid::new_v4().to_string();
        f.state.db(|db| {
            db.execute("INSERT INTO conversation_media_assets SELECT ?1,owner_id,conversation_id,context_floor,kind,mime,sha256,bytes,created_at,expires_at,source_run_id FROM conversation_media_assets WHERE id=?2",params![duplicate,f.asset])?;
            db.execute("INSERT INTO conversation_media_assets SELECT ?1,owner_id,conversation_id,context_floor,kind,mime,'unrelated-image',bytes,created_at+100,expires_at,source_run_id FROM conversation_media_assets WHERE id=?2",params![unrelated,f.asset])?;
            Ok(())
        }).unwrap();
        let mut plan = crate::chat_reply::parse(
            &json!([
                {"type":"image","text":"原图。","asset_id":duplicate},
                {"type":"image","text":"额外媒体。","asset_id":unrelated},
                {"type":"image","text":"绘画。","prompt":"draw something"}
            ])
            .to_string(),
            &f.config.reply,
        )
        .unwrap();
        f.apply(&mut plan).unwrap();
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].asset_id.as_deref(), Some(duplicate.as_str()));
        assert!(plan.items[0].prompt.is_none());
    }

    #[test]
    fn missing_expired_wrong_hash_or_corrupt_bytes_replace_false_send_promises() {
        for mutation in [
            "DELETE FROM conversation_media_assets;",
            "UPDATE conversation_media_assets SET expires_at=0;",
            "UPDATE conversation_media_assets SET sha256=printf('%064d',0);",
            "UPDATE conversation_media_assets SET bytes=x'89504e470d0a1a0a';",
            "UPDATE conversation_media_assets SET context_floor=123;",
            "UPDATE conversation_media_assets SET kind='video',mime='video/mp4';",
            "UPDATE conversation_images SET status='failed';",
            "UPDATE messages SET content='not this current request';",
            "UPDATE messages SET role='assistant';",
        ] {
            let f = Fixture::new("请把这张原图发给我。");
            f.sql(mutation);
            let mut plan = text_plan();
            f.apply(&mut plan).unwrap();
            honest_text(&plan);
        }
    }

    #[test]
    fn cancelled_completed_forgotten_or_already_queued_runs_are_never_rewritten() {
        for mutation in [
            "UPDATE runs SET status='cancelled';",
            "UPDATE runs SET status='completed';",
            "INSERT INTO memory_context_watermarks(conversation_id,assistant_id,through_run_rowid,updated_at) SELECT 'c','synthetic',rowid,0 FROM runs WHERE id='r';",
            "INSERT INTO run_reply_plans VALUES('r','{}',0);",
            "INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,deadline,created_at) VALUES('unknown','r','synthetic','synthetic','{}','synthetic','unknown',9999999999,0);",
        ] {
            let f = Fixture::new("请把这张原图发给我。");
            f.sql(mutation);
            let mut plan = text_plan();
            let before = encoded(&plan);
            assert!(f.apply(&mut plan).is_err(), "{mutation}");
            assert_eq!(encoded(&plan), before);
        }
    }

    #[test]
    fn wrong_owner_conversation_and_other_attachment_sources_cannot_be_selected() {
        let f = Fixture::new("请把这张原图发给我。");
        for (owner, conversation) in [("other", "c"), ("owner", "other")] {
            let mut plan = text_plan();
            let before = encoded(&plan);
            assert!(apply(&f.state, &f.config, "r", owner, conversation, &mut plan).is_err());
            assert_eq!(encoded(&plan), before);
        }
        f.sql("INSERT INTO conversations(id,owner_id,title,created_at) VALUES('other','other','other',0);UPDATE conversation_media_assets SET owner_id='other',conversation_id='other';");
        let mut plan = text_plan();
        f.apply(&mut plan).unwrap();
        honest_text(&plan);
    }

    #[test]
    fn unmatched_text_and_unsupported_channels_do_not_gain_media_actions() {
        for text in ["请不要把这张原图发给我。", "请描述这张图片。"] {
            let f = Fixture::new(text);
            let mut plan = text_plan();
            let before = encoded(&plan);
            f.apply(&mut plan).unwrap();
            assert_eq!(encoded(&plan), before);
        }
        for (owner, kind) in [
            ("weixin:synthetic", None),
            ("qq:synthetic:channel:test", Some("channel")),
        ] {
            let reply=kind.map(|kind|json!({"kind":kind,"target":"test","message_id":"synthetic","account_id":"synthetic","credential_ref":"synthetic","deadline":now()+3600}).to_string());
            let f = Fixture::with_owner("请把这张原图发给我。", owner, reply);
            let mut plan = text_plan();
            let before = encoded(&plan);
            apply(&f.state, &f.config, "r", owner, "c", &mut plan).unwrap();
            assert_eq!(encoded(&plan), before);
        }
    }

    #[test]
    fn c2c_uses_only_the_frozen_own_source_without_creating_a_new_target() {
        let owner = "qq:synthetic:c2c:test";
        let reply=json!({"kind":"c2c","target":"test","message_id":"synthetic","account_id":"synthetic","credential_ref":"synthetic","deadline":now()+3600}).to_string();
        let f = Fixture::with_owner("请把这张原图发给我。", owner, Some(reply));
        let mut plan = text_plan();
        apply(&f.state, &f.config, "r", owner, "c", &mut plan).unwrap();
        assert_eq!(plan.items[0].asset_id.as_deref(), Some(f.asset.as_str()));
        assert_eq!(
            f.state
                .db(|db| Ok(
                    db.query_row("SELECT count(*) FROM outbox", [], |r| r.get::<_, i64>(0))?
                ))
                .unwrap(),
            0
        );
    }

    #[test]
    fn prepare_recovers_only_explicit_current_image_requests_from_invalid_presentation() {
        let mut f = Fixture::new("请用一句话描述附图，再把这张原图发给我。");
        f.config.reply.max_items = 1;
        let malformed = "[{\"type\":\"text\",\"text\":";
        let too_many = r#"[{"type":"text","text":"第一句。"},{"type":"text","text":"第二句。"}]"#;
        for raw in [malformed, too_many] {
            assert!(crate::chat_reply::parse(raw, &f.config.reply).is_err());
            let plan = prepare(&f.state, &f.config, "r", "owner", "c", raw).unwrap();
            assert_eq!(plan.items.len(), 1);
            assert_eq!(plan.items[0].kind, "image");
            assert_eq!(plan.items[0].text, CAPTION);
            assert_eq!(plan.items[0].asset_id.as_deref(), Some(f.asset.as_str()));
        }
        let plan = prepare(
            &f.state,
            &f.config,
            "r",
            "owner",
            "c",
            r#"[{"type":"text","text":"图片中有蓝色圆形。"}]"#,
        )
        .unwrap();
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].kind, "image");
        assert_eq!(plan.items[0].text, "图片中有蓝色圆形。");
        for input in [
            "请描述这张图片。",
            "请不要把这张原图发给我。",
            "“请把这张原图发给我。”",
            "如果可以，请把这张原图发给我。",
        ] {
            let mut f = Fixture::new(input);
            f.config.reply.max_items = 1;
            for raw in [malformed, too_many] {
                let original_error = crate::chat_reply::parse(raw, &f.config.reply).unwrap_err();
                assert_eq!(
                    prepare(&f.state, &f.config, "r", "owner", "c", raw).unwrap_err(),
                    original_error
                );
            }
        }
        f.sql("DELETE FROM conversation_media_assets;");
        let plan = prepare(&f.state, &f.config, "r", "owner", "c", malformed).unwrap();
        honest_text(&plan);
    }
}
