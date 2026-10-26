use super::*;
use crate::{conversation_identity, conversation_state, turn_control};
const OWNER: &str = "qq:synthetic:c2c:a";
const AT: i64 = 1_793_030_000;

fn database() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    schema(&db);
    db
}
fn schema(db: &Connection) {
    db.execute_batch(include_str!("../schema.sql")).unwrap();
    db.execute_batch("ALTER TABLE runs ADD COLUMN run_kind TEXT NOT NULL DEFAULT 'model';
      ALTER TABLE runs ADD COLUMN character_snapshot_json TEXT;
      CREATE TABLE conversation_character_bindings(conversation_id TEXT PRIMARY KEY,draft_card_json TEXT);
      CREATE TABLE run_prompt_snapshots(run_id TEXT PRIMARY KEY,messages_json TEXT);
      CREATE TABLE memory_context_watermarks(conversation_id TEXT PRIMARY KEY,through_run_rowid INTEGER NOT NULL);
      INSERT INTO settings VALUES('assistant.identity.v1','stable');
      INSERT INTO conversations VALUES('a','qq:synthetic:c2c:a','A','qq:synthetic:c2c:a',0),('b','qq:synthetic:c2c:b','B','qq:synthetic:c2c:b',0),('g','qq:synthetic:group:team','Group','qq:synthetic:group:team',0);").unwrap();
    conversation_identity::initialize(db).unwrap();
    turn_control::ensure_schema(db).unwrap();
    conversation_state::initialize(db).unwrap();
}
fn add(
    db: &mut Connection,
    id: &str,
    conversation: &str,
    text: &str,
    at: i64,
    sender: Option<&str>,
) {
    let tx = db.transaction().unwrap();
    let owner: String = tx
        .query_row(
            "SELECT owner_id FROM conversations WHERE id=?1",
            [conversation],
            |r| r.get(0),
        )
        .unwrap();
    tx.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,created_at,updated_at) VALUES(?1,?2,?3,?1,?4,'{}','running',?5,?5)",params![id,conversation,owner,text,at]).unwrap();
    tx.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES(?1,?2,?1,'user',?3,?4)",params![id,conversation,text,at]).unwrap();
    conversation_identity::freeze_run_in(&tx, id).unwrap();
    if conversation == "g" {
        let author = sender
            .map(|s| json!({"member_openid":s}))
            .unwrap_or_else(|| json!({}));
        conversation_identity::record_qq_message_in(
            &tx,
            id,
            "GROUP_AT_MESSAGE_CREATE",
            &json!({"id":id,"group_openid":"team","author":author}),
        )
        .unwrap();
    }
    let speaker = if conversation == "g" {
        sender.unwrap_or("")
    } else {
        owner.as_str()
    };
    turn_control::register_input_in(&tx, id, id, text, speaker, conversation != "g", at * 1000)
        .unwrap();
    observe_input_in(&tx, id, at, &FocusPolicy::default()).unwrap();
    tx.commit().unwrap();
}
fn context(db: &mut Connection, run: &str, at: i64) -> FocusContext {
    let tx = db.transaction().unwrap();
    let value = context_for_run_in(&tx, run, at).unwrap();
    tx.commit().unwrap();
    value
}

#[test]
fn continuation_and_ambiguous_reference_are_scoped_and_bounded() {
    let mut db = database();
    add(&mut db, "one", "a", "讨论书房照明方案", AT, None);
    add(&mut db, "two", "a", "继续", AT + 1, None);
    let current = context(&mut db, "two", AT + 1);
    assert_eq!(current.current_focus.unwrap().source_run_id, "one");
    assert!(!current.requires_clarification);
    add(&mut db, "other", "a", "讨论周末远足方案", AT + 2, None);
    add(&mut db, "deictic", "a", "那个方案", AT + 3, None);
    let ambiguous = context(&mut db, "deictic", AT + 3);
    assert_eq!(ambiguous.status, "ambiguous");
    assert!(ambiguous.requires_clarification);
    assert_eq!(ambiguous.alternatives.len(), 2);
    for index in 0..9 {
        add(
            &mut db,
            &format!("topic-{index}"),
            "a",
            &format!("独立讨论主题{index}"),
            AT + 10 + index,
            None,
        );
    }
    let active:i64=db.query_row("SELECT count(*) FROM conversation_state_entities WHERE kind='focus' AND status='active' AND json_extract(content_json,'$.mode')='topic'",[],|r|r.get(0)).unwrap();
    assert_eq!(active, 4);
}

