//! Owner-scoped work schedules. The existing trigger scheduler calls `tick_in`;
//! no timer, model loop, network client or per-activity worker is created here.
use crate::trigger_store::{conflict, fail, missing};
use chrono::{DateTime, Datelike, Duration, LocalResult, NaiveDate, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const MAX_ITEMS: usize = 500;
const MAX_RANGE: i64 = 93 * 86400;
const READ_CHARS: usize = 4096;

pub fn initialize(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS schedule_activities(
          id TEXT PRIMARY KEY,owner_id TEXT NOT NULL,assistant_id TEXT NOT NULL,revision INTEGER NOT NULL,
          title TEXT NOT NULL,kind TEXT NOT NULL,creator TEXT NOT NULL,state TEXT NOT NULL,
          definition_json TEXT NOT NULL,grant_id TEXT,grant_revision INTEGER,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL);
         CREATE INDEX IF NOT EXISTS schedule_activity_owner ON schedule_activities(owner_id,state,updated_at);
         CREATE TABLE IF NOT EXISTS schedule_revisions(activity_id TEXT NOT NULL,revision INTEGER NOT NULL,definition_json TEXT NOT NULL,state TEXT NOT NULL,created_at INTEGER NOT NULL,PRIMARY KEY(activity_id,revision));
         CREATE TABLE IF NOT EXISTS schedule_exceptions(activity_id TEXT NOT NULL,nominal_at INTEGER NOT NULL,definition_json TEXT,state TEXT NOT NULL,revision INTEGER NOT NULL,PRIMARY KEY(activity_id,nominal_at));
         CREATE TABLE IF NOT EXISTS schedule_occurrences(
          id TEXT PRIMARY KEY,activity_id TEXT NOT NULL,owner_id TEXT NOT NULL,activity_revision INTEGER NOT NULL,
          nominal_at INTEGER NOT NULL,start_at INTEGER NOT NULL,end_at INTEGER NOT NULL,definition_json TEXT NOT NULL,
          status TEXT NOT NULL,block_reason TEXT,checkpoint INTEGER NOT NULL DEFAULT 0,total_chars INTEGER,
          resource_hash TEXT,result_json TEXT,started_at INTEGER,completed_at INTEGER,updated_at INTEGER NOT NULL,
          fence TEXT,attempts INTEGER NOT NULL DEFAULT 0,
          reading_generation INTEGER NOT NULL DEFAULT 0,
          UNIQUE(activity_id,nominal_at));
         CREATE INDEX IF NOT EXISTS schedule_occurrence_due ON schedule_occurrences(status,start_at);
         CREATE INDEX IF NOT EXISTS schedule_occurrence_owner ON schedule_occurrences(owner_id,start_at);
         CREATE TABLE IF NOT EXISTS schedule_resources(
          id TEXT PRIMARY KEY,owner_id TEXT NOT NULL,title TEXT NOT NULL,kind TEXT NOT NULL,content TEXT,
          content_hash TEXT NOT NULL,revision INTEGER NOT NULL DEFAULT 1,revoked_at INTEGER,created_at INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS schedule_autonomy_grants(
          id TEXT PRIMARY KEY,owner_id TEXT NOT NULL,revision INTEGER NOT NULL,policy_json TEXT NOT NULL,
          revoked_at INTEGER,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS schedule_plans(
          id TEXT PRIMARY KEY,owner_id TEXT NOT NULL,goal TEXT NOT NULL,revision INTEGER NOT NULL,
          definition_json TEXT NOT NULL,state TEXT NOT NULL,last_planned_day TEXT,last_planned_at INTEGER,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,block_reason TEXT);
         CREATE TABLE IF NOT EXISTS schedule_mutations(owner_id TEXT NOT NULL,day TEXT NOT NULL,kind TEXT NOT NULL,count INTEGER NOT NULL,PRIMARY KEY(owner_id,day,kind));
         CREATE TABLE IF NOT EXISTS schedule_activity_budgets(owner_id TEXT NOT NULL,day TEXT NOT NULL,activity_id TEXT NOT NULL,seconds INTEGER NOT NULL,PRIMARY KEY(owner_id,day,activity_id));
         CREATE TABLE IF NOT EXISTS schedule_reading_progress(owner_id TEXT NOT NULL,resource_id TEXT NOT NULL,resource_hash TEXT NOT NULL,generation INTEGER NOT NULL,checkpoint INTEGER NOT NULL,total_chars INTEGER NOT NULL,result_json TEXT,updated_at INTEGER NOT NULL,PRIMARY KEY(owner_id,resource_id,resource_hash));
         CREATE TABLE IF NOT EXISTS schedule_admin_approvals(id TEXT PRIMARY KEY,activity_id TEXT NOT NULL,owner_id TEXT NOT NULL,actor_id TEXT NOT NULL,approved_revision INTEGER NOT NULL,definition_hash TEXT NOT NULL,scope TEXT NOT NULL,previous_grant_id TEXT,created_at INTEGER NOT NULL,revoked_at INTEGER);
         CREATE INDEX IF NOT EXISTS schedule_approval_active ON schedule_admin_approvals(activity_id,revoked_at);
         CREATE TABLE IF NOT EXISTS schedule_idempotency(owner_id TEXT NOT NULL,request_key TEXT NOT NULL,request_hash TEXT NOT NULL,response_json TEXT NOT NULL,created_at INTEGER NOT NULL,PRIMARY KEY(owner_id,request_key));
         CREATE TABLE IF NOT EXISTS schedule_runtime(id INTEGER PRIMARY KEY CHECK(id=1),last_tick INTEGER NOT NULL,last_plan_tick INTEGER NOT NULL);
         INSERT OR IGNORE INTO schedule_runtime VALUES(1,0,0);",
    )?;
    for (table, column, definition) in [
        (
            "schedule_occurrences",
            "reading_generation",
            "reading_generation INTEGER NOT NULL DEFAULT 0",
        ),
        ("schedule_plans", "block_reason", "block_reason TEXT"),
    ] {
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name=?2)",
            params![table, column],
            |r| r.get(0),
        )?;
        if !exists {
            db.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {definition}"))?;
        }
    }
    // A checkpoint is committed with every bounded read. A restart continues it,
    // never reruns an unconfirmed external effect (none are enabled here).
    db.execute("UPDATE schedule_occurrences SET status='partial',block_reason='restart_checkpoint' WHERE status='running'", [])?;
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repeat {
    pub frequency: String,
    #[serde(default = "one")]
    pub interval: i64,
    pub until: Option<i64>,
    pub count: Option<usize>,
}
fn one() -> i64 {
    1
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Activity {
    pub title: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub purpose: String,
    pub start_at: i64,
    pub end_at: i64,
    pub time_zone: String,
    pub resource_id: Option<String>,
    pub target_ref: Option<String>,
    pub repeat: Option<Repeat>,
    #[serde(default)]
    pub movable: bool,
    pub grant_id: Option<String>,
    #[serde(default)]
    pub restart_reading: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub title: String,
    pub allowed_types: Vec<String>,
    pub resource_ids: Vec<String>,
    pub time_zone: String,
    pub valid_from: i64,
    pub valid_until: i64,
    pub horizon_days: i64,
    pub window_start: String,
    pub window_end: String,
    pub max_daily_activities: i64,
    pub max_daily_minutes: i64,
    pub max_daily_changes: i64,
    #[serde(default = "default_calls")]
    pub max_daily_model_calls: i64,
    #[serde(default = "default_tokens")]
    pub max_daily_tokens: i64,
    #[serde(default = "default_output")]
    pub max_output_tokens: i64,
}
fn default_calls() -> i64 {
    8
}
fn default_tokens() -> i64 {
    24000
}
fn default_output() -> i64 {
    1200
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub goal: String,
    pub grant_id: String,
    pub resource_ids: Vec<String>,
    pub duration_minutes: i64,
    pub state: String,
    #[serde(default = "default_planning")]
    pub planning_mode: String,
}
fn default_planning() -> String {
    "bounded_rule".into()
}

fn text_ok(s: &str, max: usize) -> bool {
    !s.trim().is_empty() && s.len() <= max && !s.contains('\0')
}
fn tz(s: &str) -> anyhow::Result<Tz> {
    s.parse()
        .map_err(|_| fail("invalid_time_zone", "需要有效的 IANA 时区"))
}
fn instant(n: i64) -> anyhow::Result<DateTime<Utc>> {
    DateTime::from_timestamp(n, 0).ok_or_else(|| fail("invalid_time", "时间超出支持范围"))
}
fn assistant(db: &Connection) -> anyhow::Result<String> {
    Ok(db.query_row(
        "SELECT value FROM settings WHERE key='assistant.identity.v1'",
        [],
        |r| r.get(0),
    )?)
}
fn audit(db: &Connection, owner: &str, action: &str, id: &str, now: i64) -> anyhow::Result<()> {
    db.execute(
        "INSERT INTO audit_events(actor,action,resource,created_at) VALUES(?1,?2,?3,?4)",
        params![owner, action, id, now],
    )?;
    Ok(())
}
fn validate(a: &Activity) -> anyhow::Result<()> {
    if !text_ok(&a.title, 240) || !text_ok(&a.purpose, 4000) {
        return Err(fail("invalid_activity", "标题和目的不能为空或过长"));
    }
    if ![
        "reading",
        "writing",
        "video",
        "consolidation",
        "chat",
        "action",
    ]
    .contains(&a.kind.as_str())
    {
        return Err(fail("invalid_activity_type", "活动类型无效"));
    }
    if a.end_at
        .checked_sub(a.start_at)
        .is_none_or(|seconds| !(1..=8 * 3600).contains(&seconds))
    {
        return Err(fail("invalid_duration", "活动时长须大于0且不超过8小时"));
    }
    let zone = tz(&a.time_zone)?;
    let start = instant(a.start_at)?;
    let end = instant(a.end_at)?;
    if start.with_timezone(&zone).date_naive() != end.with_timezone(&zone).date_naive() {
        return Err(fail("cross_day_activity", "请将跨日活动拆成各自然日分片"));
    }
    if let Some(r) = &a.repeat {
        if !["daily", "weekly", "monthly"].contains(&r.frequency.as_str())
            || !(1..=31).contains(&r.interval)
            || r.count.is_some_and(|n| n == 0 || n > 3660)
            || r.until.is_some_and(|n| n < a.start_at)
        {
            return Err(fail("invalid_repeat", "重复规则、结束时间或次数无效"));
        }
    }
    if a.kind == "reading" && a.resource_id.is_none() {
        return Err(fail("resource_required", "阅读须引用已授权资料"));
    }
    if a.kind == "chat" && a.target_ref.as_deref().is_none_or(|s| !text_ok(s, 256)) {
        return Err(fail("target_required", "聊天须绑定固定目标引用"));
    }
    Ok(())
}
fn parse_activity(v: &Value) -> anyhow::Result<Activity> {
    let a = serde_json::from_value(v.clone())
        .map_err(|_| fail("invalid_activity", "活动字段或类型无效"))?;
    validate(&a)?;
    Ok(a)
}
fn window(s: &str) -> anyhow::Result<u32> {
    let parsed = chrono::NaiveTime::parse_from_str(s, "%H:%M")
        .map_err(|_| fail("invalid_window", "可用窗口使用HH:MM"))?;
    Ok(parsed.hour() * 60 + parsed.minute())
}

pub fn create_resource(db: &Connection, owner: &str, v: &Value, now: i64) -> anyhow::Result<Value> {
    let obj = v
        .as_object()
        .ok_or_else(|| fail("invalid_resource", "资料须为对象"))?;
    if obj
        .keys()
        .any(|k| !["title", "content", "type"].contains(&k.as_str()))
    {
        return Err(fail("invalid_resource", "资料包含未知字段"));
    }
    let title = v["title"]
        .as_str()
        .filter(|s| text_ok(s, 240))
        .ok_or_else(|| fail("invalid_resource", "需要资料标题"))?;
    let kind = v["type"].as_str().unwrap_or("text");
    if kind != "text" {
        return Err(fail(
            "unsupported_resource",
            "当前只支持直接授权的UTF-8文本；不接收URL或系统路径",
        ));
    }
    let content = v["content"]
        .as_str()
        .filter(|s| text_ok(s, 512 * 1024))
        .ok_or_else(|| fail("invalid_resource", "文本须为1至512KiB"))?;
    let id = uuid::Uuid::new_v4().to_string();
    let hash = hex::encode(Sha256::digest(content.as_bytes()));
    db.execute("INSERT INTO schedule_resources(id,owner_id,title,kind,content,content_hash,created_at) VALUES(?1,?2,?3,'text',?4,?5,?6)",params![id,owner,title,content,hash,now])?;
    audit(db, owner, "schedule.resource.authorize", &id, now)?;
    Ok(
        json!({"id":id,"title":title,"type":kind,"revision":1,"content_hash":hash,"characters":content.chars().count(),"visibility":"private","owner_id":owner}),
    )
}
pub fn list_resources(db: &Connection, owner: &str) -> anyhow::Result<Value> {
    let mut items=db.prepare("SELECT id,title,kind,revision,content_hash,length(content),revoked_at FROM schedule_resources WHERE owner_id=?1 ORDER BY created_at DESC LIMIT 100")?.query_map([owner],|r|Ok(json!({"id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"type":r.get::<_,String>(2)?,"revision":r.get::<_,i64>(3)?,"content_hash":r.get::<_,String>(4)?,"characters":r.get::<_,Option<i64>>(5)?,"revoked_at":r.get::<_,Option<i64>>(6)?,"visibility":"private"})))?.collect::<Result<Vec<_>,_>>()?;
    for item in &mut items {
        item["progress"] =
            resource_progress(db, owner, item["id"].as_str().unwrap_or(""))?.unwrap_or(Value::Null);
    }
    Ok(json!({"items":items}))
}
pub fn resource_progress(db: &Connection, owner: &str, id: &str) -> anyhow::Result<Option<Value>> {
    Ok(db.query_row("SELECT p.generation,p.checkpoint,p.total_chars,p.resource_hash,p.updated_at FROM schedule_reading_progress p JOIN schedule_resources r ON r.id=p.resource_id AND r.owner_id=p.owner_id AND r.content_hash=p.resource_hash WHERE p.owner_id=?1 AND p.resource_id=?2 AND r.revoked_at IS NULL",params![owner,id],|r|Ok(json!({"generation":r.get::<_,i64>(0)?,"processed_chars":r.get::<_,i64>(1)?,"total_chars":r.get::<_,i64>(2)?,"resource_hash":r.get::<_,String>(3)?,"updated_at":r.get::<_,i64>(4)?}))).optional()?)
}
pub fn revoke_resource(
    db: &Connection,
    owner: &str,
    id: &str,
    expected: i64,
    now: i64,
) -> anyhow::Result<Value> {
    if db.execute("UPDATE schedule_resources SET revoked_at=?1,content=NULL,revision=revision+1 WHERE id=?2 AND owner_id=?3 AND revision=?4 AND revoked_at IS NULL",params![now,id,owner,expected])?==0{return Err(conflict());}
    db.execute("UPDATE schedule_occurrences SET status='blocked',block_reason='resource_revoked',result_json=NULL WHERE owner_id=?1 AND json_extract(definition_json,'$.resource_id')=?2",params![owner,id])?;
    db.execute(
        "DELETE FROM schedule_reading_progress WHERE owner_id=?1 AND resource_id=?2",
        params![owner, id],
    )?;
    audit(db, owner, "schedule.resource.revoke", id, now)?;
    Ok(json!({"id":id,"revoked":true,"revision":expected+1}))
}
fn resource_valid(db: &Connection, owner: &str, id: &str) -> anyhow::Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM schedule_resources WHERE id=?1 AND owner_id=?2 AND revoked_at IS NULL AND content IS NOT NULL)",params![id,owner],|r|r.get(0))?)
}

fn parse_grant(db: &Connection, owner: &str, v: &Value, now: i64) -> anyhow::Result<Grant> {
    let g: Grant = serde_json::from_value(v.clone())
        .map_err(|_| fail("invalid_grant", "授权字段或类型无效"))?;
    tz(&g.time_zone)?;
    instant(g.valid_from)?;
    instant(g.valid_until)?;
    if !text_ok(&g.title, 240)
        || g.allowed_types.is_empty()
        || g.allowed_types.len() > 5
        || g.allowed_types.iter().any(|s| {
            ![
                "reading",
                "writing",
                "video",
                "consolidation",
                "chat",
                "action",
            ]
            .contains(&s.as_str())
        })
        || g.resource_ids.len() > 100
        || g.valid_until <= now
        || g.valid_until <= g.valid_from
        || g.valid_until - g.valid_from > 366 * 86400
        || !(1..=90).contains(&g.horizon_days)
        || !(1..=24).contains(&g.max_daily_activities)
        || !(1..=480).contains(&g.max_daily_minutes)
        || !(1..=100).contains(&g.max_daily_changes)
        || !(1..=48).contains(&g.max_daily_model_calls)
        || !(4000..=250000).contains(&g.max_daily_tokens)
        || !(256..=2000).contains(&g.max_output_tokens)
        || window(&g.window_start)? >= window(&g.window_end)?
    {
        return Err(fail("invalid_grant", "授权范围、有效期或预算无效"));
    }
    for id in &g.resource_ids {
        if !resource_valid(db, owner, id)? {
            return Err(fail(
                "unauthorized_resource",
                "授权只能引用本人仍有效的资料",
            ));
        }
    }
    Ok(g)
}
pub fn create_grant(db: &Connection, owner: &str, v: &Value, now: i64) -> anyhow::Result<Value> {
    let g = parse_grant(db, owner, v, now)?;
    let id = uuid::Uuid::new_v4().to_string();
    db.execute(
        "INSERT INTO schedule_autonomy_grants VALUES(?1,?2,1,?3,NULL,?4,?4)",
        params![id, owner, serde_json::to_string(&g)?, now],
    )?;
    audit(db, owner, "schedule.autonomy.grant", &id, now)?;
    grant_detail(db, owner, &id)
}
pub fn grant_detail(db: &Connection, owner: &str, id: &str) -> anyhow::Result<Value> {
    db.query_row("SELECT revision,policy_json,revoked_at FROM schedule_autonomy_grants WHERE id=?1 AND owner_id=?2",params![id,owner],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<i64>>(2)?))).optional()?.map(|(rev,p,at)|Ok(json!({"id":id,"revision":rev,"policy":serde_json::from_str::<Value>(&p)?,"revoked_at":at}))).unwrap_or_else(||Err(missing()))
}
pub fn list_grants(db: &Connection, owner: &str) -> anyhow::Result<Value> {
    let ids=db.prepare("SELECT id FROM schedule_autonomy_grants WHERE owner_id=?1 ORDER BY created_at DESC LIMIT 100")?.query_map([owner],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
    Ok(
        json!({"items":ids.iter().map(|id|grant_detail(db,owner,id)).collect::<anyhow::Result<Vec<_>>>()?}),
    )
}
pub fn change_grant(
    db: &Connection,
    owner: &str,
    id: &str,
    expected: i64,
    v: Option<&Value>,
    now: i64,
) -> anyhow::Result<Value> {
    let current = grant_detail(db, owner, id)?;
    if current["revision"] != expected {
        return Err(conflict());
    }
    let (p, revoked) = match v {
        Some(v) => (
            serde_json::to_string(&parse_grant(db, owner, v, now)?)?,
            None,
        ),
        None => (current["policy"].to_string(), Some(now)),
    };
    db.execute("UPDATE schedule_autonomy_grants SET revision=revision+1,policy_json=?1,revoked_at=?2,updated_at=?3 WHERE id=?4 AND owner_id=?5 AND revision=?6",params![p,revoked,now,id,owner,expected])?;
    db.execute("UPDATE schedule_occurrences SET status='blocked',block_reason='grant_changed' WHERE activity_id IN(SELECT id FROM schedule_activities WHERE grant_id=?1 AND owner_id=?2 AND creator='autonomous') AND status IN('pending','partial','running')",params![id,owner])?;
    audit(
        db,
        owner,
        if v.is_some() {
            "schedule.autonomy.update"
        } else {
            "schedule.autonomy.revoke"
        },
        id,
        now,
    )?;
    grant_detail(db, owner, id)
}

/// Expansion is pure and bounded. Civil recurrences keep local wall time; DST
/// gaps and ambiguous local instants are skipped rather than silently shifted.
pub fn expand(a: &Activity, start: i64, end: i64, limit: usize) -> anyhow::Result<Vec<(i64, i64)>> {
    let zone = tz(&a.time_zone)?;
    let base = instant(a.start_at)?.with_timezone(&zone);
    let mut out = Vec::new();
    let Some(r) = &a.repeat else {
        if a.start_at < end && a.end_at > start {
            out.push((a.start_at, a.end_at));
        }
        return Ok(out);
    };
    let span = a.end_at - a.start_at;
    let mut n = 0usize;
    // At most ten years of opportunities, even when callers request a distant range.
    while n < 3660 && n < r.count.unwrap_or(3660) {
        let date = match r.frequency.as_str() {
            "daily" => base
                .date_naive()
                .checked_add_signed(Duration::days(n as i64 * r.interval)),
            "weekly" => base
                .date_naive()
                .checked_add_signed(Duration::weeks(n as i64 * r.interval)),
            "monthly" => {
                let month =
                    (base.year() as i64 * 12 + base.month0() as i64) + n as i64 * r.interval;
                NaiveDate::from_ymd_opt((month / 12) as i32, (month % 12 + 1) as u32, base.day())
            }
            _ => return Err(fail("invalid_repeat", "重复频率无效")),
        };
        n += 1;
        let Some(date) = date else { continue };
        let LocalResult::Single(local) = zone.from_local_datetime(&date.and_time(base.time()))
        else {
            continue;
        };
        let at = local.timestamp();
        if at >= end || r.until.is_some_and(|until| at > until) {
            break;
        }
        if at + span > start {
            out.push((at, at + span));
            if out.len() > limit {
                return Err(fail(
                    "calendar_expansion_limit",
                    "范围内活动过多，请缩短时间范围",
                ));
            }
        }
    }
    Ok(out)
}
fn activity_row(
    db: &Connection,
    owner: &str,
    id: &str,
) -> anyhow::Result<(i64, String, String, Activity, Option<i64>)> {
    let (revision,state,creator,d,grant_revision)=db.query_row("SELECT revision,state,creator,definition_json,grant_revision FROM schedule_activities WHERE id=?1 AND owner_id=?2",params![id,owner],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,Option<i64>>(4)?))).optional()?.ok_or_else(missing)?;
    Ok((
        revision,
        state,
        creator,
        serde_json::from_str(&d)?,
        grant_revision,
    ))
}
fn exception(
    db: &Connection,
    id: &str,
    nominal: i64,
    a: &Activity,
) -> anyhow::Result<Option<(Activity, String)>> {
    let row=db.query_row("SELECT definition_json,state FROM schedule_exceptions WHERE activity_id=?1 AND nominal_at=?2",params![id,nominal],|r|Ok((r.get::<_,Option<String>>(0)?,r.get::<_,String>(1)?))).optional()?;
    match row {
        Some((d, s)) => Ok(Some((
            d.map(|d| serde_json::from_str(&d))
                .transpose()?
                .unwrap_or(a.clone()),
            s,
        ))),
        None => Ok(None),
    }
}
/// Query moved exceptions by their effective interval as well as their nominal
/// recurrence key. Moving one day into another month must remain visible and
/// executable when the original series no longer overlaps the queried month.
fn expanded_with_exceptions(
    db: &Connection,
    id: &str,
    a: &Activity,
    start: i64,
    end: i64,
    default_state: &str,
) -> anyhow::Result<Vec<(i64, Activity, String)>> {
    let mut map = std::collections::BTreeMap::new();
    for (nominal, to) in expand(a, start, end, MAX_ITEMS)? {
        let (x, state) = exception(db, id, nominal, a)?.unwrap_or_else(|| {
            let mut x = a.clone();
            x.start_at = nominal;
            x.end_at = to;
            (x, default_state.into())
        });
        if x.start_at < end && x.end_at > start {
            map.insert(nominal, (x, state));
        }
    }
    let rows=db.prepare("SELECT nominal_at,definition_json,state FROM schedule_exceptions WHERE activity_id=?1 AND definition_json IS NOT NULL AND json_extract(definition_json,'$.start_at')<?2 AND json_extract(definition_json,'$.end_at')>?3 LIMIT 501")?.query_map(params![id,end,start],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?.collect::<Result<Vec<_>,_>>()?;
    if rows.len() > MAX_ITEMS {
        return Err(fail("calendar_expansion_limit", "改期活动过多，请缩短范围"));
    }
    for (nominal, d, state) in rows {
        map.insert(nominal, (serde_json::from_str(&d)?, state));
    }
    Ok(map
        .into_iter()
        .map(|(nominal, (a, state))| (nominal, a, state))
        .collect())
}
fn conflicts(
    db: &Connection,
    owner: &str,
    a: &Activity,
    exclude: Option<&str>,
    now: i64,
) -> anyhow::Result<Vec<Value>> {
    let end = a
        .repeat
        .as_ref()
        .and_then(|r| r.until)
        .unwrap_or(a.start_at + 90 * 86400)
        .min(a.start_at + 90 * 86400);
    let ours = expand(a, a.start_at, end.max(a.end_at) + 1, MAX_ITEMS)?;
    let rows=db.prepare("SELECT id,owner_id,title,definition_json FROM schedule_activities WHERE state='enabled' AND (?1 IS NULL OR id<>?1) LIMIT 2001")?.query_map([exclude],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?.collect::<Result<Vec<_>,_>>()?;
    if rows.len() > 2000 {
        return Err(fail(
            "schedule_capacity",
            "活动容量超过首版协调上限，请先归档历史活动",
        ));
    }
    let mut found = Vec::new();
    for (id, o, title, d) in rows {
        let other: Activity = serde_json::from_str(&d)?;
        for (_, x, s) in expanded_with_exceptions(
            db,
            &id,
            &other,
            a.start_at,
            end.max(a.end_at) + 1,
            "enabled",
        )? {
            if x.end_at <= now || s != "enabled" {
                continue;
            }
            if ours
                .iter()
                .any(|(from, to)| *from < x.end_at && *to > x.start_at)
            {
                // No title, owner, identifier or per-owner count crosses the ACL.
                found.push(if o == owner {
                    json!({"activity_id":id,"title":title,"start_at":x.start_at,"end_at":x.end_at})
                } else {
                    json!({"start_at":x.start_at,"end_at":x.end_at,"busy":true})
                });
                if found.len() >= 20 {
                    return Ok(found);
                }
            }
        }
    }
    Ok(found)
}
fn load_grant(db: &Connection, owner: &str, id: &str) -> anyhow::Result<(i64, Grant)> {
    let v = grant_detail(db, owner, id)?;
    if !v["revoked_at"].is_null() {
        return Err(fail("autonomy_revoked", "自主授权已经撤销"));
    }
    Ok((
        v["revision"].as_i64().unwrap(),
        serde_json::from_value(v["policy"].clone())?,
    ))
}
fn grant_check(
    db: &Connection,
    owner: &str,
    a: &Activity,
    expected: Option<i64>,
    exclude: Option<&str>,
    now: i64,
) -> anyhow::Result<i64> {
    let id = a
        .grant_id
        .as_deref()
        .ok_or_else(|| fail("autonomy_grant_required", "自主活动需要已有授权"))?;
    let (revision, g) = load_grant(db, owner, id)?;
    if expected.is_some_and(|n| n != revision) {
        return Err(fail(
            "autonomy_revision_changed",
            "自主授权版本已改变，请重新规划",
        ));
    }
    if !g.allowed_types.contains(&a.kind)
        || a.time_zone != g.time_zone
        || a.start_at < g.valid_from
        || a.end_at > g.valid_until
        || a.start_at < now
        || a.start_at > now + g.horizon_days * 86400
        || a.repeat.is_some()
        || a.resource_id
            .as_ref()
            .is_some_and(|r| !g.resource_ids.contains(r))
    {
        return Err(fail(
            "autonomy_out_of_scope",
            "活动类型、资料、时区、有效期或规划范围超出授权",
        ));
    }
    let zone = tz(&g.time_zone)?;
    let from = instant(a.start_at)?.with_timezone(&zone);
    let to = instant(a.end_at)?.with_timezone(&zone);
    let minute = |d: &DateTime<Tz>| d.hour() * 60 + d.minute();
    if minute(&from) < window(&g.window_start)?
        || minute(&to) > window(&g.window_end)?
        || (minute(&to) == window(&g.window_end)? && to.second() > 0)
    {
        return Err(fail("autonomy_outside_window", "活动超出可用时间窗口"));
    }
    let day = from.date_naive().to_string();
    let (existing_count,existing_seconds):(i64,i64)=db.query_row("SELECT count(*),COALESCE(sum(seconds),0) FROM schedule_activity_budgets WHERE owner_id=?1 AND day=?2 AND (?3 IS NULL OR activity_id<>?3)",params![owner,day,exclude],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let previous_seconds:i64=db.query_row("SELECT COALESCE((SELECT seconds FROM schedule_activity_budgets WHERE owner_id=?1 AND day=?2 AND activity_id=?3),0)",params![owner,day,exclude],|r|r.get(0))?;
    let count = existing_count + 1;
    let seconds = existing_seconds + previous_seconds.max(a.end_at - a.start_at);
    let changes:i64=db.query_row("SELECT COALESCE((SELECT count FROM schedule_mutations WHERE owner_id=?1 AND day=?2 AND kind='autonomous'),0)",params![owner,day],|r|r.get(0))?;
    if count > g.max_daily_activities
        || seconds > g.max_daily_minutes * 60
        || changes >= g.max_daily_changes
    {
        return Err(fail(
            "autonomy_budget_exhausted",
            "当日次数、总时长或变更预算已经耗尽",
        ));
    }
    Ok(revision)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanningSlot {
    pub slot_id: String,
    pub start_at: i64,
    pub end_at: i64,
    pub time_zone: String,
    pub duration_minutes: i64,
}
/// Compute bounded, already-authorized alternatives; models select identifiers
/// instead of calculating civil times or Unix timestamps themselves.
pub fn planning_slots(
    db: &Connection,
    owner: &str,
    p: &Plan,
    g: &Grant,
    grant_revision: i64,
    now: i64,
) -> anyhow::Result<Vec<PlanningSlot>> {
    planning_slots_for_kind(db, owner, p, g, grant_revision, now, "reading")
}
pub fn planning_slots_for_kind(
    db: &Connection,
    owner: &str,
    p: &Plan,
    g: &Grant,
    grant_revision: i64,
    now: i64,
    kind: &str,
) -> anyhow::Result<Vec<PlanningSlot>> {
    let zone = tz(&g.time_zone)?;
    let horizon = (now + g.horizon_days.min(3) * 86400).min(g.valid_until);
    let rows=db.prepare("SELECT id,definition_json FROM schedule_activities WHERE state='enabled' ORDER BY id LIMIT 2001")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<Result<Vec<_>,_>>()?;
    if rows.len() > 2000 {
        return Err(fail("planning_capacity", "活动协调数量超出有界规划范围"));
    }
    let mut busy = Vec::new();
    for (id, d) in rows {
        let a: Activity = serde_json::from_str(&d)?;
        for (_, a, state) in expanded_with_exceptions(db, &id, &a, now, horizon, "enabled")? {
            if state == "enabled" {
                busy.push((a.start_at, a.end_at));
            }
        }
        if busy.len() > 10000 {
            return Err(fail("planning_capacity", "忙闲块超过有界规划范围"));
        }
    }
    busy.sort_unstable();
    let mut merged: Vec<(i64, i64)> = Vec::new();
    for (start, end) in busy {
        if let Some(last) = merged.last_mut() {
            if start <= last.1 {
                last.1 = last.1.max(end);
                continue;
            }
        }
        merged.push((start, end));
    }
    let resource = p.resource_ids.iter().find(|id| g.resource_ids.contains(id));
    if kind == "reading" && resource.is_none() {
        return Ok(Vec::new());
    }
    let mut day = instant(now)?.with_timezone(&zone).date_naive();
    let mut slots = Vec::new();
    let duration = p.duration_minutes * 60;
    for _ in 0..4 {
        let from = window(&g.window_start)?;
        let to = window(&g.window_end)?;
        let (LocalResult::Single(start), LocalResult::Single(end)) = (
            zone.from_local_datetime(&day.and_hms_opt(from / 60, from % 60, 0).unwrap()),
            zone.from_local_datetime(&day.and_hms_opt(to / 60, to % 60, 0).unwrap()),
        ) else {
            day += Duration::days(1);
            continue;
        };
        // Seven minutes covers the configured maximum 300-second model request
        // plus DNS and scheduling overhead without silently moving its choice.
        let mut cursor = start
            .timestamp()
            .max(((now + 420 + 59) / 60) * 60)
            .max(g.valid_from);
        let end = end.timestamp().min(horizon);
        let mut scans = 0;
        while cursor + duration <= end && slots.len() < 8 && scans < 128 {
            scans += 1;
            if let Some((_, busy_end)) = merged
                .iter()
                .find(|(s, e)| cursor < *e && cursor + duration > *s)
            {
                cursor = ((busy_end + 59) / 60) * 60;
                continue;
            }
            let a = Activity {
                title: "授权阅读候选时段".into(),
                kind: kind.into(),
                purpose: p.goal.clone(),
                start_at: cursor,
                end_at: cursor + duration,
                time_zone: g.time_zone.clone(),
                resource_id: resource.cloned(),
                target_ref: None,
                repeat: None,
                movable: true,
                grant_id: Some(p.grant_id.clone()),
                restart_reading: false,
            };
            match grant_check(db, owner, &a, Some(grant_revision), None, now) {
                Ok(_) => {}
                Err(error) => {
                    if error
                        .downcast_ref::<crate::trigger_store::TriggerError>()
                        .is_some_and(|e| e.code == "autonomy_budget_exhausted")
                    {
                        break;
                    }
                    return Err(error);
                }
            }
            let digest = crate::trigger_store::digest(
                &json!({"grant_id":p.grant_id,"grant_revision":grant_revision,"start_at":cursor,"end_at":cursor+duration,"time_zone":g.time_zone}),
            );
            slots.push(PlanningSlot {
                slot_id: format!("slot-{}", &digest[..20]),
                start_at: cursor,
                end_at: cursor + duration,
                time_zone: g.time_zone.clone(),
                duration_minutes: p.duration_minutes,
            });
            cursor += 15 * 60;
        }
        if slots.len() >= 8 {
            break;
        }
        day += Duration::days(1);
    }
    Ok(slots)
}
pub fn check_model_proposal(
    db: &Connection,
    owner: &str,
    a: &Activity,
    expected_grant: i64,
    now: i64,
) -> anyhow::Result<()> {
    validate(a)?;
    grant_check(db, owner, a, Some(expected_grant), None, now)?;
    Ok(())
}
fn mutate_budget(db: &Connection, owner: &str, a: &Activity) -> anyhow::Result<()> {
    let day = instant(a.start_at)?
        .with_timezone(&tz(&a.time_zone)?)
        .date_naive()
        .to_string();
    db.execute("INSERT INTO schedule_mutations VALUES(?1,?2,'autonomous',1) ON CONFLICT(owner_id,day,kind) DO UPDATE SET count=count+1",params![owner,day])?;
    Ok(())
}
fn allocate_activity_budget(
    db: &Connection,
    owner: &str,
    id: &str,
    a: &Activity,
) -> anyhow::Result<()> {
    let day = instant(a.start_at)?
        .with_timezone(&tz(&a.time_zone)?)
        .date_naive()
        .to_string();
    db.execute("INSERT INTO schedule_activity_budgets VALUES(?1,?2,?3,?4) ON CONFLICT(owner_id,day,activity_id) DO UPDATE SET seconds=MAX(seconds,excluded.seconds)",params![owner,day,id,a.end_at-a.start_at])?;
    Ok(())
}
fn idempotent(db: &Connection, owner: &str, key: &str, v: &Value) -> anyhow::Result<Option<Value>> {
    if key.is_empty() || key.len() > 128 {
        return Err(fail("invalid_idempotency_key", "需要1至128字节幂等键"));
    }
    let row=db.query_row("SELECT request_hash,response_json FROM schedule_idempotency WHERE owner_id=?1 AND request_key=?2",params![owner,key],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional()?;
    match row {
        Some((hash, response)) if hash == crate::trigger_store::digest(v) => {
            Ok(Some(serde_json::from_str(&response)?))
        }
        Some(_) => Err(conflict()),
        None => Ok(None),
    }
}
pub fn create(
    db: &Connection,
    owner: &str,
    v: &Value,
    key: &str,
    autonomous: bool,
    now: i64,
) -> anyhow::Result<Value> {
    let cache_request = json!({"activity":v,"autonomous":autonomous});
    if let Some(cached) = idempotent(db, owner, key, &cache_request)? {
        return Ok(cached);
    }
    let a = parse_activity(v)?;
    if let Some(resource) = &a.resource_id {
        if !resource_valid(db, owner, resource)? {
            return Err(fail(
                "unauthorized_resource",
                "资料不存在、已撤销或不属于当前来源域",
            ));
        }
    }
    let mut state = "enabled";
    let mut reason = None;
    let mut grant_revision = None;
    if autonomous {
        match grant_check(db, owner, &a, None, None, now) {
            Ok(rev) => grant_revision = Some(rev),
            Err(e) => {
                state = "pending_approval";
                reason = Some(e.to_string());
            }
        }
    }
    let conflict_items = conflicts(db, owner, &a, None, now)?;
    if !conflict_items.is_empty() {
        return Err(fail(
            "schedule_conflict",
            "计划时间与已有忙闲块冲突，请先预演或改期",
        ));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let definition = serde_json::to_string(&a)?;
    db.execute(
        "INSERT INTO schedule_activities VALUES(?1,?2,?3,1,?4,?5,?6,?7,?8,?9,?10,?11,?11)",
        params![
            id,
            owner,
            assistant(db)?,
            a.title,
            a.kind,
            if autonomous { "autonomous" } else { "user" },
            state,
            definition,
            a.grant_id,
            grant_revision,
            now
        ],
    )?;
    db.execute(
        "INSERT INTO schedule_revisions VALUES(?1,1,?2,?3,?4)",
        params![id, definition, state, now],
    )?;
    if autonomous && state == "enabled" {
        mutate_budget(db, owner, &a)?;
        allocate_activity_budget(db, owner, &id, &a)?;
    }
    audit(db, owner, "schedule.activity.create", &id, now)?;
    let mut response = detail(db, owner, &id)?;
    response["approval_reason"] = json!(reason);
    db.execute(
        "INSERT INTO schedule_idempotency VALUES(?1,?2,?3,?4,?5)",
        params![
            owner,
            key,
            crate::trigger_store::digest(&cache_request),
            response.to_string(),
            now
        ],
    )?;
    Ok(response)
}
pub fn detail(db: &Connection, owner: &str, id: &str) -> anyhow::Result<Value> {
    let (revision, state, creator, a, grant_revision) = activity_row(db, owner, id)?;
    let occurrences=db.prepare("SELECT id,nominal_at,start_at,end_at,status,block_reason,checkpoint,total_chars,resource_hash,result_json,started_at,completed_at FROM schedule_occurrences WHERE activity_id=?1 AND owner_id=?2 ORDER BY nominal_at DESC LIMIT 100")?.query_map(params![id,owner],|r|Ok(json!({"id":r.get::<_,String>(0)?,"nominal_at":r.get::<_,i64>(1)?,"start_at":r.get::<_,i64>(2)?,"end_at":r.get::<_,i64>(3)?,"status":r.get::<_,String>(4)?,"block_reason":r.get::<_,Option<String>>(5)?,"processed_chars":r.get::<_,i64>(6)?,"total_chars":r.get::<_,Option<i64>>(7)?,"resource_hash":r.get::<_,Option<String>>(8)?,"result":r.get::<_,Option<String>>(9)?.and_then(|s|serde_json::from_str::<Value>(&s).ok()),"started_at":r.get::<_,Option<i64>>(10)?,"completed_at":r.get::<_,Option<i64>>(11)?})))?.collect::<Result<Vec<_>,_>>()?;
    let exceptions=db.prepare("SELECT nominal_at,state,revision,definition_json FROM schedule_exceptions WHERE activity_id=?1 ORDER BY nominal_at LIMIT 500")?.query_map([id],|r|Ok(json!({"nominal_at":r.get::<_,i64>(0)?,"state":r.get::<_,String>(1)?,"revision":r.get::<_,i64>(2)?,"definition":r.get::<_,Option<String>>(3)?.and_then(|s|serde_json::from_str::<Value>(&s).ok())})))?.collect::<Result<Vec<_>,_>>()?;
    Ok(
        json!({"id":id,"activity_id":id,"revision":revision,"state":state,"creator":creator,"definition":a,"grant_revision":grant_revision,"admin_approval":approval_reference(db,owner,id)?,"visibility":"private","occurrences":occurrences,"exceptions":exceptions}),
    )
}
fn approval_digest(db: &Connection, id: &str, a: &Activity) -> anyhow::Result<String> {
    let exceptions=db.prepare("SELECT nominal_at,definition_json,state FROM schedule_exceptions WHERE activity_id=?1 ORDER BY nominal_at LIMIT 501")?.query_map([id],|r|Ok(json!({"nominal_at":r.get::<_,i64>(0)?,"definition":r.get::<_,Option<String>>(1)?.and_then(|s|serde_json::from_str::<Value>(&s).ok()),"state":r.get::<_,String>(2)?})))?.collect::<Result<Vec<_>,_>>()?;
    if exceptions.len() > 500 {
        return Err(fail(
            "approval_expansion_limit",
            "单次系列批准最多包含500个例外，请先拆分",
        ));
    }
    Ok(crate::trigger_store::digest(
        &json!({"definition":a,"exceptions":exceptions}),
    ))
}
fn approval_reference(db: &Connection, owner: &str, id: &str) -> anyhow::Result<Option<Value>> {
    let (_, _, _, a, _) = activity_row(db, owner, id)?;
    let hash = approval_digest(db, id, &a)?;
    Ok(db.query_row("SELECT id,actor_id,approved_revision,scope,created_at,definition_hash FROM schedule_admin_approvals WHERE activity_id=?1 AND owner_id=?2 AND actor_id=?2 AND definition_hash=?3 AND revoked_at IS NULL ORDER BY created_at DESC,rowid DESC LIMIT 1",params![id,owner,hash],|r|Ok(json!({"id":r.get::<_,String>(0)?,"actor_id":r.get::<_,String>(1)?,"approved_revision":r.get::<_,i64>(2)?,"scope":r.get::<_,String>(3)?,"approved_at":r.get::<_,i64>(4)?,"definition_hash":r.get::<_,String>(5)?}))).optional()?)
}
pub fn admin_approved_in(db: &Connection, owner: &str, id: &str) -> anyhow::Result<bool> {
    Ok(approval_reference(db, owner, id)?.is_some())
}

/// Administrative HTTP path only. This is intentionally absent from model
/// proposal handling: preserving the AI creator does not grant self-approval.
pub fn approve_admin_in(
    db: &Connection,
    owner: &str,
    actor: &str,
    id: &str,
    expected: i64,
    confirmed: bool,
    scope: &str,
    now: i64,
) -> anyhow::Result<Value> {
    if !confirmed || scope != "series" || actor != owner {
        return Err(fail(
            "explicit_series_approval_required",
            "须由当前管理人明确确认批准整个已列定义系列",
        ));
    }
    let (revision, state, creator, mut a, _) = activity_row(db, owner, id)?;
    if revision != expected {
        return Err(conflict());
    }
    if state != "pending_approval" {
        return Err(fail("activity_not_pending_approval", "只能批准待审核活动"));
    }
    validate(&a)?;
    let mut definitions = vec![a.clone()];
    let rows=db.prepare("SELECT nominal_at,definition_json FROM schedule_exceptions WHERE activity_id=?1 AND definition_json IS NOT NULL LIMIT 501")?.query_map([id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?)))?.collect::<Result<Vec<_>,_>>()?;
    if rows.len() > 500 {
        return Err(fail(
            "approval_expansion_limit",
            "单次系列批准最多包含500个例外",
        ));
    }
    for (_, d) in &rows {
        definitions.push(serde_json::from_str(d)?);
    }
    for definition in &definitions {
        validate(definition)?;
        if let Some(resource) = &definition.resource_id {
            if !resource_valid(db, owner, resource)? {
                return Err(fail(
                    "unauthorized_resource",
                    "批准不能读取已撤销或其他来源域的资料",
                ));
            }
        }
        if !conflicts(db, owner, definition, Some(id), now)?.is_empty() {
            return Err(fail(
                "schedule_conflict",
                "待批准定义与已有活动冲突，请先改期再批准",
            ));
        }
    }
    if expanded_with_exceptions(db, id, &a, now, now + 90 * 86400, "enabled")?.is_empty() {
        return Err(fail(
            "approval_window_expired",
            "没有未来可执行机会，请先改期再批准",
        ));
    }
    let previous_grant = a.grant_id.take();
    let definition = serde_json::to_string(&a)?;
    for (nominal, d) in rows {
        let mut exception: Activity = serde_json::from_str(&d)?;
        exception.grant_id = None;
        db.execute("UPDATE schedule_exceptions SET definition_json=?1 WHERE activity_id=?2 AND nominal_at=?3",params![serde_json::to_string(&exception)?,id,nominal])?;
    }
    db.execute("UPDATE schedule_admin_approvals SET revoked_at=?1 WHERE activity_id=?2 AND revoked_at IS NULL",params![now,id])?;
    let hash = approval_digest(db, id, &a)?;
    let approval_id = uuid::Uuid::new_v4().to_string();
    db.execute("UPDATE schedule_activities SET revision=revision+1,state='enabled',definition_json=?1,grant_id=NULL,grant_revision=NULL,updated_at=?2 WHERE id=?3 AND owner_id=?4 AND revision=?5",params![definition,now,id,owner,expected])?;
    db.execute(
        "INSERT INTO schedule_admin_approvals VALUES(?1,?2,?3,?4,?5,?6,'series',?7,?8,NULL)",
        params![
            approval_id,
            id,
            owner,
            actor,
            revision + 1,
            hash,
            previous_grant,
            now
        ],
    )?;
    db.execute(
        "INSERT INTO schedule_revisions VALUES(?1,?2,?3,'enabled',?4)",
        params![id, revision + 1, definition, now],
    )?;
    db.execute("DELETE FROM schedule_occurrences WHERE activity_id=?1 AND status IN('pending','blocked','skipped') AND nominal_at>=?2",params![id,now])?;
    audit(db, actor, "schedule.activity.approve_series", id, now)?;
    let mut result = detail(db, owner, id)?;
    result["creator"] = json!(creator);
    Ok(result)
}
pub fn list(db: &Connection, owner: &str) -> anyhow::Result<Value> {
    let ids=db.prepare("SELECT id FROM schedule_activities WHERE owner_id=?1 ORDER BY updated_at DESC LIMIT 100")?.query_map([owner],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
    Ok(
        json!({"items":ids.iter().map(|id|detail(db,owner,id)).collect::<anyhow::Result<Vec<_>>>()?}),
    )
}
pub fn preview(
    db: &Connection,
    owner: &str,
    v: &Value,
    exclude: Option<&str>,
    now: i64,
) -> anyhow::Result<Value> {
    let a = parse_activity(v)?;
    let end = (a.start_at + 31 * 86400).max(a.end_at + 1);
    let occurrences = expand(&a, a.start_at, end, MAX_ITEMS)?
        .iter()
        .map(|(s, e)| json!({"start_at":s,"end_at":e}))
        .collect::<Vec<_>>();
    Ok(
        json!({"occurrences":occurrences,"conflicts":conflicts(db,owner,&a,exclude,now)?,"read_only":true,"time_zone":a.time_zone,"capability_block":capability_block(&a),"autonomy":if a.grant_id.is_some(){match grant_check(db,owner,&a,None,exclude,now){Ok(rev)=>json!({"allowed":true,"grant_revision":rev}),Err(e)=>json!({"allowed":false,"reason":e.to_string()})}}else{Value::Null}}),
    )
}
pub fn change(
    db: &Connection,
    owner: &str,
    id: &str,
    expected: i64,
    scope: &str,
    nominal: Option<i64>,
    v: Option<&Value>,
    state: Option<&str>,
    autonomous: bool,
    now: i64,
) -> anyhow::Result<Value> {
    let (revision, current, creator, old, grant_rev) = activity_row(db, owner, id)?;
    let had_admin_approval = admin_approved_in(db, owner, id)?;
    if revision != expected {
        return Err(conflict());
    }
    if current == "pending_approval" && state.is_some_and(|s| s != "cancelled") {
        return Err(fail(
            "explicit_series_approval_required",
            "待审核活动须通过明确确认的批准入口；可编辑定义或取消",
        ));
    }
    if !["series", "single", "following"].contains(&scope) {
        return Err(fail(
            "invalid_edit_scope",
            "修改范围须为single、following或series",
        ));
    }
    if autonomous && (creator != "autonomous" || !old.movable) {
        return Err(fail(
            "activity_locked",
            "自主规划只能修改自己创建且允许移动的活动",
        ));
    }
    if state.is_some_and(|s| !["enabled", "paused", "cancelled"].contains(&s)) {
        return Err(fail("invalid_activity_state", "活动状态无效"));
    }
    let a = match v {
        Some(v) => parse_activity(v)?,
        None => old.clone(),
    };
    let next_state = state.unwrap_or(&current);
    if current == "cancelled" {
        return Err(fail(
            "activity_cancelled",
            "已取消的活动不能重新启用，请新建活动",
        ));
    }
    if let Some(resource) = &a.resource_id {
        if !resource_valid(db, owner, resource)? {
            return Err(fail(
                "unauthorized_resource",
                "资料已撤销或不属于当前来源域",
            ));
        }
    }
    if next_state == "enabled" && !conflicts(db, owner, &a, Some(id), now)?.is_empty() {
        return Err(fail(
            "schedule_conflict",
            "改期与已有活动冲突，原计划保持不变",
        ));
    }
    let new_grant = if autonomous && next_state == "enabled" {
        Some(grant_check(db, owner, &a, grant_rev, Some(id), now)?)
    } else {
        grant_rev
    };
    if scope != "series" {
        let at = nominal.ok_or_else(|| fail("nominal_required", "单次及此后修改需要原计划时刻"))?;
        if at < now
            || expand(&old, at, at + 1, MAX_ITEMS)?
                .iter()
                .all(|(s, _)| *s != at)
        {
            return Err(fail("invalid_occurrence", "只可修改尚未执行的有效单次机会"));
        }
        let finished:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM schedule_occurrences WHERE activity_id=?1 AND nominal_at>=?2 AND (?3='following' OR nominal_at=?2) AND status IN('completed','running','partial','unknown'))",params![id,at,scope],|r|r.get(0))?;
        if finished {
            return Err(fail(
                "occurrence_immutable",
                "已开始或有真实结果的历史不允许改写",
            ));
        }
        if scope == "single" {
            let mut x = a.clone();
            x.repeat = None;
            if v.is_none() {
                x.start_at = at;
                x.end_at = at + (old.end_at - old.start_at);
            }
            db.execute("INSERT INTO schedule_exceptions VALUES(?1,?2,?3,?4,?5) ON CONFLICT(activity_id,nominal_at) DO UPDATE SET definition_json=excluded.definition_json,state=excluded.state,revision=excluded.revision",params![id,at,serde_json::to_string(&x)?,next_state,revision+1])?;
            db.execute("DELETE FROM schedule_occurrences WHERE activity_id=?1 AND nominal_at=?2 AND status IN('pending','blocked','skipped')",params![id,at])?;
        } else {
            // Split the future series at its original civil occurrence. Old
            // materialized history and its rule version stay immutable.
            let mut prior = old.clone();
            if let Some(r) = prior.repeat.as_mut() {
                r.until = Some(at - 1);
            } else {
                return Err(fail("not_a_series", "此后修改需要重复系列"));
            }
            db.execute(
                "UPDATE schedule_activities SET definition_json=?1 WHERE id=?2",
                params![serde_json::to_string(&prior)?, id],
            )?;
            let mut following = a.clone();
            if v.is_none() {
                following.start_at = at;
                following.end_at = at + (old.end_at - old.start_at);
                if let Some(rule) = following.repeat.as_mut() {
                    if let Some(count) = rule.count {
                        let zone = tz(&old.time_zone)?;
                        let base = instant(old.start_at)?.with_timezone(&zone);
                        let split = instant(at)?.with_timezone(&zone);
                        let ordinal = match rule.frequency.as_str() {
                            "daily" => {
                                (split.date_naive() - base.date_naive()).num_days() / rule.interval
                            }
                            "weekly" => {
                                (split.date_naive() - base.date_naive()).num_days()
                                    / (7 * rule.interval)
                            }
                            "monthly" => {
                                ((split.year() - base.year()) as i64 * 12 + split.month0() as i64
                                    - base.month0() as i64)
                                    / rule.interval
                            }
                            _ => 0,
                        };
                        rule.count = Some(count.saturating_sub(ordinal.max(0) as usize).max(1));
                    }
                }
            }
            let follow_id = uuid::Uuid::new_v4().to_string();
            let following_state = if creator == "autonomous"
                && following.grant_id.is_none()
                && next_state == "enabled"
            {
                "pending_approval"
            } else {
                next_state
            };
            db.execute(
                "INSERT INTO schedule_activities VALUES(?1,?2,?3,1,?4,?5,?6,?7,?8,?9,?10,?11,?11)",
                params![
                    follow_id,
                    owner,
                    assistant(db)?,
                    following.title,
                    following.kind,
                    creator,
                    following_state,
                    serde_json::to_string(&following)?,
                    following.grant_id,
                    new_grant,
                    now
                ],
            )?;
            db.execute(
                "INSERT INTO schedule_revisions VALUES(?1,1,?2,?3,?4)",
                params![
                    follow_id,
                    serde_json::to_string(&following)?,
                    following_state,
                    now
                ],
            )?;
            db.execute("DELETE FROM schedule_occurrences WHERE activity_id=?1 AND nominal_at>=?2 AND status IN('pending','blocked','skipped')",params![id,at])?;
        }
        db.execute(
            "UPDATE schedule_activities SET revision=revision+1,updated_at=?1 WHERE id=?2",
            params![now, id],
        )?;
    } else {
        if autonomous && a.start_at < now {
            return Err(fail("past_activity", "不可自主修改过去活动"));
        }
        db.execute("UPDATE schedule_activities SET revision=revision+1,title=?1,kind=?2,state=?3,definition_json=?4,grant_id=?5,grant_revision=?6,updated_at=?7 WHERE id=?8",params![a.title,a.kind,next_state,serde_json::to_string(&a)?,a.grant_id,new_grant,now,id])?;
        db.execute("UPDATE schedule_occurrences SET status=?1,block_reason=CASE WHEN ?1='cancelled' THEN 'cancelled_by_owner' ELSE block_reason END WHERE activity_id=?2 AND status IN('pending','blocked')",params![if next_state=="cancelled"{"cancelled"}else{"pending"},id])?;
        if v.is_some() {
            db.execute("DELETE FROM schedule_occurrences WHERE activity_id=?1 AND nominal_at>=?2 AND status IN('pending','blocked','skipped')",params![id,now])?;
        }
    }
    if had_admin_approval && !admin_approved_in(db, owner, id)? {
        db.execute("UPDATE schedule_admin_approvals SET revoked_at=?1 WHERE activity_id=?2 AND revoked_at IS NULL",params![now,id])?;
        db.execute("UPDATE schedule_activities SET state='pending_approval' WHERE id=?1 AND state!='cancelled'",[id])?;
    }
    if creator == "autonomous" && a.grant_id.is_none() && !admin_approved_in(db, owner, id)? {
        db.execute("UPDATE schedule_activities SET state='pending_approval' WHERE id=?1 AND state='enabled'",[id])?;
    }
    let saved = activity_row(db, owner, id)?;
    db.execute(
        "INSERT INTO schedule_revisions VALUES(?1,?2,?3,?4,?5)",
        params![
            id,
            revision + 1,
            serde_json::to_string(&saved.3)?,
            saved.1,
            now
        ],
    )?;
    if autonomous {
        mutate_budget(db, owner, &a)?;
        allocate_activity_budget(db, owner, id, &a)?;
    }
    audit(db, owner, "schedule.activity.change", id, now)?;
    detail(db, owner, id)
}
fn capability_block(a: &Activity) -> Option<&'static str> {
    match a.kind.as_str() {
        "video" => Some("video_provider_unavailable"),
        "chat" => Some("contact_consent_and_verified_delivery_required"),
        "action" => Some("registered_action_broker_required"),
        "consolidation" => Some("consolidation_system_job_only"),
        _ => None,
    }
}
/// Only movable, granted activities can be changed by a model. System jobs,
/// manually locked series and external contact are not mutable by this notice.
pub fn notice_candidates(
    db: &Connection,
    at: i64,
) -> anyhow::Result<Vec<(String, i64, i64, Activity)>> {
    let rows=db.prepare("SELECT id,revision,definition_json FROM schedule_activities WHERE state='enabled' AND creator='autonomous' AND kind IN('reading','writing') ORDER BY created_at DESC LIMIT 200")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?)))?.collect::<Result<Vec<_>,_>>()?;
    let mut items = vec![];
    for (id, revision, raw) in rows {
        let a: Activity = serde_json::from_str(&raw)?;
        if !a.movable {
            continue;
        }
        for (nominal, x, state) in
            expanded_with_exceptions(db, &id, &a, at + 1, at + 61, "enabled")?
        {
            if state == "enabled" && x.start_at > at && x.start_at <= at + 60 {
                items.push((id.clone(), revision, nominal, x));
            }
        }
    }
    Ok(items)
}

pub fn calendar(
    db: &Connection,
    owner: &str,
    start: i64,
    end: i64,
    time_zone: &str,
    kind: Option<&str>,
    creator_filter: Option<&str>,
    state_filter: Option<&str>,
    offset: usize,
    limit: usize,
) -> anyhow::Result<Value> {
    tz(time_zone)?;
    instant(start)?;
    instant(end)?;
    if end <= start || end - start > MAX_RANGE || limit == 0 || limit > MAX_ITEMS || offset > 10000
    {
        return Err(fail(
            "invalid_calendar_range",
            "日历范围不超过93天，每页1至500条",
        ));
    }
    let rows=db.prepare("SELECT id,revision,state,creator,definition_json FROM schedule_activities WHERE owner_id=?1 ORDER BY id LIMIT 2001")?.query_map([owner],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?)))?.collect::<Result<Vec<_>,_>>()?;
    if rows.len() > 2000 {
        return Err(fail("schedule_capacity", "请先归档历史活动后查询"));
    }
    let mut items = Vec::new();
    for (id, revision, state, creator, d) in rows {
        let a: Activity = serde_json::from_str(&d)?;
        if kind.is_some_and(|s| s != a.kind)
            || creator_filter.is_some_and(|s| s != creator)
            || state_filter.is_some_and(|s| s != state)
        {
            continue;
        }
        for (nominal, x, exception_state) in
            expanded_with_exceptions(db, &id, &a, start, end, &state)?
        {
            if x.start_at >= end || x.end_at <= start {
                continue;
            }
            let occurrence=db.query_row("SELECT id,status,block_reason,checkpoint,total_chars,started_at,completed_at FROM schedule_occurrences WHERE activity_id=?1 AND nominal_at=?2",params![id,nominal],|r|Ok(json!({"id":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,"block_reason":r.get::<_,Option<String>>(2)?,"processed_chars":r.get::<_,i64>(3)?,"total_chars":r.get::<_,Option<i64>>(4)?,"started_at":r.get::<_,Option<i64>>(5)?,"completed_at":r.get::<_,Option<i64>>(6)?}))).optional()?;
            items.push(json!({"origin":"schedule","id":format!("{}:{}",id,nominal),"activity_id":id,"revision":revision,"nominal_at":nominal,"title":x.title,"type":x.kind,"creator":creator,"state":exception_state,"status":occurrence.as_ref().map(|v|v["status"].clone()).unwrap_or_else(||json!(if exception_state=="enabled"{"planned"}else{&exception_state})),"start_at":x.start_at,"end_at":x.end_at,"time_zone":x.time_zone,"movable":x.movable,"block_reason":occurrence.as_ref().map(|v|v["block_reason"].clone()).unwrap_or_else(||json!(capability_block(&x))),"progress":occurrence,"delivery_status":if x.kind=="chat"{json!("not_sent")}else{Value::Null}}));
        }
        // Completed occurrence snapshots survive edits to the current series.
        let history=db.prepare("SELECT id,nominal_at,start_at,end_at,status,block_reason,checkpoint,total_chars,definition_json FROM schedule_occurrences WHERE activity_id=?1 AND owner_id=?2 AND start_at<?3 AND end_at>?4 AND status IN('completed','partial','failed','unknown')")?.query_map(params![id,owner,end,start],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,String>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,i64>(6)?,r.get::<_,Option<i64>>(7)?,r.get::<_,String>(8)?)))?.collect::<Result<Vec<_>,_>>()?;
        for (oid, nominal, s, e, status, reason, checkpoint, total, snapshot) in history {
            items.retain(|v| !(v["activity_id"] == id && v["nominal_at"] == nominal));
            let x: Activity = serde_json::from_str(&snapshot)?;
            items.push(json!({"origin":"schedule","id":oid,"activity_id":id,"revision":revision,"nominal_at":nominal,"title":x.title,"type":x.kind,"creator":creator,"state":state,"status":status,"start_at":s,"end_at":e,"time_zone":x.time_zone,"block_reason":reason,"progress":{"processed_chars":checkpoint,"total_chars":total}}));
        }
        if items.len() > 10000 {
            return Err(fail("calendar_expansion_limit", "活动过多，请缩短查询范围"));
        }
    }
    for item in crate::daily_life::calendar(db, start, end)? {
        if kind.is_none_or(|k| item["type"] == k)
            && creator_filter.is_none_or(|c| item["creator"] == c)
            && state_filter.is_none_or(|s| item["state"] == s)
        {
            items.push(item);
        }
    }
    let mut legacy_events = Vec::new();
    let has_legacy: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='trigger_jobs')",
        [],
        |r| r.get(0),
    )?;
    if has_legacy {
        let calendars = crate::trigger_calendar::load(db)?;
        let jobs=db.prepare("SELECT id,title,revision,lifecycle,next_fire_at,active_revision,draft_revision FROM trigger_jobs WHERE owner_id=?1 ORDER BY updated_at DESC LIMIT 100")?.query_map([owner],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?,r.get::<_,Option<i64>>(4)?,r.get::<_,Option<i64>>(5)?,r.get::<_,i64>(6)?)))?.collect::<Result<Vec<_>,_>>()?;
        for (id, title, revision, lifecycle, next, active, draft) in jobs {
            let d:Option<String>=db.query_row("SELECT definition_json FROM trigger_job_revisions WHERE job_id=?1 AND revision=?2",params![id,active.unwrap_or(draft)],|r|r.get(0)).optional()?;
            let definition = d
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .unwrap_or(Value::Null);
            if definition["trigger"]["type"] == "event" {
                legacy_events.push(json!({"id":id,"title":title,"revision":revision,"state":lifecycle,"origin":"legacy_trigger","type":"event","rule":definition["trigger"]}));
            }
            if active.is_some()
                && lifecycle == "enabled"
                && definition["trigger"]["type"] != "event"
            {
                let def: crate::trigger_rule::TaskDefinition =
                    serde_json::from_value(definition.clone())?;
                let anchor: Option<i64> =
                    db.query_row("SELECT anchor FROM trigger_jobs WHERE id=?1", [&id], |r| {
                        r.get(0)
                    })?;
                let mut cursor = start
                    .saturating_sub(1)
                    .max(next.unwrap_or(start).saturating_sub(1));
                for n in 0..=MAX_ITEMS {
                    match crate::trigger_rule::next_due(&def, anchor, cursor, &calendars) {
                        Ok(Some(due)) if due.at < end => {
                            if n == MAX_ITEMS {
                                return Err(fail(
                                    "calendar_expansion_limit",
                                    "兼容触发次数过多，请缩短查询范围",
                                ));
                            }
                            items.push(json!({"origin":"legacy_trigger","id":format!("legacy:{}:{}",id,due.at),"activity_id":id,"title":title,"revision":revision,"state":lifecycle,"status":"planned","start_at":due.at,"end_at":Value::Null,"type":"legacy_trigger","duration_unspecified":true,"time_zone":definition["time_zone"]}));
                            cursor = due.at;
                        }
                        Err(reason) => {
                            legacy_events.push(json!({"id":id,"title":title,"revision":revision,"state":"blocked","origin":"legacy_trigger","type":"calendar_block","block_reason":reason}));
                            break;
                        }
                        _ => break,
                    }
                }
            }
        }
        let history=db.prepare("SELECT id,job_id,nominal_at,status,delivery_status,definition_json FROM trigger_occurrences WHERE owner_id=?1 AND nominal_at>=?2 AND nominal_at<?3 ORDER BY nominal_at LIMIT 501")?.query_map(params![owner,start,end],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?)))?.collect::<Result<Vec<_>,_>>()?;
        for (oid, id, at, status, delivery, d) in history {
            items.retain(|v| {
                !(v["origin"] == "legacy_trigger" && v["activity_id"] == id && v["start_at"] == at)
            });
            let d: Value = serde_json::from_str(&d)?;
            items.push(json!({"origin":"legacy_trigger","id":oid,"activity_id":id,"title":d["title"],"status":status,"start_at":at,"end_at":Value::Null,"type":"legacy_trigger","duration_unspecified":true,"delivery_status":delivery,"time_zone":d["time_zone"]}));
        }
    }
    let has_diary: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='diary_model_jobs')",
        [],
        |r| r.get(0),
    )?;
    if has_diary
        && kind.is_none_or(|kind| kind == "consolidation")
        && creator_filter.is_none_or(|creator| creator == "system")
    {
        // A projection of the authoritative diary ledger, never a new activity
        // or second consolidation dispatch. Titles contain no diary content.
        let jobs=db.prepare("SELECT j.entry_id,j.status,j.error,j.updated_at,e.period,e.period_start,e.timezone FROM diary_model_jobs j JOIN diary_entries e ON e.id=j.entry_id WHERE (e.source_owner_id=?1 OR e.visibility='shared') AND e.deleted_at IS NULL AND j.updated_at>=?2 AND j.updated_at<?3 ORDER BY j.updated_at LIMIT 500")?.query_map(params![owner,start,end],|r|Ok(json!({"origin":"diary_system","id":format!("diary:{}",r.get::<_,String>(0)?),"activity_id":Value::Null,"entry_id":r.get::<_,String>(0)?,"title":format!("{} 记录整理（{}）",r.get::<_,String>(5)?,r.get::<_,String>(4)?),"type":"consolidation","creator":"system","status":r.get::<_,String>(1)?,"block_reason":r.get::<_,Option<String>>(2)?,"start_at":r.get::<_,i64>(3)?,"end_at":Value::Null,"time_zone":r.get::<_,String>(6)?,"read_only":true,"duration_unspecified":true,"actual_job_projection":true})))?.collect::<Result<Vec<_>,_>>()?;
        items.extend(jobs);
        let scopes=db.prepare("SELECT DISTINCT timezone FROM diary_scope_checkpoints WHERE source_owner_id=?1 OR visibility='shared' LIMIT 8")?.query_map([owner],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        for zone_name in scopes {
            let zone = tz(&zone_name)?;
            let mut day = instant(start.max(crate::state::now()))?
                .with_timezone(&zone)
                .date_naive();
            for _ in 0..94 {
                let LocalResult::Single(local) =
                    zone.from_local_datetime(&day.and_hms_opt(0, 5, 0).unwrap())
                else {
                    day += Duration::days(1);
                    continue;
                };
                let at = local.timestamp();
                if at >= end {
                    break;
                }
                if at >= start && at >= crate::state::now() {
                    items.push(json!({"origin":"diary_system","id":format!("diary-fallback:{}:{}",zone_name,day),"activity_id":Value::Null,"title":"每日记录兜底整理","type":"consolidation","creator":"system","status":"planned","start_at":at,"end_at":Value::Null,"time_zone":zone_name,"read_only":true,"duration_unspecified":true,"system_rule_projection":true}));
                }
                day += Duration::days(1);
            }
        }
    }
    if items.len() > 10000 {
        return Err(fail(
            "calendar_expansion_limit",
            "活动展开总数过多，请缩短时间范围",
        ));
    }
    items.sort_by_key(|v| v["start_at"].as_i64().unwrap_or(0));
    let total = items.len();
    Ok(
        json!({"items":items.into_iter().skip(offset).take(limit).collect::<Vec<_>>(),"total":total,"limit":limit,"offset":offset,"legacy_events":legacy_events,"time_zone":time_zone,"start":start,"end":end,"truncated":total>offset+limit,"capabilities":{"text_reading":true,"video":false,"proactive_chat":false,"model_planning":true}}),
    )
}

