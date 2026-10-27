//! Bounded public daily activities and one-minute schedule decisions. No chat
//! bodies, recipients, paths or URLs are accepted as executable instructions.
use crate::{character, rest_store as rest, schedule};
use anyhow::{ensure, Context};
use chrono::{NaiveTime, TimeZone};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const POLICY: &str = "schedule.personal.v1";
pub const SCRIPT: &str = include_str!("../../../tools/actions/schedule_decision.py");
const POEMS: &[(&str, &str, &str, &str)] = &[
    (
        "chunxiao",
        "春晓",
        "孟浩然",
        "春眠不觉晓，处处闻啼鸟。\n夜来风雨声，花落知多少。",
    ),
    (
        "jingyesi",
        "静夜思",
        "李白",
        "床前明月光，疑是地上霜。\n举头望明月，低头思故乡。",
    ),
    (
        "luchai",
        "鹿柴",
        "王维",
        "空山不见人，但闻人语响。\n返景入深林，复照青苔上。",
    ),
    (
        "jiangxue",
        "江雪",
        "柳宗元",
        "千山鸟飞绝，万径人踪灭。\n孤舟蓑笠翁，独钓寒江雪。",
    ),
];
fn exists(db: &Connection) -> anyhow::Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='daily_life_events')",
        [],
        |r| r.get(0),
    )?)
}
pub fn initialize(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS daily_life_events(
      id TEXT PRIMARY KEY,kind TEXT NOT NULL,policy_revision INTEGER NOT NULL,nominal_at INTEGER NOT NULL,
      due_at INTEGER NOT NULL,status TEXT NOT NULL DEFAULT 'planned',decision TEXT NOT NULL DEFAULT 'execute',
      decision_status TEXT NOT NULL DEFAULT 'pending',fence TEXT,attempts INTEGER NOT NULL DEFAULT 0,
      postponements INTEGER NOT NULL DEFAULT 0,error_code TEXT,request_sha256 TEXT,output_sha256 TEXT,
      notified_at INTEGER,decided_at INTEGER,finished_at INTEGER,operation_id TEXT,
      activity_id TEXT,activity_revision INTEGER,
      UNIQUE(kind,policy_revision,nominal_at));
      CREATE TABLE IF NOT EXISTS daily_life_offers(run_id TEXT PRIMARY KEY,policy_revision INTEGER NOT NULL,offer_json TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS daily_life_activities(activity_id TEXT PRIMARY KEY,poem_id TEXT NOT NULL,source_run_id TEXT UNIQUE,activity_revision INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS daily_life_decision_attempts(fence TEXT PRIMARY KEY,event_id TEXT NOT NULL,due_at INTEGER NOT NULL,notified_at INTEGER NOT NULL,finished_at INTEGER,status TEXT NOT NULL,decision TEXT,delay_minutes INTEGER,error_code TEXT,request_sha256 TEXT NOT NULL,output_sha256 TEXT);
      CREATE TABLE IF NOT EXISTS sleep_context_boundaries(conversation_id TEXT PRIMARY KEY,through_run_rowid INTEGER NOT NULL,cycle_id TEXT NOT NULL,created_at INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS sleep_context_uses(run_id TEXT PRIMARY KEY,through_run_rowid INTEGER NOT NULL);
      UPDATE daily_life_events SET decision_status='failed',error_code='decision_process_restarted',fence=NULL WHERE decision_status='running';
      UPDATE daily_life_decision_attempts SET status='interrupted',error_code='decision_process_restarted' WHERE status='running';")?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS daily_life_texts(activity_id TEXT PRIMARY KEY,source_owner_id TEXT NOT NULL,source_conversation_id TEXT NOT NULL,title TEXT NOT NULL,content TEXT NOT NULL,content_sha256 TEXT NOT NULL,prepared_at INTEGER NOT NULL);")?;
    let p = personal_policy(db)?;
    if p.get("format").is_none() && p.get("grant_id").and_then(Value::as_str).is_some() {
        db.execute_batch("SAVEPOINT generalize_daily_life")?;
        let result = (|| -> anyhow::Result<()> {
            let owner = p["owner_id"].as_str().context("daily_life_owner_missing")?;
            let id = p["grant_id"].as_str().unwrap();
            let g = schedule::grant_detail(db, owner, id)?;
            if g["revoked_at"].is_null() && g["policy"]["title"] == "嘉卉的抄诗小安排" {
                let mut policy = g["policy"].clone();
                policy["title"] = json!("日常自主安排");
                schedule::change_grant(
                    db,
                    owner,
                    id,
                    g["revision"].as_i64().unwrap(),
                    Some(&policy),
                    crate::state::now(),
                )?;
            }
            let mut p = p.clone();
            p["format"] = json!("text_v1");
            p["revision"] = json!(p["revision"].as_i64().unwrap_or(0) + 1);
            db.execute(
                "UPDATE settings SET value=?1 WHERE key=?2",
                params![p.to_string(), POLICY],
            )?;
            Ok(())
        })();
        if let Err(error) = result {
            db.execute_batch("ROLLBACK TO generalize_daily_life; RELEASE generalize_daily_life")?;
            return Err(error);
        }
        db.execute_batch("RELEASE generalize_daily_life")?;
    }
    Ok(())
}
pub fn initialize_plan_cache(db: &Connection) -> anyhow::Result<()> {
    let columns = db
        .prepare("PRAGMA table_info(rest_deferred_presentations)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    db.execute_batch("SAVEPOINT self_plan_cache")?;
    for (name, sql) in [
        (
            "self_plan_status",
            "ALTER TABLE rest_deferred_presentations ADD COLUMN self_plan_status TEXT",
        ),
        (
            "self_plan_json",
            "ALTER TABLE rest_deferred_presentations ADD COLUMN self_plan_json TEXT",
        ),
        (
            "self_plan_hash",
            "ALTER TABLE rest_deferred_presentations ADD COLUMN self_plan_hash TEXT",
        ),
    ] {
        if !columns.iter().any(|c| c == name) {
            db.execute_batch(sql)?;
        }
    }
    db.execute("UPDATE rest_deferred_presentations SET self_plan_status='interrupted',self_plan_json=NULL WHERE self_plan_status='checking'",[])?;
    db.execute_batch("RELEASE self_plan_cache")?;
    Ok(())
}
pub fn personal_policy(db: &Connection) -> anyhow::Result<Value> {
    Ok(db
        .query_row("SELECT value FROM settings WHERE key=?1", [POLICY], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
        .map(|s| serde_json::from_str(&s))
        .transpose()?
        .unwrap_or(json!({"revision":0,"enabled":false,"daily_limit":4})))
}
pub fn save_personal_policy(
    db: &Connection,
    actor: &str,
    revision: i64,
    enabled: bool,
    limit: i64,
    at: i64,
) -> anyhow::Result<Value> {
    ensure!(!db.is_autocommit(), "daily_life_transaction_required");
    let old = personal_policy(db)?;
    ensure!(old["revision"] == revision, "daily_life_policy_conflict");
    ensure!((1..=8).contains(&limit), "daily_life_limit_invalid");
    ensure!(
        old["owner_id"].as_str().is_none_or(|id| id == actor),
        "daily_life_policy_owner_mismatch"
    );
    if let Some(id) = old["grant_id"].as_str() {
        let grant = schedule::grant_detail(db, actor, id)?;
        if grant["revoked_at"].is_null() {
            schedule::change_grant(db, actor, id, grant["revision"].as_i64().unwrap(), None, at)?;
        }
    }
    let grant = if enabled {
        Some(schedule::create_grant(db,actor,&json!({"title":"日常自主安排","allowed_types":["writing"],"resource_ids":[],"time_zone":"Asia/Shanghai","valid_from":at,"valid_until":at+365*86400,"horizon_days":1,"window_start":"07:00","window_end":"23:00","max_daily_activities":limit,"max_daily_minutes":limit*10,"max_daily_changes":limit*3}),at)?["id"].clone())
    } else {
        None
    };
    let value = json!({"revision":revision+1,"enabled":enabled,"owner_id":actor,"grant_id":grant,"daily_limit":limit,"format":"text_v1","valid_until":if enabled{Some(at+365*86400)}else{None}});
    db.execute(
        "INSERT INTO settings VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![POLICY, value.to_string()],
    )?;
    db.execute("INSERT INTO audit_events(actor,action,resource,created_at) VALUES(?1,'schedule.personal.policy',?2,?3)",params![actor,format!("revision:{}",revision+1),at])?;
    Ok(value)
}
pub fn context_floor(db: &Connection, conversation: &str) -> anyhow::Result<i64> {
    if !exists(db)? {
        return Ok(0);
    }
    Ok(db
        .query_row(
            "SELECT through_run_rowid FROM sleep_context_boundaries WHERE conversation_id=?1",
            [conversation],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}
pub fn history_floor(db: &Connection, conversation: &str) -> anyhow::Result<i64> {
    Ok(crate::memory_context::history_floor(db, conversation)?
        .max(context_floor(db, conversation)?))
}
pub fn freeze_context_in(db: &Connection, cycle: &str, at: i64) -> anyhow::Result<()> {
    if !exists(db)? || !rest::policy_in(db)?.1.reset_context_on_sleep {
        return Ok(());
    }
    // This is a prompt boundary, never a deletion or a source revocation. Logs,
    // diaries, grants and the in-flight transport retain their original records.
    db.execute("INSERT INTO sleep_context_boundaries SELECT conversation_id,max(rowid),?1,?2 FROM runs GROUP BY conversation_id ON CONFLICT(conversation_id) DO UPDATE SET through_run_rowid=MAX(through_run_rowid,excluded.through_run_rowid),cycle_id=excluded.cycle_id,created_at=excluded.created_at",params![cycle,at])?;
    Ok(())
}
pub fn freeze_run_in(db: &Connection, run: &str, conversation: &str) -> anyhow::Result<()> {
    if exists(db)? {
        db.execute(
            "INSERT OR IGNORE INTO sleep_context_uses VALUES(?1,?2)",
            params![run, context_floor(db, conversation)?],
        )?;
    }
    Ok(())
}
pub fn context_valid(db: &Connection, run: &str) -> anyhow::Result<bool> {
    if !exists(db)? {
        return Ok(true);
    }
    let row:Option<(i64,String)>=db.query_row("SELECT u.through_run_rowid,r.conversation_id FROM sleep_context_uses u JOIN runs r ON r.id=u.run_id WHERE r.id=?1",[run],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    Ok(match row {
        Some((floor, c)) => floor == context_floor(db, &c)?,
        None => true,
    })
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub poem_id: String,
    pub slot_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    reply: Vec<crate::chat_reply::ReplyItem>,
    self_activity: Option<Selection>,
}
pub fn split_reply(raw: &str) -> Result<(String, Option<Selection>), String> {
    let trimmed = raw.trim();
    let candidate = trimmed
        .strip_prefix("```json\n")
        .or_else(|| trimmed.strip_prefix("```json\r\n"))
        .and_then(|s| s.strip_suffix("```"))
        .unwrap_or(trimmed)
        .trim();
    if !candidate.starts_with('{') {
        return Ok((raw.to_owned(), None));
    }
    let value = crate::trigger_rule::strict_value(candidate.as_bytes())
        .map_err(|_| "回复JSON无效".to_owned())?;
    if value.get("reply").is_none() {
        return Ok((raw.to_owned(), None));
    }
    let envelope: Envelope =
        serde_json::from_value(value).map_err(|_| "日程回复字段无效".to_owned())?;
    Ok((
        serde_json::to_string(&envelope.reply).map_err(|_| "回复无效".to_owned())?,
        envelope.self_activity,
    ))
}

pub fn selection_for_reply(
    db: &Connection,
    run: &str,
    raw: &str,
    _spoken: &str,
) -> anyhow::Result<Option<Selection>> {
    let (_, selection) = split_reply(raw).map_err(anyhow::Error::msg)?;
    if selection.is_some() {
        return Ok(selection);
    }
    let cached:Option<String>=db.query_row("SELECT self_plan_json FROM rest_deferred_presentations WHERE run_id=?1 AND self_plan_status='checked' AND self_plan_hash=?2",params![run,character::digest(raw.as_bytes())],|r|r.get(0)).optional()?.flatten();
    cached
        .map(|s| serde_json::from_str::<Value>(&s).map(|v| v["self_activity"].clone()))
        .transpose()?
        .filter(|v| !v.is_null())
        .map(serde_json::from_value)
        .transpose()
        .map_err(Into::into)
}

/// One optional, bounded semantic check on a newly generated self-intention.
/// It sees the generated reply and public slot metadata, never another source.
pub async fn complete_reply_plan(
    state: &crate::state::AppState,
    run: &str,
    config: &crate::config::RuntimeConfig,
    raw: String,
) -> Result<String, String> {
    let (reply, selection) = split_reply(&raw)?;
    if selection.is_some() {
        return Ok(raw);
    }
    let plan = crate::chat_reply::parse(&reply, &config.reply)?;
    let spoken = plan
        .items
        .iter()
        .map(|i| i.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if !spoken.contains('我')
        || ![
            "去",
            "趁",
            "准备",
            "打算",
            "先",
            "现在",
            "这会",
            "待会",
            "接下来",
            "安排",
            "写",
            "读",
            "整理",
        ]
        .iter()
        .any(|p| spoken.contains(p))
    {
        return Ok(raw);
    }
    let offer = state
        .db(|db| {
            let raw: Option<String> = db
                .query_row(
                    "SELECT offer_json FROM daily_life_offers WHERE run_id=?1",
                    [run],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(raw.map(|s| serde_json::from_str::<Value>(&s)).transpose()?)
        })
        .map_err(|_| "日程状态暂不可用".to_owned())?;
    let offer = offer.unwrap_or_else(|| json!({"slots":[]}));
    let input =
        json!({"reply":spoken,"slots":offer["slots"],"capabilities":["save_text_artifact"]});
    let hash = character::digest(raw.as_bytes());
    let checked=state.db(|db| {
        let cached:(Option<String>,Option<String>,Option<String>)=db.query_row("SELECT self_plan_status,self_plan_json,self_plan_hash FROM rest_deferred_presentations WHERE run_id=?1",[run],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        if let Some(status)=cached.0 {
            anyhow::ensure!(status=="checked" && cached.2.as_deref()==Some(&hash),"self_plan_prior_attempt_unconfirmed");
            return Ok(cached.1);
        }
        db.execute("UPDATE rest_deferred_presentations SET self_plan_status='checking',self_plan_hash=?1 WHERE run_id=?2 AND self_plan_status IS NULL",params![hash,run])?;
        Ok(None)
    }).map_err(|_|"此前日程核对尚未确认，未重复请求".to_owned())?;
    let messages = vec![
        json!({"role":"system","content":"DSH_SELF_PLAN_COMMIT_V2\n核对下面回复是否明确决定了自己接下来要做的一件事。引用、问题、否定、过去经历、一般愿望、只是答应以后登记、以及对用户的建议都不是新安排；没有明确安排就返回{\"self_activity\":null}。当前可执行能力是保存一份文字成果；能以文字落实的安排可以使用这项能力，不要求回复使用技术术语。没有外出、物理动作、外发或任意代码能力；没有所给时段时也不能登记。若回复明确承诺了当前做不到的新活动，返回{\"self_activity\":null,\"unfulfilled\":true}，不能把未落实的承诺当成没有安排。若是可执行的新安排，返回{\"self_activity\":{\"slot_id\":\"所给时段ID\",\"title\":\"简短安排名称\",\"content\":\"要保存的完整文字成果\"}}；只选择一个现有时段，不编造权限、时间或完成状态。成果最多3000字，不需要为了使用能力而创造安排。只返回此JSON，不添加解释或其他字段。"}),
        json!({"role":"user","content":input.to_string()}),
    ];
    let output = if let Some(cached) = checked {
        cached
    } else {
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            crate::worker::model_request(state, config, messages, 4096),
        )
        .await
        .map_err(|_| "日程核对超时，未发布未落实的安排".to_owned())??
    };
    let output = output
        .trim()
        .strip_prefix("```json\n")
        .and_then(|s| s.strip_suffix("```"))
        .unwrap_or(output.trim())
        .trim();
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Decision {
        self_activity: Option<Selection>,
        #[serde(default)]
        unfulfilled: bool,
    }
    let value = crate::trigger_rule::strict_value(output.as_bytes())
        .map_err(|_| "日程核对返回无效".to_owned())?;
    if value.get("self_activity").is_none() {
        return Err("日程核对没有明确返回决定".into());
    }
    let decision: Decision =
        serde_json::from_value(value.clone()).map_err(|_| "日程核对字段无效".to_owned())?;
    state.db(|db| {anyhow::ensure!(db.execute("UPDATE rest_deferred_presentations SET self_plan_status='checked',self_plan_json=?1 WHERE run_id=?2 AND self_plan_hash=?3 AND self_plan_status IN('checking','checked')",params![value.to_string(),run,hash])?==1,"self_plan_cache_invalidated");Ok(())}).map_err(|_|"日程核对结果未获保存".to_owned())?;
    if decision.unfulfilled {
        return Err("未落实的安排承诺未发布".into());
    }
    match decision.self_activity {
        Some(selection)=>Ok(json!({"reply":serde_json::from_str::<Value>(&reply).unwrap_or_else(|_|json!(plan.items)),"self_activity":selection}).to_string()),
        None=>Ok(raw),
    }
}

pub fn completion_prompt(db: &Connection, run: &str) -> anyhow::Result<String> {
    if !exists(db)? {
        return Ok(String::new());
    }
    let raw: Option<String> = db
        .query_row(
            "SELECT offer_json FROM daily_life_offers WHERE run_id=?1",
            [run],
            |r| r.get(0),
        )
        .optional()?;
    let offered = raw
        .map(|s| serde_json::from_str::<Value>(&s))
        .transpose()?
        .is_some_and(|v| v["slots"].as_array().is_some_and(|a| !a.is_empty()));
    Ok(if offered {
        "\nDSH_DAILY_LIFE_COMPLETION_V2\n若本轮明确决定做一件当前可执行的事，最终输出使用{\"reply\":[上述逐句回复对象],\"self_activity\":{\"slot_id\":\"本轮时段ID\",\"title\":\"安排名称\",\"content\":\"完整文字成果\"}}。日程与回复同一事务受理，全部句数、间隔及媒体限制继续约束reply。没有安排就返回原数组，不为表现生活感而硬添安排；不能只说要做却不登记，不能虚构外出、物理动作、外发或已完成。".into()
    } else {
        String::new()
    })
}

pub fn prompt_in(db: &Connection, run: &str, at: i64) -> anyhow::Result<String> {
    if !exists(db)? {
        return Ok(String::new());
    }
    let p = personal_policy(db)?;
    let (source_owner, conversation): (String, String) = db.query_row(
        "SELECT owner_id,conversation_id FROM runs WHERE id=?1",
        [run],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let floor = history_floor(db, &conversation)?;
    let mut offer = json!({"slots":[]});
    let grants=db.prepare("SELECT owner_id,id FROM schedule_autonomy_grants WHERE revoked_at IS NULL ORDER BY created_at DESC LIMIT 100")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<Result<Vec<_>,_>>()?;
    for (owner, gid) in grants {
        let gv = schedule::grant_detail(db, &owner, &gid)?;
        let grant: schedule::Grant = serde_json::from_value(gv["policy"].clone())?;
        if !grant.allowed_types.iter().any(|t| t == "writing")
            || at < grant.valid_from
            || at >= grant.valid_until
        {
            continue;
        }
        let plan = schedule::Plan {
            goal: "自主安排".into(),
            grant_id: gid.clone(),
            resource_ids: vec![],
            duration_minutes: 10,
            state: "enabled".into(),
            planning_mode: "bounded_rule".into(),
        };
        let slots = schedule::planning_slots_for_kind(
            db,
            &owner,
            &plan,
            &grant,
            gv["revision"].as_i64().unwrap(),
            at,
            "writing",
        )?;
        if slots.is_empty() {
            continue;
        }
        offer = json!({"slots":slots.into_iter().take(3).collect::<Vec<_>>(),"owner_id":owner,"grant_id":gid,"grant_revision":gv["revision"]});
        break;
    }
    db.execute(
        "INSERT OR IGNORE INTO daily_life_offers VALUES(?1,?2,?3)",
        params![run, p["revision"].as_i64().unwrap_or(0), offer.to_string()],
    )?;
    let recent=db.prepare("SELECT a.title,json_extract(a.definition_json,'$.start_at'),json_extract(a.definition_json,'$.end_at'),a.state,o.status FROM daily_life_activities l JOIN schedule_activities a ON a.id=l.activity_id JOIN runs r ON r.id=l.source_run_id LEFT JOIN schedule_occurrences o ON o.activity_id=a.id WHERE r.owner_id=?1 AND r.rowid>?2 ORDER BY a.created_at DESC LIMIT 4")?.query_map(params![source_owner,floor],|r|Ok(json!({"title":r.get::<_,String>(0)?,"start_at":r.get::<_,i64>(1)?,"end_at":r.get::<_,i64>(2)?,"state":r.get::<_,String>(3)?,"status":r.get::<_,Option<String>>(4)?})))?.collect::<Result<Vec<_>,_>>()?;
    offer.as_object_mut().unwrap().remove("owner_id");
    offer.as_object_mut().unwrap().remove("grant_id");
    Ok(format!("\nDSH_DAILY_LIFE_V2\n以下是本次实际安排与可用时段，不是爱好设定，也不要求每轮制造活动。具体想做什么由眼前交流与自己的选择决定。当前可执行操作是在所选时段保存一份文字成果，正文通过self_activity提供；不要承诺当前没有能力执行的外部或物理动作。只根据实际记录回答是否已经安排、开始或完成，不重复登记已经存在的同一安排。没有可用时段就自然聊天或表达想法。\n{}",json!({"available":offer,"actual_activities":recent})))
}

pub fn commit_selection(
    db: &Connection,
    run: &str,
    selection: &Selection,
    at: i64,
) -> anyhow::Result<Value> {
    let p = personal_policy(db)?;
    let (rev, raw): (i64, String) = db.query_row(
        "SELECT policy_revision,offer_json FROM daily_life_offers WHERE run_id=?1",
        [run],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if selection.poem_id.is_empty() {
        return commit_text(
            db,
            run,
            selection,
            &serde_json::from_str::<Value>(&raw)?,
            at,
        );
    }
    ensure!(
        p["enabled"] == true && p["revision"] == rev,
        "daily_life_policy_changed"
    );
    let offer: Value = serde_json::from_str(&raw)?;
    let slot = offer["slots"]
        .as_array()
        .and_then(|a| a.iter().find(|v| v["slot_id"] == selection.slot_id))
        .context("daily_life_unknown_slot")?;
    let (_, title, author, _) = POEMS
        .iter()
        .find(|p| p.0 == selection.poem_id)
        .context("daily_life_unknown_poem")?;
    let owner = p["owner_id"].as_str().context("daily_life_owner_missing")?;
    let gid = p["grant_id"].as_str().context("daily_life_grant_missing")?;
    let grant = schedule::grant_detail(db, owner, gid)?;
    ensure!(
        grant["revision"] == offer["grant_revision"] && grant["revoked_at"].is_null(),
        "daily_life_grant_changed"
    );
    let activity = json!({"title":format!("抄诗：《{title}》"),"type":"writing","purpose":format!("电子抄录{author}《{title}》，保存完整诗文。"),"start_at":slot["start_at"],"end_at":slot["end_at"],"time_zone":"Asia/Shanghai","resource_id":null,"target_ref":null,"repeat":null,"movable":true,"grant_id":gid});
    let result = schedule::create(db, owner, &activity, &format!("personal:{run}"), true, at)?;
    ensure!(
        result["state"] == "enabled",
        "daily_life_activity_not_authorized"
    );
    db.execute(
        "INSERT OR IGNORE INTO daily_life_activities VALUES(?1,?2,?3,1)",
        params![result["id"].as_str(), selection.poem_id, run],
    )?;
    Ok(result)
}
fn commit_text(
    db: &Connection,
    run: &str,
    s: &Selection,
    offer: &Value,
    at: i64,
) -> anyhow::Result<Value> {
    let title = s.title.as_deref().unwrap_or("").trim();
    let content = s.content.as_deref().unwrap_or("").trim();
    ensure!(
        !title.is_empty()
            && title.chars().count() <= 80
            && !content.is_empty()
            && content.len() <= 12000
            && content.chars().count() <= 3000
            && !title.contains('\0')
            && !content.contains('\0'),
        "daily_life_invalid_text"
    );
    let (source, conversation): (String, String) = db.query_row(
        "SELECT owner_id,conversation_id FROM runs WHERE id=?1",
        [run],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let hash = character::digest(content.as_bytes());
    let slot = offer["slots"]
        .as_array()
        .and_then(|a| a.iter().find(|v| v["slot_id"] == s.slot_id))
        .context("daily_life_unknown_slot")?;
    let owner = offer["owner_id"]
        .as_str()
        .context("daily_life_owner_missing")?;
    let gid = offer["grant_id"]
        .as_str()
        .context("daily_life_grant_missing")?;
    let grant = schedule::grant_detail(db, owner, gid)?;
    ensure!(
        grant["revision"] == offer["grant_revision"] && grant["revoked_at"].is_null(),
        "daily_life_grant_changed"
    );
    let duplicate:Option<(String,String)>=db.query_row("SELECT a.id,a.owner_id FROM daily_life_texts t JOIN schedule_activities a ON a.id=t.activity_id LEFT JOIN schedule_occurrences o ON o.activity_id=a.id WHERE t.source_owner_id=?1 AND t.content_sha256=?2 AND a.state='enabled' AND a.grant_id=?4 AND a.grant_revision=?5 AND (json_extract(a.definition_json,'$.start_at')>=?3 OR (o.status IN('pending','running') AND o.end_at>?3)) LIMIT 1",params![source,hash,at,gid,grant["revision"].as_i64().unwrap()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((id, owner)) = duplicate {
        return schedule::detail(db, &owner, &id);
    }
    let activity = json!({"title":title,"type":"writing","purpose":"保存文字成果并记录实际执行状态","start_at":slot["start_at"],"end_at":slot["end_at"],"time_zone":slot["time_zone"],"resource_id":null,"target_ref":null,"repeat":null,"movable":true,"grant_id":gid});
    let result = schedule::create(db, owner, &activity, &format!("personal:{run}"), true, at)?;
    ensure!(
        result["state"] == "enabled",
        "daily_life_activity_not_authorized"
    );
    let id = result["id"]
        .as_str()
        .context("daily_life_missing_activity")?;
    db.execute(
        "INSERT OR IGNORE INTO daily_life_activities VALUES(?1,'text_v1',?2,1)",
        params![id, run],
    )?;
    db.execute(
        "INSERT OR IGNORE INTO daily_life_texts VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![id, source, conversation, title, content, hash, at],
    )?;
    Ok(result)
}
fn execute_text(db: &Connection, id: &str, activity: &str, at: i64) -> anyhow::Result<()> {
    let (owner,conversation,title,content,hash,prepared):(String,String,String,String,String,i64)=db.query_row("SELECT source_owner_id,source_conversation_id,title,content,content_sha256,prepared_at FROM daily_life_texts WHERE activity_id=?1",[activity],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
    let source_order:i64=db.query_row("SELECT r.rowid FROM runs r JOIN daily_life_activities l ON l.source_run_id=r.id WHERE l.activity_id=?1",[activity],|r|r.get(0))?;
    if source_order <= crate::memory_context::history_floor(db, &conversation)? {
        db.execute("UPDATE schedule_occurrences SET status='blocked',block_reason='source_forgotten',updated_at=?1 WHERE id=?2",params![at,id])?;
        db.execute(
            "UPDATE daily_life_texts SET content='',content_sha256='' WHERE activity_id=?1",
            [activity],
        )?;
        return Ok(());
    }
    let result = json!({"method":"saved_text_artifact","title":title,"text":content,"sha256":hash,"prepared_at":prepared,"saved_at":at,"delivery_status":"not_applicable"});
    db.execute("UPDATE schedule_occurrences SET status='completed',result_json=?1,started_at=?2,completed_at=?2,checkpoint=?3,total_chars=?3,block_reason=NULL,updated_at=?2 WHERE id=?4 AND status='pending'",params![result.to_string(),at,content.chars().count(),id])?;
    // Never promote generated source-private content into shared diary facts.
    let record: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM diary_source_policies WHERE source_owner_id=?1 AND enabled=1)",
        [&owner],
        |r| r.get(0),
    )?;
    if record {
        let actor: String = db.query_row(
            "SELECT owner_id FROM schedule_activities WHERE id=?1",
            [activity],
            |r| r.get(0),
        )?;
        let event = crate::diary::note_in(
            db,
            &actor,
            &crate::diary::Note {
                content: "完成了一项已登记的文字安排，成果保存在对应日程中。".into(),
                source_conversation_id: Some(conversation),
                visibility: "private".into(),
                publish: false,
                happened_at: Some(at),
            },
        )
        .map_err(|_| anyhow::anyhow!("daily_life_diary_failed"))?;
        db.execute(
            "UPDATE diary_events SET origin_type='schedule_activity',origin_id=?1 WHERE id=?2",
            params![id, event.id],
        )?;
    }
    db.execute(
        "UPDATE daily_life_events SET status='completed',finished_at=?1 WHERE activity_id=?2",
        params![at, activity],
    )?;
    Ok(())
}
pub fn execute_writing(db: &Connection, at: i64) -> anyhow::Result<()> {
    if !exists(db)? {
        return Ok(());
    }
    let row:Option<(String,String,String,i64)>=db.query_row("SELECT o.id,o.activity_id,l.poem_id,l.activity_revision FROM schedule_occurrences o JOIN daily_life_activities l ON l.activity_id=o.activity_id JOIN schedule_activities a ON a.id=o.activity_id WHERE o.status='pending' AND a.state='enabled' AND a.revision=l.activity_revision AND o.activity_revision=l.activity_revision AND o.start_at<=?1 AND o.end_at>?1 ORDER BY o.start_at LIMIT 1",[at],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    if let Some((id, activity, poem, _)) = row {
        if poem == "text_v1" {
            return execute_text(db, &id, &activity, at);
        }
        let (_, title, author, text) = POEMS
            .iter()
            .find(|p| p.0 == poem)
            .context("daily_life_unknown_poem")?;
        let result = json!({"method":"electronic_poetry_copy","title":title,"author":author,"text":text,"source":"bundled_public_domain_poetry","sha256":character::digest(text.as_bytes()),"delivery_status":"not_applicable"});
        db.execute("UPDATE schedule_occurrences SET status='completed',result_json=?1,started_at=?2,completed_at=?2,checkpoint=?3,total_chars=?3,block_reason=NULL,updated_at=?2 WHERE id=?4 AND status='pending'",params![result.to_string(),at,text.chars().count(),id])?;
        // Only catalog text and the actual result are shared. No originating
        // user's message, owner, conversation or private resource is published.
        let owner: String = db.query_row(
            "SELECT owner_id FROM schedule_activities WHERE id=?1",
            [&activity],
            |r| r.get(0),
        )?;
        if db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='diary_events')",
            [],
            |r| r.get::<_, bool>(0),
        )? {
            let event = crate::diary::note_in(
                db,
                &owner,
                &crate::diary::Note {
                    content: format!("已完成电子抄录：{author}《{title}》。\n{text}"),
                    source_conversation_id: None,
                    visibility: "shared".into(),
                    publish: true,
                    happened_at: Some(at),
                },
            )
            .map_err(|_| anyhow::anyhow!("poetry_diary_record_failed"))?;
            db.execute(
                "UPDATE diary_events SET origin_type='schedule_activity',origin_id=?1 WHERE id=?2",
                params![id, event.id],
            )?;
        }
        db.execute(
            "UPDATE daily_life_events SET status='completed',finished_at=?1 WHERE activity_id=?2",
            params![at, activity],
        )?;
    }
    Ok(())
}

fn civil(at: i64, zone: chrono_tz::Tz, time: &str) -> anyhow::Result<Option<i64>> {
    let date = chrono::DateTime::from_timestamp(at, 0)
        .context("invalid_routine_time")?
        .with_timezone(&zone)
        .date_naive();
    let time = NaiveTime::parse_from_str(time, "%H:%M")?;
    Ok(match zone.from_local_datetime(&date.and_time(time)) {
        chrono::LocalResult::Single(v) => Some(v.timestamp()),
        _ => None,
    })
}
pub fn routine_tick(db: &Connection, clock: &rest::ClockSample, at: i64) -> anyhow::Result<()> {
    if !exists(db)? {
        return Ok(());
    }
    let (revision, p) = rest::policy_in(db)?;
    db.execute("UPDATE daily_life_events SET status='cancelled',error_code='routine_policy_changed',finished_at=?1 WHERE policy_revision<>?2 AND status='planned'",params![at,revision])?;
    if !p.schedule_enabled || !p.anticipation_enabled {
        return Ok(());
    }
    let trusted: bool = db.query_row(
        "SELECT clock_trusted FROM trigger_runtime WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    if !trusted {
        return Ok(());
    }
    db.execute("UPDATE daily_life_events SET status='completed',finished_at=?1 WHERE kind='sleep' AND status='running' AND operation_id IN(SELECT o.id FROM rest_operations o JOIN rest_cycles c ON json_extract(o.result_json,'$.cycle_id')=c.id WHERE c.finished_at IS NOT NULL)",[at])?;
    let zone = p.time_zone.parse()?;
    for (activity_id, activity_revision, nominal, a) in schedule::notice_candidates(db, at)? {
        db.execute("INSERT OR IGNORE INTO daily_life_events(id,kind,policy_revision,nominal_at,due_at,activity_id,activity_revision) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![format!("activity:{activity_id}:{nominal}"),a.kind,revision,nominal,a.start_at,activity_id,activity_revision])?;
    }
    for (kind, time) in [
        ("sleep", p.sleep_local.as_deref()),
        ("wake", p.wake_local.as_deref()),
    ] {
        let Some(time) = time else { continue };
        let Some(due) = civil(at, zone, time)? else {
            continue;
        };
        if at >= due - 60 && at < due + 60 {
            db.execute("INSERT OR IGNORE INTO daily_life_events(id,kind,policy_revision,nominal_at,due_at) VALUES(?1,?2,?3,?4,?4)",params![format!("{kind}:{revision}:{due}"),kind,revision,due])?;
        }
    }
    let rows=db.prepare("SELECT id,kind,due_at,decision FROM daily_life_events WHERE policy_revision=?1 AND kind IN('sleep','wake') AND status='planned' AND due_at<=?2 ORDER BY due_at LIMIT 8")?.query_map(params![revision,at],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?)))?.collect::<Result<Vec<_>,_>>()?;
    for (id, kind, due, decision) in rows {
        if at - due > 60 {
            db.execute("UPDATE daily_life_events SET status='skipped',error_code='missed_routine_window',finished_at=?1 WHERE id=?2",params![at,id])?;
            continue;
        }
        let state = rest::state_in(db)?;
        if decision == "cancel" {
            continue;
        }
        if kind == "sleep" && state.phase == rest::RestPhase::Awake {
            let requester = rest::requester_for_decided_schedule_in(db, &id, clock)?;
            let operation = rest::sleep_in(
                db,
                &requester,
                state.generation,
                &id,
                &rest::SleepRequest { wake_at: None },
                clock,
            )?;
            db.execute(
                "UPDATE daily_life_events SET status='running',operation_id=?1 WHERE id=?2",
                params![operation.id, id],
            )?;
        } else {
            db.execute(
                "UPDATE daily_life_events SET status=?1,finished_at=?2 WHERE id=?3",
                params![
                    if kind == "wake"
                        && (matches!(
                            state.phase,
                            rest::RestPhase::Awake | rest::RestPhase::Waking
                        ) || state.wake_at.is_some_and(|v| v <= at))
                    {
                        "completed"
                    } else {
                        "skipped"
                    },
                    at,
                    id
                ],
            )?;
        }
    }
    Ok(())
}
#[derive(Clone, Debug)]
pub struct Decision {
    pub id: String,
    pub kind: String,
    pub due_at: i64,
    pub policy_revision: i64,
    pub fence: String,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    pub decision: String,
    #[serde(default)]
    pub delay_minutes: i64,
}
pub fn prepare_decision(
    db: &Connection,
    at: i64,
) -> anyhow::Result<Option<(Decision, Vec<Value>)>> {
    if !exists(db)? {
        return Ok(None);
    }
    let (revision, p) = rest::policy_in(db)?;
    if !p.anticipation_enabled {
        return Ok(None);
    }
    let row:Option<(String,String,i64,i64)>=db.query_row("SELECT id,kind,due_at,policy_revision FROM daily_life_events WHERE status='planned' AND decision_status='pending' AND attempts<1 AND due_at>?1 AND due_at<=?1+60 AND policy_revision=?2 ORDER BY due_at LIMIT 1",params![at,revision],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let Some((id, kind, due_at, policy_revision)) = row else {
        return Ok(None);
    };
    let fence = uuid::Uuid::new_v4().to_string();
    let (_, config) = character::current_config_in(db)?;
    let persona = character::effective_personality_prompt(db, &config)?;
    let event = json!({"event_id":id,"kind":kind,"due_at":due_at,"seconds_until":due_at-at,"time_zone":p.time_zone,"phase":rest::state_in(db)?.phase,"choices":["execute","cancel","postpone"],"max_postpone_minutes":30,"max_postponements":2});
    let mut event = event;
    if let Some(aid) = db.query_row(
        "SELECT activity_id FROM daily_life_events WHERE id=?1",
        [&id],
        |r| r.get::<_, Option<String>>(0),
    )? {
        let raw: String = db.query_row(
            "SELECT definition_json FROM schedule_activities WHERE id=?1",
            [&aid],
            |r| r.get(0),
        )?;
        let activity: Value = serde_json::from_str(&raw)?;
        event["activity"] = json!({"title":activity["title"],"purpose":activity["purpose"]});
    }
    let messages = vec![
        json!({"role":"system","content":format!("{persona}\nDSH_SCHEDULE_NOTICE_V1\n还有一分钟将到下列日程。选择如期执行execute、取消本次cancel，或推迟postpone。只输出工具参数JSON：{{\"decision\":\"execute\",\"delay_minutes\":0}}；推迟时delay_minutes为1到30。宿主将调用schedule_decision.py检查并受理；每次日程最多推迟两次，不修改每日固定时刻。无需聊天、道别或向任何人发送消息，不声称操作已经完成。没有其他信息时选择如期执行。" )}),
        json!({"role":"user","content":event.to_string()}),
    ];
    db.execute("UPDATE daily_life_events SET decision_status='running',fence=?1,attempts=attempts+1,notified_at=?2,request_sha256=?3 WHERE id=?4",params![fence,at,character::digest(&serde_json::to_vec(&messages)?),id])?;
    db.execute("INSERT INTO daily_life_decision_attempts(fence,event_id,due_at,notified_at,status,request_sha256) VALUES(?1,?2,?3,?4,'running',?5)",params![fence,id,due_at,at,character::digest(&serde_json::to_vec(&messages)?)])?;
    Ok(Some((
        Decision {
            id,
            kind,
            due_at,
            policy_revision,
            fence,
        },
        messages,
    )))
}
pub fn fail_decision(db: &Connection, d: &Decision, code: &str, at: i64) -> anyhow::Result<()> {
    db.execute("UPDATE daily_life_events SET decision_status='failed',error_code=?1,fence=NULL,decided_at=?2 WHERE id=?3 AND fence=?4 AND decision_status='running'",params![code,at,d.id,d.fence])?;
    db.execute("UPDATE daily_life_decision_attempts SET status='failed',finished_at=?1,error_code=?2 WHERE fence=?3 AND status='running'",params![at,code,d.fence])?;
    Ok(())
}
pub fn commit_decision(
    db: &Connection,
    d: &Decision,
    choice: &Choice,
    raw: &str,
    at: i64,
) -> anyhow::Result<()> {
    ensure!(
        db.query_row(
            "SELECT clock_trusted FROM trigger_runtime WHERE id=1",
            [],
            |r| r.get::<_, bool>(0)
        )?,
        "schedule_decision_clock_untrusted"
    );
    ensure!(
        ["execute", "cancel", "postpone"].contains(&choice.decision.as_str()),
        "invalid_schedule_decision"
    );
    ensure!(
        if choice.decision == "postpone" {
            (1..=30).contains(&choice.delay_minutes)
        } else {
            choice.delay_minutes == 0
        },
        "invalid_schedule_delay"
    );
    let (rev, p) = rest::policy_in(db)?;
    ensure!(
        p.anticipation_enabled && rev == d.policy_revision && at < d.due_at,
        "schedule_decision_stale"
    );
    let (status, fence, due, postponements): (String, Option<String>, i64, i64) = db.query_row(
        "SELECT status,fence,due_at,postponements FROM daily_life_events WHERE id=?1",
        [&d.id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    ensure!(
        status == "planned" && fence.as_deref() == Some(&d.fence) && due == d.due_at,
        "schedule_decision_fence_stale"
    );
    ensure!(
        choice.decision != "postpone" || postponements < 2,
        "schedule_postpone_limit"
    );
    if !["sleep", "wake"].contains(&d.kind.as_str()) {
        ensure!(db.query_row("SELECT EXISTS(SELECT 1 FROM daily_life_events e JOIN schedule_activities a ON a.id=e.activity_id AND a.revision=e.activity_revision WHERE e.id=?1 AND a.state='enabled')",[&d.id],|r|r.get::<_,bool>(0))?,"schedule_activity_changed");
    }
    let next = due + choice.delay_minutes * 60;
    if d.kind == "wake" && choice.decision != "execute" {
        let state = rest::state_in(db)?;
        ensure!(
            state.phase == rest::RestPhase::Awake || state.wake_at == Some(d.due_at),
            "routine_wake_deadline_changed"
        );
        rest::change_routine_wake_in(
            db,
            if choice.decision == "cancel" {
                None
            } else {
                Some(next)
            },
            at,
        )?;
    }
    if !["sleep", "wake"].contains(&d.kind.as_str()) && choice.decision != "execute" {
        let (aid, arev, nominal): (String, i64, i64) = db.query_row(
            "SELECT activity_id,activity_revision,nominal_at FROM daily_life_events WHERE id=?1",
            [&d.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let owner: String = db.query_row(
            "SELECT owner_id FROM schedule_activities WHERE id=?1",
            [&aid],
            |r| r.get(0),
        )?;
        let detail = schedule::detail(db, &owner, &aid)?;
        ensure!(detail["revision"] == arev, "schedule_activity_changed");
        let mut definition = detail["exceptions"]
            .as_array()
            .and_then(|v| {
                v.iter()
                    .find(|e| e["nominal_at"] == nominal && e["state"] == "enabled")
            })
            .map(|e| e["definition"].clone())
            .unwrap_or_else(|| detail["definition"].clone());
        if choice.decision == "postpone" {
            let duration = definition["end_at"]
                .as_i64()
                .context("invalid_schedule_duration")?
                - definition["start_at"]
                    .as_i64()
                    .context("invalid_schedule_duration")?;
            definition["start_at"] = json!(next);
            definition["end_at"] = json!(next + duration);
        }
        let result = schedule::change(
            db,
            &owner,
            &aid,
            arev,
            "single",
            Some(nominal),
            if choice.decision == "postpone" {
                Some(&definition)
            } else {
                None
            },
            if choice.decision == "cancel" {
                Some("cancelled")
            } else {
                None
            },
            true,
            at,
        )?;
        ensure!(
            result["state"] == "enabled",
            "schedule_activity_not_enabled"
        );
        db.execute(
            "UPDATE daily_life_events SET activity_revision=?1 WHERE id=?2",
            params![result["revision"].as_i64(), d.id],
        )?;
        db.execute(
            "UPDATE daily_life_activities SET activity_revision=?1 WHERE activity_id=?2",
            params![result["revision"].as_i64(), aid],
        )?;
    }
    db.execute("UPDATE daily_life_events SET decision=?1,decision_status='completed',status=?2,due_at=?3,postponements=postponements+?4,decided_at=?5,fence=NULL,output_sha256=?6 WHERE id=?7",params![choice.decision,if choice.decision=="cancel"{"cancelled"}else{"planned"},next,if choice.decision=="postpone"{1}else{0},at,character::digest(raw.as_bytes()),d.id])?;
    db.execute("UPDATE daily_life_decision_attempts SET status='completed',finished_at=?1,decision=?2,delay_minutes=?3,output_sha256=?4 WHERE fence=?5 AND status='running'",params![at,choice.decision,choice.delay_minutes,character::digest(raw.as_bytes()),d.fence])?;
    // A postponed occurrence receives a fresh notice at the new deadline;
    // total postponements bound the entire original occurrence across restarts.
    if choice.decision == "postpone" {
        db.execute(
            "UPDATE daily_life_events SET decision_status='pending',attempts=0 WHERE id=?1",
            [&d.id],
        )?;
    }
    db.execute("INSERT INTO audit_events(actor,action,resource,created_at) VALUES('schedule:model','schedule.decision',?1,?2)",params![d.id,at])?;
    Ok(())
}
pub async fn run_script(raw: &str) -> anyhow::Result<Choice> {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        process::Command,
    };
    ensure!(raw.len() <= 4096, "schedule_decision_too_large");
    let value = crate::trigger_rule::strict_value(raw.trim().as_bytes())
        .map_err(|_| anyhow::anyhow!("schedule_decision_invalid_json"))?;
    let choice: Choice = serde_json::from_value(value)?;
    let mut child = Command::new(if cfg!(windows) { "python" } else { "python3" })
        .args(["-I", "-S", "-c", SCRIPT])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdin = child.stdin.take().context("schedule_script_stdin")?;
    stdin
        .write_all(serde_json::to_string(&choice)?.as_bytes())
        .await?;
    drop(stdin);
    let mut stdout = child.stdout.take().context("schedule_script_stdout")?;
    let operation = async {
        let mut out = Vec::new();
        stdout.read_to_end(&mut out).await?;
        ensure!(out.len() < 4096, "schedule_script_output_bound");
        ensure!(child.wait().await?.success(), "schedule_script_rejected");
        Ok::<_, anyhow::Error>(serde_json::from_slice::<Choice>(&out)?)
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), operation).await?
}

pub fn calendar(db: &Connection, start: i64, end: i64) -> anyhow::Result<Vec<Value>> {
    if !exists(db)? {
        return Ok(vec![]);
    }
    let (rev, p) = rest::policy_in(db)?;
    let activated_at: i64 =
        db.query_row("SELECT updated_at FROM rest_policies LIMIT 1", [], |r| {
            r.get(0)
        })?;
    if !p.schedule_enabled {
        return Ok(vec![]);
    }
    let zone: chrono_tz::Tz = p.time_zone.parse()?;
    let mut day = chrono::DateTime::from_timestamp(start, 0)
        .context("invalid_routine_range")?
        .with_timezone(&zone)
        .date_naive()
        .pred_opt()
        .context("invalid_routine_range")?;
    let last = chrono::DateTime::from_timestamp(end, 0)
        .context("invalid_routine_range")?
        .with_timezone(&zone)
        .date_naive();
    let mut items = vec![];
    for _ in 0..=94 {
        if day > last {
            break;
        }
        for (kind, time, title) in [
            ("sleep", p.sleep_local.as_deref(), "睡觉"),
            ("wake", p.wake_local.as_deref(), "起床"),
        ] {
            let Some(time) = time else { continue };
            let time = NaiveTime::parse_from_str(time, "%H:%M")?;
            let chrono::LocalResult::Single(instant) =
                zone.from_local_datetime(&day.and_time(time))
            else {
                continue;
            };
            let nominal = instant.timestamp();
            let id = format!("{kind}:{rev}:{nominal}");
            let saved:Option<(i64,String,String,Option<String>)>=db.query_row("SELECT due_at,status,decision_status,error_code FROM daily_life_events WHERE id=?1",[&id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
            if saved.is_none() && nominal < activated_at {
                continue;
            }
            let due = saved.as_ref().map(|r| r.0).unwrap_or(nominal);
            let finish = if kind == "sleep" {
                if let Some(wake) = &p.wake_local {
                    let time = NaiveTime::parse_from_str(wake, "%H:%M")?;
                    let wake_day = if time
                        <= NaiveTime::parse_from_str(
                            p.sleep_local.as_deref().unwrap_or("23:00"),
                            "%H:%M",
                        )? {
                        day.succ_opt().context("invalid_routine_range")?
                    } else {
                        day
                    };
                    match zone.from_local_datetime(&wake_day.and_time(time)) {
                        chrono::LocalResult::Single(v) => v.timestamp(),
                        _ => due + p.max_sleep_seconds,
                    }
                } else {
                    due + p.default_sleep_seconds.unwrap_or(p.max_sleep_seconds)
                }
            } else {
                due + 300
            };
            if due < end && finish > start {
                items.push(json!({"origin":"routine","id":id,"activity_id":null,"title":title,"type":kind,"creator":"system","state":"enabled","status":saved.as_ref().map(|r|r.1.as_str()).unwrap_or("planned"),"start_at":due,"end_at":finish,"nominal_at":nominal,"time_zone":p.time_zone,"read_only":true,"decision_status":saved.as_ref().map(|r|r.2.as_str()),"block_reason":saved.as_ref().and_then(|r|r.3.as_deref())}));
            }
        }
        day = day.succ_opt().context("invalid_routine_range")?;
    }
    Ok(items)
}