#[test]
fn a_referential_instruction_preserves_sources_and_keeps_multiple_candidates_ambiguous() {
    let mut db = database();
    add(&mut db, "topic", "a", "讨论庭院排水步骤", AT, None);
    add(&mut db, "next", "a", "继续，先讲需要什么材料", AT + 1, None);
    let next = context(&mut db, "next", AT + 1);
    assert!(next.continuation);
    assert_eq!(next.current_focus.unwrap().source_run_id, "topic");
    add(&mut db, "other", "a", "另一个书房隔音方案", AT + 2, None);
    add(
        &mut db,
        "which",
        "a",
        "那个方案再具体说明一下",
        AT + 3,
        None,
    );
    let ambiguous = context(&mut db, "which", AT + 3);
    assert_eq!(ambiguous.status, "ambiguous");
    assert_eq!(ambiguous.alternatives.len(), 2);
    let topics: i64 = db.query_row("SELECT count(*) FROM conversation_state_entities WHERE status='active' AND json_extract(content_json,'$.mode')='topic'",[],|r|r.get(0)).unwrap();
    assert_eq!(topics, 2, "a continuation must not replace the real topic");
    add(&mut db, "switch", "a", "先换个话题，讨论门锁", AT + 4, None);
    add(&mut db, "new-next", "a", "继续", AT + 5, None);
    assert_eq!(
        context(&mut db, "new-next", AT + 5)
            .current_focus
            .unwrap()
            .source_run_id,
        "switch"
    );
}