pub fn create_plan(db: &Connection, owner: &str, v: &Value, now: i64) -> anyhow::Result<Value> {
    let p: Plan = serde_json::from_value(v.clone())
        .map_err(|_| fail("invalid_work_plan", "工作目标字段无效"))?;
    if !text_ok(&p.goal, 2000)
        || !(1..=120).contains(&p.duration_minutes)
        || p.resource_ids.is_empty()
        || p.resource_ids.len() > 100
        || !["enabled", "paused"].contains(&p.state.as_str())
        || !["bounded_rule", "model"].contains(&p.planning_mode.as_str())
    {
        return Err(fail("invalid_work_plan", "目标、时长、资料或状态无效"));
    }
    let (_, g) = load_grant(db, owner, &p.grant_id)?;
    if !g.allowed_types.contains(&"reading".into())
        || p.resource_ids.iter().any(|r| !g.resource_ids.contains(r))
    {
        return Err(fail(
            "work_plan_out_of_scope",
            "阅读目标必须在已有自主授权内",
        ));
    }
    let count: i64 = db.query_row(
        "SELECT count(*) FROM schedule_plans WHERE owner_id=?1 AND state='enabled'",
        [owner],
        |r| r.get(0),
    )?;
    if count >= 20 {
        return Err(fail("work_plan_limit", "每人最多20个运行中的目标"));
    }
    let id = uuid::Uuid::new_v4().to_string();
    db.execute(
        "INSERT INTO schedule_plans(id,owner_id,goal,revision,definition_json,state,last_planned_day,last_planned_at,created_at,updated_at) VALUES(?1,?2,?3,1,?4,?5,NULL,NULL,?6,?6)",
        params![id, owner, p.goal, serde_json::to_string(&p)?, p.state, now],
    )?;
    audit(db, owner, "schedule.plan.create", &id, now)?;
    Ok(
        json!({"id":id,"revision":1,"definition":p,"planning":p.planning_mode,"model_planning":p.planning_mode=="model"}),
    )
}
pub fn list_plans(db: &Connection, owner: &str) -> anyhow::Result<Value> {
    let mut items=db.prepare("SELECT id,revision,definition_json,state,last_planned_day,last_planned_at,block_reason FROM schedule_plans WHERE owner_id=?1 ORDER BY created_at DESC LIMIT 100")?.query_map([owner],|r|Ok(json!({"id":r.get::<_,String>(0)?,"revision":r.get::<_,i64>(1)?,"definition":serde_json::from_str::<Value>(&r.get::<_,String>(2)?).unwrap_or(Value::Null),"state":r.get::<_,String>(3)?,"last_planned_day":r.get::<_,Option<String>>(4)?,"last_planned_at":r.get::<_,Option<i64>>(5)?,"block_reason":r.get::<_,Option<String>>(6)?,"planning":serde_json::from_str::<Value>(&r.get::<_,String>(2)?).ok().and_then(|v|v["planning_mode"].as_str().map(str::to_owned)).unwrap_or_else(||"bounded_rule".into())})))?.collect::<Result<Vec<_>,_>>()?;
    for item in &mut items {
        item["planning_diagnostics"] =
            crate::schedule_model::planning_metadata(db, owner, item["id"].as_str().unwrap_or(""))?;
    }
    Ok(json!({"items":items}))
}
pub fn change_plan(
    db: &Connection,
    owner: &str,
    id: &str,
    expected: i64,
    state: &str,
    now: i64,
) -> anyhow::Result<Value> {
    if !["enabled", "paused", "cancelled"].contains(&state) {
        return Err(fail("invalid_work_plan", "目标状态无效"));
    }
    if db.execute("UPDATE schedule_plans SET state=?1,revision=revision+1,block_reason=NULL,updated_at=?2 WHERE id=?3 AND owner_id=?4 AND revision=?5",params![state,now,id,owner,expected])?==0{return Err(conflict());}
    audit(db, owner, "schedule.plan.change", id, now)?;
    Ok(json!({"id":id,"revision":expected+1,"state":state}))
}

