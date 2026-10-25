//! Durable scheduling metadata. Network work always belongs to the native worker.
use crate::{
    state::{now, token_hash, AppState},
    trigger_calendar, trigger_rule,
};
use axum::http::StatusCode;
use chrono::{DateTime, Duration, TimeZone, Utc};
use chrono_tz::Tz;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Debug)]
pub struct TriggerError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}
impl std::fmt::Display for TriggerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for TriggerError {}
pub fn fail(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    TriggerError {
        status: StatusCode::UNPROCESSABLE_ENTITY,
        code,
        message: message.into(),
    }
    .into()
}
pub fn conflict() -> anyhow::Error {
    TriggerError {
        status: StatusCode::CONFLICT,
        code: "revision_conflict",
        message: "任务已更新，请重新载入合并并预演".into(),
    }
    .into()
}
pub fn missing() -> anyhow::Error {
    TriggerError {
        status: StatusCode::NOT_FOUND,
        code: "not_found",
        message: "任务或执行记录不存在".into(),
    }
    .into()
}
pub fn digest(value: &Value) -> String {
    hex::encode(Sha256::digest(
        serde_json::to_vec(value).unwrap_or_default(),
    ))
}
pub fn definition(value: &Value) -> anyhow::Result<trigger_rule::TaskDefinition> {
    let parsed =
        trigger_rule::validate_definition(value, now()).map_err(|s| fail("invalid_trigger", s))?;
    if let Some(content) = value["action"]["content"].as_str() {
        let expanded = content
            .replace("{task_title}", value["title"].as_str().unwrap_or(""))
            .replace("{scheduled_local}", &"x".repeat(64))
            .replace("{event.summary}", &"x".repeat(64));
        if expanded.len() > 16000 {
            return Err(fail(
                "trigger_result_limit",
                "提醒模板替换后的最大长度超过16000字节，请缩短内容",
            ));
        }
    }
    Ok(parsed)
}