#[test]
fn referential_controls_reject_quotes_hypotheticals_and_unrelated_new_questions() {
    for text in [
        "如果按这个预算安排会怎样",
        "他说“继续，讲具体一点”，是什么意思",
        "举例说明那个方案再详细一点这句话",
        "介绍一下继续教育是什么",
        "计算这次采购总额",
    ] {
        assert_eq!(continuation(text), None, "{text}");
    }
    assert_eq!(continuation("按这个按钮可以重新开机吗"), None);
    assert!(!closure("谢谢，就这样吧，另外水管怎么选"));
    assert!(closure("谢谢，就这样吧。"));
    assert!(closure("先聊到这里。"));
}
#[test]
fn closure_is_expression_only_and_a_real_new_question_reopens() {
    let mut db = database();
    add(&mut db, "topic", "a", "讨论一张海报", AT, None);
    add(&mut db, "close", "a", "晚安，先不聊了", AT, None);
    let closed = context(&mut db, "close", AT);
    assert!(closed.closed);
    assert_eq!(closed.status, "closed");
    assert_eq!(
        db.query_row(
            "SELECT decision FROM run_publication_decisions WHERE run_id='topic'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "active"
    );
    add(&mut db, "new", "a", "十七乘十九是多少", AT, None);
    add(&mut db, "follow", "a", "继续", AT, None);
    let reopened = context(&mut db, "follow", AT);
    assert!(!reopened.closed);
    assert_eq!(reopened.current_focus.unwrap().source_run_id, "new");
    add(
        &mut db,
        "quoted",
        "a",
        "他说“先不聊了”，这句话如何翻译",
        AT + 1,
        None,
    );
    assert!(!context(&mut db, "quoted", AT + 1).closed);
}
#[test]
fn explicit_switch_retires_old_topics_and_temporary_preferences_are_not_global() {
    let mut db = database();
    add(&mut db, "topic", "a", "分析我的这段SQL", AT, None);
    add(&mut db, "pref", "a", "这题详细点", AT + 1, None);
    assert_eq!(
        context(&mut db, "pref", AT + 1)
            .response_preference
            .unwrap()
            .detail,
        "detailed"
    );
    add(&mut db, "switch", "a", "换个话题：讨论新窗帘", AT + 2, None);
    let fresh = context(&mut db, "switch", AT + 2);
    assert_eq!(fresh.current_focus.unwrap().source_run_id, "switch");
    assert!(fresh.response_preference.is_none());
    add(&mut db, "general", "a", "以后简短些", AT + 3, None);
    add(&mut db, "later", "a", "新的技术问题", AT + 4, None);
    assert_eq!(
        context(&mut db, "later", AT + 4)
            .response_preference
            .unwrap()
            .detail,
        "brief"
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM settings WHERE key LIKE 'assistant.personality%'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}
#[test]
fn literal_correction_survives_successor_turns_without_rewriting_original_or_claiming_a_schedule() {
    let mut db = database();
    add(&mut db, "request", "a", "请讨论明天的路线", AT, None);
    add(&mut db, "correction", "a", "等等，改成后天", AT + 1, None);
    add(&mut db, "follow", "a", "继续", AT + 2, None);
    let current = context(&mut db, "follow", AT + 2);
    assert_eq!(current.corrections.len(), 1);
    assert_eq!(current.corrections[0].target_run_id, "request");
    assert_eq!(current.corrections[0].correction_text, "等等，改成后天");
    assert_eq!(
        db.query_row("SELECT input FROM runs WHERE id='request'", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "请讨论明天的路线"
    );
    assert!(!serde_json::to_string(&current)
        .unwrap()
        .contains("scheduled"));
    add(
        &mut db,
        "switch",
        "a",
        "换个话题：介绍一本新书",
        AT + 3,
        None,
    );
    assert!(context(&mut db, "switch", AT + 3).corrections.is_empty());
}

#[test]
fn correction_replacement_is_complete_and_unrelated_new_questions_do_not_inherit_it() {
    let mut db = database();
    add(&mut db, "request", "a", "请讨论后续路线", AT, None);
    let correction = format!("等等，改成后天：{}", "保留完整条件".repeat(80));
    assert!(correction.len() > 1024 && correction.len() < 2048);
    add(&mut db, "correction", "a", &correction, AT + 1, None);
    add(&mut db, "follow", "a", "继续", AT + 2, None);
    assert_eq!(
        context(&mut db, "follow", AT + 2).corrections[0].correction_text,
        correction
    );
    add(
        &mut db,
        "unrelated",
        "a",
        "介绍一个新的算法概念",
        AT + 3,
        None,
    );
    assert!(context(&mut db, "unrelated", AT + 3).corrections.is_empty());
}

#[test]
fn an_explicit_reference_to_corrected_parameters_keeps_the_correction_for_later_reasoning() {
    let mut db = database();
    add(&mut db, "request", "a", "讨论350元的材料预算", AT, None);
    add(
        &mut db,
        "correction",
        "a",
        "刚才那个数字是470，不是350。",
        AT + 1,
        None,
    );
    add(
        &mut db,
        "follow",
        "a",
        "按这个预算分配不同材料",
        AT + 2,
        None,
    );
    let corrected = context(&mut db, "follow", AT + 2);
    assert!(corrected.continuation);
    assert_eq!(corrected.current_focus.unwrap().source_run_id, "request");
    assert_eq!(corrected.corrections.len(), 1);
    assert_eq!(
        corrected.corrections[0].correction_text,
        "刚才那个数字是470，不是350。"
    );
    add(
        &mut db,
        "new",
        "a",
        "介绍一个不相关的物理概念",
        AT + 3,
        None,
    );
    assert!(context(&mut db, "new", AT + 3).corrections.is_empty());
}
#[test]
fn other_private_owner_and_group_speakers_do_not_inherit_focus_or_preferences() {
    let mut db = database();
    add(&mut db, "a-private", "a", "CANARY_A_PRIVATE", AT, None);
    add(&mut db, "b-private", "b", "继续", AT + 1, None);
    let b = context(&mut db, "b-private", AT + 1);
    assert!(b.current_focus.is_none());
    assert!(b.requires_clarification);
    add(&mut db, "ga", "g", "组内甲的话题", AT + 2, Some("member_a"));
    add(&mut db, "gp", "g", "以后简短些", AT + 3, Some("member_a"));
    add(&mut db, "gb", "g", "继续", AT + 4, Some("member_b"));
    let gb = context(&mut db, "gb", AT + 4);
    assert!(gb.current_focus.is_none());
    assert!(gb.response_preference.is_none());
    add(&mut db, "gu", "g", "继续", AT + 5, None);
    let unknown = context(&mut db, "gu", AT + 5);
    assert_eq!(unknown.status, "unknown_speaker");
    assert!(unknown.entity_refs.is_empty());
    add(&mut db, "ga-again", "g", "继续", AT + 6, Some("member_a"));
    assert_eq!(
        context(&mut db, "ga-again", AT + 6)
            .current_focus
            .unwrap()
            .source_run_id,
        "ga"
    );
}
#[test]
fn expiry_and_cross_day_require_live_sources_and_preserve_original_user_records() {
    let mut db = database();
    let day = chrono::DateTime::parse_from_rfc3339("2026-10-26T23:50:00+08:00")
        .unwrap()
        .timestamp();
    add(
        &mut db,
        "night",
        "a",
        "只有获准历史支持的夜间话题",
        day,
        None,
    );
    add(&mut db, "after-midnight", "a", "继续", day + 1200, None);
    assert!(
        context(&mut db, "after-midnight", day + 1200)
            .current_focus
            .unwrap()
            .cross_day_source_verified
    );
    add(
        &mut db,
        "expired",
        "a",
        "继续",
        day + DEFAULT_TTL_SECONDS + 1,
        None,
    );
    let missing = context(&mut db, "expired", day + DEFAULT_TTL_SECONDS + 1);
    assert!(missing.current_focus.is_none());
    assert!(missing.requires_clarification);
    assert_eq!(missing.status, "expired");
    let tx = db.transaction().unwrap();
    assert!(state::expire_in(&tx, day + DEFAULT_TTL_SECONDS + 1, 128).unwrap() > 0);
    tx.commit().unwrap();
    assert_eq!(
        db.query_row("SELECT input FROM runs WHERE id='night'", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "只有获准历史支持的夜间话题"
    );
}
#[test]
fn source_invalidation_purges_body_usage_and_prevents_reobservation_resurrection() {
    let mut db = database();
    add(&mut db, "source", "a", "CANARY_FOCUS_FORGET", AT, None);
    add(&mut db, "consumer", "a", "继续", AT + 1, None);
    let selected = context(&mut db, "consumer", AT + 1);
    let tx = db.transaction().unwrap();
    state::record_usage_in(&tx, "consumer", &selected.entity_refs, AT + 1).unwrap();
    tx.execute(
        "INSERT INTO run_prompt_snapshots VALUES('consumer','CANARY_FOCUS_FORGET')",
        [],
    )
    .unwrap();
    let result = state::invalidate_source_in(&tx, "run_input", "source", Some(1), AT + 2).unwrap();
    assert!(result.affected_run_ids.contains(&"consumer".into()));
    assert!(result.entities > 0);
    assert!(!state::validate_context_in(&tx, "consumer").unwrap());
    assert_eq!(
        tx.query_row("SELECT count(*) FROM run_prompt_snapshots", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    tx.commit().unwrap();
    add(&mut db, "future", "a", "继续", AT + 3, None);
    assert!(context(&mut db, "future", AT + 3).current_focus.is_none());
    let tx = db.transaction().unwrap();
    observe_input_in(&tx, "source", AT + 3, &FocusPolicy::default()).unwrap();
    assert!(context_for_run_in(&tx, "source", AT + 3)
        .unwrap()
        .current_focus
        .is_none());
    let bodies:i64=tx.query_row("SELECT count(*) FROM conversation_state_entities WHERE content_json LIKE '%CANARY_FOCUS_FORGET%'",[],|r|r.get(0)).unwrap();
    assert_eq!(bodies, 0);
}
#[test]
fn input_digest_replay_is_idempotent_and_future_input_cannot_enter_an_older_run() {
    let mut db = database();
    add(&mut db, "old", "a", "旧主题", AT, None);
    add(
        &mut db,
        "future",
        "a",
        "FUTURE_PRIVATE_MARKER",
        AT + 1,
        None,
    );
    let old = context(&mut db, "old", AT + 2);
    assert!(!serde_json::to_string(&old)
        .unwrap()
        .contains("FUTURE_PRIVATE_MARKER"));
    let tx = db.transaction().unwrap();
    let first = observe_input_in(&tx, "future", AT + 2, &FocusPolicy::default()).unwrap();
    let repeated = observe_input_in(&tx, "future", AT + 3, &FocusPolicy::default()).unwrap();
    assert_eq!(first.input_digest, repeated.input_digest);
    tx.execute(
        "UPDATE runs SET input='changed after observation' WHERE id='future'",
        [],
    )
    .unwrap();
    tx.execute(
        "UPDATE messages SET content='changed after observation' WHERE run_id='future'",
        [],
    )
    .unwrap();
    assert!(observe_input_in(&tx, "future", AT + 4, &FocusPolicy::default()).is_err());
}
#[test]
fn raw_control_sensitive_draft_and_untrusted_quotes_do_not_create_privileged_state() {
    let mut db = database();
    add(
        &mut db,
        "quote",
        "a",
        "如果我说以后简短些，该怎么理解",
        AT,
        None,
    );
    assert!(context(&mut db, "quote", AT).response_preference.is_none());
    add(&mut db, "secret", "a", "验证码是123456", AT + 1, None);
    add(
        &mut db,
        "password",
        "a",
        "password = synthetic_secret_value",
        AT + 2,
        None,
    );
    add(
        &mut db,
        "password-correction",
        "a",
        "等等，改成后天",
        AT + 3,
        None,
    );
    let tx = db.transaction().unwrap();
    assert_eq!(
        observation(&tx, "secret").unwrap().unwrap().intent,
        "control_or_sensitive"
    );
    assert_eq!(
        observation(&tx, "password").unwrap().unwrap().intent,
        "control_or_sensitive"
    );
    assert_eq!(
        observation(&tx, "password-correction")
            .unwrap()
            .unwrap()
            .intent,
        "control_or_sensitive"
    );
    assert_eq!(tx.query_row("SELECT count(*) FROM conversation_state_entities WHERE content_json LIKE '%synthetic_secret_value%'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    tx.execute(
        "UPDATE runs SET character_snapshot_json='{\"is_test\":true}' WHERE id='secret'",
        [],
    )
    .unwrap();
    assert!(state::scope_for_run_in(&tx, "secret").is_err());
    assert!(FocusPolicy {
        ttl_seconds: MAX_TTL_SECONDS + 1,
        ..Default::default()
    }
    .validate()
    .is_err());
    assert!(FocusPolicy {
        max_active_topics: 5,
        ..Default::default()
    }
    .validate()
    .is_err());
}
#[test]
fn management_scope_restores_real_channel_owner_without_minting_model_snapshot() {
    let mut db = database();
    add(
        &mut db,
        "source",
        "a",
        "管理员可审阅来源，但模型不继承管理员权限",
        AT,
        None,
    );
    db.execute("UPDATE runs SET status='completed' WHERE id='source'", [])
        .unwrap();
    db.execute(
        "INSERT INTO admins VALUES('admin','synthetic-admin','not-a-real-password',0)",
        [],
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    let admin = crate::auth::Admin {
        id: "admin".into(),
        username: "synthetic-admin".into(),
        role: "admin".into(),
        csrf_token: "synthetic".into(),
    };
    let scope = state::management_scope_for_source_in(&tx, &admin, "source").unwrap();
    assert_eq!(scope.owner_id(), OWNER);
    assert_eq!(scope.management_actor_id(), Some("admin"));
    assert!(state::validate_scope_in(&tx, &scope).unwrap());
    let model = state::scope_for_run_in(&tx, "source").unwrap();
    assert!(model.management_actor_id().is_none());
    assert_eq!(
        tx.query_row("SELECT count(*) FROM run_conversation_contexts", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
    let bad = crate::auth::Admin {
        role: "member".into(),
        ..admin
    };
    assert!(state::management_scope_for_source_in(&tx, &bad, "source").is_err());
}
#[test]
fn persisted_focus_recovers_after_connection_restart_with_source_and_ttl_checks() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("focus.sqlite");
    {
        let mut db = Connection::open(&path).unwrap();
        schema(&db);
        add(&mut db, "source", "a", "重启后合法话题", AT, None);
    }
    let mut db = Connection::open(&path).unwrap();
    conversation_state::initialize(&db).unwrap();
    add(&mut db, "follow", "a", "继续", AT + 2, None);
    assert_eq!(
        context(&mut db, "follow", AT + 2)
            .current_focus
            .unwrap()
            .source_run_id,
        "source"
    );
    let tx = db.transaction().unwrap();
    tx.execute(
        "INSERT INTO memory_context_watermarks VALUES('a',999999)",
        [],
    )
    .unwrap();
    assert!(context_for_run_in(&tx, "follow", AT + 3)
        .unwrap()
        .current_focus
        .is_none());
}

#[test]
fn a_frozen_context_crossing_ttl_is_rejected_without_rewriting_prompt_or_releasing_lease() {
    let mut db = database();
    add(&mut db, "topic", "a", "短期有效的当前话题", AT, None);
    add(&mut db, "consumer", "a", "继续", AT + 1, None);
    let selected = context(&mut db, "consumer", AT + 1);
    let tx = db.transaction().unwrap();
    state::record_usage_in(&tx, "consumer", &selected.entity_refs, AT + 1).unwrap();
    tx.execute(
        "CREATE TABLE native_worker_leases(run_id TEXT PRIMARY KEY,claimed_at INTEGER)",
        [],
    )
    .unwrap();
    tx.execute("INSERT INTO native_worker_leases VALUES('consumer',0)", [])
        .unwrap();
    tx.execute(
        "INSERT INTO run_prompt_snapshots VALUES('consumer','immutable-original-prompt')",
        [],
    )
    .unwrap();
    assert!(state::validate_context_at_in(&tx, "consumer", AT + 2).unwrap());
    assert!(!state::validate_context_at_in(&tx, "consumer", AT + DEFAULT_TTL_SECONDS + 1).unwrap());
    assert_eq!(
        tx.query_row(
            "SELECT messages_json FROM run_prompt_snapshots WHERE run_id='consumer'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "immutable-original-prompt"
    );
    assert_eq!(
        tx.query_row("SELECT count(*) FROM native_worker_leases", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