fn plan_tick(db: &Connection, now: i64) -> anyhow::Result<usize> {
    let last: i64 = db.query_row(
        "SELECT last_plan_tick FROM schedule_runtime WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    if now - last < 60 {
        return Ok(0);
    }
    db.execute(
        "UPDATE schedule_runtime SET last_plan_tick=?1 WHERE id=1",
        [now],
    )?;
    let plans=db.prepare("SELECT id,owner_id,definition_json,last_planned_day,last_planned_at FROM schedule_plans WHERE state='enabled' ORDER BY COALESCE(last_planned_at,0),id LIMIT 20")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,Option<i64>>(4)?)))?.collect::<Result<Vec<_>,_>>()?;
    for (id, owner, d, last_day, last_at) in plans {
        if last_at.is_some_and(|at| now - at < 3600) {
            continue;
        }
        let p: Plan = serde_json::from_str(&d)?;
        if p.planning_mode != "bounded_rule" {
            continue;
        }
        let Ok((_, g)) = load_grant(db, &owner, &p.grant_id) else {
            continue;
        };
        let zone = tz(&g.time_zone)?;
        let date = instant(now)?.with_timezone(&zone).date_naive();
        let day = date.to_string();
        if last_day.as_deref() == Some(&day) || now < g.valid_from || now >= g.valid_until {
            continue;
        }
        let start_minute = window(&g.window_start)?;
        let Some(local) = date.and_hms_opt(start_minute / 60, start_minute % 60, 0) else {
            continue;
        };
        let LocalResult::Single(window_start) = zone.from_local_datetime(&local) else {
            continue;
        };
        let mut at = window_start.timestamp().max(((now + 59) / 60) * 60);
        let mut resource = None;
        let mut missing_resource = false;
        for rid in &p.resource_ids {
            if !g.resource_ids.contains(rid) || !resource_valid(db, &owner, rid)? {
                missing_resource = true;
                continue;
            }
            let progress = resource_progress(db, &owner, rid)?;
            if progress
                .as_ref()
                .is_none_or(|v| v["processed_chars"].as_i64() < v["total_chars"].as_i64())
            {
                resource = Some(rid);
                break;
            }
        }
        let Some(resource) = resource else {
            db.execute("UPDATE schedule_plans SET state=?1,block_reason=?2,last_planned_at=?3,updated_at=?3 WHERE id=?4",params![if missing_resource{"blocked"}else{"completed"},if missing_resource{Some("resource_unavailable_or_out_of_scope")}else{None},now,id])?;
            continue;
        };
        for _ in 0..96 {
            let a = Activity {
                title: format!("阅读：{}", p.goal.chars().take(60).collect::<String>()),
                kind: "reading".into(),
                purpose: p.goal.clone(),
                start_at: at,
                end_at: at + p.duration_minutes * 60,
                time_zone: g.time_zone.clone(),
                resource_id: Some(resource.clone()),
                target_ref: None,
                repeat: None,
                movable: true,
                grant_id: Some(p.grant_id.clone()),
                restart_reading: false,
            };
            if validate(&a).is_err() || grant_check(db, &owner, &a, None, None, now).is_err() {
                break;
            }
            if conflicts(db, &owner, &a, None, now)?.is_empty() {
                let key = format!("plan:{}:{}", id, day);
                create(db, &owner, &serde_json::to_value(a)?, &key, true, now)?;
                db.execute("UPDATE schedule_plans SET last_planned_day=?1,last_planned_at=?2,updated_at=?2 WHERE id=?3",params![day,now,id])?;
                return Ok(1);
            }
            at += 15 * 60;
        }
        db.execute(
            "UPDATE schedule_plans SET last_planned_at=?1 WHERE id=?2",
            params![now, id],
        )?;
    }
    Ok(0)
}