pub fn initialize(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS trigger_jobs(id TEXT PRIMARY KEY,owner_id TEXT NOT NULL,title TEXT NOT NULL,revision INTEGER NOT NULL,draft_revision INTEGER NOT NULL,active_revision INTEGER,lifecycle TEXT NOT NULL,block_reason TEXT,schedule_epoch INTEGER NOT NULL DEFAULT 0,dispatch_generation INTEGER NOT NULL DEFAULT 0,next_fire_at INTEGER,anchor INTEGER,cursor_at INTEGER NOT NULL DEFAULT 0,event_cursor INTEGER NOT NULL DEFAULT 0,last_event_at INTEGER,last_started_at INTEGER,next_resume_at INTEGER,last_evaluated_at INTEGER NOT NULL DEFAULT 0,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS trigger_due ON trigger_jobs(lifecycle,next_fire_at,id);
CREATE INDEX IF NOT EXISTS trigger_owner ON trigger_jobs(owner_id,updated_at DESC);
CREATE TABLE IF NOT EXISTS trigger_job_revisions(job_id TEXT NOT NULL REFERENCES trigger_jobs(id),revision INTEGER NOT NULL,definition_json TEXT NOT NULL,snapshot_json TEXT,created_at INTEGER NOT NULL,PRIMARY KEY(job_id,revision));
CREATE TABLE IF NOT EXISTS trigger_execution_grants(job_id TEXT NOT NULL,revision INTEGER NOT NULL,owner_id TEXT NOT NULL,approved_at INTEGER NOT NULL,revoked_at INTEGER,PRIMARY KEY(job_id,revision));
CREATE TABLE IF NOT EXISTS trigger_occurrences(id TEXT PRIMARY KEY,job_id TEXT NOT NULL REFERENCES trigger_jobs(id),job_revision INTEGER NOT NULL,owner_id TEXT NOT NULL,schedule_epoch INTEGER NOT NULL,fire_key TEXT NOT NULL,origin TEXT NOT NULL,nominal_at INTEGER NOT NULL,deadline INTEGER NOT NULL,definition_json TEXT NOT NULL,snapshot_json TEXT NOT NULL,status TEXT NOT NULL,run_id TEXT UNIQUE,response TEXT,error TEXT,delivery_status TEXT NOT NULL DEFAULT 'pending',created_at INTEGER NOT NULL,started_at INTEGER,completed_at INTEGER,cancelled_at INTEGER,calendar_revision INTEGER,UNIQUE(job_id,schedule_epoch,fire_key));
CREATE INDEX IF NOT EXISTS trigger_occurrence_job ON trigger_occurrences(job_id,created_at DESC);
CREATE INDEX IF NOT EXISTS trigger_occurrence_status ON trigger_occurrences(status,created_at);
CREATE TABLE IF NOT EXISTS trigger_attempts(run_id TEXT PRIMARY KEY,occurrence_id TEXT NOT NULL,fence TEXT NOT NULL,started_at INTEGER NOT NULL,finished_at INTEGER,status TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS trigger_skipped_ranges(job_id TEXT NOT NULL,schedule_epoch INTEGER NOT NULL,reason TEXT NOT NULL,first_at INTEGER NOT NULL,last_at INTEGER NOT NULL,count INTEGER NOT NULL,PRIMARY KEY(job_id,schedule_epoch,reason));
CREATE TABLE IF NOT EXISTS trigger_coalesced(job_id TEXT PRIMARY KEY,nominal_at INTEGER NOT NULL,fire_key TEXT NOT NULL,event_json TEXT,calendar_revision INTEGER,count INTEGER NOT NULL DEFAULT 1);
CREATE TABLE IF NOT EXISTS trigger_events(seq INTEGER PRIMARY KEY AUTOINCREMENT,event_id TEXT NOT NULL,source_id TEXT NOT NULL,event_type TEXT NOT NULL,owner_id TEXT NOT NULL,data_json TEXT NOT NULL,received_at INTEGER NOT NULL,UNIQUE(source_id,event_id));
CREATE TABLE IF NOT EXISTS trigger_event_gaps(owner_id TEXT NOT NULL,source_id TEXT NOT NULL,first_at INTEGER NOT NULL,last_at INTEGER NOT NULL,count INTEGER NOT NULL,PRIMARY KEY(owner_id,source_id));
CREATE TABLE IF NOT EXISTS trigger_event_matches(job_id TEXT NOT NULL,schedule_epoch INTEGER NOT NULL,source_id TEXT NOT NULL,event_id TEXT NOT NULL,PRIMARY KEY(job_id,schedule_epoch,source_id,event_id));
CREATE TABLE IF NOT EXISTS trigger_previews(token_hash TEXT PRIMARY KEY,owner_id TEXT NOT NULL,job_id TEXT,expected_revision INTEGER,draft_revision INTEGER,definition_digest TEXT NOT NULL,dependency_digest TEXT NOT NULL,expires_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS trigger_idempotency(owner_id TEXT NOT NULL,operation TEXT NOT NULL,request_key TEXT NOT NULL,request_hash TEXT NOT NULL,response_json TEXT NOT NULL,created_at INTEGER NOT NULL,PRIMARY KEY(owner_id,operation,request_key));
CREATE TABLE IF NOT EXISTS trigger_budget_windows(id TEXT PRIMARY KEY,job_id TEXT NOT NULL,start_at INTEGER NOT NULL,end_at INTEGER NOT NULL,time_zone TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS trigger_budget_reservations(occurrence_id TEXT PRIMARY KEY,job_id TEXT NOT NULL,window_id TEXT NOT NULL,utc_day TEXT NOT NULL,model INTEGER NOT NULL,status TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS trigger_runtime(id INTEGER PRIMARY KEY CHECK(id=1),clock_trusted INTEGER NOT NULL DEFAULT 0,space_ok INTEGER NOT NULL DEFAULT 1,boot_id TEXT NOT NULL,last_tick INTEGER NOT NULL DEFAULT 0);")?;
    db.execute("INSERT INTO trigger_runtime(id,clock_trusted,boot_id,last_tick) VALUES(1,0,?1,0) ON CONFLICT(id) DO UPDATE SET clock_trusted=0,boot_id=excluded.boot_id,last_tick=0",[uuid::Uuid::new_v4().to_string()])?;
    db.execute("UPDATE trigger_occurrences SET status=CASE WHEN (SELECT status FROM runs WHERE id=run_id)='completed' THEN 'completed' ELSE 'needs_attention' END,error=CASE WHEN (SELECT status FROM runs WHERE id=run_id)='completed' THEN error ELSE '服务重启，外部结果未确认，不会自动重试' END,completed_at=?1 WHERE status='running'",[now()])?;
    db.execute(
        "UPDATE trigger_attempts SET status='interrupted',finished_at=?1 WHERE status='running'",
        [now()],
    )?;
    db.execute("UPDATE trigger_budget_reservations SET status='released' WHERE status='reserved' AND occurrence_id IN(SELECT id FROM trigger_occurrences WHERE status IN ('cancelled','expired','failed','needs_attention'))",[])?;
    Ok(())
}

#[derive(Clone, Debug)]
pub struct TriggerContext {
    pub occurrence_id: String,
    pub definition: Value,
    pub snapshot: Value,
    pub origin: String,
}
pub fn run_context(db: &Connection, run_id: &str) -> anyhow::Result<Option<TriggerContext>> {
    let row=db.query_row("SELECT id,definition_json,snapshot_json,origin FROM trigger_occurrences WHERE run_id=?1",[run_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?))).optional()?;
    row.map(|(id, d, s, o)| {
        Ok(TriggerContext {
            occurrence_id: id,
            definition: serde_json::from_str(&d)?,
            snapshot: serde_json::from_str(&s)?,
            origin: o,
        })
    })
    .transpose()
}
pub fn is_trigger_in(db: &Connection, run_id: &str) -> anyhow::Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM trigger_occurrences WHERE run_id=?1)",
        [run_id],
        |r| r.get(0),
    )?)
}
pub fn owner_job(db: &Connection, owner: &str, id: &str) -> anyhow::Result<Value> {
    let row=db.query_row("SELECT id,title,revision,draft_revision,active_revision,lifecycle,block_reason,next_fire_at,created_at,updated_at,schedule_epoch,anchor,next_resume_at FROM trigger_jobs WHERE id=?1 AND owner_id=?2",params![id,owner],|r|Ok(json!({"id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"revision":r.get::<_,i64>(2)?,"draft_revision":r.get::<_,i64>(3)?,"active_revision":r.get::<_,Option<i64>>(4)?,"lifecycle":r.get::<_,String>(5)?,"block_reason":r.get::<_,Option<String>>(6)?,"next_fire_at":r.get::<_,Option<i64>>(7)?,"created_at":r.get::<_,i64>(8)?,"updated_at":r.get::<_,i64>(9)?,"schedule_epoch":r.get::<_,i64>(10)?,"anchor":r.get::<_,Option<i64>>(11)?,"next_resume_at":r.get::<_,Option<i64>>(12)?}))).optional()?.ok_or_else(missing)?;
    let mut item = row;
    let draft: String = db.query_row(
        "SELECT definition_json FROM trigger_job_revisions WHERE job_id=?1 AND revision=?2",
        params![id, item["draft_revision"].as_i64()],
        |r| r.get(0),
    )?;
    item["task_trigger_draft"] = serde_json::from_str(&draft)?;
    item["active_definition"] = if let Some(rev) = item["active_revision"].as_i64() {
        let text: String = db.query_row(
            "SELECT definition_json FROM trigger_job_revisions WHERE job_id=?1 AND revision=?2",
            params![id, rev],
            |r| r.get(0),
        )?;
        serde_json::from_str(&text)?
    } else {
        Value::Null
    };
    let last:Option<String>=db.query_row("SELECT id FROM trigger_occurrences WHERE job_id=?1 ORDER BY created_at DESC,rowid DESC LIMIT 1",[id],|r|r.get(0)).optional()?;
    item["last_occurrence"] = last
        .map(|id| occurrence(db, owner, &id))
        .transpose()?
        .unwrap_or(Value::Null);
    Ok(item)
}
pub fn list_jobs(db: &Connection, owner: &str, limit: i64, offset: i64) -> anyhow::Result<Value> {
    let total: i64 = db.query_row(
        "SELECT count(*) FROM trigger_jobs WHERE owner_id=?1",
        [owner],
        |r| r.get(0),
    )?;
    let ids=db.prepare("SELECT id FROM trigger_jobs WHERE owner_id=?1 ORDER BY updated_at DESC,rowid DESC LIMIT ?2 OFFSET ?3")?.query_map(params![owner,limit,offset],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
    let items = ids
        .iter()
        .map(|id| owner_job(db, owner, id))
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(json!({"items":items,"total":total,"limit":limit,"offset":offset}))
}
fn audit(db: &Connection, owner: &str, action: &str, resource: &str) -> anyhow::Result<()> {
    db.execute(
        "INSERT INTO audit_events(actor,action,resource,created_at) VALUES(?1,?2,?3,?4)",
        params![owner, action, resource, now()],
    )?;
    Ok(())
}
pub fn cached(
    db: &Connection,
    owner: &str,
    operation: &str,
    key: Option<&str>,
    request: &Value,
) -> anyhow::Result<Option<Value>> {
    let Some(key) = key else { return Ok(None) };
    if key.is_empty() || key.len() > 128 {
        return Err(fail("invalid_idempotency_key", "幂等编号须为1到128字节"));
    }
    let row=db.query_row("SELECT request_hash,response_json FROM trigger_idempotency WHERE owner_id=?1 AND operation=?2 AND request_key=?3",params![owner,operation,key],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional()?;
    match row {
        Some((hash, response)) if hash == digest(request) => {
            Ok(Some(serde_json::from_str(&response)?))
        }
        Some(_) => Err(conflict()),
        None => Ok(None),
    }
}
pub fn cache(
    db: &Connection,
    owner: &str,
    operation: &str,
    key: Option<&str>,
    request: &Value,
    response: &Value,
) -> anyhow::Result<()> {
    if let Some(key) = key {
        db.execute("INSERT INTO trigger_idempotency(owner_id,operation,request_key,request_hash,response_json,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![owner,operation,key,digest(request),serde_json::to_string(response)?,now()])?;
    }
    Ok(())
}
pub fn save(
    db: &Connection,
    owner: &str,
    id: Option<&str>,
    expected: Option<i64>,
    value: &Value,
) -> anyhow::Result<Value> {
    definition(value)?;
    let stamp = now();
    let new_id = uuid::Uuid::new_v4().to_string();
    let id = id.unwrap_or(&new_id);
    if let Some(expected) = expected {
        let old = owner_job(db, owner, id)?;
        if old["revision"] != expected || old["lifecycle"] == "archived" {
            return Err(conflict());
        };
        let rev = old["draft_revision"].as_i64().unwrap() + 1;
        db.execute("INSERT INTO trigger_job_revisions(job_id,revision,definition_json,created_at) VALUES(?1,?2,?3,?4)",params![id,rev,serde_json::to_string(value)?,stamp])?;
        db.execute("UPDATE trigger_jobs SET title=?1,revision=revision+1,draft_revision=?2,updated_at=?3 WHERE id=?4",params![value["title"].as_str(),rev,stamp,id])?;
    } else {
        let count: i64 = db.query_row("SELECT count(*) FROM trigger_jobs", [], |r| r.get(0))?;
        if count >= 256 {
            return Err(fail("trigger_limit", "最多保存256条任务定义"));
        };
        db.execute("INSERT INTO trigger_jobs(id,owner_id,title,revision,draft_revision,lifecycle,created_at,updated_at) VALUES(?1,?2,?3,1,1,'draft',?4,?4)",params![id,owner,value["title"].as_str(),stamp])?;
        db.execute("INSERT INTO trigger_job_revisions(job_id,revision,definition_json,created_at) VALUES(?1,1,?2,?3)",params![id,serde_json::to_string(value)?,stamp])?;
    }
    audit(db, owner, "trigger.save", id)?;
    owner_job(db, owner, id)
}
pub fn event_catalog() -> Value {
    json!([
 {"source_id":"dsh.runs","event_type":"dsh.run.completed.v1","fields":["data.status","data.run_kind"],"scope":"own_interactive_runs"},
 {"source_id":"dsh.runs","event_type":"dsh.run.failed.v1","fields":["data.status","data.run_kind"],"scope":"own_interactive_runs"},
 {"source_id":"dsh.service","event_type":"dsh.service.started.v1","fields":["data.severity"],"scope":"instance_status"}])
}
pub fn capabilities_in(db: &Connection, owner: &str, stamp: i64) -> anyhow::Result<Value> {
    let skills = crate::skills::list(db)?
        .into_iter()
        .filter(|s| s.enabled)
        .map(|s| json!({"id":s.id,"name":s.name,"revision":s.revision,"kind":s.kind}))
        .collect::<Vec<_>>();
    let _ = owner;
    let utc = DateTime::<Utc>::from_timestamp(stamp, 0)
        .ok_or_else(|| fail("invalid_clock", "服务器时间无效"))?;
    let characters=db.prepare("SELECT c.id,r.revision,json_extract(r.canonical_json,'$.data.name') FROM character_cards c JOIN character_card_revisions r ON r.card_id=c.id WHERE c.archived_at IS NULL AND r.revision=(SELECT max(revision) FROM character_card_revisions WHERE card_id=c.id) ORDER BY c.created_at DESC,c.id LIMIT 256")?.query_map([],|r|Ok(json!({"card_id":r.get::<_,String>(0)?,"revision":r.get::<_,i64>(1)?,"name":r.get::<_,String>(2)?})))?.collect::<Result<Vec<_>,_>>()?;
    Ok(
        json!({"available":true,"spec_version":"1","now":stamp,"now_iso":utc.to_rfc3339(),"now_local":utc.with_timezone(&chrono_tz::Asia::Shanghai).to_rfc3339(),"time_zone":"Asia/Shanghai","calendars":trigger_calendar::list(db)?,"event_types":event_catalog(),"skills":skills,"characters":characters,"retention":{"result_days":30,"event_body_days":7},"limits":{"jobs":256,"enabled":64,"pending":128,"model_interval_seconds":60,"notification_interval_seconds":1,"scheduled_model_concurrency":1,"global_daily_model_runs":500,"max_output_tokens":4096},"delivery":["web_inbox"]}),
    )
}
fn dependency_snapshot(db: &Connection, owner: &str, value: &Value) -> anyhow::Result<Value> {
    crate::trigger_execution::trigger_snapshot_in(db, owner, value)
}
fn dependency_digest(db: &Connection, snapshot: &Value) -> anyhow::Result<String> {
    let active = trigger_calendar::list(db)?
        .into_iter()
        .filter(|c| c.active)
        .map(|c| json!({"id":c.id,"sha256":c.sha256,"revision":c.revision}))
        .collect::<Vec<_>>();
    let (revision, _) = crate::character::current_config_in(db)?;
    Ok(digest(
        &json!({"snapshot":snapshot,"calendars":active,"tzdb":trigger_rule::TZDB_VERSION,"config_revision":revision,"operation":"activate"}),
    ))
}
pub fn preview(
    db: &Connection,
    owner: &str,
    value: &Value,
    job_id: Option<&str>,
    expected: Option<i64>,
    draft_revision: Option<i64>,
    make_token: bool,
) -> anyhow::Result<Value> {
    let stamp = now();
    let def = definition(value)?;
    if let Some(id) = job_id {
        let job = owner_job(db, owner, id)?;
        if expected != job["revision"].as_i64()
            || draft_revision != job["draft_revision"].as_i64()
            || job["task_trigger_draft"] != *value
        {
            return Err(conflict());
        }
    }
    let anchor = trigger_rule::activation_anchor(&def, stamp)
        .map_err(|s| fail("invalid_trigger_time", s))?;
    let calendars = trigger_calendar::load(db)?;
    let p = trigger_rule::preview(&def, anchor, stamp, 10, &calendars)
        .map_err(|s| fail("invalid_trigger_time", s))?;
    let snapshot = dependency_snapshot(db, owner, value)?;
    let p = serde_json::to_value(p)?;
    let token = if make_token
        && (p["blocked_reason"].is_null() || p["items"].as_array().is_some_and(|a| !a.is_empty()))
    {
        let token = crate::state::random_token();
        db.execute("INSERT INTO trigger_previews(token_hash,owner_id,job_id,expected_revision,draft_revision,definition_digest,dependency_digest,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![token_hash(&token),owner,job_id,expected,draft_revision,digest(value),dependency_digest(db,&snapshot)?,stamp+600])?;
        Some(token)
    } else {
        None
    };
    Ok(
        json!({"valid":true,"errors":[],"now":stamp,"next_times":p["items"],"preview_token":token,"estimated":matches!(value["trigger"]["type"].as_str(),Some("interval"))||value["trigger"]["mode"]=="after","blocked_reason":p["blocked_reason"],"calendar_coverage":trigger_calendar::list(db)?,"coverage_end":p["coverage_end"],"tzdb_version":p["tzdb_version"],"task_trigger_draft":value}),
    )
}
pub fn activate(
    db: &Connection,
    owner: &str,
    id: &str,
    expected: i64,
    draft_revision: i64,
    token: &str,
) -> anyhow::Result<Value> {
    let job = owner_job(db, owner, id)?;
    if job["revision"] != expected
        || job["draft_revision"] != draft_revision
        || job["lifecycle"] == "archived"
    {
        return Err(conflict());
    }
    if job["active_revision"].as_i64() == Some(draft_revision) {
        return Err(fail(
            "draft_already_active",
            "此草稿已应用；暂停后请使用恢复，重新安排请保存新草稿",
        ));
    }
    let value = &job["task_trigger_draft"];
    let snapshot = dependency_snapshot(db, owner, value)?;
    let proof=db.query_row("SELECT job_id,expected_revision,draft_revision,definition_digest,dependency_digest,expires_at FROM trigger_previews WHERE token_hash=?1 AND owner_id=?2",params![token_hash(token),owner],|r|Ok((r.get::<_,Option<String>>(0)?,r.get::<_,Option<i64>>(1)?,r.get::<_,Option<i64>>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,i64>(5)?))).optional()?.ok_or_else(conflict)?;
    if proof.0.as_deref() != Some(id)
        || proof.1 != Some(expected)
        || proof.2 != Some(draft_revision)
        || proof.3 != digest(value)
        || proof.4 != dependency_digest(db, &snapshot)?
        || proof.5 < now()
    {
        return Err(conflict());
    }
    if job["lifecycle"] != "enabled" {
        let enabled: i64 = db.query_row(
            "SELECT count(*) FROM trigger_jobs WHERE lifecycle='enabled'",
            [],
            |r| r.get(0),
        )?;
        if enabled >= 64 {
            return Err(fail("enabled_trigger_limit", "最多启用64条任务"));
        }
    }
    let def = definition(value)?;
    let stamp = now();
    let same_schedule = job["active_definition"]["trigger"] == value["trigger"]
        && job["active_definition"]["time_zone"] == value["time_zone"]
        && job["active_definition"]["active_window"] == value["active_window"];
    let anchor = if same_schedule {
        job["anchor"].as_i64()
    } else {
        trigger_rule::activation_anchor(&def, stamp).map_err(|s| fail("invalid_trigger_time", s))?
    };
    let due = trigger_rule::next_due(&def, anchor, stamp, &trigger_calendar::load(db)?)
        .map_err(|s| fail("blocked_calendar", s))?;
    if due.is_none() && value["trigger"]["type"] != "event" {
        return Err(fail("exhausted_trigger", "规则没有可执行的未来机会"));
    }
    cancel_pending(db, id, "任务版本已切换", false)?;
    db.execute("UPDATE trigger_job_revisions SET snapshot_json=?1 WHERE job_id=?2 AND revision=?3 AND snapshot_json IS NULL",params![serde_json::to_string(&snapshot)?,id,draft_revision])?;
    db.execute(
        "UPDATE trigger_execution_grants SET revoked_at=?1 WHERE job_id=?2 AND revoked_at IS NULL",
        params![stamp, id],
    )?;
    db.execute("INSERT INTO trigger_execution_grants(job_id,revision,owner_id,approved_at) VALUES(?1,?2,?3,?4) ON CONFLICT(job_id,revision) DO UPDATE SET revoked_at=NULL,approved_at=excluded.approved_at",params![id,draft_revision,owner,stamp])?;
    let event_cursor: i64 =
        db.query_row("SELECT coalesce(max(seq),0) FROM trigger_events", [], |r| {
            r.get(0)
        })?;
    db.execute("UPDATE trigger_jobs SET revision=revision+1,active_revision=?1,lifecycle='enabled',block_reason=NULL,schedule_epoch=schedule_epoch+?2,dispatch_generation=dispatch_generation+1,next_fire_at=?3,anchor=?4,cursor_at=?5,event_cursor=?6,updated_at=?5 WHERE id=?7",params![draft_revision,if same_schedule{0}else{1},due.map(|d|d.at),anchor,stamp,event_cursor,id])?;
    db.execute(
        "DELETE FROM trigger_previews WHERE token_hash=?1",
        [token_hash(token)],
    )?;
    audit(db, owner, "trigger.activate", id)?;
    owner_job(db, owner, id)
}
fn cancel_pending(
    db: &Connection,
    id: &str,
    reason: &str,
    stop_running: bool,
) -> anyhow::Result<()> {
    let statuses = if stop_running {
        "('waiting_capacity','queued','running')"
    } else {
        "('waiting_capacity','queued')"
    };
    let sql=format!("UPDATE runs SET status='cancelled',error=?1,updated_at=?2 WHERE id IN(SELECT run_id FROM trigger_occurrences WHERE job_id=?3 AND status IN {statuses}) AND status IN ('queued','running')");
    db.execute(&sql, params![reason, now(), id])?;
    db.execute(&format!("UPDATE trigger_occurrences SET status='cancelled',error=?1,cancelled_at=?2,completed_at=?2 WHERE job_id=?3 AND status IN {statuses}"),params![reason,now(),id])?;
    db.execute("UPDATE trigger_budget_reservations SET status='released' WHERE status='reserved' AND occurrence_id IN(SELECT id FROM trigger_occurrences WHERE job_id=?1 AND status='cancelled')",[id])?;
    db.execute("DELETE FROM trigger_coalesced WHERE job_id=?1", [id])?;
    Ok(())
}
pub fn lifecycle(
    db: &Connection,
    owner: &str,
    id: &str,
    expected: i64,
    action: &str,
) -> anyhow::Result<Value> {
    let job = owner_job(db, owner, id)?;
    if job["revision"] != expected || job["lifecycle"] == "archived" {
        return Err(conflict());
    }
    let stamp = now();
    match action {
        "pause" | "stop" | "archive" => {
            cancel_pending(
                db,
                id,
                if action == "stop" {
                    "任务已停止并取消当前执行"
                } else {
                    "任务已暂停或归档"
                },
                action != "pause",
            )?;
            db.execute("UPDATE trigger_jobs SET revision=revision+1,lifecycle=?1,dispatch_generation=dispatch_generation+1,block_reason=NULL,updated_at=?2 WHERE id=?3",params![if action=="archive"{"archived"}else{"paused"},stamp,id])?;
            if action == "archive" {
                db.execute(
                    "UPDATE trigger_execution_grants SET revoked_at=?1 WHERE job_id=?2",
                    params![stamp, id],
                )?;
            }
        }
        "resume" => {
            if job["lifecycle"] != "paused" || job["active_revision"].is_null() {
                return Err(fail(
                    "invalid_trigger_state",
                    "只有已启用过的暂停任务可恢复",
                ));
            };
            let enabled: i64 = db.query_row(
                "SELECT count(*) FROM trigger_jobs WHERE lifecycle='enabled'",
                [],
                |r| r.get(0),
            )?;
            if enabled >= 64 {
                return Err(fail("enabled_trigger_limit", "最多启用64条任务"));
            }
            let grant: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM trigger_execution_grants WHERE job_id=?1 AND revision=?2 AND owner_id=?3 AND revoked_at IS NULL)",
                params![id,job["active_revision"].as_i64(),owner],
                |r| r.get(0),
            )?;
            if !grant {
                return Err(fail(
                    "trigger_authorization_revoked",
                    "执行授权已撤销，请保存新草稿并重新预演启用",
                ));
            }
            let def = definition(&job["active_definition"])?;
            let snapshot = revision_snapshot(db, id, job["active_revision"].as_i64().unwrap())?;
            crate::trigger_execution::validate_dependencies_in(
                db,
                owner,
                &job["active_definition"],
                &snapshot,
            )?;
            let due = trigger_rule::next_due(
                &def,
                job["anchor"].as_i64(),
                stamp,
                &trigger_calendar::load(db)?,
            )
            .map_err(|s| fail("blocked_calendar", s))?;
            let event_cursor: i64 =
                db.query_row("SELECT coalesce(max(seq),0) FROM trigger_events", [], |r| {
                    r.get(0)
                })?;
            db.execute("UPDATE trigger_jobs SET revision=revision+1,lifecycle=?1,block_reason=NULL,next_fire_at=?2,cursor_at=?3,event_cursor=?4,dispatch_generation=dispatch_generation+1,updated_at=?3 WHERE id=?5",params![if due.is_some()||job["active_definition"]["trigger"]["type"]=="event"{"enabled"}else{"completed"},due.map(|d|d.at),stamp,event_cursor,id])?;
        }
        _ => return Err(fail("invalid_trigger_action", "未知任务操作")),
    };
    audit(db, owner, &format!("trigger.{action}"), id)?;
    owner_job(db, owner, id)
}
fn revision_snapshot(db: &Connection, id: &str, revision: i64) -> anyhow::Result<Value> {
    let text: String = db.query_row(
        "SELECT snapshot_json FROM trigger_job_revisions WHERE job_id=?1 AND revision=?2",
        params![id, revision],
        |r| r.get(0),
    )?;
    Ok(serde_json::from_str(&text)?)
}

pub fn occurrence(db: &Connection, owner: &str, id: &str) -> anyhow::Result<Value> {
    let result=db.query_row("SELECT id,job_id,job_revision,origin,nominal_at,status,run_id,CASE WHEN json_extract(definition_json,'$.action.output_mode')='native_image' AND response IS NOT NULL THEN '[原生生图结果]' ELSE response END,error,delivery_status,created_at,started_at,completed_at FROM trigger_occurrences WHERE id=?1 AND owner_id=?2",params![id,owner],|r|Ok(json!({"id":r.get::<_,String>(0)?,"job_id":r.get::<_,String>(1)?,"job_revision":r.get::<_,i64>(2)?,"origin":r.get::<_,String>(3)?,"nominal_at":r.get::<_,i64>(4)?,"status":r.get::<_,String>(5)?,"run_id":r.get::<_,Option<String>>(6)?,"response":r.get::<_,Option<String>>(7)?,"error":r.get::<_,Option<String>>(8)?,"delivery_status":r.get::<_,String>(9)?,"created_at":r.get::<_,i64>(10)?,"started_at":r.get::<_,Option<i64>>(11)?,"completed_at":r.get::<_,Option<i64>>(12)?}))).optional()?.ok_or_else(missing)?;
    let mut result = result;
    if crate::trigger_retention::result_expired_in(db, id)? {
        result["result_expired"] = json!(true);
        result["response"] = json!("[执行正文已按30天保留策略清理]");
    }
    let zone: String = db.query_row(
        "SELECT json_extract(definition_json,'$.time_zone') FROM trigger_occurrences WHERE id=?1",
        [id],
        |r| r.get(0),
    )?;
    result["time_zone"] = json!(zone);
    if result["response"] == "[原生生图结果]" {
        result["generated_image_url"] = json!(format!(
            "/api/admin/v1/task-triggers/occurrences/{id}/image"
        ));
    }
    Ok(result)
}
pub fn occurrences(
    db: &Connection,
    owner: &str,
    id: &str,
    limit: i64,
    offset: i64,
) -> anyhow::Result<Value> {
    owner_job(db, owner, id)?;
    let total: i64 = db.query_row(
        "SELECT count(*) FROM trigger_occurrences WHERE job_id=?1",
        [id],
        |r| r.get(0),
    )?;
    let ids=db.prepare("SELECT id FROM trigger_occurrences WHERE job_id=?1 ORDER BY created_at DESC,rowid DESC LIMIT ?2 OFFSET ?3")?.query_map(params![id,limit,offset],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
    let items = ids
        .iter()
        .map(|id| occurrence(db, owner, id))
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(json!({"items":items,"total":total,"limit":limit,"offset":offset}))
}

/// First representable civil instant on/after a date boundary. Political
/// transitions can remove midnight or an entire date. Search at most48hours,
/// with second refinement inside the first valid minute, rather than assuming
/// a midnight exists. Repeated midnight chooses the first occurrence.
fn civil_day_boundary(tz: Tz, date: chrono::NaiveDate) -> anyhow::Result<i64> {
    let midnight = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| fail("invalid_clock", "预算日边界无效"))?;
    if let Some(value) = tz.from_local_datetime(&midnight).earliest() {
        return Ok(value.timestamp());
    }
    for minute in 1..=48 * 60 {
        let candidate = midnight
            .checked_add_signed(Duration::minutes(minute))
            .ok_or_else(|| fail("invalid_clock", "预算日边界越界"))?;
        if tz.from_local_datetime(&candidate).earliest().is_some() {
            for second in ((minute - 1) * 60 + 1)..=minute * 60 {
                let candidate = midnight
                    .checked_add_signed(Duration::seconds(second))
                    .ok_or_else(|| fail("invalid_clock", "预算日边界越界"))?;
                if let Some(value) = tz.from_local_datetime(&candidate).earliest() {
                    return Ok(value.timestamp());
                }
            }
        }
    }
    Err(fail("invalid_clock", "48小时内找不到有效预算日边界"))
}
fn budget_window(db: &Connection, job: &str, zone: &str, stamp: i64) -> anyhow::Result<String> {
    let previous=db.query_row("SELECT id,start_at,end_at,time_zone FROM trigger_budget_windows WHERE job_id=?1 ORDER BY end_at DESC LIMIT 1",[job],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?))).optional()?;
    // Never reopen a spent day after a wall-clock rollback or time-zone edit.
    if let Some((id, _, end, _)) = &previous {
        if stamp < *end {
            return Ok(id.clone());
        }
    }
    let tz: Tz = zone
        .parse()
        .map_err(|_| fail("invalid_timezone", "时区无效"))?;
    let date = DateTime::from_timestamp(stamp, 0)
        .ok_or_else(|| fail("invalid_clock", "系统时间无效"))?
        .with_timezone(&tz)
        .date_naive();
    let civil_start = civil_day_boundary(tz, date)?;
    let next = date
        .succ_opt()
        .ok_or_else(|| fail("invalid_clock", "预算日期越界"))?;
    let mut end = civil_day_boundary(tz, next)?;
    // Rare backward shifts across midnight can revisit the previous date after
    // an early midnight. Use the final representation, never mint a fresh quota.
    if end <= stamp {
        if let Some(later) = tz
            .from_local_datetime(&next.and_hms_opt(0, 0, 0).unwrap())
            .latest()
        {
            if later.timestamp() > stamp {
                end = later.timestamp();
            }
        }
    }
    if end <= stamp {
        return Err(fail("invalid_clock", "无法确定未来预算日边界"));
    }
    if let Some((id, start, _, old_zone)) = &previous {
        let same_day = old_zone == zone
            && DateTime::<Utc>::from_timestamp(*start, 0)
                .is_some_and(|at| at.with_timezone(&tz).date_naive() == date);
        if same_day {
            db.execute(
                "UPDATE trigger_budget_windows SET end_at=?1 WHERE id=?2",
                params![end, id],
            )?;
            return Ok(id.clone());
        }
    }
    let start = civil_start.max(previous.as_ref().map(|p| p.2).unwrap_or(i64::MIN));
    if start > stamp || end <= start {
        return Err(fail("invalid_clock", "预算日区间不包含当前时刻"));
    }
    let id = uuid::Uuid::new_v4().to_string();
    db.execute("INSERT INTO trigger_budget_windows(id,job_id,start_at,end_at,time_zone) VALUES(?1,?2,?3,?4,?5)",params![id,job,start,end,zone])?;
    Ok(id)
}
fn reserve_budget(
    db: &Connection,
    occ: &str,
    job: &str,
    value: &Value,
    stamp: i64,
    spend: bool,
) -> anyhow::Result<bool> {
    let window = budget_window(
        db,
        job,
        value["time_zone"].as_str().unwrap_or("Asia/Shanghai"),
        stamp,
    )?;
    let day = DateTime::<Utc>::from_timestamp(stamp, 0)
        .ok_or_else(|| fail("invalid_clock", "时钟无效"))?
        .format("%Y-%m-%d")
        .to_string();
    let model = value["action"]["type"] == "assistant_run";
    if let Some((old, status,old_day)) = db
        .query_row(
            "SELECT window_id,status,utc_day FROM trigger_budget_reservations WHERE occurrence_id=?1",
            [occ],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?,r.get::<_,String>(2)?)),
        )
        .optional()?
    {
        if status == "spent" {
            return Ok(true);
        };
        if status == "reserved" && old == window && old_day==day {
            if spend {
                db.execute(
                    "UPDATE trigger_budget_reservations SET status='spent' WHERE occurrence_id=?1",
                    [occ],
                )?;
            }
            return Ok(true);
        };
        db.execute(
            "UPDATE trigger_budget_reservations SET status='released' WHERE occurrence_id=?1",
            [occ],
        )?;
    }
    let (total,models):(i64,i64)=db.query_row("SELECT count(*),coalesce(sum(model),0) FROM trigger_budget_reservations WHERE window_id=?1 AND status IN ('reserved','spent')",[&window],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let global:i64=db.query_row("SELECT count(*) FROM trigger_budget_reservations WHERE utc_day=?1 AND model=1 AND status IN ('reserved','spent')",[&day],|r|r.get(0))?;
    if total
        >= value["budget"]["daily_occurrences"]
            .as_i64()
            .unwrap_or(1000)
        || (model
            && (models >= value["budget"]["daily_model_runs"].as_i64().unwrap_or(10)
                || global >= 500))
    {
        let end: i64 = db.query_row(
            "SELECT end_at FROM trigger_budget_windows WHERE id=?1",
            [&window],
            |r| r.get(0),
        )?;
        db.execute(
            "UPDATE trigger_jobs SET block_reason='budget',next_resume_at=?1 WHERE id=?2",
            params![end, job],
        )?;
        return Ok(false);
    }
    db.execute("INSERT INTO trigger_budget_reservations(occurrence_id,job_id,window_id,utc_day,model,status) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(occurrence_id) DO UPDATE SET window_id=excluded.window_id,utc_day=excluded.utc_day,model=excluded.model,status=excluded.status",params![occ,job,window,day,model,if spend{"spent"}else{"reserved"}])?;
    Ok(true)
}
// Explicit manual tests are authorized once by their authenticated mutation.
// Recurring admission must still hold the exact live grant approved on activation.
// This guard is deliberately not used when finishing an already running snapshot.
fn occurrence_authorized(db: &Connection, occurrence: &str) -> anyhow::Result<bool> {
    Ok(db.query_row(
        "SELECT CASE WHEN o.origin='manual_test' THEN 1 WHEN o.origin='trigger' THEN EXISTS(SELECT 1 FROM trigger_execution_grants g JOIN trigger_jobs j ON j.id=g.job_id WHERE g.job_id=o.job_id AND g.revision=o.job_revision AND g.owner_id=o.owner_id AND g.revoked_at IS NULL AND j.owner_id=o.owner_id AND j.active_revision=o.job_revision AND j.lifecycle='enabled') ELSE 0 END FROM trigger_occurrences o WHERE o.id=?1",
        [occurrence],
        |r| r.get(0),
    )?)
}
pub fn prepare_trigger_run_in(db: &Connection, run_id: &str) -> anyhow::Result<bool> {
    let Some(context) = run_context(db, run_id)? else {
        return Ok(true);
    };
    let stamp = now();
    let (job, owner, status, deadline): (String, String, String, i64) = db.query_row(
        "SELECT job_id,owner_id,status,deadline FROM trigger_occurrences WHERE id=?1",
        [&context.occurrence_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let trusted: bool = db.query_row(
        "SELECT clock_trusted AND space_ok FROM trigger_runtime WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    let dependency = crate::trigger_execution::validate_dependencies_in(
        db,
        &owner,
        &context.definition,
        &context.snapshot,
    );
    let mut reason = if !trusted {
        Some("时钟或可用存储未通过安全检查，任务未发送".to_owned())
    } else if status != "queued" || deadline < stamp {
        Some("任务已取消或超过派发期限".to_owned())
    } else if !occurrence_authorized(db, &context.occurrence_id)? {
        db.execute("UPDATE trigger_jobs SET block_reason='authorization' WHERE id=?1 AND lifecycle='enabled'",[&job])?;
        Some("任务执行授权已撤销或不再对应当前生效版本".to_owned())
    } else {
        dependency
            .err()
            .map(|_| "任务依赖已停用、权限或配置已失效".to_owned())
    };
    if reason.is_none()
        && !reserve_budget(
            db,
            &context.occurrence_id,
            &job,
            &context.definition,
            stamp,
            true,
        )?
    {
        reason = Some("当前每日预算已用尽，未发送模型请求".into())
    }
    if let Some(reason) = reason {
        db.execute("UPDATE runs SET status='cancelled',error=?1,updated_at=?2 WHERE id=?3 AND status IN ('queued','running')",params![reason,stamp,run_id])?;
        db.execute("UPDATE trigger_occurrences SET status='expired',error=?1,completed_at=?2 WHERE id=?3 AND status='queued'",params![reason,stamp,context.occurrence_id])?;
        db.execute("UPDATE trigger_budget_reservations SET status='released' WHERE occurrence_id=?1 AND status='reserved'",[context.occurrence_id])?;
        return Ok(false);
    }
    db.execute(
        "UPDATE trigger_occurrences SET status='running',started_at=?1 WHERE id=?2",
        params![stamp, context.occurrence_id],
    )?;
    db.execute(
        "UPDATE trigger_jobs SET last_started_at=?1 WHERE id=?2",
        params![stamp, job],
    )?;
    db.execute("INSERT OR IGNORE INTO trigger_attempts(run_id,occurrence_id,fence,started_at,status) VALUES(?1,?2,?3,?4,'running')",params![run_id,context.occurrence_id,uuid::Uuid::new_v4().to_string(),stamp])?;
    Ok(true)
}
pub fn finish_run_in(
    db: &Connection,
    run_id: &str,
    status: &str,
    response: Option<&str>,
    error: Option<&str>,
) -> anyhow::Result<()> {
    let Some(context) = run_context(db, run_id)? else {
        return Ok(());
    };
    let stamp = now();
    db.execute("UPDATE trigger_occurrences SET status=?1,response=?2,error=?3,delivery_status=?4,completed_at=?5 WHERE id=?6 AND run_id=?7 AND status IN ('running','queued') AND cancelled_at IS NULL",params![if status=="completed"{"completed"}else if status=="failed"{"failed"}else{"needs_attention"},response,error,if status=="completed"{"stored"}else{"pending"},stamp,context.occurrence_id,run_id])?;
    db.execute(
        "UPDATE trigger_attempts SET status=?1,finished_at=?2 WHERE run_id=?3 AND status='running'",
        params![status, stamp, run_id],
    )?;
    Ok(())
}

pub fn emit_event(
    db: &Connection,
    source: &str,
    id: &str,
    kind: &str,
    owner: &str,
    data: &Value,
) -> anyhow::Result<()> {
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM trigger_events WHERE source_id=?1 AND event_id=?2)",
        params![source, id],
        |r| r.get(0),
    )?;
    if exists {
        return Ok(());
    }
    let pending:i64=db.query_row("SELECT count(*) FROM trigger_events WHERE seq>(SELECT coalesce(min(event_cursor),(SELECT coalesce(max(seq),0) FROM trigger_events)) FROM trigger_jobs j JOIN trigger_job_revisions r ON r.job_id=j.id AND r.revision=j.active_revision WHERE j.lifecycle='enabled' AND json_extract(r.definition_json,'$.trigger.type')='event')",[],|r|r.get(0))?;
    if pending >= 4096 {
        db.execute("INSERT INTO trigger_event_gaps(owner_id,source_id,first_at,last_at,count) VALUES(?1,?2,?3,?3,1) ON CONFLICT(owner_id,source_id) DO UPDATE SET last_at=excluded.last_at,count=count+1",params![owner,source,now()])?;
        db.execute("UPDATE trigger_jobs SET block_reason='event_source' WHERE lifecycle='enabled' AND (owner_id=?1 OR ?1='*') AND id IN(SELECT job_id FROM trigger_job_revisions r WHERE revision=trigger_jobs.active_revision AND json_extract(definition_json,'$.trigger.type')='event' AND json_extract(definition_json,'$.trigger.source_id')=?2)",params![owner,source])?;
        return Ok(());
    }
    db.execute("INSERT OR IGNORE INTO trigger_events(event_id,source_id,event_type,owner_id,data_json,received_at) VALUES(?1,?2,?3,?4,?5,?6)",params![id,source,kind,owner,serde_json::to_string(data)?,now()])?;
    Ok(())
}
pub fn observe_interactive_run_in(
    db: &Connection,
    run_id: &str,
    status: &str,
) -> anyhow::Result<()> {
    if !["completed", "failed"].contains(&status) || is_trigger_in(db, run_id)? {
        return Ok(());
    }
    let row = db
        .query_row(
            "SELECT owner_id,run_kind,character_snapshot_json FROM runs WHERE id=?1",
            [run_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((owner, kind, persona)) = row else {
        return Ok(());
    };
    let admin: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM admins WHERE id=?1)",
        [&owner],
        |r| r.get(0),
    )?;
    let test = persona
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .is_some_and(|v| v["is_test"] == true);
    if admin && !test && ["model", "skill_text", "skill_image"].contains(&kind.as_str()) {
        emit_event(
            db,
            "dsh.runs",
            &format!("{run_id}:{status}"),
            &format!("dsh.run.{status}.v1"),
            &owner,
            &json!({"status":status,"run_kind":kind}),
        )?;
    }
    Ok(())
}
pub fn service_started_in(db: &Connection) -> anyhow::Result<()> {
    let boot: String = db.query_row("SELECT boot_id FROM trigger_runtime WHERE id=1", [], |r| {
        r.get(0)
    })?;
    emit_event(
        db,
        "dsh.service",
        &boot,
        "dsh.service.started.v1",
        "*",
        &json!({"severity":"info"}),
    )
}

fn skip(
    db: &Connection,
    job: &str,
    epoch: i64,
    at: i64,
    count: i64,
    reason: &str,
) -> anyhow::Result<()> {
    if count <= 0 {
        return Ok(());
    };
    db.execute("INSERT INTO trigger_skipped_ranges(job_id,schedule_epoch,reason,first_at,last_at,count) VALUES(?1,?2,?3,?4,?4,?5) ON CONFLICT(job_id,schedule_epoch,reason) DO UPDATE SET last_at=excluded.last_at,count=count+excluded.count",params![job,epoch,reason,at,count])?;
    Ok(())
}
fn active_count(db: &Connection, job: &str) -> anyhow::Result<i64> {
    Ok(db.query_row("SELECT count(*) FROM trigger_occurrences WHERE job_id=?1 AND status IN ('waiting_capacity','queued','running')",[job],|r|r.get(0))?)
}
fn insert_occurrence(
    db: &Connection,
    job: &Value,
    value: &Value,
    snapshot: &Value,
    at: i64,
    key: &str,
    origin: &str,
    calendar_revision: Option<i64>,
    event: Option<&Value>,
    stamp: i64,
) -> anyhow::Result<Option<String>> {
    let id = uuid::Uuid::new_v4().to_string();
    let epoch = job["schedule_epoch"].as_i64().unwrap_or(0);
    let mut snapshot = snapshot.clone();
    if let Some(event) = event {
        snapshot["event"] = event.clone();
    }
    let deadline = at.saturating_add(value["policy"]["grace_seconds"].as_i64().unwrap_or(300));
    let deadline = if at < stamp && value["policy"]["misfire"] != "skip" {
        stamp + value["policy"]["grace_seconds"].as_i64().unwrap_or(300)
    } else {
        deadline
    };
    let inserted=db.execute("INSERT OR IGNORE INTO trigger_occurrences(id,job_id,job_revision,owner_id,schedule_epoch,fire_key,origin,nominal_at,deadline,definition_json,snapshot_json,status,created_at,calendar_revision) SELECT ?1,?2,?3,owner_id,?4,?5,?6,?7,?8,?9,?10,'waiting_capacity',?11,?12 FROM trigger_jobs WHERE id=?2",params![id,job["id"].as_str(),if origin=="manual_test" {job["draft_revision"].as_i64()}else{job["active_revision"].as_i64()},epoch,key,origin,at,deadline,serde_json::to_string(value)?,serde_json::to_string(&snapshot)?,stamp,calendar_revision])?;
    Ok(if inserted == 1 { Some(id) } else { None })
}
fn dispatch(db: &Connection, id: &str) -> anyhow::Result<bool> {
    dispatch_at(db, id, now())
}
fn dispatch_at(db: &Connection, id: &str, stamp: i64) -> anyhow::Result<bool> {
    let (job,owner,text,snapshot,deadline,status):(String,String,String,String,i64,String)=db.query_row("SELECT job_id,owner_id,definition_json,snapshot_json,deadline,status FROM trigger_occurrences WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
    if status != "waiting_capacity" {
        return Ok(false);
    }
    let value: Value = serde_json::from_str(&text)?;
    let snapshot: Value = serde_json::from_str(&snapshot)?;
    if deadline < stamp {
        db.execute("UPDATE trigger_occurrences SET status='expired',error='超过派发期限',completed_at=?1 WHERE id=?2",params![stamp,id])?;
        return Ok(false);
    }
    if !occurrence_authorized(db, id)? {
        db.execute("UPDATE trigger_jobs SET block_reason='authorization' WHERE id=?1 AND lifecycle='enabled'",[&job])?;
        db.execute("UPDATE trigger_occurrences SET status='cancelled',error='执行授权已撤销或不再对应当前生效版本',cancelled_at=?1,completed_at=?1 WHERE id=?2",params![stamp,id])?;
        db.execute("UPDATE trigger_budget_reservations SET status='released' WHERE occurrence_id=?1 AND status='reserved'",[id])?;
        return Ok(false);
    }
    if crate::trigger_execution::validate_dependencies_in(db, &owner, &value, &snapshot).is_err() {
        db.execute(
            "UPDATE trigger_jobs SET block_reason='dependency' WHERE id=?1",
            [&job],
        )?;
        db.execute("UPDATE trigger_occurrences SET status='failed',error='依赖、权限或模型配置已失效',completed_at=?1 WHERE id=?2",params![stamp,id])?;
        return Ok(false);
    }
    let pending: i64 =
        db.query_row("SELECT count(*) FROM runs WHERE status='queued'", [], |r| {
            r.get(0)
        })?;
    if pending >= 500 {
        return Ok(false);
    }
    let model = value["action"]["type"] == "assistant_run";
    if model {
        let last: Option<i64> = db.query_row(
            "SELECT last_started_at FROM trigger_jobs WHERE id=?1",
            [&job],
            |r| r.get(0),
        )?;
        if last.is_some_and(|last| stamp < last + 60) {
            return Ok(false);
        }
    }
    if !reserve_budget(db, id, &job, &value, stamp, !model)? {
        db.execute("UPDATE trigger_occurrences SET status='expired',error='每日预算已用尽',completed_at=?1 WHERE id=?2",params![stamp,id])?;
        return Ok(false);
    }
    if model {
        let run = crate::trigger_execution::enqueue_trigger_in(db, &owner, id, &value, &snapshot)?;
        db.execute(
            "UPDATE trigger_occurrences SET status='queued',run_id=?1 WHERE id=?2",
            params![run, id],
        )?;
    } else {
        let nominal: i64 = db.query_row(
            "SELECT nominal_at FROM trigger_occurrences WHERE id=?1",
            [id],
            |r| r.get(0),
        )?;
        let content = value["action"]["content"]
            .as_str()
            .unwrap_or("")
            .replace("{task_title}", value["title"].as_str().unwrap_or(""))
            .replace("{scheduled_local}", &at_local(&value, nominal))
            .replace(
                "{event.summary}",
                if snapshot["event"].is_null() {
                    ""
                } else {
                    "收到已授权的事件"
                },
            );
        if content.len() > 16000 {
            db.execute("UPDATE trigger_occurrences SET status='failed',error='提醒替换后超过16000字节',completed_at=?1 WHERE id=?2",params![stamp,id])?;
            return Ok(false);
        };
        db.execute("UPDATE trigger_occurrences SET status='completed',response=?1,delivery_status='stored',started_at=?2,completed_at=?2 WHERE id=?3",params![content,stamp,id])?;
    }
    db.execute("UPDATE trigger_jobs SET block_reason=NULL,next_resume_at=NULL WHERE id=?1 AND block_reason IN ('capacity','dependency')",[job])?;
    Ok(true)
}
fn at_local(value: &Value, stamp: i64) -> String {
    value["time_zone"]
        .as_str()
        .and_then(|s| s.parse::<Tz>().ok())
        .and_then(|tz| {
            DateTime::<Utc>::from_timestamp(stamp, 0).map(|d| d.with_timezone(&tz).to_rfc3339())
        })
        .unwrap_or_default()
}

pub fn test_run(
    db: &Connection,
    owner: &str,
    id: &str,
    expected: i64,
    key: &str,
) -> anyhow::Result<Value> {
    if key.is_empty() {
        return Err(fail("idempotency_required", "手动执行需要幂等编号"));
    };
    let job = owner_job(db, owner, id)?;
    if job["revision"] != expected || job["lifecycle"] == "archived" {
        return Err(conflict());
    };
    let value = job["task_trigger_draft"].clone();
    definition(&value)?;
    let snapshot = dependency_snapshot(db, owner, &value)?;
    if active_count(db, id)? > 0 {
        return Err(fail("trigger_busy", "本任务已有未结束执行"));
    };
    let total:i64=db.query_row("SELECT count(*) FROM trigger_occurrences WHERE status IN ('waiting_capacity','queued','running')",[],|r|r.get(0))?;
    if total >= 128 {
        return Err(fail("trigger_capacity", "自动任务等待队列已满"));
    }
    let occ = insert_occurrence(
        db,
        &job,
        &value,
        &snapshot,
        now(),
        &format!("manual:{key}"),
        "manual_test",
        None,
        None,
        now(),
    )?
    .ok_or_else(conflict)?;
    dispatch(db, &occ)?;
    occurrence(db, owner, &occ)
}

pub fn tick_in(db: &Connection, stamp: i64) -> anyhow::Result<()> {
    // Native leases are managed by worker; this loop only admits bounded work.
    let clock: bool = db.query_row(
        "SELECT clock_trusted AND space_ok FROM trigger_runtime WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    if !clock {
        db.execute("UPDATE trigger_jobs SET block_reason='clock' WHERE lifecycle='enabled' AND (block_reason IS NULL OR block_reason='clock')",[])?;
        return Ok(());
    }
    db.execute(
        "UPDATE trigger_jobs SET block_reason=NULL WHERE block_reason='clock'",
        [],
    )?;
    db.execute("UPDATE trigger_jobs SET block_reason=NULL,next_resume_at=NULL WHERE lifecycle='enabled' AND block_reason='budget' AND next_resume_at<=?1",[stamp])?;
    let waiting=db.prepare("SELECT id FROM trigger_occurrences WHERE status='waiting_capacity' ORDER BY created_at LIMIT 32")?.query_map([],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
    for id in waiting {
        dispatch_at(db, &id, stamp)?;
    }
    let jobs=db.prepare("SELECT id,owner_id FROM trigger_jobs WHERE lifecycle='enabled' AND (block_reason IS NULL OR block_reason IN ('calendar','dependency','config','capacity')) ORDER BY last_evaluated_at,id LIMIT 32")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<Result<Vec<_>,_>>()?;
    let calendars = trigger_calendar::load(db)?;
    for (id, owner) in jobs {
        db.execute(
            "UPDATE trigger_jobs SET last_evaluated_at=?1 WHERE id=?2",
            params![stamp, id],
        )?;
        let job = owner_job(db, &owner, &id)?;
        let value = &job["active_definition"];
        if value.is_null() {
            continue;
        };
        let def = definition(value)?;
        let revision = job["active_revision"].as_i64().unwrap();
        let snapshot = revision_snapshot(db, &id, revision)?;
        let epoch = job["schedule_epoch"].as_i64().unwrap();
        let gap: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM trigger_event_gaps WHERE (owner_id=?1 OR owner_id='*') AND source_id=?2)",
            params![owner,value["trigger"]["source_id"].as_str().unwrap_or("")],
            |r| r.get(0),
        )?;
        if value["trigger"]["type"] == "event" {
            if gap {
                db.execute(
                    "UPDATE trigger_jobs SET block_reason='event_source' WHERE id=?1",
                    [&id],
                )?;
                continue;
            };
            process_events(db, &job, value, &snapshot, &def, &calendars, stamp)?;
            continue;
        }
        let current: Option<i64> = db.query_row(
            "SELECT next_fire_at FROM trigger_jobs WHERE id=?1",
            [&id],
            |r| r.get(0),
        )?;
        if current.is_some_and(|at| at > stamp) {
            continue;
        }
        let max_count = if value["policy"]["misfire"] == "bounded_catch_up" {
            value["policy"]["max_catch_up"].as_u64().unwrap_or(1) as usize
        } else {
            1
        };
        let candidates = if let Some(current) = current {
            match trigger_rule::recent_due(
                &def,
                job["anchor"].as_i64(),
                stamp,
                max_count,
                &calendars,
            ) {
                Ok(v) => v
                    .into_iter()
                    .filter(|d| d.at >= current)
                    .collect::<Vec<_>>(),
                Err(_) => {
                    db.execute(
                        "UPDATE trigger_jobs SET block_reason='calendar' WHERE id=?1",
                        [&id],
                    )?;
                    continue;
                }
            }
        } else {
            Vec::new()
        };
        if let Some(current) = current {
            if let Some(every) = value["trigger"]["every_seconds"].as_i64() {
                let elapsed = (stamp - current).max(0) / every + 1;
                let omitted = elapsed.saturating_sub(candidates.len() as i64);
                if omitted > 0 {
                    skip(db, &id, epoch, current, omitted, "missed_interval")?;
                }
            } else if candidates.is_empty() {
                skip(db, &id, epoch, current, 1, "missed")?;
            }
        }
        let mut capacity_blocked = false;
        for due in candidates {
            if active_count(db, &id)? > 0 {
                if value["policy"]["overlap"] == "coalesce_one" {
                    db.execute("INSERT INTO trigger_coalesced(job_id,nominal_at,fire_key,calendar_revision,count) VALUES(?1,?2,?3,?4,1) ON CONFLICT(job_id) DO UPDATE SET nominal_at=excluded.nominal_at,fire_key=excluded.fire_key,calendar_revision=excluded.calendar_revision,count=count+1",params![id,due.at,due.key,due.calendar_revision])?;
                } else {
                    skip(db, &id, epoch, due.at, 1, "overlap")?;
                }
            } else {
                let total:i64=db.query_row("SELECT count(*) FROM trigger_occurrences WHERE status IN ('waiting_capacity','queued','running')",[],|r|r.get(0))?;
                if total >= 128 {
                    db.execute(
                        "UPDATE trigger_jobs SET block_reason='capacity' WHERE id=?1",
                        [&id],
                    )?;
                    capacity_blocked = true;
                    break;
                }
                if let Some(occ) = insert_occurrence(
                    db,
                    &job,
                    value,
                    &snapshot,
                    due.at,
                    &due.key,
                    "trigger",
                    due.calendar_revision,
                    None,
                    stamp,
                )? {
                    dispatch_at(db, &occ, stamp)?;
                }
            }
        }
        if capacity_blocked {
            continue;
        }
        match trigger_rule::next_due(&def, job["anchor"].as_i64(), stamp, &calendars) {
            Ok(next) => {
                db.execute("UPDATE trigger_jobs SET next_fire_at=?1,cursor_at=?2,block_reason=CASE WHEN block_reason='calendar' THEN NULL ELSE block_reason END,lifecycle=CASE WHEN ?1 IS NULL AND ?3=0 THEN 'completed' ELSE lifecycle END WHERE id=?4",params![next.as_ref().map(|n|n.at),stamp,active_count(db,&id)?,id])?;
            }
            Err(_) => {
                db.execute("UPDATE trigger_jobs SET next_fire_at=NULL,cursor_at=?1,block_reason='calendar' WHERE id=?2",params![stamp,id])?;
            }
        }
    }
    // A single coalesced marker can become one fresh admission when the prior run settles.
    let coalesced=db.prepare("SELECT c.job_id,j.owner_id,c.nominal_at,c.fire_key,c.event_json,c.calendar_revision FROM trigger_coalesced c JOIN trigger_jobs j ON j.id=c.job_id WHERE j.lifecycle='enabled' LIMIT 32")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,Option<i64>>(5)?)))?.collect::<Result<Vec<_>,_>>()?;
    for (id, owner, at, key, event, calendar) in coalesced {
        if active_count(db, &id)? > 0 {
            continue;
        };
        let job = owner_job(db, &owner, &id)?;
        let value = &job["active_definition"];
        let allowed = stamp <= at + value["policy"]["grace_seconds"].as_i64().unwrap_or(300)
            || (value["policy"]["misfire"] != "skip"
                && stamp
                    <= at
                        + value["policy"]["catch_up_window_seconds"]
                            .as_i64()
                            .unwrap_or(0));
        if allowed {
            let snapshot = revision_snapshot(db, &id, job["active_revision"].as_i64().unwrap())?;
            let event = event
                .map(|s| serde_json::from_str::<Value>(&s))
                .transpose()?;
            if let Some(occ) = insert_occurrence(
                db,
                &job,
                value,
                &snapshot,
                at,
                &key,
                "trigger",
                calendar,
                event.as_ref(),
                stamp,
            )? {
                dispatch_at(db, &occ, stamp)?;
            }
        } else {
            skip(
                db,
                &id,
                job["schedule_epoch"].as_i64().unwrap(),
                at,
                1,
                "coalesced_expired",
            )?;
        }
        db.execute("DELETE FROM trigger_coalesced WHERE job_id=?1", [id])?;
    }
    db.execute("UPDATE trigger_jobs SET lifecycle='completed' WHERE lifecycle='enabled' AND block_reason IS NULL AND active_revision IS NOT NULL AND next_fire_at IS NULL AND json_extract((SELECT definition_json FROM trigger_job_revisions WHERE job_id=trigger_jobs.id AND revision=active_revision),'$.trigger.type')!='event' AND NOT EXISTS(SELECT 1 FROM trigger_occurrences WHERE job_id=trigger_jobs.id AND status IN ('waiting_capacity','queued','running')) AND NOT EXISTS(SELECT 1 FROM trigger_coalesced WHERE job_id=trigger_jobs.id)",[])?;
    db.execute(
        "UPDATE trigger_runtime SET last_tick=?1 WHERE id=1",
        [stamp],
    )?;
    db.execute("DELETE FROM trigger_previews WHERE expires_at<?1", [stamp])?;
    Ok(())
}
fn process_events(
    db: &Connection,
    job: &Value,
    value: &Value,
    snapshot: &Value,
    def: &trigger_rule::TaskDefinition,
    calendars: &trigger_calendar::CalendarSet,
    stamp: i64,
) -> anyhow::Result<()> {
    let id = job["id"].as_str().unwrap();
    let (owner, cursor, last): (String, i64, Option<i64>) = db.query_row(
        "SELECT owner_id,event_cursor,last_event_at FROM trigger_jobs WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let epoch = job["schedule_epoch"].as_i64().unwrap();
    let events=db.prepare("SELECT seq,event_id,source_id,event_type,data_json,received_at FROM trigger_events WHERE seq>?1 AND (owner_id=?2 OR owner_id='*') ORDER BY seq LIMIT 64")?.query_map(params![cursor,owner],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,i64>(5)?)))?.collect::<Result<Vec<_>,_>>()?;
    let mut last = last;
    for (seq, event_id, source, event_type, data, received) in events {
        let data: Value = serde_json::from_str(&data)?;
        let matched =
            trigger_rule::event_matches(def, &source, &event_type, &data, received, calendars);
        db.execute(
            "UPDATE trigger_jobs SET event_cursor=?1 WHERE id=?2",
            params![seq, id],
        )?;
        let matched = match matched {
            Ok(b) => b,
            Err(_) => {
                skip(db, id, epoch, received, 1, "blocked_calendar")?;
                db.execute(
                    "UPDATE trigger_jobs SET block_reason='calendar' WHERE id=?1",
                    [id],
                )?;
                continue;
            }
        };
        if !matched {
            continue;
        };
        let grace = value["policy"]["grace_seconds"].as_i64().unwrap_or(300);
        if received + grace < stamp {
            skip(db, id, epoch, received, 1, "event_expired")?;
            continue;
        };
        let cooldown = value["trigger"]["cooldown_seconds"].as_i64().unwrap_or(0);
        if last.is_some_and(|t| received < t + cooldown) {
            skip(db, id, epoch, received, 1, "cooldown")?;
            continue;
        };
        let inserted=db.execute("INSERT OR IGNORE INTO trigger_event_matches(job_id,schedule_epoch,source_id,event_id) VALUES(?1,?2,?3,?4)",params![id,epoch,source,event_id])?;
        if inserted == 0 {
            continue;
        };
        let key = format!("event:{source}:{event_id}");
        let event = json!({"source":source,"type":event_type,"id":event_id,"received_at":received,"data":data});
        if active_count(db, id)? > 0 {
            if value["policy"]["overlap"] == "coalesce_one" {
                db.execute("INSERT INTO trigger_coalesced(job_id,nominal_at,fire_key,event_json,count) VALUES(?1,?2,?3,?4,1) ON CONFLICT(job_id) DO UPDATE SET nominal_at=excluded.nominal_at,fire_key=excluded.fire_key,event_json=excluded.event_json,count=count+1",params![id,received,key,serde_json::to_string(&event)?])?;
            } else {
                skip(db, id, epoch, received, 1, "overlap")?;
            }
        } else {
            let total:i64=db.query_row("SELECT count(*) FROM trigger_occurrences WHERE status IN ('waiting_capacity','queued','running')",[],|r|r.get(0))?;
            if total < 128 {
                if let Some(occ) = insert_occurrence(
                    db,
                    job,
                    value,
                    snapshot,
                    received,
                    &key,
                    "trigger",
                    None,
                    Some(&event),
                    stamp,
                )? {
                    dispatch_at(db, &occ, stamp)?;
                }
            } else {
                skip(db, id, epoch, received, 1, "capacity")?;
            }
        }
        last = Some(received);
        db.execute(
            "UPDATE trigger_jobs SET last_event_at=?1 WHERE id=?2",
            params![received, id],
        )?;
    }
    Ok(())
}

