//! Authenticated, bounded runtime journal. Context and message bodies remain in
//! their original stores so forgetting context cannot leave another copy here.
use crate::{
    auth::Admin,
    error::{ApiError, ApiResult},
    state::{now, AppState},
};
use axum::{
    extract::{Query, State},
    routing::get,
    Json, Router,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

const MAX_EVENTS: i64 = 20_000;
const DAYS: i64 = 30;
const MAX_CONTENT: usize = 16_384;
const KINDS: &[&str] = &[
    "input", "output", "run", "tool", "tts", "asr", "subagent", "vision", "memory", "delivery",
    "system",
];

pub fn router() -> Router<AppState> {
    Router::new().route("/api/admin/v1/runtime-logs", get(list))
}

/// Install only after the stores have initialized. All triggers use ordinary
/// SQLite builtins, including when an older compatible binary opens this DB.
pub fn initialize(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS runtime_log_events(
      id INTEGER PRIMARY KEY AUTOINCREMENT, owner_id TEXT NOT NULL,
      kind TEXT NOT NULL,status TEXT NOT NULL,run_id TEXT,conversation_id TEXT,
      title TEXT NOT NULL,reference_type TEXT NOT NULL DEFAULT '',reference_id TEXT,
      details_json TEXT NOT NULL DEFAULT '{}',created_at INTEGER NOT NULL);
      CREATE INDEX IF NOT EXISTS runtime_log_owner ON runtime_log_events(owner_id,id);
      CREATE INDEX IF NOT EXISTS runtime_log_run ON runtime_log_events(run_id,id);
      CREATE TRIGGER IF NOT EXISTS runtime_log_retention AFTER INSERT ON runtime_log_events BEGIN
        DELETE FROM runtime_log_events WHERE id<=NEW.id-20000 OR created_at<CAST(strftime('%s','now') AS INTEGER)-2592000;
      END;")?;
    trigger(
        db,
        "runs",
        "run",
        "NEW.owner_id",
        "NEW.status",
        "NEW.id",
        "NEW.conversation_id",
        "'运行 · '||NEW.run_kind",
        "run",
        "NEW.id",
        "json_object('run_kind',NEW.run_kind)",
        true,
    )?;
    db.execute_batch("CREATE TRIGGER IF NOT EXISTS runtime_log_messages_insert AFTER INSERT ON messages BEGIN
      INSERT INTO runtime_log_events(owner_id,kind,status,run_id,conversation_id,title,reference_type,reference_id,created_at)
      SELECT c.owner_id,CASE NEW.role WHEN 'user' THEN 'input' ELSE 'output' END,'recorded',NEW.run_id,NEW.conversation_id,
      CASE NEW.role WHEN 'user' THEN 'Agent 输入' ELSE 'Agent 输出' END,'message',NEW.id,CAST(strftime('%s','now') AS INTEGER)
      FROM conversations c WHERE c.id=NEW.conversation_id;
      END;")?;
    trigger(
        db,
        "run_prompt_snapshots",
        "input",
        "(SELECT owner_id FROM runs WHERE id=NEW.run_id)",
        "'recorded'",
        "NEW.run_id",
        "(SELECT conversation_id FROM runs WHERE id=NEW.run_id)",
        "'模型请求上下文'",
        "prompt",
        "NEW.run_id",
        "json_object('digest',NEW.digest,'compiler_version',NEW.compiler_version)",
        false,
    )?;
    trigger(
        db,
        "outbox",
        "delivery",
        "(SELECT owner_id FROM runs WHERE id=NEW.run_id)",
        "NEW.status",
        "NEW.run_id",
        "(SELECT conversation_id FROM runs WHERE id=NEW.run_id)",
        "'QQ 回复投递'",
        "outbox",
        "NEW.id",
        "'{}'",
        true,
    )?;
    trigger(
        db,
        "weixin_outbox",
        "delivery",
        "(SELECT owner_id FROM runs WHERE id=NEW.run_id)",
        "NEW.status",
        "NEW.run_id",
        "(SELECT conversation_id FROM runs WHERE id=NEW.run_id)",
        "'微信回复投递'",
        "weixin_outbox",
        "NEW.id",
        "'{}'",
        true,
    )?;
    trigger(db,"outbox_reply_parts","delivery","(SELECT r.owner_id FROM outbox o JOIN runs r ON r.id=o.run_id WHERE o.id=NEW.outbox_id)","NEW.status","(SELECT run_id FROM outbox WHERE id=NEW.outbox_id)","(SELECT r.conversation_id FROM outbox o JOIN runs r ON r.id=o.run_id WHERE o.id=NEW.outbox_id)","'QQ 逐句投递'","part","NEW.outbox_id||':'||NEW.seq","json_object('sequence',NEW.seq,'delay_ms',NEW.delay_ms)",true)?;
    trigger(
        db,
        "conversation_images",
        "vision",
        "NEW.owner_id",
        "NEW.status",
        "NEW.run_id",
        "NEW.conversation_id",
        "'图片理解'",
        "vision",
        "NEW.run_id",
        "json_object('width',NEW.width,'height',NEW.height,'image_sha256',NEW.image_sha256)",
        true,
    )?;
    trigger(
        db,
        "plugin_factory_jobs",
        "tool",
        "NEW.owner_id",
        "NEW.status",
        "NULL",
        "NULL",
        "'插件工厂 · '||NEW.kind",
        "plugin_job",
        "NEW.id",
        "json_object('job_id',NEW.id,'version_id',NEW.version_id,'operation',NEW.kind)",
        true,
    )?;
    trigger(
        db,
        "summary_jobs",
        "memory",
        "NEW.source_owner_id",
        "NEW.status",
        "NEW.trigger_run_id",
        "NEW.conversation_id",
        "'会话摘要'",
        "summary_job",
        "NEW.id",
        "json_object('job_id',NEW.id,'source_count',json_array_length(NEW.source_ids_json))",
        true,
    )?;
    trigger(db,"qq_api_calls","tool","NEW.owner_id","NEW.outcome","NULL","NULL","'QQ API · '||NEW.operation_id","qq_call","NEW.id","json_object('operation',NEW.operation_id,'http_status',NEW.http_status,'business_code',NEW.business_code)",false)?;
    trigger(
        db,
        "speech_assets",
        "tts",
        "NEW.owner_id",
        "'completed'",
        "NEW.source_run_id",
        "NEW.conversation_id",
        "'语音制品已生成'",
        "speech_asset",
        "NEW.id",
        "NEW.metadata_json",
        false,
    )?;
    trigger(
        db,
        "memory_candidate_jobs",
        "memory",
        "NEW.source_owner_id",
        "NEW.status",
        "NULL",
        "NEW.conversation_id",
        "'记忆候选提取'",
        "candidate_job",
        "NEW.id",
        "json_object('job_id',NEW.id,'source_count',NEW.source_count)",
        true,
    )?;
    trigger(
        db,
        "image_analysis_jobs",
        "vision",
        "NEW.owner_id",
        "NEW.status",
        "NULL",
        "NULL",
        "'图片诊断请求'",
        "image_job",
        "NEW.id",
        "json_object('job_id',NEW.id,'width',NEW.width,'height',NEW.height)",
        true,
    )?;
    trigger(
        db,
        "wasm_workbench_jobs",
        "tool",
        "NEW.owner_id",
        "NEW.status",
        "NULL",
        "NULL",
        "'受限计算诊断 · '||NEW.kind",
        "wasm_job",
        "NEW.id",
        "json_object('job_id',NEW.id,'operation',NEW.kind)",
        true,
    )?;
    trigger(
        db,
        "audit_events",
        "system",
        "NEW.actor",
        "'recorded'",
        "NULL",
        "NULL",
        "'管理事件 · '||NEW.action",
        "audit",
        "CAST(NEW.id AS TEXT)",
        "json_object('action',NEW.action,'resource',NEW.resource)",
        false,
    )?;
    // Import existing records once as current observations, never invented transitions.
    let imported: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM settings WHERE key='runtime.logs.imported.v1')",
        [],
        |r| r.get(0),
    )?;
    if !imported {
        // Allocate journal IDs in one chronological order across all stores.
        // The latest 20,000 observations are chosen before ascending insertion;
        // ties use a stable source/reference order for reproducible pagination.
        let transaction = db.unchecked_transaction()?;
        transaction.execute_batch("WITH historical(owner_id,kind,status,run_id,conversation_id,title,reference_type,reference_id,details_json,created_at) AS (
          SELECT owner_id,'run',status,id,conversation_id,'历史运行 · '||run_kind,'run',id,json_object('run_kind',run_kind,'historical',json('true')),updated_at FROM runs
          UNION ALL
          SELECT c.owner_id,CASE m.role WHEN 'user' THEN 'input' ELSE 'output' END,'recorded',m.run_id,m.conversation_id,
          CASE m.role WHEN 'user' THEN '历史 Agent 输入' ELSE '历史 Agent 输出' END,'message',m.id,'{\"historical\":true}',m.created_at
          FROM messages m JOIN conversations c ON c.id=m.conversation_id
          UNION ALL
          SELECT r.owner_id,'input','recorded',p.run_id,r.conversation_id,'历史模型请求上下文','prompt',p.run_id,json_object('digest',p.digest,'historical',json('true')),p.created_at
          FROM run_prompt_snapshots p JOIN runs r ON r.id=p.run_id
          UNION ALL
          SELECT owner_id,'tts','completed',source_run_id,conversation_id,'历史语音制品','speech_asset',id,json_set(metadata_json,'$.historical',json('true')),created_at FROM speech_assets
        ), recent AS (
          SELECT * FROM historical WHERE created_at>=CAST(strftime('%s','now') AS INTEGER)-2592000
          ORDER BY created_at DESC,reference_type DESC,reference_id DESC LIMIT 20000
        )
          INSERT INTO runtime_log_events(owner_id,kind,status,run_id,conversation_id,title,reference_type,reference_id,details_json,created_at)
          SELECT * FROM recent ORDER BY created_at,reference_type,reference_id;
          INSERT INTO settings(key,value) VALUES('runtime.logs.imported.v1','1');")?;
        transaction.commit()?;
    }
    // Subprocesses from another daemon cannot remain live. Preserve the start
    // event and append an honest terminal observation instead of replaying it.
    db.execute_batch("INSERT INTO runtime_log_events(owner_id,kind,status,run_id,conversation_id,title,details_json,created_at)
      SELECT e.owner_id,e.kind,'interrupted',e.run_id,e.conversation_id,'服务重启，操作结果未确认',e.details_json,CAST(strftime('%s','now') AS INTEGER)
      FROM runtime_log_events e WHERE e.kind IN ('tts','tool') AND e.status='running'
      AND json_extract(e.details_json,'$.operation_id') IS NOT NULL
      AND NOT EXISTS(SELECT 1 FROM runtime_log_events done WHERE done.id>e.id
        AND json_extract(done.details_json,'$.operation_id')=json_extract(e.details_json,'$.operation_id')
        AND done.status IN ('completed','failed','cancelled','interrupted')); ")?;
    db.execute("DELETE FROM runtime_log_events WHERE created_at<?1 OR id<=(SELECT coalesce(max(id),0)-?2 FROM runtime_log_events)",params![now()-DAYS*86400,MAX_EVENTS])?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn trigger(
    db: &Connection,
    table: &str,
    kind: &str,
    owner: &str,
    status: &str,
    run: &str,
    conversation: &str,
    title: &str,
    reference: &str,
    id: &str,
    details: &str,
    updates: bool,
) -> anyhow::Result<()> {
    let insert = format!("INSERT INTO runtime_log_events(owner_id,kind,status,run_id,conversation_id,title,reference_type,reference_id,details_json,created_at) VALUES({owner},'{kind}',{status},{run},{conversation},{title},'{reference}',{id},{details},CAST(strftime('%s','now') AS INTEGER));");
    db.execute_batch(&format!("CREATE TRIGGER IF NOT EXISTS runtime_log_{table}_insert AFTER INSERT ON {table} BEGIN {insert} END;"))?;
    if updates {
        db.execute_batch(&format!("CREATE TRIGGER IF NOT EXISTS runtime_log_{table}_update AFTER UPDATE OF status ON {table} WHEN OLD.status<>NEW.status BEGIN {insert} END;"))?;
    }
    Ok(())
}

/// Only controlled metadata belongs here. Bodies are resolved by reference.
#[allow(clippy::too_many_arguments)]
pub fn record(
    state: &AppState,
    owner: &str,
    kind: &str,
    status: &str,
    run: Option<&str>,
    conversation: Option<&str>,
    title: &str,
    details: Value,
) {
    let _ = state.db(|db| {
        db.execute("INSERT INTO runtime_log_events(owner_id,kind,status,run_id,conversation_id,title,details_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![owner,kind,status,run,conversation,title,details.to_string(),now()])?;
        Ok(())
    }).map_err(|_| tracing::warn!("runtime journal metadata write failed"));
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filters {
    limit: Option<usize>,
    cursor: Option<String>,
    kind: Option<String>,
    status: Option<String>,
    run_id: Option<String>,
    q: Option<String>,
}
impl Filters {
    fn validate(&self) -> ApiResult<(usize, i64)> {
        let limit = self.limit.unwrap_or(50);
        let cursor = match self.cursor.as_deref() {
            None | Some("") => i64::MAX,
            Some(value) => value
                .parse::<i64>()
                .ok()
                .filter(|v| *v > 0)
                .ok_or_else(|| ApiError::bad("invalid_log_cursor", "日志游标无效"))?,
        };
        if !(1..=100).contains(&limit)
            || self
                .kind
                .as_deref()
                .is_some_and(|s| !s.is_empty() && !KINDS.contains(&s))
            || self.status.as_ref().is_some_and(|s| s.len() > 32)
            || self.run_id.as_ref().is_some_and(|s| s.len() > 128)
            || self.q.as_ref().is_some_and(|s| s.len() > 200)
        {
            return Err(ApiError::bad("invalid_log_filter", "日志筛选条件超出范围"));
        }
        Ok((limit, cursor))
    }
}
async fn list(
    State(state): State<AppState>,
    admin: Admin,
    Query(filters): Query<Filters>,
) -> ApiResult<Json<Value>> {
    filters.validate()?;
    let references: Vec<(String, String)> = state.db(|db| {
        let mut statement = db.prepare("SELECT id,kind FROM secrets")?;
        let values = statement
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(values)
    })?;
    let secrets: Vec<String> = references
        .iter()
        .filter_map(|(id, kind)| state.secret(id, kind).ok())
        .filter(|v| !v.is_empty())
        .collect();
    Ok(Json(
        state.db(|db| page(db, &admin.id, &filters, &secrets))?,
    ))
}

fn page(
    db: &Connection,
    owner: &str,
    filters: &Filters,
    secrets: &[String],
) -> anyhow::Result<Value> {
    let (limit, mut cursor) = filters
        .validate()
        .map_err(|_| anyhow::anyhow!("invalid filters"))?;
    let kind = filters.kind.as_deref().unwrap_or("");
    let status = filters.status.as_deref().unwrap_or("");
    let run = filters.run_id.as_deref().unwrap_or("");
    let query = filters.q.as_deref().unwrap_or("").to_lowercase();
    let mut items = vec![];
    let mut has_more = false;
    // Scan in bounded batches: cursor records the last examined row, including
    // filtered rows, so sparse text searches remain stable and make progress.
    for batch in 0..20 {
        let mut stmt = db.prepare("SELECT e.id,e.kind,e.status,e.run_id,e.conversation_id,e.title,e.reference_type,e.reference_id,e.details_json,e.created_at,e.owner_id
          FROM runtime_log_events e WHERE e.id<?1 AND e.created_at>=?2
          AND (e.owner_id=?3 OR ((e.owner_id LIKE 'qq:%' OR e.owner_id LIKE 'weixin:%') AND NOT EXISTS(SELECT 1 FROM admins a WHERE a.id=e.owner_id)))
          AND (?4='' OR e.kind=?4) AND (?5='' OR e.status=?5) AND (?6='' OR e.run_id=?6)
          ORDER BY e.id DESC LIMIT 200")?;
        let rows = stmt
            .query_map(
                params![cursor, now() - DAYS * 86400, owner, kind, status, run],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, Option<String>>(7)?,
                        r.get::<_, String>(8)?,
                        r.get::<_, i64>(9)?,
                        r.get::<_, String>(10)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let count = rows.len();
        for (
            id,
            kind,
            status,
            run,
            conversation,
            title,
            reference,
            reference_id,
            details,
            created_at,
            event_owner,
        ) in rows
        {
            let content = resolve(
                db,
                &reference,
                reference_id.as_deref(),
                run.as_deref(),
                conversation.as_deref(),
                &event_owner,
            )?;
            let content = redact(&content, secrets);
            let details: Value = serde_json::from_str(&details).unwrap_or_else(|_| json!({}));
            let searchable = format!(
                "{title}\n{content}\n{details}\n{}",
                run.as_deref().unwrap_or("")
            )
            .to_lowercase();
            if !query.is_empty() && !searchable.contains(&query) {
                cursor = id;
                continue;
            }
            if items.len() == limit {
                has_more = true;
                break;
            }
            cursor = id;
            let source = if event_owner.starts_with("qq:") {
                "qq"
            } else if event_owner.starts_with("weixin:") {
                "weixin"
            } else {
                "web"
            };
            items.push(json!({"id":id,"kind":kind,"status":status,"run_id":run,"conversation_id":conversation,"title":title,"content":content,"details":details,"created_at":created_at,"source":source}));
        }
        if has_more || count < 200 {
            break;
        }
        // A bounded scan can return a continuation even if no row matched q.
        // Returning null here would incorrectly hide older matching events.
        if batch == 19 {
            has_more = true;
        }
    }
    Ok(
        json!({"items":items,"next_cursor":if has_more {Some(cursor.to_string())}else{None},"capabilities":{"subagent":false,"asr":false,"tts":true},"retention":{"days":DAYS,"max_events":MAX_EVENTS,"max_content_bytes":MAX_CONTENT},"scope":"own_web_and_instance_channels"}),
    )
}

fn resolve(
    db: &Connection,
    reference: &str,
    id: Option<&str>,
    run: Option<&str>,
    conversation: Option<&str>,
    owner: &str,
) -> anyhow::Result<String> {
    if let (Some(run), Some(conversation)) = (run, conversation) {
        let visible: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM runs r WHERE r.id=?1 AND r.owner_id=?2 AND r.conversation_id=?3 AND r.rowid>coalesce((SELECT through_run_rowid FROM memory_context_watermarks w WHERE w.conversation_id=r.conversation_id),0))",params![run,owner,conversation],|r|r.get(0))?;
        if !visible {
            return Ok("[上下文已失效或清理，仅保留事件元数据]".into());
        }
    }
    let id = id.unwrap_or("");
    let value: Option<String> = match reference {
        "message" => db.query_row("SELECT CASE WHEN m.role='assistant' AND r.run_kind IN ('skill_image','trigger_image') THEN '[原生生图结果；二进制内容不进入日志]' ELSE substr(m.content,1,65536) END FROM messages m LEFT JOIN runs r ON r.id=m.run_id WHERE m.id=?1",[id],|r|r.get(0)).optional()?,
        "prompt" => db.query_row("SELECT substr(messages_json,1,65536) FROM run_prompt_snapshots WHERE run_id=?1",[id],|r|r.get(0)).optional()?,
        "run" => db.query_row("SELECT error FROM runs WHERE id=?1",[id],|r|r.get::<_,Option<String>>(0)).optional()?.flatten(),
        "outbox" => db.query_row("SELECT error FROM outbox WHERE id=?1",[id],|r|r.get::<_,Option<String>>(0)).optional()?.flatten(),
        "weixin_outbox" => db.query_row("SELECT error_code FROM weixin_outbox WHERE id=?1",[id],|r|r.get::<_,Option<String>>(0)).optional()?.flatten(),
        "plugin_job" => db.query_row("SELECT error_code FROM plugin_factory_jobs WHERE id=?1",[id],|r|r.get::<_,Option<String>>(0)).optional()?.flatten(),
        "summary_job" => db.query_row("SELECT error_code FROM summary_jobs WHERE id=?1",[id],|r|r.get::<_,Option<String>>(0)).optional()?.flatten(),
        _ => None,
    };
    Ok(value.unwrap_or_else(|| {
        if reference == "prompt" || reference == "message" {
            "[内容已失效或清理]".into()
        } else {
            String::new()
        }
    }))
}

fn redact(value: &str, secrets: &[String]) -> String {
    let mut value = value.to_owned();
    for secret in secrets {
        value = value.replace(secret, "[已隐藏凭证]");
    }
    // Also protect common pasted credentials even when not configured here.
    let mut words = vec![];
    let mut hide_next = false;
    for word in value.split_inclusive(char::is_whitespace) {
        let normalized = word.trim().trim_matches(['\"', '\'']).to_ascii_lowercase();
        if hide_next {
            words.push("[已隐藏凭证] ".to_string());
            hide_next = false;
            continue;
        }
        if normalized == "bearer" || normalized == "authorization:" {
            words.push(word.to_string());
            hide_next = true;
            continue;
        }
        if normalized.starts_with("sk-") && normalized.len() > 12
            || ["api_key=", "apikey=", "token=", "password=", "secret="]
                .iter()
                .any(|key| normalized.contains(key))
        {
            words.push("[已隐藏凭证] ".into());
        } else {
            words.push(word.to_owned());
        }
    }
    let mut result = words.concat();
    if result.len() > MAX_CONTENT {
        let mut end = MAX_CONTENT;
        while !result.is_char_boundary(end) {
            end -= 1;
        }
        result.truncate(end);
        result.push_str("\n[内容超过日志显示上限，已截断]");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn redacts_configured_and_pasted_secrets_with_utf8_bound() {
        let value = redact(
            "测试 secret-value Bearer abcdef token=123 sk-abcdefghijklmnop",
            &["secret-value".into()],
        );
        assert!(!value.contains("secret-value"));
        assert!(!value.contains("abcdef"));
        assert!(!value.contains("123"));
        assert!(redact(&"语".repeat(10000), &[]).len() < MAX_CONTENT + 100);
    }
    #[test]
    fn filter_limits_are_strict() {
        assert!(Filters {
            limit: Some(101),
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(Filters {
            cursor: Some("-1".into()),
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(Filters {
            kind: Some("unknown".into()),
            ..Default::default()
        }
        .validate()
        .is_err());
    }
    #[test]
    fn historical_backfill_orders_sources_together_by_timestamp() {
        let directory = tempfile::tempdir().unwrap();
        let state = AppState::open(crate::state::Options {
            data_dir: directory.path().to_owned(),
            origin: "http://127.0.0.1:18419".into(),
            allow_private_model: true,
            cookie_secure: false,
            web_dir: directory.path().to_owned(),
        })
        .unwrap();
        state.db(|db| {
            let at=now();
            db.execute("INSERT INTO conversations(id,owner_id,title,created_at) VALUES('history','a','historical journal',?1)",[at-50])?;
            db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,created_at,updated_at) VALUES('history','history','a','history','body','{}','completed',?1,?2)",params![at-50,at-10])?;
            db.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES('history','history','history','user','body',?1)",[at-40])?;
            db.execute("INSERT INTO run_prompt_snapshots(run_id,messages_json,digest,compiler_version,created_at) VALUES('history','[]','hash','test',?1)",[at-30])?;
            db.execute("INSERT INTO speech_assets(id,owner_id,conversation_id,context_floor,metadata_json,audio,created_at,expires_at) VALUES('history','a','history',0,'{}',X'',?1,?2)",params![at-20,at+60])?;
            db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,created_at,updated_at) VALUES('expired-history','history','a','expired','old','{}','completed',?1,?1)",[at-DAYS*86400-60])?;
            db.execute("DELETE FROM runtime_log_events",[])?;
            db.execute("DELETE FROM settings WHERE key='runtime.logs.imported.v1'",[])?;
            initialize(db)?;
            let mut statement=db.prepare("SELECT reference_type,created_at,json_extract(details_json,'$.historical') FROM runtime_log_events ORDER BY id")?;
            let records=statement.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,bool>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            assert_eq!(records,vec![("message".into(),at-40,true),("prompt".into(),at-30,true),("speech_asset".into(),at-20,true),("run".into(),at-10,true)]);
            let latest=page(db,"a",&Filters::default(),&[])?;
            let timestamps=latest["items"].as_array().unwrap().iter().map(|item|item["created_at"].as_i64().unwrap()).collect::<Vec<_>>();
            assert_eq!(timestamps,vec![at-10,at-20,at-30,at-40]);
            initialize(db)?;
            assert_eq!(db.query_row("SELECT count(*) FROM runtime_log_events",[],|r|r.get::<_,i64>(0))?,4);
            Ok(())
        }).unwrap();
    }
    #[test]
    fn stores_initialize_capture_isolate_page_and_forget_without_copying_bodies() {
        let directory = tempfile::tempdir().unwrap();
        let state = AppState::open(crate::state::Options {
            data_dir: directory.path().to_owned(),
            origin: "http://127.0.0.1:18419".into(),
            allow_private_model: true,
            cookie_secure: false,
            web_dir: directory.path().to_owned(),
        })
        .unwrap();
        state.db(|db| {
            initialize(db)?;
            for (id,owner) in [("own","a"),("foreign","b"),("channel","qq:bot:c2c:user")] {
                db.execute("INSERT INTO conversations(id,owner_id,title,created_at) VALUES(?1,?2,'journal test',?3)",params![id,owner,now()])?;
                db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,created_at,updated_at) VALUES(?1,?1,?2,?1,'private body','{}','queued',?3,?3)",params![id,owner,now()])?;
                db.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES(?1,?1,?1,'user',?2,?3)",params![id,format!("private body {owner}"),now()])?;
                db.execute("UPDATE runs SET status='running' WHERE id=?1",[id])?;
            }
            db.execute("INSERT INTO run_prompt_snapshots(run_id,messages_json,digest,compiler_version,created_at) VALUES('own','[{\"role\":\"system\",\"content\":\"private memory marker\"}]','hash','test',?1)",[now()])?;
            let all=page(db,"a",&Filters::default(),&[])?;
            let text=all.to_string();assert!(text.contains("private body a"));assert!(text.contains("private body qq:"));assert!(!text.contains("private body b"));assert!(text.contains("private memory marker"));
            let first=page(db,"a",&Filters{limit:Some(2),..Default::default()},&[])?;
            let cursor=first["next_cursor"].as_str().unwrap().to_string();
            let next=page(db,"a",&Filters{limit:Some(2),cursor:Some(cursor),..Default::default()},&[])?;
            assert!(first["items"][1]["id"].as_i64().unwrap()>next["items"][0]["id"].as_i64().unwrap());
            let copies:i64=db.query_row("SELECT count(*) FROM runtime_log_events WHERE details_json LIKE '%private body%' OR details_json LIKE '%private memory marker%'",[],|r|r.get(0))?;assert_eq!(copies,0);
            db.execute("DELETE FROM run_prompt_snapshots WHERE run_id='own'",[])?;
            let forgotten=page(db,"a",&Filters::default(),&[])?;assert!(!forgotten.to_string().contains("private memory marker"));
            let rowid:i64=db.query_row("SELECT rowid FROM runs WHERE id='own'",[],|r|r.get(0))?;
            db.execute("INSERT INTO memory_context_watermarks(conversation_id,assistant_id,through_run_rowid,updated_at) VALUES('own','assistant',?1,?2)",params![rowid,now()])?;
            assert!(!page(db,"a",&Filters::default(),&[])?.to_string().contains("private body a"));
            db.execute("INSERT INTO audit_events(actor,action,resource,created_at) VALUES('a','memory.forget','record-id',?1)",[now()])?;
            assert!(page(db,"a",&Filters{kind:Some("system".into()),..Default::default()},&[])?.to_string().contains("memory.forget"));
            db.execute("INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,deadline,created_at) VALUES('out','channel','bot','key','{}','reply','queued',?1,?1)",[now()])?;
            db.execute("INSERT INTO outbox_reply_parts(outbox_id,seq,payload_json,delay_ms,status) VALUES('out',1,'{}',0,'queued')",[])?;
            db.execute("UPDATE outbox_reply_parts SET status='sent' WHERE outbox_id='out'",[])?;
            db.execute("UPDATE outbox SET status='sent' WHERE id='out'",[])?;
            db.execute("INSERT INTO weixin_contexts(id,account_id,peer,generation,token,created_at,expires_at) VALUES('ctx','bot','peer','generation',NULL,?1,?1)",[now()])?;
            db.execute("INSERT INTO weixin_outbox(id,run_id,account_id,peer,context_ref,credential_ref,generation,client_id,content,status,deadline,created_at,updated_at) VALUES('wx','own','bot','peer','ctx','key','generation','client','reply','queued',?1,?1,?1)",[now()])?;
            db.execute("UPDATE weixin_outbox SET status='accepted' WHERE id='wx'",[])?;
            db.execute("INSERT INTO conversation_images(run_id,conversation_id,owner_id,source_message_id,width,height,image_sha256,status,created_at) VALUES('channel','channel','qq:bot:c2c:user','channel',1,1,'hash','queued',?1)",[now()])?;
            db.execute("UPDATE conversation_images SET status='failed' WHERE run_id='channel'",[])?;
            db.execute("INSERT INTO plugin_factory_versions(id,owner_id,plugin_id,version,spec_json,status,created_at) VALUES('pv','a','user.test','1','{}','specified',?1)",[now()])?;
            db.execute("INSERT INTO plugin_factory_jobs(id,owner_id,version_id,kind,client_request_id,request_digest,status,created_at) VALUES('pj','a','pv','generate','client','hash','queued',?1)",[now()])?;
            db.execute("UPDATE plugin_factory_jobs SET status='failed' WHERE id='pj'",[])?;
            db.execute("INSERT INTO summary_jobs(id,trigger_run_id,conversation_id,assistant_id,source_owner_id,epoch,history_floor,base_revision,from_run_rowid,through_run_rowid,source_ids_json,source_digest,config_digest,status,created_at,updated_at) VALUES('sj','own','own','assistant','a',0,0,0,1,1,'[]','hash','hash','queued',?1,?1)",[now()])?;
            db.execute("UPDATE summary_jobs SET status='failed' WHERE id='sj'",[])?;
            db.execute("INSERT INTO qq_api_calls(id,owner_id,account_id,operation_id,outcome,created_at) VALUES('qc','a','bot','operation','accepted',?1)",[now()])?;
            db.execute("INSERT INTO speech_assets(id,owner_id,conversation_id,context_floor,metadata_json,audio,created_at,expires_at,source_run_id) VALUES('sa','a','own',0,'{}',X'',?1,?2,'own')",params![now(),now()+60])?;
            db.execute("INSERT INTO memory_candidate_jobs(id,assistant_id,conversation_id,source_owner_id,source_epoch,source_floor,source_digest,source_message_ids_json,config_json,status,revision,created_at,expires_at,source_count) VALUES('mc','assistant','own','a',0,0,'hash','[]','{}','queued',1,?1,?2,0)",params![now(),now()+60])?;
            db.execute("UPDATE memory_candidate_jobs SET status='failed' WHERE id='mc'",[])?;
            db.execute("INSERT INTO image_analysis_jobs(id,owner_id,client_request_id,request_digest,config_json,status,revision,created_at,expires_at,width,height,image_sha256) VALUES('ij','a','client','hash','{}','queued',1,?1,?2,1,1,'hash')",params![now(),now()+60])?;
            db.execute("UPDATE image_analysis_jobs SET status='failed' WHERE id='ij'",[])?;
            db.execute("INSERT INTO wasm_workbench_jobs(id,owner_id,kind,client_request_id,request_digest,status,revision,created_at,expires_at) VALUES('wj','a','execute','client','hash','queued',1,?1,?2)",params![now(),now()+60])?;
            db.execute("UPDATE wasm_workbench_jobs SET status='failed' WHERE id='wj'",[])?;
            let kinds: i64=db.query_row("SELECT count(DISTINCT kind) FROM runtime_log_events",[],|r|r.get(0))?;assert!(kinds>=8);
            db.execute("INSERT INTO runtime_log_events(id,owner_id,kind,status,title,created_at) VALUES(30000,'a','system','recorded','retention',?1)",[now()])?;
            let oldest:i64=db.query_row("SELECT min(id) FROM runtime_log_events",[],|r|r.get(0))?;assert_eq!(oldest,30000);
            Ok(())
        }).unwrap();
    }
}