/// Must run inside the application's single database transaction and persistent
/// trigger tick. No HTTP or model calls happen while holding the writer lock.
pub fn tick_in(db: &Connection, now: i64, interactive_busy: bool) -> anyhow::Result<Value> {
    let last: i64 = db.query_row(
        "SELECT last_tick FROM schedule_runtime WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    if now < last {
        // The shared scheduler calls this only after its trusted-clock gate.
        // Rebase the cursor after that proof, without deleting occurrence keys,
        // results, budgets or checkpoints and without dispatching in this tick.
        db.execute("UPDATE schedule_runtime SET last_tick=?1,last_plan_tick=MIN(last_plan_tick,?1) WHERE id=1",[now])?;
        return Ok(json!({"blocked":"clock_moved_backwards_rebased"}));
    }
    if now - last < 1 {
        return Ok(json!({"processed":0}));
    }
    db.execute("UPDATE schedule_runtime SET last_tick=?1 WHERE id=1", [now])?;
    let mut planned = 0;
    if !interactive_busy {
        planned = plan_tick(db, now)?;
    }
    let rows=db.prepare("SELECT id,owner_id,revision,creator,definition_json,grant_revision FROM schedule_activities WHERE state='enabled' ORDER BY created_at,id LIMIT 2000")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,Option<i64>>(5)?)))?.collect::<Result<Vec<_>,_>>()?;
    for (id, owner, revision, creator, d, grant_revision) in rows {
        let a: Activity = serde_json::from_str(&d)?;
        // At most the current 24-hour window is recovered. Old missed periods
        // remain visibly skipped; no unbounded backlog or contact replay.
        let from = now - 86400;
        for (nominal, x, s) in expanded_with_exceptions(db, &id, &a, from, now + 1, "enabled")? {
            if s != "enabled" || x.start_at > now {
                continue;
            }
            let oid = format!("{}:{}", id, nominal);
            let block = if x.kind=="writing" && !db.query_row("SELECT EXISTS(SELECT 1 FROM daily_life_activities WHERE activity_id=?1 AND activity_revision=?2)",params![id,revision],|r|r.get::<_,bool>(0)).unwrap_or(false) {Some("registered_text_activity_required")} else {capability_block(&x)};
            let status = if x.end_at <= now {
                "skipped"
            } else if block.is_some() {
                "blocked"
            } else {
                "pending"
            };
            db.execute("INSERT OR IGNORE INTO schedule_occurrences(id,activity_id,owner_id,activity_revision,nominal_at,start_at,end_at,definition_json,status,block_reason,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",params![oid,id,owner,revision,nominal,x.start_at,x.end_at,serde_json::to_string(&x)?,status,if status=="skipped"{Some("missed_execution_window")}else{block},now])?;
            if creator == "autonomous" && !admin_approved_in(db, &owner, &id)? {
                // A started activity may continue after its nominal start; its
                // original policy limits were checked atomically at planning.
                let reason = match x
                    .grant_id
                    .as_deref()
                    .map(|g| load_grant(db, &owner, g))
                    .transpose()
                {
                    Ok(Some((r, g)))
                        if Some(r) == grant_revision
                            && now >= g.valid_from
                            && now < g.valid_until =>
                    {
                        None
                    }
                    _ => Some("grant_revoked_expired_or_changed"),
                };
                if let Some(reason) = reason {
                    db.execute("UPDATE schedule_occurrences SET status='blocked',block_reason=?1 WHERE id=?2 AND status IN('pending','partial')",params![reason,oid])?;
                }
            }
        }
    }
    db.execute("UPDATE schedule_occurrences SET status='partial',block_reason='execution_window_elapsed' WHERE status IN('pending','partial') AND end_at<=?1 AND checkpoint>0",[now])?;
    db.execute("UPDATE schedule_occurrences SET status='skipped',block_reason='execution_window_elapsed' WHERE status='pending' AND end_at<=?1 AND checkpoint=0",[now])?;
    if interactive_busy {
        return Ok(json!({"processed":0,"planned":planned,"waiting":"interactive_priority"}));
    }
    crate::daily_life::execute_writing(db, now)?;
    // Fair single-slot admission: one chunk per tick, including all owners.
    let ready:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM schedule_occurrences o JOIN schedule_activities a ON a.id=o.activity_id WHERE a.state='enabled' AND a.kind='reading' AND o.status IN('pending','partial') AND o.start_at<=?1 AND o.end_at>?1)",[now],|r|r.get(0))?;
    Ok(json!({"processed":0,"planned":planned,"reading_ready":ready}))
}