pub fn tick(state: &AppState) -> crate::error::ApiResult<()> {
    state.db(|db| {
        let tx = db.transaction()?;
        tick_in(&tx, now())?;
        tx.commit()?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> (tempfile::TempDir, AppState) {
        let temp = tempfile::TempDir::new().unwrap();
        let state = AppState::open(crate::state::Options {
            data_dir: temp.path().into(),
            origin: "http://127.0.0.1:18419".into(),
            allow_private_model: true,
            cookie_secure: false,
            web_dir: temp.path().join("web"),
        })
        .unwrap();
        state.db(|db|{db.execute("INSERT INTO admins(id,username,password_hash,created_at) VALUES('owner','admin','unused',0),('other','other','unused',0)",[])?;db.execute("UPDATE trigger_runtime SET clock_trusted=1",[])?;Ok(())}).unwrap();
        (temp, state)
    }
    fn fixture(name: &str) -> Value {
        let text = match name {
            "interval" => include_str!("../../../docs/examples/task-triggers/interval.json"),
            "event" => include_str!("../../../docs/examples/task-triggers/event.json"),
            "model" => include_str!("../../../docs/examples/task-triggers/assistant-workday.json"),
            _ => include_str!("../../../docs/examples/task-triggers/once-after.json"),
        };
        serde_json::from_str::<Value>(text).unwrap()["task_trigger_draft"].clone()
    }
    fn enabled(db: &Connection, value: &Value) -> anyhow::Result<Value> {
        let job = save(db, "owner", None, None, value)?;
        let id = job["id"].as_str().unwrap();
        let p = preview(db, "owner", value, Some(id), Some(1), Some(1), true)?;
        activate(db, "owner", id, 1, 1, p["preview_token"].as_str().unwrap())
    }
    #[test]
    fn notification_is_persisted_once_and_epoch_does_not_reset_on_prompt_edit() {
        let (_temp, state) = state();
        state
            .db(|db| {
                let tx = db.transaction()?;
                let mut value = fixture("interval");
                value["trigger"]["every_seconds"] = json!(1);
                let job = enabled(&tx, &value)?;
                let id = job["id"].as_str().unwrap();
                let at = job["next_fire_at"].as_i64().unwrap();
                tick_in(&tx, at)?;
                tick_in(&tx, at)?;
                let count: i64 = tx.query_row(
                    "SELECT count(*) FROM trigger_occurrences WHERE job_id=?1",
                    [id],
                    |r| r.get(0),
                )?;
                assert_eq!(count, 1);
                let epoch = job["schedule_epoch"].as_i64().unwrap();
                value["title"] = json!("changed title");
                let edited = save(&tx, "owner", Some(id), Some(2), &value)?;
                let p = preview(&tx, "owner", &value, Some(id), Some(3), Some(2), true)?;
                let active =
                    activate(&tx, "owner", id, 3, 2, p["preview_token"].as_str().unwrap())?;
                assert_eq!(active["schedule_epoch"], epoch);
                assert_eq!(active["anchor"], job["anchor"]);
                assert_eq!(edited["active_revision"], 1);
                assert_eq!(active["active_revision"], 2);
                tx.commit()?;
                Ok(())
            })
            .unwrap();
    }
    #[test]
    fn one_second_downtime_has_three_bounded_catchups_without_expanding_history() {
        let (_temp, state) = state();
        state.db(|db|{let tx=db.transaction()?;let mut value=fixture("interval");value["trigger"]["every_seconds"]=json!(1);value["policy"]=json!({"misfire":"bounded_catch_up","grace_seconds":1,"max_catch_up":3,"overlap":"coalesce_one","catch_up_window_seconds":3600});let job=enabled(&tx,&value)?;let id=job["id"].as_str().unwrap();let stamp=now();tx.execute("UPDATE trigger_jobs SET anchor=?1,next_fire_at=?1 WHERE id=?2",params![stamp-86400,id])?;tick_in(&tx,stamp)?;let count:i64=tx.query_row("SELECT count(*) FROM trigger_occurrences WHERE job_id=?1 AND status='completed'",[id],|r|r.get(0))?;assert_eq!(count,3);let skipped:i64=tx.query_row("SELECT sum(count) FROM trigger_skipped_ranges WHERE job_id=?1",[id],|r|r.get(0))?;assert!(skipped>86000);tick_in(&tx,stamp)?;let after:i64=tx.query_row("SELECT count(*) FROM trigger_occurrences WHERE job_id=?1",[id],|r|r.get(0))?;assert_eq!(after,3);tx.commit()?;Ok(())}).unwrap();
    }
    #[test]
    fn preview_is_scoped_to_owner_definition_calendar_and_new_draft() {
        let (_temp, state) = state();
        state
            .db(|db| {
                let tx = db.transaction()?;
                let value = fixture("once");
                let job = save(&tx, "owner", None, None, &value)?;
                let id = job["id"].as_str().unwrap();
                assert!(owner_job(&tx, "other", id).is_err());
                let p = preview(&tx, "owner", &value, Some(id), Some(1), Some(1), true)?;
                assert!(p["next_times"].as_array().unwrap().len() == 1);
                assert!(
                    activate(&tx, "other", id, 1, 1, p["preview_token"].as_str().unwrap()).is_err()
                );
                let mut calendar = trigger_calendar::bundled_2026();
                calendar.source.document_number = "reviewed replacement".into();
                let changed = trigger_calendar::create_draft(&tx, calendar)?;
                trigger_calendar::apply(&tx, &changed.id, &changed.sha256, "owner")?;
                assert!(
                    activate(&tx, "owner", id, 1, 1, p["preview_token"].as_str().unwrap()).is_err()
                );
                let p = preview(&tx, "owner", &value, Some(id), Some(1), Some(1), true)?;
                let active =
                    activate(&tx, "owner", id, 1, 1, p["preview_token"].as_str().unwrap())?;
                assert_eq!(active["lifecycle"], "enabled");
                assert!(
                    activate(&tx, "owner", id, 2, 1, p["preview_token"].as_str().unwrap()).is_err()
                );
                tx.commit()?;
                Ok(())
            })
            .unwrap();
    }
    #[test]
    fn events_are_owner_scoped_deduplicated_and_cooldown_does_not_tail_send() {
        let (_temp, state) = state();
        state
            .db(|db| {
                let tx = db.transaction()?;
                let mut value = fixture("event");
                value["trigger"]["cooldown_seconds"] = json!(60);
                let job = enabled(&tx, &value)?;
                let id = job["id"].as_str().unwrap();
                emit_event(
                    &tx,
                    "dsh.runs",
                    "foreign",
                    "dsh.run.failed.v1",
                    "other",
                    &json!({"status":"failed","run_kind":"model"}),
                )?;
                emit_event(
                    &tx,
                    "dsh.runs",
                    "first",
                    "dsh.run.failed.v1",
                    "owner",
                    &json!({"status":"failed","run_kind":"model"}),
                )?;
                emit_event(
                    &tx,
                    "dsh.runs",
                    "first",
                    "dsh.run.failed.v1",
                    "owner",
                    &json!({"status":"failed","run_kind":"model"}),
                )?;
                emit_event(
                    &tx,
                    "dsh.runs",
                    "second",
                    "dsh.run.failed.v1",
                    "owner",
                    &json!({"status":"failed","run_kind":"model"}),
                )?;
                tick_in(&tx, now())?;
                let count: i64 = tx.query_row(
                    "SELECT count(*) FROM trigger_occurrences WHERE job_id=?1",
                    [id],
                    |r| r.get(0),
                )?;
                assert_eq!(count, 1);
                let coalesced: i64 = tx.query_row(
                    "SELECT count(*) FROM trigger_coalesced WHERE job_id=?1",
                    [id],
                    |r| r.get(0),
                )?;
                assert_eq!(coalesced, 0);
                let last = owner_job(&tx, "owner", id)?;
                assert_eq!(last["last_occurrence"]["delivery_status"], "stored");
                tx.commit()?;
                Ok(())
            })
            .unwrap();
    }
    #[test]
    fn utc_midnight_moves_unspent_reservation_without_resetting_local_budget() {
        let (_temp, state) = state();
        state.db(|db|{let tx=db.transaction()?;let base=fixture("once");let job=save(&tx,"owner",None,None,&base)?;let id=job["id"].as_str().unwrap();let mut value=fixture("model");value["budget"]["daily_model_runs"]=json!(1);let before=DateTime::parse_from_rfc3339("2026-10-25T23:59:59Z").unwrap().timestamp();assert!(reserve_budget(&tx,"reservation",id,&value,before,false)?);assert!(reserve_budget(&tx,"reservation",id,&value,before+2,true)?);let(day,status):(String,String)=tx.query_row("SELECT utc_day,status FROM trigger_budget_reservations WHERE occurrence_id='reservation'",[],|r|Ok((r.get(0)?,r.get(1)?)))?;assert_eq!(day,"2026-10-26");assert_eq!(status,"spent");value["time_zone"]=json!("UTC");assert!(!reserve_budget(&tx,"second",id,&value,before+3,true)?);tx.commit()?;Ok(())}).unwrap();
    }
    #[test]
    fn overflow_does_not_poison_business_commit_or_unrelated_time_tasks() {
        let (_temp, state) = state();
        state.db(|db|{let tx=db.transaction()?;let event=enabled(&tx,&fixture("event"))?;let timed=enabled(&tx,&fixture("once"))?;for index in 0..4096 {tx.execute("INSERT INTO trigger_events(event_id,source_id,event_type,owner_id,data_json,received_at) VALUES(?1,'dsh.runs','dsh.run.failed.v1','owner','{}',1)",[format!("synthetic:{index}")])?;}emit_event(&tx,"dsh.runs","overflow","dsh.run.failed.v1","owner",&json!({"status":"failed","run_kind":"model"}))?;tx.execute("INSERT INTO settings(key,value) VALUES('synthetic_business_commit','kept')",[])?;assert_eq!(owner_job(&tx,"owner",event["id"].as_str().unwrap())?["block_reason"],"event_source");assert!(owner_job(&tx,"owner",timed["id"].as_str().unwrap())?["block_reason"].is_null());tx.commit()?;let committed:String=db.query_row("SELECT value FROM settings WHERE key='synthetic_business_commit'",[],|r|r.get(0))?;assert_eq!(committed,"kept");Ok(())}).unwrap();
    }
    #[test]
    fn resume_cannot_bypass_instance_enabled_limit() {
        let (_temp, state) = state();
        state
            .db(|db| {
                let tx = db.transaction()?;
                let value = fixture("interval");
                let first = enabled(&tx, &value)?;
                let id = first["id"].as_str().unwrap();
                let paused = lifecycle(&tx, "owner", id, 2, "pause")?;
                let mut replacement = None;
                for _ in 0..64 {
                    replacement = Some(enabled(&tx, &value)?);
                }
                let error = lifecycle(
                    &tx,
                    "owner",
                    id,
                    paused["revision"].as_i64().unwrap(),
                    "resume",
                )
                .unwrap_err();
                assert!(error.to_string().contains("64"));
                assert_eq!(owner_job(&tx, "owner", id)?["lifecycle"], "paused");
                let replacement = replacement.unwrap();
                lifecycle(
                    &tx,
                    "owner",
                    replacement["id"].as_str().unwrap(),
                    2,
                    "pause",
                )?;
                assert_eq!(
                    lifecycle(&tx, "owner", id, 3, "resume")?["lifecycle"],
                    "enabled"
                );
                let count: i64 = tx.query_row(
                    "SELECT count(*) FROM trigger_jobs WHERE lifecycle='enabled'",
                    [],
                    |r| r.get(0),
                )?;
                assert_eq!(count, 64);
                Ok(())
            })
            .unwrap();
    }
    fn model_definition(db: &Connection) -> anyhow::Result<Value> {
        let key = "8f987285-3842-4b42-8230-0b7253bf23b1";
        let (_, mut config) = crate::character::current_config_in(db)?;
        config.model.credential_ref = Some(key.into());
        db.execute(
            "INSERT INTO secrets(id,kind,created_at) VALUES(?1,'model_api_key',0)",
            [key],
        )?;
        db.execute(
            "UPDATE config_revisions SET config_json=?1 WHERE revision=1",
            [serde_json::to_string(&config)?],
        )?;
        let mut value = fixture("model");
        value["trigger"] = json!({"type":"once","mode":"after","after_seconds":30});
        Ok(value)
    }
    #[test]
    fn revoked_grant_blocks_model_admission_and_queued_http_preparation() {
        let (_temp, state) = state();
        state
            .db(|db| {
                let tx = db.transaction()?;
                let value = model_definition(&tx)?;
                let job = enabled(&tx, &value)?;
                let id = job["id"].as_str().unwrap();
                let snapshot = revision_snapshot(&tx, id, 1)?;
                tx.execute(
                    "UPDATE trigger_execution_grants SET revoked_at=?1 WHERE job_id=?2",
                    params![now(), id],
                )?;
                let blocked = insert_occurrence(
                    &tx,
                    &job,
                    &value,
                    &snapshot,
                    now(),
                    "blocked-before-queue",
                    "trigger",
                    None,
                    None,
                    now(),
                )?
                .unwrap();
                assert!(!dispatch(&tx, &blocked)?);
                let runs: i64 = tx.query_row("SELECT count(*) FROM runs", [], |r| r.get(0))?;
                assert_eq!(runs, 0, "revocation must prevent even local Run creation");
                assert_eq!(occurrence(&tx, "owner", &blocked)?["status"], "cancelled");
                tx.execute(
                    "UPDATE trigger_execution_grants SET revoked_at=NULL WHERE job_id=?1",
                    [id],
                )?;
                let queued = insert_occurrence(
                    &tx,
                    &job,
                    &value,
                    &snapshot,
                    now(),
                    "blocked-before-http",
                    "trigger",
                    None,
                    None,
                    now(),
                )?
                .unwrap();
                assert!(dispatch(&tx, &queued)?);
                let run: String = tx.query_row(
                    "SELECT run_id FROM trigger_occurrences WHERE id=?1",
                    [&queued],
                    |r| r.get(0),
                )?;
                tx.execute(
                    "UPDATE trigger_execution_grants SET revoked_at=?1 WHERE job_id=?2",
                    params![now(), id],
                )?;
                assert!(
                    !prepare_trigger_run_in(&tx, &run)?,
                    "revocation after queueing must prevent HTTP preparation"
                );
                let status: String =
                    tx.query_row("SELECT status FROM runs WHERE id=?1", [&run], |r| r.get(0))?;
                assert_eq!(status, "cancelled");
                let reservation: String = tx.query_row(
                    "SELECT status FROM trigger_budget_reservations WHERE occurrence_id=?1",
                    [&queued],
                    |r| r.get(0),
                )?;
                assert_eq!(reservation, "released");
                assert_eq!(
                    owner_job(&tx, "owner", id)?["block_reason"],
                    "authorization"
                );
                Ok(())
            })
            .unwrap();
    }
    #[test]
    fn explicit_manual_test_has_no_persistent_grant_but_still_prepares_once() {
        let (_temp, state) = state();
        state
            .db(|db| {
                let tx = db.transaction()?;
                let value = model_definition(&tx)?;
                let job = save(&tx, "owner", None, None, &value)?;
                let id = job["id"].as_str().unwrap();
                let result = test_run(&tx, "owner", id, 1, "explicit-manual-once")?;
                assert_eq!(result["origin"], "manual_test");
                let count: i64 =
                    tx.query_row("SELECT count(*) FROM trigger_execution_grants", [], |r| {
                        r.get(0)
                    })?;
                assert_eq!(count, 0);
                let run = result["run_id"].as_str().unwrap();
                assert!(prepare_trigger_run_in(&tx, run)?);
                assert!(
                    !prepare_trigger_run_in(&tx, run)?,
                    "a running manual test cannot be claimed twice"
                );
                Ok(())
            })
            .unwrap();
    }
    #[test]
    fn applying_new_grant_allows_already_running_old_snapshot_to_finish() {
        let (_temp, state) = state();
        state.db(|db| {
            let tx=db.transaction()?;
            let mut value=model_definition(&tx)?;
            let job=enabled(&tx,&value)?;
            let id=job["id"].as_str().unwrap();
            let snapshot=revision_snapshot(&tx,id,1)?;
            let occ=insert_occurrence(&tx,&job,&value,&snapshot,now(),"running-old-revision","trigger",None,None,now())?.unwrap();
            assert!(dispatch(&tx,&occ)?);
            let run:String=tx.query_row("SELECT run_id FROM trigger_occurrences WHERE id=?1",[&occ],|r|r.get(0))?;
            assert!(prepare_trigger_run_in(&tx,&run)?);
            value["title"]=json!("new approved title");
            let saved=save(&tx,"owner",Some(id),Some(2),&value)?;
            let proof=preview(&tx,"owner",&value,Some(id),saved["revision"].as_i64(),Some(2),true)?;
            activate(&tx,"owner",id,3,2,proof["preview_token"].as_str().unwrap())?;
            let revoked:bool=tx.query_row("SELECT revoked_at IS NOT NULL FROM trigger_execution_grants WHERE job_id=?1 AND revision=1",[id],|r|r.get(0))?;
            assert!(revoked);
            finish_run_in(&tx,&run,"completed",Some("old frozen result"),None)?;
            let result=occurrence(&tx,"owner",&occ)?;
            assert_eq!(result["status"],"completed");
            assert_eq!(result["response"],"old frozen result");
            assert_eq!(result["job_revision"],1);
            Ok(())
        }).unwrap();
    }
    #[test]
    fn missing_midnight_budget_is_contiguous_and_preserves_daily_charge() {
        let (_temp, state) = state();
        state.db(|db| {
            let tx=db.transaction()?;
            let zone: Tz="America/Santiago".parse().unwrap();
            let day=chrono::NaiveDate::from_ymd_opt(2026,9,6).unwrap();
            assert!(zone.from_local_datetime(&day.and_hms_opt(0,0,0).unwrap()).earliest().is_none());
            let boundary=civil_day_boundary(zone,day)?;
            assert_eq!(DateTime::<Utc>::from_timestamp(boundary,0).unwrap().with_timezone(&zone).format("%Y-%m-%d %H:%M:%S").to_string(),"2026-09-06 01:00:00");
            let base=fixture("once");let job=save(&tx,"owner",None,None,&base)?;let id=job["id"].as_str().unwrap();
            let mut value=fixture("model");value["time_zone"]=json!("America/Santiago");value["budget"]["daily_model_runs"]=json!(1);
            assert!(reserve_budget(&tx,"previous-day",id,&value,boundary-1,true)?);
            assert!(reserve_budget(&tx,"new-day",id,&value,boundary,true)?);
            assert!(!reserve_budget(&tx,"over-budget",id,&value,boundary+1,true)?);
            let windows=tx.prepare("SELECT start_at,end_at FROM trigger_budget_windows WHERE job_id=?1 ORDER BY start_at")?.query_map([id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?)))?.collect::<Result<Vec<_>,_>>()?;
            assert_eq!(windows.len(),2);assert_eq!(windows[0].1,boundary);assert_eq!(windows[0].1,windows[1].0);assert_eq!(windows[1].1-windows[1].0,23*3600);
            let later=budget_window(&tx,id,"UTC",boundary+2)?;let current:String=tx.query_row("SELECT window_id FROM trigger_budget_reservations WHERE occurrence_id='new-day'",[],|r|r.get(0))?;assert_eq!(later,current,"timezone edits cannot reset the spent DST-day quota");
            Ok(())
        }).unwrap();
    }
    #[test]
    fn repeated_midnight_and_skipped_date_have_bounded_monotonic_boundaries() {
        let havana: Tz = "America/Havana".parse().unwrap();
        let fold = chrono::NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        assert!(matches!(
            havana.from_local_datetime(&fold.and_hms_opt(0, 0, 0).unwrap()),
            chrono::LocalResult::Ambiguous(_, _)
        ));
        let start = civil_day_boundary(havana, fold).unwrap();
        let end = civil_day_boundary(havana, fold.succ_opt().unwrap()).unwrap();
        assert_eq!(end - start, 25 * 3600);
        let apia: Tz = "Pacific/Apia".parse().unwrap();
        let skipped = chrono::NaiveDate::from_ymd_opt(2011, 12, 30).unwrap();
        let boundary = civil_day_boundary(apia, skipped).unwrap();
        let following = civil_day_boundary(apia, skipped.succ_opt().unwrap()).unwrap();
        assert_eq!(boundary, following);
        assert_eq!(
            DateTime::<Utc>::from_timestamp(boundary, 0)
                .unwrap()
                .with_timezone(&apia)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string(),
            "2011-12-31 00:00:00"
        );
    }
}