#[derive(Clone, Debug, Serialize)]
pub struct ReadChunk {
    pub occurrence_id: String,
    pub fence: String,
    pub activity_revision: i64,
    pub reading_generation: i64,
    pub owner_id: String,
    pub resource_id: String,
    pub resource_hash: String,
    pub resource_title: String,
    pub start_char: usize,
    pub end_char: usize,
    pub total_chars: usize,
    pub text: String,
    pub purpose: String,
    pub max_output_tokens: usize,
}
/// Claim under the same global automatic slot and model-request admission as
/// trigger_model. The caller owns the real request lease until transport ends.
/// A host-bound `owner_id` is returned; never use the last interactive session.
pub fn prepare_read_chunk(db: &Connection, now: i64) -> anyhow::Result<Option<ReadChunk>> {
    let running: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM schedule_occurrences WHERE status='running')",
        [],
        |r| r.get(0),
    )?;
    if running {
        return Ok(None);
    }
    let row=db.query_row("SELECT o.id,o.owner_id,o.definition_json,o.checkpoint,o.resource_hash,a.revision,o.reading_generation FROM schedule_occurrences o JOIN schedule_activities a ON a.id=o.activity_id WHERE a.state='enabled' AND a.kind='reading' AND o.status IN('pending','partial') AND o.start_at<=?1 AND o.end_at>?1 AND o.attempts<3 ORDER BY o.updated_at,o.start_at,o.id LIMIT 1",[now],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,usize>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,i64>(5)?,r.get::<_,i64>(6)?))).optional()?;
    let Some((oid, owner, d, _checkpoint, prior_hash, activity_revision, previous_generation)) =
        row
    else {
        return Ok(None);
    };
    let a: Activity = serde_json::from_str(&d)?;
    let Some(resource_id) = a.resource_id.as_deref() else {
        return Ok(None);
    };
    let (creator,grant_id,grant_revision):(String,Option<String>,Option<i64>)=db.query_row("SELECT a.creator,a.grant_id,a.grant_revision FROM schedule_activities a JOIN schedule_occurrences o ON o.activity_id=a.id WHERE o.id=?1",[&oid],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let activity_id: String = db.query_row(
        "SELECT activity_id FROM schedule_occurrences WHERE id=?1",
        [&oid],
        |r| r.get(0),
    )?;
    if creator == "autonomous" && !admin_approved_in(db, &owner, &activity_id)? {
        let allowed = match grant_id
            .as_deref()
            .map(|id| load_grant(db, &owner, id))
            .transpose()
        {
            Ok(Some((revision, g))) => {
                Some(revision) == grant_revision
                    && now >= g.valid_from
                    && now < g.valid_until
                    && g.allowed_types.contains(&"reading".into())
                    && g.resource_ids.iter().any(|id| id == resource_id)
                    && a.grant_id == grant_id
            }
            _ => false,
        };
        if !allowed {
            db.execute("UPDATE schedule_occurrences SET status='blocked',block_reason='grant_scope_changed',fence=NULL WHERE id=?1",[&oid])?;
            return Ok(None);
        }
    }
    let resource=db.query_row("SELECT content,content_hash,title FROM schedule_resources WHERE id=?1 AND owner_id=?2 AND revoked_at IS NULL AND content IS NOT NULL",params![resource_id,owner],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional()?;
    let Some((content, hash, title)) = resource else {
        db.execute("UPDATE schedule_occurrences SET status='blocked',block_reason='resource_revoked',updated_at=?1 WHERE id=?2",params![now,oid])?;
        return Ok(None);
    };
    if prior_hash.as_ref().is_some_and(|h| h != &hash) {
        db.execute("UPDATE schedule_occurrences SET status='blocked',block_reason='resource_version_changed' WHERE id=?1",[oid])?;
        return Ok(None);
    }
    let total = content.chars().count();
    db.execute(
        "INSERT OR IGNORE INTO schedule_reading_progress VALUES(?1,?2,?3,1,0,?4,NULL,?5)",
        params![owner, resource_id, hash, total, now],
    )?;
    if previous_generation == 0 && a.restart_reading {
        db.execute("UPDATE schedule_reading_progress SET generation=generation+1,checkpoint=0,result_json=NULL,updated_at=?1 WHERE owner_id=?2 AND resource_id=?3 AND resource_hash=?4",params![now,owner,resource_id,hash])?;
    }
    let (generation,checkpoint,previous_result):(i64,usize,Option<String>)=db.query_row("SELECT generation,checkpoint,result_json FROM schedule_reading_progress WHERE owner_id=?1 AND resource_id=?2 AND resource_hash=?3",params![owner,resource_id,hash],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    if previous_generation > 0 && previous_generation != generation {
        db.execute("UPDATE schedule_occurrences SET status='blocked',block_reason='reading_generation_changed' WHERE id=?1",[&oid])?;
        return Ok(None);
    }
    db.execute("UPDATE schedule_occurrences SET reading_generation=?1,checkpoint=?2,result_json=?3,total_chars=?4,resource_hash=?5 WHERE id=?6",params![generation,checkpoint,previous_result,total,hash,oid])?;
    if checkpoint >= total {
        db.execute("UPDATE schedule_occurrences SET status='skipped',block_reason='resource_already_read',updated_at=?1 WHERE id=?2",params![now,oid])?;
        return Ok(None);
    }
    let text = content
        .chars()
        .skip(checkpoint)
        .take(READ_CHARS)
        .collect::<String>();
    let end = checkpoint + text.chars().count();
    let fence = uuid::Uuid::new_v4().to_string();
    db.execute("UPDATE schedule_occurrences SET status='running',fence=?1,attempts=attempts+1,resource_hash=?2,total_chars=?3,started_at=COALESCE(started_at,?4),updated_at=?4 WHERE id=?5",params![fence,hash,total,now,oid])?;
    Ok(Some(ReadChunk {
        occurrence_id: oid,
        fence,
        activity_revision,
        reading_generation: generation,
        owner_id: owner,
        resource_id: resource_id.into(),
        resource_hash: hash,
        resource_title: title,
        start_char: checkpoint,
        end_char: end,
        total_chars: total,
        text,
        purpose: a.purpose,
        max_output_tokens: 1200,
    }))
}
/// Only a successful model result advances the checkpoint. Stale fences,
/// cancellation, resource revocation and grant revocation reject the note.
pub fn commit_read_note(
    db: &Connection,
    chunk: &ReadChunk,
    note: &str,
    provider: &str,
    now: i64,
) -> anyhow::Result<Value> {
    if !text_ok(note, 16000) || !text_ok(provider, 160) {
        return Err(fail("invalid_reading_note", "阅读笔记或provider无效"));
    }
    let row=db.query_row("SELECT o.status,o.fence,o.checkpoint,a.state,a.creator,a.grant_id,a.grant_revision,a.revision FROM schedule_occurrences o JOIN schedule_activities a ON a.id=o.activity_id WHERE o.id=?1 AND o.owner_id=?2",params![chunk.occurrence_id,chunk.owner_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,usize>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,Option<i64>>(6)?,r.get::<_,i64>(7)?))).optional()?.ok_or_else(missing)?;
    if row.0 != "running"
        || row.1.as_deref() != Some(chunk.fence.as_str())
        || row.2 != chunk.start_char
        || row.3 != "enabled"
        || row.7 != chunk.activity_revision
    {
        return Err(fail(
            "reading_fence_stale",
            "活动已变更、取消或阅读批次过期，结果不提交",
        ));
    }
    if !resource_valid(db, &chunk.owner_id, &chunk.resource_id)? {
        return Err(fail("resource_revoked", "资料已撤销，结果不提交"));
    }
    let cursor:Option<(i64,usize)>=db.query_row("SELECT generation,checkpoint FROM schedule_reading_progress WHERE owner_id=?1 AND resource_id=?2 AND resource_hash=?3",params![chunk.owner_id,chunk.resource_id,chunk.resource_hash],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if cursor != Some((chunk.reading_generation, chunk.start_char)) {
        return Err(fail(
            "reading_cursor_changed",
            "全局阅读版本或游标已改变，旧片段不重复提交",
        ));
    }
    let activity_id: String = db.query_row(
        "SELECT activity_id FROM schedule_occurrences WHERE id=?1",
        [&chunk.occurrence_id],
        |r| r.get(0),
    )?;
    if row.4 == "autonomous" && !admin_approved_in(db, &chunk.owner_id, &activity_id)? {
        let valid = row
            .5
            .as_deref()
            .map(|id| load_grant(db, &chunk.owner_id, id))
            .transpose()?
            .is_some_and(|(rev, g)| {
                Some(rev) == row.6 && now >= g.valid_from && now < g.valid_until
            });
        if !valid {
            return Err(fail("autonomy_revoked", "自主授权已改变或失效，结果不提交"));
        }
    }
    let old: Option<String> = db.query_row(
        "SELECT result_json FROM schedule_occurrences WHERE id=?1",
        [&chunk.occurrence_id],
        |r| r.get(0),
    )?;
    let mut fragments: Vec<Value> = old
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v["fragments"].as_array().cloned())
        .unwrap_or_default();
    fragments.push(json!({"occurrence_id":chunk.occurrence_id,"start_char":chunk.start_char,"end_char":chunk.end_char,"note":note,"provider":provider,"completed_at":now}));
    let result = json!({"method":"model_text_reading","resource_id":chunk.resource_id,"resource_hash":chunk.resource_hash,"resource_title":chunk.resource_title,"reading_generation":chunk.reading_generation,"covered_chars":{"start":0,"end":chunk.end_char,"total":chunk.total_chars},"fragments":fragments,"visibility":"private","delivery_status":"not_applicable"});
    let complete = chunk.end_char == chunk.total_chars;
    db.execute("UPDATE schedule_occurrences SET status=?1,checkpoint=?2,result_json=?3,completed_at=?4,block_reason=NULL,updated_at=?5,fence=NULL,attempts=0 WHERE id=?6",params![if complete{"completed"}else{"partial"},chunk.end_char,result.to_string(),if complete{Some(now)}else{None},now,chunk.occurrence_id])?;
    db.execute("UPDATE schedule_reading_progress SET checkpoint=?1,result_json=?2,updated_at=?3 WHERE owner_id=?4 AND resource_id=?5 AND resource_hash=?6 AND generation=?7",params![chunk.end_char,result.to_string(),now,chunk.owner_id,chunk.resource_id,chunk.resource_hash,chunk.reading_generation])?;
    Ok(result)
}
pub fn fail_read_chunk(
    db: &Connection,
    chunk: &ReadChunk,
    reason: &str,
    now: i64,
) -> anyhow::Result<()> {
    // No source text or provider error is retained in the diagnostic reason.
    let reason = if reason == "cancelled" {
        "reading_cancelled"
    } else {
        "reading_model_failed"
    };
    db.execute("UPDATE schedule_occurrences SET status=CASE WHEN attempts>=3 THEN 'failed' ELSE 'partial' END,block_reason=?1,fence=NULL,updated_at=?2 WHERE id=?3 AND fence=?4 AND status='running'",params![reason,now,chunk.occurrence_id,chunk.fence])?;
    Ok(())
}

#[cfg(test)]
#[path = "schedule_tests.rs"]
mod tests;
