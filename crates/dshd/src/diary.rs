//! Source-partitioned diary ledger. The database, rather than generated prose,
//! decides identity, visibility, revisions and deletion precedence.
use crate::{
    error::{ApiError, ApiResult},
    memory,
    state::now,
};
use chrono::{Datelike, Duration, NaiveDate, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const TIMEZONE: &str = "Asia/Shanghai";
const MAX_CONTENT: usize = 12000;
const MAX_EVENTS: usize = 200;
#[path = "diary_aggregation.rs"]
mod aggregation;
pub use aggregation::{FragmentExcerpt, ParentExcerpt, ParentReference, ReductionStep};

trait DbResult<T> {
    fn api(self) -> ApiResult<T>;
}
impl<T> DbResult<T> for rusqlite::Result<T> {
    fn api(self) -> ApiResult<T> {
        self.map_err(|_| ApiError::internal())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Note {
    pub content: String,
    pub source_conversation_id: Option<String>,
    #[serde(default = "private")]
    pub visibility: String,
    #[serde(default)]
    pub publish: bool,
    pub happened_at: Option<i64>,
}
fn private() -> String {
    "private".into()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePolicy {
    pub source_owner_id: String,
    pub enabled: bool,
    #[serde(default)]
    pub allow_model: bool,
    #[serde(default = "default_timezone")]
    pub timezone: String,
}
fn default_timezone() -> String {
    TIMEZONE.into()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ListOptions {
    pub q: Option<String>,
    pub period: Option<String>,
    pub source_owner_id: Option<String>,
    pub visibility: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub limit: usize,
    pub offset: usize,
}
impl Default for ListOptions {
    fn default() -> Self {
        Self {
            q: None,
            period: None,
            source_owner_id: None,
            visibility: None,
            from: None,
            to: None,
            limit: 50,
            offset: 0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub assistant_id: String,
    pub revision: i64,
    pub visibility: String,
    pub source_owner_id: Option<String>,
    pub source_conversation_id: Option<String>,
    pub source_message_id: Option<String>,
    pub origin_type: String,
    pub origin_id: String,
    pub happened_at: i64,
    pub observed_at: i64,
    pub content: String,
    pub assertion_kind: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub assistant_id: String,
    pub revision: i64,
    pub period: String,
    pub period_start: String,
    pub period_end: String,
    pub timezone: String,
    pub visibility: String,
    pub source_owner_id: Option<String>,
    pub source_conversation_id: Option<String>,
    pub facts: String,
    pub feelings: String,
    pub content: String,
    pub status: String,
    pub generator: String,
    pub source_count: usize,
    pub observation_complete: bool,
    pub coverage_watermark: i64,
    pub deletion_epoch: i64,
    pub updated_at: i64,
}
#[derive(Debug, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: i64,
    pub limit: usize,
    pub offset: usize,
}

pub fn initialize(db: &mut Connection) -> anyhow::Result<()> {
    let tx = db.transaction()?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS diary_meta(id INTEGER PRIMARY KEY CHECK(id=1),schema_version INTEGER NOT NULL,deletion_epoch INTEGER NOT NULL,last_tick INTEGER NOT NULL,started_at INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS diary_source_policies(source_owner_id TEXT PRIMARY KEY,revision INTEGER NOT NULL,enabled INTEGER NOT NULL,allow_model INTEGER NOT NULL,timezone TEXT NOT NULL,authorized_at INTEGER NOT NULL,updated_at INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS diary_events(id TEXT PRIMARY KEY,assistant_id TEXT NOT NULL,revision INTEGER NOT NULL,visibility TEXT NOT NULL CHECK(visibility IN ('shared','private')),source_owner_id TEXT,source_conversation_id TEXT,source_message_id TEXT,origin_type TEXT NOT NULL,origin_id TEXT NOT NULL,happened_at INTEGER NOT NULL,observed_at INTEGER NOT NULL,content TEXT,assertion_kind TEXT NOT NULL,deleted_at INTEGER,CHECK(visibility='shared' OR source_owner_id IS NOT NULL),UNIQUE(assistant_id,origin_type,origin_id));
      CREATE INDEX IF NOT EXISTS diary_events_scope ON diary_events(assistant_id,visibility,source_owner_id,happened_at,deleted_at);
      CREATE TABLE IF NOT EXISTS diary_event_versions(event_id TEXT NOT NULL,revision INTEGER NOT NULL,content TEXT,actor_id TEXT NOT NULL,change_kind TEXT NOT NULL,created_at INTEGER NOT NULL,PRIMARY KEY(event_id,revision));
      CREATE TABLE IF NOT EXISTS diary_entries(id TEXT PRIMARY KEY,assistant_id TEXT NOT NULL,revision INTEGER NOT NULL,period TEXT NOT NULL,period_start TEXT NOT NULL,period_end TEXT NOT NULL,timezone TEXT NOT NULL,visibility TEXT NOT NULL,source_owner_id TEXT,source_conversation_id TEXT,facts TEXT NOT NULL,feelings TEXT NOT NULL,content TEXT NOT NULL,status TEXT NOT NULL,generator TEXT NOT NULL,source_count INTEGER NOT NULL,observation_complete INTEGER NOT NULL,coverage_watermark INTEGER NOT NULL,deletion_epoch INTEGER NOT NULL,source_fingerprint TEXT NOT NULL,updated_at INTEGER NOT NULL,deleted_at INTEGER,UNIQUE(assistant_id,period,period_start,timezone,visibility,source_owner_id));
      CREATE INDEX IF NOT EXISTS diary_entries_scope ON diary_entries(assistant_id,source_owner_id,period_start,deleted_at,status);
      CREATE TABLE IF NOT EXISTS diary_entry_versions(entry_id TEXT NOT NULL,revision INTEGER NOT NULL,facts TEXT,feelings TEXT,generator TEXT NOT NULL,actor_id TEXT NOT NULL,created_at INTEGER NOT NULL,PRIMARY KEY(entry_id,revision));
      CREATE TABLE IF NOT EXISTS diary_entry_sources(entry_id TEXT NOT NULL,event_id TEXT NOT NULL,event_revision INTEGER NOT NULL,PRIMARY KEY(entry_id,event_id));
      CREATE INDEX IF NOT EXISTS diary_source_derived ON diary_entry_sources(event_id,entry_id);
      CREATE TABLE IF NOT EXISTS diary_entry_parents(entry_id TEXT NOT NULL,parent_id TEXT NOT NULL,parent_revision INTEGER NOT NULL,PRIMARY KEY(entry_id,parent_id));
      CREATE TABLE IF NOT EXISTS diary_legacy_map(memory_id TEXT PRIMARY KEY,event_id TEXT NOT NULL UNIQUE,last_revision INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS diary_scope_checkpoints(scope_key TEXT PRIMARY KEY,visibility TEXT NOT NULL,source_owner_id TEXT,timezone TEXT NOT NULL,last_date TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS diary_observation_gaps(id INTEGER PRIMARY KEY AUTOINCREMENT,start_at INTEGER NOT NULL,end_at INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS diary_model_jobs(entry_id TEXT PRIMARY KEY,entry_revision INTEGER NOT NULL,status TEXT NOT NULL,attempts INTEGER NOT NULL DEFAULT 0,error TEXT,updated_at INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS diary_run_uses(run_id TEXT NOT NULL,entry_id TEXT NOT NULL,revision INTEGER NOT NULL,PRIMARY KEY(run_id,entry_id));
      CREATE TABLE IF NOT EXISTS diary_source_revocations(source_owner_id TEXT PRIMARY KEY,revoked_at INTEGER NOT NULL,epoch INTEGER NOT NULL,through_event_rowid INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS diary_capture_skips(run_id TEXT PRIMARY KEY,reason TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS diary_consolidation_blocks(scope_key TEXT NOT NULL,period TEXT NOT NULL,period_start TEXT NOT NULL,reason TEXT NOT NULL,updated_at INTEGER NOT NULL,PRIMARY KEY(scope_key,period,period_start));")?;
    tx.execute(
        "INSERT OR IGNORE INTO diary_meta VALUES(1,1,0,?1,?1)",
        [now()],
    )?;
    let version: i64 = tx.query_row(
        "SELECT schema_version FROM diary_meta WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    anyhow::ensure!((1..=2).contains(&version), "Unsupported diary schema");
    aggregation::initialize(&tx)?;
    tx.execute("UPDATE diary_meta SET schema_version=2 WHERE id=1", [])?;
    initialize_search(&tx)?;
    // A single legacy transaction is mirrored into the canonical ledger; the
    // old IDs remain compatibility projections until the migration gate closes.
    tx.execute_batch("DROP TRIGGER IF EXISTS diary_legacy_update;
      CREATE TRIGGER IF NOT EXISTS diary_legacy_insert AFTER INSERT ON memory_records BEGIN
      INSERT OR IGNORE INTO diary_events(id,assistant_id,revision,visibility,source_owner_id,source_conversation_id,source_message_id,origin_type,origin_id,happened_at,observed_at,content,assertion_kind,deleted_at) VALUES('legacy:'||new.id,new.assistant_id,new.revision,new.visibility,new.source_owner_id,new.source_conversation_id,new.source_message_id,'legacy_memory',new.id,new.created_at,new.updated_at,new.content,'self_report',new.deleted_at);
      INSERT OR REPLACE INTO diary_legacy_map VALUES(new.id,'legacy:'||new.id,new.revision);
      INSERT OR IGNORE INTO diary_scope_checkpoints(scope_key,visibility,source_owner_id,timezone,last_date) VALUES(new.visibility||':'||coalesce(new.source_owner_id,''),new.visibility,new.source_owner_id,'Asia/Shanghai',date(new.created_at,'unixepoch','+8 hours','-1 day'));
      END;
      CREATE TRIGGER IF NOT EXISTS diary_legacy_update AFTER UPDATE ON memory_records BEGIN
      UPDATE diary_events SET revision=new.revision,visibility=new.visibility,source_owner_id=new.source_owner_id,content=new.content,deleted_at=new.deleted_at,observed_at=new.updated_at WHERE id='legacy:'||new.id;
      UPDATE diary_meta SET deletion_epoch=deletion_epoch+1 WHERE id=1;
      UPDATE diary_entries SET facts='',feelings='',content='',status='invalidated',revision=revision+1,deletion_epoch=(SELECT deletion_epoch FROM diary_meta WHERE id=1) WHERE id IN (SELECT entry_id FROM diary_entry_sources WHERE event_id='legacy:'||new.id);
      UPDATE diary_entry_versions SET facts=NULL,feelings=NULL WHERE entry_id IN (SELECT entry_id FROM diary_entry_sources WHERE event_id='legacy:'||new.id);
      DELETE FROM diary_model_fragments WHERE entry_id IN (SELECT entry_id FROM diary_entry_sources WHERE event_id='legacy:'||new.id);
      DELETE FROM diary_model_reductions WHERE entry_id IN (SELECT entry_id FROM diary_entry_sources WHERE event_id='legacy:'||new.id);
      DELETE FROM diary_aggregation_coverage WHERE entry_id IN (SELECT entry_id FROM diary_entry_sources WHERE event_id='legacy:'||new.id);
      UPDATE diary_event_versions SET content=NULL WHERE event_id='legacy:'||new.id;
      UPDATE diary_legacy_map SET last_revision=new.revision WHERE memory_id=new.id;
      UPDATE diary_scope_checkpoints SET last_date=min(last_date,date(new.created_at,'unixepoch','+8 hours','-1 day')) WHERE scope_key=new.visibility||':'||coalesce(new.source_owner_id,'');
      END;")?;
    tx.execute("INSERT OR IGNORE INTO diary_events(id,assistant_id,revision,visibility,source_owner_id,source_conversation_id,source_message_id,origin_type,origin_id,happened_at,observed_at,content,assertion_kind,deleted_at) SELECT 'legacy:'||id,assistant_id,revision,visibility,source_owner_id,source_conversation_id,source_message_id,'legacy_memory',id,created_at,?1,content,'self_report',deleted_at FROM memory_records",[now()])?;
    tx.execute("INSERT OR IGNORE INTO diary_legacy_map SELECT id,'legacy:'||id,revision FROM memory_records",[])?;
    let assistant = memory::identity(&tx)
        .map_err(|_| anyhow::anyhow!("Diary assistant identity unavailable"))?;
    ensure_scope(
        &tx,
        "private",
        Some(&format!("assistant:{assistant}")),
        TIMEZONE,
        now(),
    )
    .map_err(|_| anyhow::anyhow!("Diary daily checkpoint unavailable"))?;
    let scopes = {
        let mut stmt=tx.prepare("SELECT visibility,source_owner_id,min(happened_at) FROM diary_events WHERE deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM diary_scope_checkpoints c WHERE c.scope_key=diary_events.visibility||':'||coalesce(diary_events.source_owner_id,'')) GROUP BY visibility,source_owner_id")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (visibility, owner, happened) in scopes {
        ensure_scope(&tx, &visibility, owner.as_deref(), TIMEZONE, happened)
            .map_err(|_| anyhow::anyhow!("Diary migration checkpoint unavailable"))?;
    }
    tx.execute("UPDATE diary_model_jobs SET status='queued',error='interrupted_before_commit' WHERE status='running'",[])?;
    tx.commit()?;
    Ok(())
}

fn initialize_search(db: &Connection) -> anyhow::Result<()> {
    for (table,fts,valid) in [
        ("diary_events","diary_event_fts","new.deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM diary_source_revocations v WHERE v.source_owner_id=new.source_owner_id AND new.rowid<=v.through_event_rowid)"),
        ("diary_entries","diary_entry_fts","new.deleted_at IS NULL AND new.status!='invalidated'"),
    ]{
        db.execute_batch(&format!("CREATE VIRTUAL TABLE IF NOT EXISTS {fts} USING fts5(content,assistant_token,visibility_token,owner_token,tokenize='trigram');
         CREATE TRIGGER IF NOT EXISTS {fts}_insert AFTER INSERT ON {table} WHEN {valid} BEGIN
          INSERT INTO {fts}(rowid,content,assistant_token,visibility_token,owner_token) VALUES(new.rowid,new.content,new.assistant_id,new.visibility,coalesce(new.source_owner_id,''));END;
         CREATE TRIGGER IF NOT EXISTS {fts}_update AFTER UPDATE ON {table} BEGIN
          DELETE FROM {fts} WHERE rowid=old.rowid;
          INSERT INTO {fts}(rowid,content,assistant_token,visibility_token,owner_token) SELECT new.rowid,new.content,new.assistant_id,new.visibility,coalesce(new.source_owner_id,'') WHERE {valid};END;
         CREATE TRIGGER IF NOT EXISTS {fts}_delete AFTER DELETE ON {table} BEGIN DELETE FROM {fts} WHERE rowid=old.rowid;END;"))?;
        db.execute(&format!("INSERT INTO {fts}({fts},rank) VALUES('secure-delete',1)"),[])?;
        let valid_existing=valid.replace("new.","d.");
        db.execute_batch(&format!("INSERT INTO {fts}(rowid,content,assistant_token,visibility_token,owner_token) SELECT d.rowid,d.content,d.assistant_id,d.visibility,coalesce(d.source_owner_id,'') FROM {table} d WHERE {valid_existing} AND NOT EXISTS(SELECT 1 FROM {fts} f WHERE f.rowid=d.rowid);"))?;
    }
    Ok(())
}

fn search_clause(
    fts: &str,
    query: &str,
    assistant: &str,
    owner: Option<&str>,
    parameter: usize,
) -> (String, String) {
    if query.chars().count() < 3 {
        return (
            format!("(?{parameter}='' OR instr(content,?{parameter})>0)"),
            query.into(),
        );
    }
    fn quote(s: &str) -> String {
        format!("\"{}\"", s.replace('"', "\"\""))
    }
    let mut expression = format!(
        "content : {} AND assistant_token : {}",
        quote(query),
        quote(assistant)
    );
    if let Some(owner) = owner.filter(|v| v.chars().count() >= 3) {
        expression.push_str(&format!(
            " AND (visibility_token : \"shared\" OR owner_token : {})",
            quote(owner)
        ));
    }
    (
        format!("rowid IN (SELECT rowid FROM {fts} WHERE {fts} MATCH ?{parameter})"),
        expression,
    )
}

fn event_row(r: &Row<'_>) -> rusqlite::Result<Event> {
    Ok(Event {
        id: r.get(0)?,
        assistant_id: r.get(1)?,
        revision: r.get(2)?,
        visibility: r.get(3)?,
        source_owner_id: r.get(4)?,
        source_conversation_id: r.get(5)?,
        source_message_id: r.get(6)?,
        origin_type: r.get(7)?,
        origin_id: r.get(8)?,
        happened_at: r.get(9)?,
        observed_at: r.get(10)?,
        content: r.get(11)?,
        assertion_kind: r.get(12)?,
    })
}
const EVENT_COLUMNS:&str="id,assistant_id,revision,visibility,source_owner_id,source_conversation_id,source_message_id,origin_type,origin_id,happened_at,observed_at,coalesce(content,''),assertion_kind";
fn entry_row(r: &Row<'_>) -> rusqlite::Result<Entry> {
    Ok(Entry {
        id: r.get(0)?,
        assistant_id: r.get(1)?,
        revision: r.get(2)?,
        period: r.get(3)?,
        period_start: r.get(4)?,
        period_end: r.get(5)?,
        timezone: r.get(6)?,
        visibility: r.get(7)?,
        source_owner_id: r.get(8)?,
        source_conversation_id: r.get(9)?,
        facts: r.get(10)?,
        feelings: r.get(11)?,
        content: r.get(12)?,
        status: r.get(13)?,
        generator: r.get(14)?,
        source_count: r.get::<_, i64>(15)? as usize,
        observation_complete: r.get(16)?,
        coverage_watermark: r.get(17)?,
        deletion_epoch: r.get(18)?,
        updated_at: r.get(19)?,
    })
}
const ENTRY_COLUMNS:&str="id,assistant_id,revision,period,period_start,period_end,timezone,visibility,source_owner_id,source_conversation_id,facts,feelings,content,status,generator,source_count,observation_complete,coverage_watermark,deletion_epoch,updated_at";

fn valid_visibility(v: &str) -> ApiResult<()> {
    if matches!(v, "shared" | "private") {
        Ok(())
    } else {
        Err(ApiError::bad(
            "invalid_visibility",
            "可见范围必须为shared或private",
        ))
    }
}
fn validate_text(s: &str) -> ApiResult<()> {
    if s.trim().is_empty() || s.len() > MAX_CONTENT {
        return Err(ApiError::bad(
            "invalid_diary_content",
            "正文不能为空且不得超过12000字节",
        ));
    }
    if sensitive(s) {
        return Err(ApiError::bad(
            "sensitive_diary_content",
            "凭证、验证码与密钥内容不进入长期日记",
        ));
    }
    Ok(())
}
fn sensitive(s: &str) -> bool {
    crate::conversation_summary::contains_sensitive(s)
}
fn date(value: &str) -> ApiResult<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| ApiError::bad("invalid_date", "日期必须为YYYY-MM-DD"))
}
fn tz(value: &str) -> ApiResult<Tz> {
    value
        .parse()
        .map_err(|_| ApiError::bad("invalid_timezone", "时区必须为有效IANA时区"))
}
fn local_date(ts: i64, zone: Tz) -> ApiResult<NaiveDate> {
    Utc.timestamp_opt(ts, 0)
        .single()
        .map(|v| v.with_timezone(&zone).date_naive())
        .ok_or_else(|| ApiError::bad("invalid_timestamp", "时间超出范围"))
}
fn boundary(day: NaiveDate, zone: Tz) -> ApiResult<i64> {
    zone.from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .map(|v| v.timestamp())
        .ok_or_else(|| ApiError::bad("invalid_local_day", "时区日期边界不可用"))
}
fn audit(db: &Connection, actor: &str, action: &str, id: &str) -> ApiResult<()> {
    db.execute(
        "INSERT INTO audit_events(actor,action,resource,created_at) VALUES(?1,?2,?3,?4)",
        params![actor, action, id, now()],
    )
    .api()?;
    Ok(())
}
fn owner_scope(db: &Connection, actor: &str, input: &Note) -> ApiResult<Option<String>> {
    valid_visibility(&input.visibility)?;
    if input.visibility == "shared" && !input.publish {
        return Err(ApiError::bad(
            "publish_confirmation_required",
            "共享日记事件须明确确认发布",
        ));
    }
    if let Some(conversation) = &input.source_conversation_id {
        let owner: Option<String> = db
            .query_row(
                "SELECT owner_id FROM conversations WHERE id=?1",
                [conversation],
                |r| r.get(0),
            )
            .optional()
            .api()?;
        return owner.map(Some).ok_or_else(ApiError::not_found);
    }
    Ok(if input.visibility == "private" {
        Some(actor.into())
    } else {
        None
    })
}

pub fn note(db: &mut Connection, actor: &str, input: &Note) -> ApiResult<Event> {
    let tx = db.transaction().api()?;
    let item = note_in(&tx, actor, input)?;
    tx.commit().api()?;
    Ok(item)
}
pub fn note_in(db: &Connection, actor: &str, input: &Note) -> ApiResult<Event> {
    validate_text(&input.content)?;
    let owner = owner_scope(db, actor, input)?;
    let timestamp = now();
    let happened = input.happened_at.unwrap_or(timestamp);
    if happened < 0 || happened > timestamp + 300 {
        return Err(ApiError::bad(
            "invalid_event_time",
            "事件时间不可为未来时间",
        ));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let assistant = memory::identity(db)?;
    db.execute("INSERT INTO diary_events(id,assistant_id,revision,visibility,source_owner_id,source_conversation_id,origin_type,origin_id,happened_at,observed_at,content,assertion_kind) VALUES(?1,?2,1,?3,?4,?5,'explicit_note',?1,?6,?7,?8,'self_report')",params![id,assistant,input.visibility,owner,input.source_conversation_id,happened,timestamp,input.content]).api()?;
    db.execute(
        "INSERT INTO diary_event_versions VALUES(?1,1,?2,?3,'created',?4)",
        params![id, input.content, actor, timestamp],
    )
    .api()?;
    dirty_periods_in(db, &input.visibility, owner.as_deref(), happened)?;
    audit(db, actor, "diary.note", &id)?;
    event(db, &id, None)
}

pub fn event(db: &Connection, id: &str, owner: Option<&str>) -> ApiResult<Event> {
    let item=db.query_row(&format!("SELECT {EVENT_COLUMNS} FROM diary_events WHERE id=?1 AND assistant_id=?2 AND deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM diary_source_revocations v WHERE v.source_owner_id=diary_events.source_owner_id AND diary_events.rowid<=v.through_event_rowid) AND (?3 IS NULL OR visibility='shared' OR source_owner_id=?3)"),params![id,memory::identity(db)?,owner],event_row).optional().api()?.ok_or_else(ApiError::not_found)?;
    Ok(item)
}
pub fn events(
    db: &Connection,
    options: &ListOptions,
    owner: Option<&str>,
) -> ApiResult<Page<Event>> {
    let limit = options.limit.clamp(1, 100);
    let offset = options.offset.min(100000);
    let assistant = memory::identity(db)?;
    let query = options.q.as_deref().unwrap_or("");
    if query.len() > 200 {
        return Err(ApiError::bad("query_too_long", "搜索词过长"));
    }
    let (search, query) = search_clause("diary_event_fts", query, &assistant, owner, 5);
    let sql=format!("FROM diary_events WHERE assistant_id=?1 AND deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM diary_source_revocations v WHERE v.source_owner_id=diary_events.source_owner_id AND diary_events.rowid<=v.through_event_rowid) AND (?2 IS NULL OR visibility='shared' OR source_owner_id=?2) AND (?3 IS NULL OR source_owner_id=?3) AND (?4 IS NULL OR visibility=?4) AND {search}");
    let values = params![
        assistant,
        owner,
        options.source_owner_id,
        options.visibility,
        query
    ];
    let total = db
        .query_row(&format!("SELECT count(*) {sql}"), values, |r| r.get(0))
        .api()?;
    let mut stmt = db
        .prepare(&format!(
            "SELECT {EVENT_COLUMNS} {sql} ORDER BY happened_at DESC,id LIMIT ?6 OFFSET ?7"
        ))
        .api()?;
    let rows = stmt
        .query_map(
            params![
                assistant,
                owner,
                options.source_owner_id,
                options.visibility,
                query,
                limit as i64,
                offset as i64
            ],
            event_row,
        )
        .api()?;
    let items = rows.collect::<rusqlite::Result<Vec<_>>>().api()?;
    Ok(Page {
        items,
        total,
        limit,
        offset,
    })
}

pub fn entry(db: &Connection, id: &str, owner: Option<&str>) -> ApiResult<Entry> {
    db.query_row(&format!("SELECT {ENTRY_COLUMNS} FROM diary_entries WHERE id=?1 AND assistant_id=?2 AND deleted_at IS NULL AND status!='invalidated' AND (?3 IS NULL OR visibility='shared' OR source_owner_id=?3)"),params![id,memory::identity(db)?,owner],entry_row).optional().api()?.ok_or_else(ApiError::not_found)
}
pub fn list(db: &Connection, options: &ListOptions, owner: Option<&str>) -> ApiResult<Page<Entry>> {
    if let Some(p) = &options.period {
        if !matches!(p.as_str(), "day" | "week" | "month") {
            return Err(ApiError::bad("invalid_period", "周期为day/week/month"));
        }
    }
    if let Some(v) = &options.visibility {
        valid_visibility(v)?;
    }
    for d in [&options.from, &options.to].into_iter().flatten() {
        date(d)?;
    }
    let limit = options.limit.clamp(1, 100);
    let offset = options.offset.min(100000);
    let query = options.q.as_deref().unwrap_or("");
    if query.len() > 200 {
        return Err(ApiError::bad("query_too_long", "搜索词过长"));
    }
    let assistant = memory::identity(db)?;
    let (search, query) = search_clause("diary_entry_fts", query, &assistant, owner, 8);
    let sql=format!("FROM diary_entries WHERE assistant_id=?1 AND deleted_at IS NULL AND status!='invalidated' AND (?2 IS NULL OR visibility='shared' OR source_owner_id=?2) AND (?3 IS NULL OR source_owner_id=?3) AND (?4 IS NULL OR visibility=?4) AND (?5 IS NULL OR period=?5) AND (?6 IS NULL OR period_end>?6) AND (?7 IS NULL OR period_start<=?7) AND (?8='' OR {})",search);
    let total = db
        .query_row(
            &format!("SELECT count(*) {sql}"),
            params![
                assistant,
                owner,
                options.source_owner_id,
                options.visibility,
                options.period,
                options.from,
                options.to,
                query
            ],
            |r| r.get(0),
        )
        .api()?;
    let mut stmt=db.prepare(&format!("SELECT {ENTRY_COLUMNS} {sql} ORDER BY period_start DESC,updated_at DESC,id LIMIT ?9 OFFSET ?10")).api()?;
    let rows = stmt
        .query_map(
            params![
                assistant,
                owner,
                options.source_owner_id,
                options.visibility,
                options.period,
                options.from,
                options.to,
                query,
                limit as i64,
                offset as i64
            ],
            entry_row,
        )
        .api()?;
    let items = rows.collect::<rusqlite::Result<Vec<_>>>().api()?;
    Ok(Page {
        items,
        total,
        limit,
        offset,
    })
}
pub fn sources(db: &Connection, id: &str, owner: Option<&str>) -> ApiResult<Vec<Event>> {
    Ok(source_page(db, id, owner, MAX_EVENTS, 0)?.items)
}
#[derive(Debug, Serialize)]
pub struct SourcePage {
    pub items: Vec<Event>,
    pub total: i64,
    pub limit: usize,
    pub offset: usize,
    pub has_more: bool,
}
pub fn source_page(
    db: &Connection,
    id: &str,
    owner: Option<&str>,
    limit: usize,
    offset: usize,
) -> ApiResult<SourcePage> {
    entry(db, id, owner)?;
    let limit = limit.clamp(1, MAX_EVENTS);
    let offset = offset.min(100000);
    let scope="FROM diary_events WHERE id IN (SELECT event_id FROM diary_entry_sources WHERE entry_id=?1) AND deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM diary_source_revocations v WHERE v.source_owner_id=diary_events.source_owner_id AND diary_events.rowid<=v.through_event_rowid) AND (?2 IS NULL OR visibility='shared' OR source_owner_id=?2)";
    let total: i64 = db
        .query_row(
            &format!("SELECT count(*) {scope}"),
            params![id, owner],
            |r| r.get(0),
        )
        .api()?;
    let mut stmt = db
        .prepare(&format!(
            "SELECT {EVENT_COLUMNS} {scope} ORDER BY happened_at,id LIMIT ?3 OFFSET ?4"
        ))
        .api()?;
    let rows = stmt
        .query_map(params![id, owner, limit as i64, offset as i64], event_row)
        .api()?;
    let items = rows.collect::<rusqlite::Result<Vec<_>>>().api()?;
    Ok(SourcePage {
        items,
        total,
        limit,
        offset,
        has_more: offset + limit < total as usize,
    })
}

pub fn correct(
    db: &mut Connection,
    actor: &str,
    id: &str,
    expected: i64,
    content: &str,
) -> ApiResult<Event> {
    validate_text(content)?;
    let tx = db.transaction().api()?;
    let current = event(&tx, id, None)?;
    if current.revision != expected {
        return Err(ApiError::conflict());
    }
    if current.origin_type == "legacy_memory" {
        memory::update_in(
            &tx,
            memory::Access::Admin(actor),
            &current.origin_id,
            expected,
            &memory::UpdateMemory {
                content: content.into(),
                visibility: None,
                publish: false,
            },
        )?;
        let result = event(&tx, id, None)?;
        tx.commit().api()?;
        return Ok(result);
    }
    invalidate_event_in(&tx, id)?;
    tx.execute("UPDATE diary_events SET content=?1,revision=revision+1,observed_at=?2 WHERE id=?3 AND revision=?4",params![content,now(),id,expected]).api()?;
    tx.execute(
        "UPDATE diary_event_versions SET content=NULL WHERE event_id=?1",
        [id],
    )
    .api()?;
    tx.execute(
        "INSERT INTO diary_event_versions VALUES(?1,?2,?3,?4,'corrected',?5)",
        params![id, expected + 1, content, actor, now()],
    )
    .api()?;
    dirty_periods_in(
        &tx,
        &current.visibility,
        current.source_owner_id.as_deref(),
        current.happened_at,
    )?;
    audit(&tx, actor, "diary.correct", id)?;
    let result = event(&tx, id, None)?;
    tx.commit().api()?;
    Ok(result)
}
pub fn forget(db: &mut Connection, actor: &str, id: &str, expected: i64) -> ApiResult<Value> {
    let tx = db.transaction().api()?;
    let current = event(&tx, id, None)?;
    if current.revision != expected {
        return Err(ApiError::conflict());
    }
    if current.origin_type == "legacy_memory" {
        let result = memory::forget_in(
            &tx,
            memory::Access::Admin(actor),
            &current.origin_id,
            expected,
        )?;
        let value = serde_json::to_value(result).map_err(|_| ApiError::internal())?;
        tx.commit().api()?;
        return Ok(value);
    }
    invalidate_event_in(&tx, id)?;
    tx.execute(
        "UPDATE diary_events SET content=NULL,deleted_at=?1,revision=revision+1 WHERE id=?2",
        params![now(), id],
    )
    .api()?;
    tx.execute(
        "UPDATE diary_event_versions SET content=NULL WHERE event_id=?1",
        [id],
    )
    .api()?;
    ensure_scope(
        &tx,
        &current.visibility,
        current.source_owner_id.as_deref(),
        &source_timezone(&tx, current.source_owner_id.as_deref())?,
        current.happened_at,
    )?;
    audit(&tx, actor, "diary.forget", id)?;
    let epoch: i64 = tx
        .query_row(
            "SELECT deletion_epoch FROM diary_meta WHERE id=1",
            [],
            |r| r.get(0),
        )
        .api()?;
    tx.commit().api()?;
    Ok(
        json!({"id":id,"revision":expected+1,"deleted":true,"deletion_epoch":epoch,"cleanup":{"live_diary":"cleared","derived_entries":"invalidated","original_messages":"separate_retention","offline_backups":"replay_tombstones_before_restore"}}),
    )
}

/// Called before changing/deleting source content, in the same transaction.
pub fn invalidate_event_in(db: &Connection, id: &str) -> ApiResult<()> {
    let current = event(db, id, None)?;
    db.execute(
        "UPDATE diary_meta SET deletion_epoch=deletion_epoch+1 WHERE id=1",
        [],
    )
    .api()?;
    let derived = {
        let mut stmt = db
            .prepare("SELECT entry_id FROM diary_entry_sources WHERE event_id=?1")
            .api()?;
        let rows = stmt.query_map([id], |r| r.get::<_, String>(0)).api()?;
        rows.collect::<rusqlite::Result<Vec<_>>>().api()?
    };
    for entry_id in derived {
        invalidate_entry_in(db, &entry_id)?;
    }
    crate::relationships::invalidate_source_in(db, "diary_event", id)
        .map_err(|_| ApiError::internal())?;
    invalidate_contexts_in(
        db,
        if current.visibility == "shared" {
            None
        } else {
            current.source_owner_id.as_deref()
        },
    )?;
    Ok(())
}
fn invalidate_entry_in(db: &Connection, id: &str) -> ApiResult<()> {
    aggregation::clear(db, id)?;
    db.execute("UPDATE diary_entries SET facts='',feelings='',content='',status='invalidated',revision=revision+1,deletion_epoch=(SELECT deletion_epoch FROM diary_meta WHERE id=1),updated_at=?1 WHERE id=?2",params![now(),id]).api()?;
    db.execute(
        "UPDATE diary_entry_versions SET facts=NULL,feelings=NULL WHERE entry_id=?1",
        [id],
    )
    .api()?;
    db.execute("DELETE FROM diary_model_jobs WHERE entry_id=?1", [id])
        .api()?;
    crate::relationships::invalidate_source_in(db, "diary_entry", id)
        .map_err(|_| ApiError::internal())?;
    Ok(())
}
/// A conservative owner-domain invalidation also covers copied earlier answers.
pub fn invalidate_contexts_in(db: &Connection, owner: Option<&str>) -> ApiResult<()> {
    let mut stmt = db
        .prepare("SELECT id FROM conversations WHERE ?1 IS NULL OR owner_id=?1")
        .api()?;
    let conversations = stmt
        .query_map([owner], |r| r.get::<_, String>(0))
        .api()?
        .collect::<rusqlite::Result<Vec<_>>>()
        .api()?;
    drop(stmt);
    for c in conversations {
        let floor: i64 = db
            .query_row(
                "SELECT coalesce(max(rowid),0) FROM runs WHERE conversation_id=?1",
                [&c],
                |r| r.get(0),
            )
            .api()?;
        db.execute("INSERT INTO memory_context_watermarks(conversation_id,assistant_id,through_run_rowid,updated_at) VALUES(?1,?2,?3,?4) ON CONFLICT(conversation_id) DO UPDATE SET through_run_rowid=max(through_run_rowid,excluded.through_run_rowid),updated_at=excluded.updated_at",params![c,memory::identity(db)?,floor,now()]).api()?;
        crate::conversation_summary::invalidate(db, &memory::identity(db)?, &c)
            .map_err(|_| ApiError::internal())?;
        crate::memory_candidates::invalidate(db, &memory::identity(db)?, &c)
            .map_err(|_| ApiError::internal())?;
        crate::conversation_images::invalidate_in(db, &c).map_err(|_| ApiError::internal())?;
        crate::weixin_store::cancel_queued_for_conversation_in(db, &c)
            .map_err(|_| ApiError::internal())?;
        db.execute("UPDATE runs SET status='cancelled',error='日记来源已更正或遗忘，旧上下文已失效',updated_at=?1 WHERE conversation_id=?2 AND status='running' AND run_kind IN ('model','skill_text')",params![now(),c]).api()?;
        db.execute("UPDATE outbox SET status='cancelled',error='diary_source_invalidated' WHERE status='queued' AND run_id IN (SELECT id FROM runs WHERE conversation_id=?1)",[&c]).api()?;
        db.execute("DELETE FROM run_prompt_snapshots WHERE run_id IN (SELECT id FROM runs WHERE conversation_id=?1 AND rowid<=?2)",params![c,floor]).api()?;
    }
    Ok(())
}

fn scope_key(visibility: &str, owner: Option<&str>) -> String {
    format!("{visibility}:{}", owner.unwrap_or(""))
}
fn source_timezone(db: &Connection, owner: Option<&str>) -> ApiResult<String> {
    Ok(db
        .query_row(
            "SELECT timezone FROM diary_source_policies WHERE source_owner_id=?1",
            [owner],
            |r| r.get(0),
        )
        .optional()
        .api()?
        .unwrap_or_else(|| TIMEZONE.into()))
}
fn ensure_scope(
    db: &Connection,
    visibility: &str,
    owner: Option<&str>,
    timezone: &str,
    timestamp: i64,
) -> ApiResult<()> {
    let initial = local_date(timestamp, tz(timezone)?)? - Duration::days(1);
    db.execute("INSERT INTO diary_scope_checkpoints VALUES(?1,?2,?3,?4,?5) ON CONFLICT(scope_key) DO UPDATE SET last_date=min(last_date,excluded.last_date)",params![scope_key(visibility,owner),visibility,owner,timezone,initial.to_string()]).api()?;
    Ok(())
}
fn dirty_periods_in(
    db: &Connection,
    visibility: &str,
    owner: Option<&str>,
    happened: i64,
) -> ApiResult<()> {
    let mut stmt=db.prepare("SELECT id,timezone,period_start,period_end FROM diary_entries WHERE visibility=?1 AND source_owner_id IS ?2 AND deleted_at IS NULL").api()?;
    let items = stmt
        .query_map(params![visibility, owner], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .api()?
        .collect::<rusqlite::Result<Vec<_>>>()
        .api()?;
    drop(stmt);
    for (id, timezone, start, end) in items {
        let day = local_date(happened, tz(&timezone)?)?.to_string();
        if day >= start && day < end {
            invalidate_entry_in(db, &id)?;
        }
    }
    ensure_scope(
        db,
        visibility,
        owner,
        &source_timezone(db, owner)?,
        happened,
    )
}

pub fn policies(db: &Connection) -> ApiResult<Value> {
    let mut stmt=db.prepare("SELECT source_owner_id,revision,enabled,allow_model,timezone,authorized_at,updated_at FROM diary_source_policies ORDER BY source_owner_id").api()?;
    let items=stmt.query_map([],|r|Ok(json!({"source_owner_id":r.get::<_,String>(0)?,"revision":r.get::<_,i64>(1)?,"enabled":r.get::<_,bool>(2)?,"allow_model":r.get::<_,bool>(3)?,"timezone":r.get::<_,String>(4)?,"authorized_at":r.get::<_,i64>(5)?,"updated_at":r.get::<_,i64>(6)?}))).api()?.collect::<rusqlite::Result<Vec<_>>>().api()?;
    Ok(
        json!({"items":items,"default":"disabled","migration":"legacy_memory_compatibility_projection","automatic_model_concurrency":1}),
    )
}
pub fn save_policy(
    db: &mut Connection,
    actor: &str,
    expected: i64,
    input: &SourcePolicy,
) -> ApiResult<Value> {
    tz(&input.timezone)?;
    if input.source_owner_id.trim().is_empty() || input.source_owner_id.len() > 256 {
        return Err(ApiError::bad("invalid_source_owner", "来源身份无效"));
    }
    let tx = db.transaction().api()?;
    let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM conversations WHERE owner_id=?1) OR EXISTS(SELECT 1 FROM admins WHERE id=?1)",[&input.source_owner_id],|r|r.get(0)).api()?;
    if !exists {
        return Err(ApiError::bad(
            "unknown_source_owner",
            "来源必须是现有会话身份或管理身份",
        ));
    }
    let current: Option<(i64, String)> = tx
        .query_row(
            "SELECT revision,timezone FROM diary_source_policies WHERE source_owner_id=?1",
            [&input.source_owner_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .api()?;
    if current.as_ref().map(|v| v.0).unwrap_or(0) != expected {
        return Err(ApiError::conflict());
    }
    if current.as_ref().is_some_and(|v| v.1 != input.timezone) {
        return Err(ApiError::bad(
            "timezone_migration_required",
            "已有来源改变时区须先显式迁移日记周期",
        ));
    }
    tx.execute("INSERT INTO diary_source_policies VALUES(?1,1,?2,?3,?4,?5,?5) ON CONFLICT(source_owner_id) DO UPDATE SET revision=revision+1,enabled=excluded.enabled,allow_model=excluded.allow_model,updated_at=excluded.updated_at",params![input.source_owner_id,input.enabled,input.allow_model,input.timezone,now()]).api()?;
    if input.enabled {
        ensure_scope(
            &tx,
            "private",
            Some(&input.source_owner_id),
            &input.timezone,
            now(),
        )?;
    } else {
        tx.execute(
            "UPDATE diary_meta SET deletion_epoch=deletion_epoch+1 WHERE id=1",
            [],
        )
        .api()?;
        tx.execute("INSERT INTO diary_source_revocations SELECT ?1,?2,deletion_epoch,(SELECT coalesce(max(rowid),0) FROM diary_events WHERE source_owner_id=?1) FROM diary_meta WHERE id=1 ON CONFLICT(source_owner_id) DO UPDATE SET revoked_at=excluded.revoked_at,epoch=excluded.epoch,through_event_rowid=max(through_event_rowid,excluded.through_event_rowid)",params![input.source_owner_id,now()]).api()?;
        tx.execute("DELETE FROM diary_event_fts WHERE rowid IN (SELECT rowid FROM diary_events WHERE source_owner_id=?1)",[&input.source_owner_id]).api()?;
        let event_ids = {
            let mut st = tx
                .prepare("SELECT id FROM diary_events WHERE source_owner_id=?1")
                .api()?;
            let rows = st
                .query_map([&input.source_owner_id], |r| r.get::<_, String>(0))
                .api()?;
            rows.collect::<rusqlite::Result<Vec<_>>>().api()?
        };
        for id in event_ids {
            crate::relationships::invalidate_source_in(&tx, "diary_event", &id)
                .map_err(|_| ApiError::internal())?;
        }
        let ids = {
            let mut s = tx
                .prepare(
                    "SELECT id FROM diary_entries WHERE source_owner_id=?1 AND deleted_at IS NULL",
                )
                .api()?;
            let rows = s
                .query_map([&input.source_owner_id], |r| r.get::<_, String>(0))
                .api()?;
            rows.collect::<rusqlite::Result<Vec<_>>>().api()?
        };
        for id in ids {
            invalidate_entry_in(&tx, &id)?;
        }
        let shared:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM diary_events WHERE source_owner_id=?1 AND visibility='shared')",[&input.source_owner_id],|r|r.get(0)).api()?;
        invalidate_contexts_in(
            &tx,
            if shared {
                None
            } else {
                Some(&input.source_owner_id)
            },
        )?;
    }
    audit(&tx, actor, "diary.source_policy", &input.source_owner_id)?;
    tx.commit().api()?;
    Ok(
        json!({"source_owner_id":input.source_owner_id,"revision":expected+1,"enabled":input.enabled,"allow_model":input.allow_model,"applied":true}),
    )
}

/// Install after delivery_context::ensure_schema, before serving requests. The
/// one-time legacy pass completes in bounded transactions before readiness, so
/// an old generated candidate cannot remain a usable diary fact during upgrade.
pub fn initialize_delivery_capture(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS diary_capture_delivery_state(
      run_id TEXT PRIMARY KEY,event_id TEXT NOT NULL UNIQUE,event_revision INTEGER NOT NULL,
      delivery_revision INTEGER NOT NULL,projection_digest TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS diary_capture_dirty(run_id TEXT PRIMARY KEY,revision INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS diary_capture_migrations(key TEXT PRIMARY KEY,value INTEGER NOT NULL);")?;
    for action in ["INSERT", "UPDATE"] {
        db.execute_batch(&format!("CREATE TRIGGER IF NOT EXISTS diary_delivery_dirty_{} AFTER {action} ON delivery_context_revisions
          WHEN EXISTS(SELECT 1 FROM diary_events e WHERE e.origin_type='conversation_run' AND e.origin_id=new.run_id)
          BEGIN INSERT INTO diary_capture_dirty VALUES(new.run_id,new.revision)
          ON CONFLICT(run_id) DO UPDATE SET revision=max(revision,excluded.revision);END;",action.to_ascii_lowercase()))?;
    }
    let done: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM diary_capture_migrations WHERE key='receipt_projection_v1' AND value=1)",[],|r|r.get(0))?;
    if done {
        return Ok(());
    }
    anyhow::ensure!(
        db.is_autocommit(),
        "diary delivery migration must precede serving requests"
    );
    let mut after = 0i64;
    loop {
        let tx = db.unchecked_transaction()?;
        let rows = {
            let mut statement=tx.prepare("SELECT rowid,id,origin_id FROM diary_events e WHERE origin_type='conversation_run' AND rowid>?1 AND NOT EXISTS(SELECT 1 FROM diary_capture_delivery_state s WHERE s.event_id=e.id) ORDER BY rowid LIMIT 64")?;
            let rows = statement
                .query_map([after], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        if rows.is_empty() {
            tx.execute(
                "INSERT OR REPLACE INTO diary_capture_migrations VALUES('receipt_projection_v1',1)",
                [],
            )?;
            tx.commit()?;
            break;
        }
        for (rowid, event_id, run) in rows {
            let manually_corrected:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM diary_event_versions WHERE event_id=?1 AND change_kind='corrected')",[&event_id],|r|r.get(0))?;
            if manually_corrected {
                // Human-authored corrections are already explicit evidence;
                // receipt updates cannot replace them with an automatic recap.
                tx.execute("INSERT OR IGNORE INTO diary_capture_delivery_state SELECT ?1,id,revision,-1,'manual_override' FROM diary_events WHERE id=?2",params![run,event_id])?;
                after = rowid;
                continue;
            }
            if !capture_completed_in(&tx, &run)
                .map_err(|_| anyhow::anyhow!("legacy diary delivery projection failed"))?
            {
                // Tombstones and revoked events stay inaccessible. An active
                // legacy record lacking an eligible source loses its derived
                // candidate text; original messages are retained separately.
                if let Ok(current) = event(&tx, &event_id, None) {
                    if !tx.query_row("SELECT EXISTS(SELECT 1 FROM diary_capture_delivery_state WHERE event_id=?1)",[&event_id],|r|r.get::<_,bool>(0))? {
                        invalidate_event_in(&tx,&event_id).map_err(|_|anyhow::anyhow!("legacy diary invalidation failed"))?;
                        tx.execute("UPDATE diary_events SET content='旧自动采集记录缺少有效投递或来源证明；原始用户消息单独保留。',revision=revision+1 WHERE id=?1",[&event_id])?;
                        tx.execute("UPDATE diary_event_versions SET content=NULL WHERE event_id=?1",[&event_id])?;
                        tx.execute("INSERT OR IGNORE INTO diary_capture_skips VALUES(?1,'legacy_capture_source_unavailable')",[&run])?;
                        tx.execute("INSERT OR IGNORE INTO diary_capture_delivery_state VALUES(?1,?2,?3,-1,'unavailable')",params![run,event_id,current.revision+1])?;
                    }
                }
            }
            after = rowid;
        }
        tx.commit()?;
    }
    Ok(())
}

fn capture_content(projection: &crate::delivery_context::RunProjection) -> String {
    let delivery = &projection.delivery;
    let evidence = if delivery.local_completion {
        "Web/CLI结果已在本地可用，未确认有人阅读。".to_string()
    } else if delivery.parts.is_empty() {
        "尚无渠道投递证明；不据此断言已发送或从未尝试发送。".to_string()
    } else {
        let statuses = delivery
            .parts
            .iter()
            .map(|part| {
                format!(
                    "{}:{}:outbox={}:{}",
                    part.kind,
                    part.status,
                    part.outbox_status,
                    if part.platform_accepted {
                        "平台已接受"
                    } else {
                        "未获接受证明"
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("；");
        format!(
            "{statuses}。结果未知不能当作未发出；平台接受不证明已读、已播放或媒体内容已被理解。"
        )
    };
    format!(
        "对方：{}\n我的已证实文本：{}\n投递说明：{}",
        clip(&projection.user_content, 4000),
        projection
            .assistant_content
            .as_deref()
            .map(|s| clip(s, 4000))
            .unwrap_or_else(|| "无可引用的已证实文本；媒体或未知结果另见投递证据。".into()),
        evidence
    )
}

fn invalidate_capture_event_in(db: &Connection, id: &str, legacy: bool) -> ApiResult<()> {
    let derived:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM diary_entry_sources WHERE event_id=?1) OR EXISTS(SELECT 1 FROM relationship_assertions WHERE source_type='diary_event' AND source_id=?1 AND deleted_at IS NULL) OR EXISTS(SELECT 1 FROM relationship_edges WHERE source_type='diary_event' AND source_id=?1 AND deleted_at IS NULL) OR EXISTS(SELECT 1 FROM relationship_affinity_events WHERE source_type='diary_event' AND source_id=?1 AND deleted_at IS NULL)",[id],|r|r.get(0)).api()?;
    if legacy || derived {
        invalidate_event_in(db, id)
    } else {
        crate::relationships::invalidate_source_in(db, "diary_event", id)
            .map_err(|_| ApiError::internal())
    }
}

/// Capture authorized completed conversations using actual delivery evidence.
/// Receipt changes may revise an existing automatic event, never a tombstone,
/// revoked source, or a later human correction.
pub fn capture_completed_in(db: &Connection, run_id: &str) -> ApiResult<bool> {
    if db.is_autocommit() {
        let tx = db.unchecked_transaction().api()?;
        let result = capture_completed_in(&tx, run_id)?;
        tx.commit().api()?;
        return Ok(result);
    }
    let row:Option<(String,String,i64,Option<String>)>=db.query_row("SELECT r.owner_id,r.conversation_id,r.created_at,(SELECT id FROM messages WHERE run_id=r.id AND role='user') FROM runs r JOIN diary_source_policies p ON p.source_owner_id=r.owner_id WHERE r.id=?1 AND r.status='completed' AND r.run_kind IN ('model','skill_text') AND p.enabled=1 AND r.created_at>=p.authorized_at AND NOT EXISTS(SELECT 1 FROM diary_source_revocations v WHERE v.source_owner_id=r.owner_id AND r.created_at<=v.revoked_at) AND coalesce(CASE WHEN json_valid(r.character_snapshot_json) THEN json_extract(r.character_snapshot_json,'$.is_test') ELSE 0 END,0)=0 AND NOT EXISTS(SELECT 1 FROM conversation_character_bindings b WHERE b.conversation_id=r.conversation_id AND b.draft_card_json IS NOT NULL)",[run_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().api()?;
    let Some((owner, conversation, happened, message)) = row else {
        return Ok(false);
    };
    let existing:Option<(String,i64,Option<i64>,String)>=db.query_row("SELECT id,revision,deleted_at,coalesce(content,'') FROM diary_events WHERE origin_type='conversation_run' AND origin_id=?1",[run_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().api()?;
    if let Some((id, _, deleted, _)) = &existing {
        if deleted.is_some() || event(db, id, Some(&owner)).is_err() {
            return Ok(false);
        }
    }
    let state:Option<(i64,i64,String)>=db.query_row("SELECT event_revision,delivery_revision,projection_digest FROM diary_capture_delivery_state WHERE run_id=?1",[run_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().api()?;
    if state
        .as_ref()
        .is_some_and(|(_, _, digest)| digest == "manual_override")
    {
        return Ok(false);
    }
    if let (Some((_, revision, _, _)), Some((expected, _, _))) = (&existing, &state) {
        if revision != expected {
            return Ok(false);
        } // A human correction wins.
    }
    let mut projection =
        crate::delivery_context::project_capture_run_in(db, &owner, &conversation, run_id)
            .map_err(|_| ApiError::internal())?;
    let unsettled = projection.delivery.parts.iter().any(|part| {
        part.status == "sending"
            || (part.status == "queued"
                && matches!(part.outbox_status.as_str(), "queued" | "sending"))
    });
    // New captures wait for bounded sending to settle. A legacy raw record is
    // sanitized immediately during migration, including currently pending parts.
    if unsettled && (existing.is_none() || state.is_some()) {
        return Ok(false);
    }
    if projection.user_content.trim_start().starts_with('/')
        || sensitive(&projection.user_content)
        || projection
            .assistant_content
            .as_deref()
            .is_some_and(sensitive)
    {
        db.execute(
            "INSERT OR IGNORE INTO diary_capture_skips VALUES(?1,'control_or_sensitive')",
            [run_id],
        )
        .api()?;
        return Ok(false);
    }
    if state.as_ref().is_some_and(|(_, revision, digest)| {
        *revision == projection.delivery.delivery_revision
            && digest == &projection.delivery.projection_digest
    }) {
        return Ok(false);
    }
    let mut content = capture_content(&projection);
    let (id, revision, changed) = if let Some((id, revision, _, old)) = existing {
        if old == content {
            (id, revision, false)
        } else {
            invalidate_capture_event_in(db, &id, state.is_none())?;
            // Invalidation may cancel not-yet-claimed output. Freeze evidence
            // after that change, rather than storing a pre-cancellation claim.
            projection =
                crate::delivery_context::project_capture_run_in(db, &owner, &conversation, run_id)
                    .map_err(|_| ApiError::internal())?;
            content = capture_content(&projection);
            db.execute(
                "UPDATE diary_events SET content=?1,revision=revision+1,observed_at=?2 WHERE id=?3",
                params![content, now(), id],
            )
            .api()?;
            db.execute(
                "UPDATE diary_event_versions SET content=NULL WHERE event_id=?1",
                [&id],
            )
            .api()?;
            (id, revision + 1, true)
        }
    } else {
        let id = uuid::Uuid::new_v4().to_string();
        db.execute("INSERT INTO diary_events(id,assistant_id,revision,visibility,source_owner_id,source_conversation_id,source_message_id,origin_type,origin_id,happened_at,observed_at,content,assertion_kind) VALUES(?1,?2,1,'private',?3,?4,?5,'conversation_run',?6,?7,?8,?9,'direct_experience')",params![id,memory::identity(db)?,owner,conversation,message,run_id,happened,now(),content]).api()?;
        (id, 1, true)
    };
    if changed {
        db.execute("INSERT INTO diary_event_versions VALUES(?1,?2,?3,'system:delivery_capture','delivery_projection',?4)",params![id,revision,content,now()]).api()?;
        dirty_periods_in(db, "private", Some(&owner), happened)?;
    }
    let target = format!(
        "{id}:r{revision}:d{}:{}",
        projection.delivery.delivery_revision, projection.delivery.projection_digest
    );
    crate::delivery_context::snapshot_sources_in(
        db,
        "diary_capture",
        &target,
        &owner,
        &conversation,
        std::slice::from_ref(&projection),
        now().saturating_mul(1000),
    )
    .map_err(|_| ApiError::internal())?;
    db.execute("INSERT INTO diary_capture_delivery_state VALUES(?1,?2,?3,?4,?5) ON CONFLICT(run_id) DO UPDATE SET event_revision=excluded.event_revision,delivery_revision=excluded.delivery_revision,projection_digest=excluded.projection_digest",params![run_id,id,revision,projection.delivery.delivery_revision,projection.delivery.projection_digest]).api()?;
    Ok(changed)
}

fn refresh_delivery_captures_in(db: &Connection) -> ApiResult<usize> {
    let rows = {
        let mut statement = db
            .prepare("SELECT run_id,revision FROM diary_capture_dirty ORDER BY rowid LIMIT 32")
            .api()?;
        let rows = statement
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .api()?
            .collect::<rusqlite::Result<Vec<_>>>()
            .api()?;
        rows
    };
    let mut changed = 0;
    for (run, revision) in rows {
        if capture_completed_in(db, &run)? {
            changed += 1;
        }
        db.execute(
            "DELETE FROM diary_capture_dirty WHERE run_id=?1 AND revision<=?2",
            params![run, revision],
        )
        .api()?;
    }
    Ok(changed)
}
fn clip(s: &str, max: usize) -> String {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

fn period_bounds(kind: &str, day: NaiveDate) -> ApiResult<(NaiveDate, NaiveDate)> {
    match kind {
        "day" => Ok((day, day + Duration::days(1))),
        "week" => {
            let start = day - Duration::days(day.weekday().num_days_from_monday().into());
            Ok((start, start + Duration::days(7)))
        }
        "month" => {
            let start = day.with_day(1).unwrap();
            let end = if day.month() == 12 {
                NaiveDate::from_ymd_opt(day.year() + 1, 1, 1)
            } else {
                NaiveDate::from_ymd_opt(day.year(), day.month() + 1, 1)
            }
            .ok_or_else(|| ApiError::bad("invalid_date", "周期超出范围"))?;
            Ok((start, end))
        }
        _ => Err(ApiError::bad("invalid_period", "周期为day/week/month")),
    }
}

/// Deterministic evidence skeleton, subsequently replaceable by an authorized
/// model generation using the fenced source envelope below.
pub fn consolidate_in(
    db: &Connection,
    visibility: &str,
    owner: Option<&str>,
    kind: &str,
    day: &str,
    timezone: &str,
    timestamp: i64,
) -> ApiResult<Entry> {
    valid_visibility(visibility)?;
    if visibility == "private" && owner.is_none() {
        return Err(ApiError::bad(
            "private_source_required",
            "私有日记必须绑定来源",
        ));
    }
    let zone = tz(timezone)?;
    let (start, end) = period_bounds(kind, date(day)?)?;
    let begin = boundary(start, zone)?;
    let finish = boundary(end, zone)?;
    if begin > timestamp {
        return Err(ApiError::bad(
            "future_diary_period",
            "不能为未来经历生成日记",
        ));
    }
    let revoked:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM diary_source_revocations r WHERE r.source_owner_id=?1 AND NOT EXISTS(SELECT 1 FROM diary_source_policies p WHERE p.source_owner_id=r.source_owner_id AND p.enabled=1))",[owner],|r|r.get(0)).api()?;
    if revoked {
        return Err(ApiError::forbidden(
            "来源已撤销，须显式重新授权并迁移后才能继续整理旧资料",
        ));
    }
    if kind != "day" {
        return aggregation::consolidate(
            db, visibility, owner, kind, start, end, timezone, timestamp,
        );
    }
    let assistant = memory::identity(db)?;
    let mut stmt=db.prepare(&format!("SELECT {EVENT_COLUMNS} FROM diary_events WHERE assistant_id=?1 AND visibility=?2 AND source_owner_id IS ?3 AND deleted_at IS NULL AND NOT EXISTS(SELECT 1 FROM diary_source_revocations v WHERE v.source_owner_id=diary_events.source_owner_id AND diary_events.rowid<=v.through_event_rowid) AND happened_at>=?4 AND happened_at<?5 ORDER BY happened_at,id LIMIT ?6")).api()?;
    let events = stmt
        .query_map(
            params![
                assistant,
                visibility,
                owner,
                begin,
                finish,
                MAX_EVENTS as i64 + 1
            ],
            event_row,
        )
        .api()?
        .collect::<rusqlite::Result<Vec<_>>>()
        .api()?;
    drop(stmt);
    if events.len() > MAX_EVENTS {
        return Err(ApiError::bad(
            "diary_period_budget_exceeded",
            "本周期超过200项来源，保留检查点待分页整理",
        ));
    }
    let gap:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM diary_observation_gaps WHERE start_at<?1 AND end_at>?2) OR (SELECT started_at FROM diary_meta WHERE id=1)>?2",params![finish,begin],|r|r.get(0)).api()?;
    let expected_days = (end - start).num_days();
    let covered: i64 = if kind == "day" {
        1
    } else {
        db.query_row("SELECT count(*) FROM diary_entries WHERE assistant_id=?1 AND visibility=?2 AND source_owner_id IS ?3 AND period='day' AND period_start>=?4 AND period_start<?5 AND deleted_at IS NULL AND status NOT IN ('invalidated','incomplete')",params![assistant,visibility,owner,start.to_string(),end.to_string()],|r|r.get(0)).api()?
    };
    let complete = !gap && covered == expected_days;
    let epoch: i64 = db
        .query_row(
            "SELECT deletion_epoch FROM diary_meta WHERE id=1",
            [],
            |r| r.get(0),
        )
        .api()?;
    let fingerprint = fingerprint(&events, &format!("{complete}:{}", finish <= timestamp));
    let key = format!(
        "{assistant}:{visibility}:{}:{kind}:{start}:{timezone}",
        owner.unwrap_or("")
    );
    let id = format!("diary:{}", hex::encode(Sha256::digest(key.as_bytes())));
    let old: Option<(i64, String, String)> = db
        .query_row(
            "SELECT revision,source_fingerprint,status FROM diary_entries WHERE id=?1",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .api()?;
    if old
        .as_ref()
        .is_some_and(|(_, f, s)| f == &fingerprint && s != "invalidated")
    {
        return entry(db, &id, None);
    }
    let revision = old.map(|v| v.0 + 1).unwrap_or(1);
    let facts = if events.is_empty() {
        if gap {
            "本时段无已记录互动；存在未观测时段，不能据此判断没有发生经历。".into()
        } else {
            "今日无新互动 / 无新经历。".into()
        }
    } else {
        events
            .iter()
            .map(|e| {
                format!(
                    "[{} r{} {}]\n{}",
                    e.id, e.revision, e.origin_type, e.content
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    // Bounded summaries do not silently drop evidence when a period is large.
    let full_evidence = facts.len() <= MAX_CONTENT;
    let facts = if facts.len() > MAX_CONTENT {
        format!(
            "{}\n\n[摘录达到正文预算；完整证据见本条来源列表，不表示全部内容均已整理。]",
            clip(&facts, MAX_CONTENT - 180)
        )
    } else {
        facts
    };
    let status = if finish > timestamp {
        "draft"
    } else if complete {
        "final"
    } else {
        "incomplete"
    };
    db.execute("INSERT INTO diary_entries(id,assistant_id,revision,period,period_start,period_end,timezone,visibility,source_owner_id,facts,feelings,content,status,generator,source_count,observation_complete,coverage_watermark,deletion_epoch,source_fingerprint,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'',?10,?11,'evidence_excerpt_v1',?12,?13,?14,?15,?16,?17) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,facts=excluded.facts,feelings='',content=excluded.content,status=excluded.status,generator=excluded.generator,source_count=excluded.source_count,observation_complete=excluded.observation_complete,coverage_watermark=excluded.coverage_watermark,deletion_epoch=excluded.deletion_epoch,source_fingerprint=excluded.source_fingerprint,updated_at=excluded.updated_at,deleted_at=NULL",params![id,assistant,revision,kind,start.to_string(),end.to_string(),timezone,visibility,owner,facts,status,events.len() as i64,complete,events.iter().map(|e|e.observed_at).max().unwrap_or(0),epoch,fingerprint,timestamp]).api()?;
    db.execute("DELETE FROM diary_entry_sources WHERE entry_id=?1", [&id])
        .api()?;
    for e in &events {
        db.execute(
            "INSERT INTO diary_entry_sources VALUES(?1,?2,?3)",
            params![id, e.id, e.revision],
        )
        .api()?;
    }
    db.execute(
        "INSERT INTO diary_entry_versions VALUES(?1,?2,?3,'','evidence_excerpt_v1','system',?4)",
        params![id, revision, facts, timestamp],
    )
    .api()?;
    db.execute(
        "INSERT INTO diary_entry_version_coverage VALUES(?1,?2,?3)",
        params![id, revision, full_evidence],
    )
    .api()?;
    if kind != "day" {
        db.execute("DELETE FROM diary_entry_parents WHERE entry_id=?1", [&id])
            .api()?;
        db.execute("INSERT INTO diary_entry_parents SELECT ?1,id,revision FROM diary_entries WHERE assistant_id=?2 AND visibility=?3 AND source_owner_id IS ?4 AND period='day' AND period_start>=?5 AND period_start<?6 AND deleted_at IS NULL AND status!='invalidated'",params![id,assistant,visibility,owner,start.to_string(),end.to_string()]).api()?;
    }
    let allow_model:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM diary_source_policies WHERE source_owner_id=?1 AND enabled=1 AND allow_model=1)",[owner],|r|r.get(0)).api()?;
    if allow_model && !events.is_empty() {
        db.execute("INSERT INTO diary_model_jobs(entry_id,entry_revision,status,updated_at) VALUES(?1,?2,'queued',?3) ON CONFLICT(entry_id) DO UPDATE SET entry_revision=excluded.entry_revision,status='queued',attempts=0,error=NULL,updated_at=excluded.updated_at",params![id,revision,timestamp]).api()?;
    }
    entry(db, &id, None)
}
fn fingerprint(events: &[Event], suffix: &str) -> String {
    let mut hash = Sha256::new();
    for e in events {
        hash.update(e.id.as_bytes());
        hash.update(e.revision.to_le_bytes());
        hash.update(e.visibility.as_bytes());
        hash.update(e.source_owner_id.as_deref().unwrap_or("").as_bytes());
    }
    hash.update(suffix.as_bytes());
    hex::encode(hash.finalize())
}

/// One shared scheduler calls this under its database transaction. Work per
/// pass is bounded; a persisted day cursor resumes after shutdown or backlog.
pub fn tick_in(db: &Connection, timestamp: i64) -> ApiResult<Value> {
    let last: i64 = db
        .query_row("SELECT last_tick FROM diary_meta WHERE id=1", [], |r| {
            r.get(0)
        })
        .api()?;
    if timestamp < last {
        return Ok(json!({"status":"blocked","reason":"clock_rollback"}));
    }
    if timestamp - last > 180 {
        db.execute(
            "INSERT INTO diary_observation_gaps(start_at,end_at) VALUES(?1,?2)",
            params![last, timestamp],
        )
        .api()?;
    }
    db.execute("UPDATE diary_meta SET last_tick=?1 WHERE id=1", [timestamp])
        .api()?;
    let runs = {
        let mut stmt=db.prepare("SELECT r.id FROM runs r JOIN diary_source_policies p ON p.source_owner_id=r.owner_id WHERE p.enabled=1 AND r.status='completed' AND r.run_kind IN ('model','skill_text') AND r.created_at>=p.authorized_at AND coalesce(CASE WHEN json_valid(r.character_snapshot_json) THEN json_extract(r.character_snapshot_json,'$.is_test') ELSE 0 END,0)=0 AND NOT EXISTS(SELECT 1 FROM conversation_character_bindings b WHERE b.conversation_id=r.conversation_id AND b.draft_card_json IS NOT NULL) AND NOT EXISTS(SELECT 1 FROM diary_events e WHERE e.origin_type='conversation_run' AND e.origin_id=r.id) AND NOT EXISTS(SELECT 1 FROM diary_capture_skips skip WHERE skip.run_id=r.id) ORDER BY r.created_at,r.id LIMIT 64").api()?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).api()?;
        rows.collect::<rusqlite::Result<Vec<_>>>().api()?
    };
    let mut captured = refresh_delivery_captures_in(db)?;
    for run in runs {
        if capture_completed_in(db, &run)? {
            captured += 1;
        }
    }
    let scopes = {
        let mut s=db.prepare("SELECT scope_key,visibility,source_owner_id,timezone,last_date FROM diary_scope_checkpoints WHERE source_owner_id IS NULL OR NOT EXISTS(SELECT 1 FROM diary_source_revocations r WHERE r.source_owner_id=diary_scope_checkpoints.source_owner_id AND NOT EXISTS(SELECT 1 FROM diary_source_policies p WHERE p.source_owner_id=r.source_owner_id AND p.enabled=1)) ORDER BY last_date LIMIT 16").api()?;
        let rows = s
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .api()?;
        rows.collect::<rusqlite::Result<Vec<_>>>().api()?
    };
    let mut generated = 0;
    for (key, visibility, owner, timezone, last_date) in scopes {
        let zone = tz(&timezone)?;
        let mut today = local_date(timestamp, zone)?;
        let local = Utc
            .timestamp_opt(timestamp, 0)
            .single()
            .ok_or_else(|| ApiError::bad("invalid_timestamp", "时间超出范围"))?
            .with_timezone(&zone);
        if local.hour() == 0 && local.minute() < 5 {
            today -= Duration::days(1);
        }
        let mut day = date(&last_date)? + Duration::days(1);
        for _ in 0..4 {
            if day >= today {
                break;
            }
            let result = consolidate_in(
                db,
                &visibility,
                owner.as_deref(),
                "day",
                &day.to_string(),
                &timezone,
                timestamp,
            );
            if let Err(ref e) = result {
                if e.code == "diary_period_budget_exceeded" {
                    record_consolidation_block(db, &key, "day", day, e.code, timestamp)?;
                    break;
                }
            }
            result?;
            clear_consolidation_block(db, &key, "day", day)?;
            generated += 1;
            for kind in ["week", "month"] {
                let (start, end) = period_bounds(kind, day)?;
                if end <= today {
                    let result = consolidate_in(
                        db,
                        &visibility,
                        owner.as_deref(),
                        kind,
                        &day.to_string(),
                        &timezone,
                        timestamp,
                    );
                    match result {
                        Ok(_) => clear_consolidation_block(db, &key, kind, start)?,
                        Err(e) if e.code == "diary_period_budget_exceeded" => {
                            record_consolidation_block(db, &key, kind, start, e.code, timestamp)?
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
            db.execute(
                "UPDATE diary_scope_checkpoints SET last_date=?1 WHERE scope_key=?2",
                params![day.to_string(), key],
            )
            .api()?;
            day += Duration::days(1);
        }
    }
    aggregation::retry_periods(db, timestamp)?;
    Ok(json!({"status":"completed","captured":captured,"periods_updated":generated}))
}

fn record_consolidation_block(
    db: &Connection,
    scope: &str,
    period: &str,
    start: NaiveDate,
    reason: &str,
    timestamp: i64,
) -> ApiResult<()> {
    db.execute("INSERT INTO diary_consolidation_blocks VALUES(?1,?2,?3,?4,?5) ON CONFLICT(scope_key,period,period_start) DO UPDATE SET reason=excluded.reason,updated_at=excluded.updated_at",params![scope,period,start.to_string(),reason,timestamp]).api()?;
    Ok(())
}
fn clear_consolidation_block(
    db: &Connection,
    scope: &str,
    period: &str,
    start: NaiveDate,
) -> ApiResult<()> {
    db.execute("DELETE FROM diary_consolidation_blocks WHERE scope_key=?1 AND period=?2 AND period_start=?3",params![scope,period,start.to_string()]).api()?;
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GenerationInput {
    pub entry_id: String,
    pub entry_revision: i64,
    pub deletion_epoch: i64,
    pub policy_revision: i64,
    pub period: String,
    pub period_start: String,
    pub period_end: String,
    pub timezone: String,
    pub source_owner_id: String,
    pub source_fingerprint: String,
    pub events: Vec<Event>,
    pub parents: Vec<aggregation::ParentExcerpt>,
    pub fragments: Vec<aggregation::FragmentExcerpt>,
    pub parent_references: Vec<aggregation::ParentReference>,
    pub raw_fingerprint: Option<String>,
    pub raw_source_count: usize,
    pub reduction: Option<aggregation::ReductionStep>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationOutput {
    pub facts: String,
    pub feelings: String,
}
/// Claims one bounded, already authorized source envelope. The caller must use
/// the common model-request lease and only run it after interactive work.
pub fn prepare_generation_in(db: &Connection) -> ApiResult<Option<GenerationInput>> {
    let candidates = {
        let mut stmt=db.prepare("SELECT j.entry_id,p.revision FROM diary_model_jobs j JOIN diary_entries e ON e.id=j.entry_id JOIN diary_source_policies p ON p.source_owner_id=e.source_owner_id WHERE j.status IN ('queued','waiting_parents') AND j.attempts<3 AND (j.attempts=0 OR j.updated_at<?1) AND (coalesce(j.error,'')!='daily_budget_wait' OR j.updated_at<?1) AND j.entry_revision=e.revision AND e.deleted_at IS NULL AND e.status!='invalidated' AND p.enabled=1 AND p.allow_model=1 ORDER BY CASE e.period WHEN 'day' THEN 0 WHEN 'week' THEN 1 ELSE 2 END,e.period_start,j.updated_at LIMIT 64").api()?;
        let rows = stmt
            .query_map([now() - 60], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })
            .api()?;
        rows.collect::<rusqlite::Result<Vec<_>>>().api()?
    };
    for (id, policy) in candidates {
        if let Some(input) = prepare_generation_item(db, &id, policy)? {
            return Ok(Some(input));
        }
    }
    Ok(None)
}
fn prepare_generation_item(
    db: &Connection,
    id: &str,
    policy_revision: i64,
) -> ApiResult<Option<GenerationInput>> {
    let item = entry(db, &id, None)?;
    if item.period != "day" {
        return aggregation::prepare(db, &item, policy_revision);
    }
    let events = sources(db, &id, None)?;
    if events.is_empty() {
        db.execute(
            "UPDATE diary_model_jobs SET status='skipped',error='no_events' WHERE entry_id=?1",
            [&id],
        )
        .api()?;
        return Ok(None);
    }
    let bytes: usize = events.iter().map(|e| e.content.len()).sum();
    if bytes > 48000 {
        db.execute("UPDATE diary_model_jobs SET status='blocked',error='source_token_budget' WHERE entry_id=?1",[&id]).api()?;
        return Ok(None);
    }
    db.execute("UPDATE diary_model_jobs SET status='running',attempts=attempts+1,updated_at=?1 WHERE entry_id=?2",params![now(),id]).api()?;
    let source_fingerprint = db
        .query_row(
            "SELECT source_fingerprint FROM diary_entries WHERE id=?1",
            [&id],
            |r| r.get(0),
        )
        .api()?;
    let deletion_epoch = db
        .query_row(
            "SELECT deletion_epoch FROM diary_meta WHERE id=1",
            [],
            |r| r.get(0),
        )
        .api()?;
    let input = GenerationInput {
        entry_id: id.into(),
        entry_revision: item.revision,
        deletion_epoch,
        policy_revision,
        period: item.period,
        period_start: item.period_start,
        period_end: item.period_end,
        timezone: item.timezone,
        source_owner_id: item.source_owner_id.unwrap_or_default(),
        source_fingerprint,
        events,
        parents: Vec::new(),
        fragments: Vec::new(),
        parent_references: Vec::new(),
        raw_fingerprint: None,
        raw_source_count: item.source_count,
        reduction: None,
    };
    if serde_json::to_vec(&aggregation::payload(&input))
        .map_err(|_| ApiError::internal())?
        .len()
        > 44000
    {
        db.execute("UPDATE diary_model_jobs SET status='blocked',error='source_token_budget' WHERE entry_id=?1",[id]).api()?;
        return Ok(None);
    }
    Ok(Some(input))
}
pub fn generation_payload(input: &GenerationInput) -> Value {
    aggregation::payload(input)
}
pub fn commit_generation_in(
    db: &Connection,
    input: &GenerationInput,
    output: &GenerationOutput,
    model: &str,
) -> ApiResult<Entry> {
    validate_text(&output.facts)?;
    if output.feelings.len() > 4000 || sensitive(&output.feelings) {
        return Err(ApiError::bad(
            "invalid_feelings",
            "感受内容超出范围或含敏感内容",
        ));
    }
    if model.trim().is_empty() || model.len() > 128 {
        return Err(ApiError::bad("invalid_generator", "生成器标识无效"));
    }
    if input.reduction.is_some() {
        return aggregation::commit(db, input, output, model);
    }
    let current = entry(db, &input.entry_id, None)?;
    let epoch: i64 = db
        .query_row(
            "SELECT deletion_epoch FROM diary_meta WHERE id=1",
            [],
            |r| r.get(0),
        )
        .api()?;
    let policy:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM diary_source_policies WHERE source_owner_id=?1 AND revision=?2 AND enabled=1 AND allow_model=1)",params![input.source_owner_id,input.policy_revision],|r|r.get(0)).api()?;
    let live_sources = sources(db, &input.entry_id, None)?;
    if current.revision != input.entry_revision
        || epoch != input.deletion_epoch
        || !policy
        || fingerprint(&live_sources, "") != fingerprint(&input.events, "")
    {
        return Err(ApiError::conflict());
    }
    let fingerprint_now: String = db
        .query_row(
            "SELECT source_fingerprint FROM diary_entries WHERE id=?1",
            [&input.entry_id],
            |r| r.get(0),
        )
        .api()?;
    if fingerprint_now != input.source_fingerprint {
        return Err(ApiError::conflict());
    }
    let generator = format!("model:{model}");
    let content = format!(
        "{}\n\n主观感受（非他人事实）：\n{}",
        output.facts, output.feelings
    );
    db.execute("UPDATE diary_entries SET revision=revision+1,facts=?1,feelings=?2,content=?3,generator=?4,updated_at=?5 WHERE id=?6 AND revision=?7",params![output.facts,output.feelings,content,generator,now(),input.entry_id,input.entry_revision]).api()?;
    db.execute(
        "INSERT INTO diary_entry_versions VALUES(?1,?2,?3,?4,?5,'authorized_model',?6)",
        params![
            input.entry_id,
            input.entry_revision + 1,
            output.facts,
            output.feelings,
            generator,
            now()
        ],
    )
    .api()?;
    db.execute(
        "INSERT INTO diary_entry_version_coverage VALUES(?1,?2,1)",
        params![input.entry_id, input.entry_revision + 1],
    )
    .api()?;
    db.execute(
        "UPDATE diary_model_jobs SET status='completed',error=NULL,updated_at=?1 WHERE entry_id=?2",
        params![now(), input.entry_id],
    )
    .api()?;
    audit(db, "system", "diary.model_consolidate", &input.entry_id)?;
    entry(db, &input.entry_id, None)
}
pub fn generation_failed_in(db: &Connection, id: &str, error: &str) -> ApiResult<()> {
    db.execute("UPDATE diary_model_jobs SET status=CASE WHEN attempts>=3 THEN 'failed' ELSE 'queued' END,error=?1,updated_at=?2 WHERE entry_id=?3 AND status='running'",params![clip(error,160),now(),id]).api()?;
    Ok(())
}
/// Only the authenticated manual consolidation endpoint calls this recovery
/// action. Scheduled passes never reset exhausted attempts or fragment work.
pub fn retry_generation_in(db: &Connection, id: &str) -> ApiResult<()> {
    let item = entry(db, id, None)?;
    let allowed:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM diary_source_policies WHERE source_owner_id=?1 AND enabled=1 AND allow_model=1)",[&item.source_owner_id],|r|r.get(0)).api()?;
    if !allowed || item.source_count == 0 || item.generator.starts_with("model:") {
        return Ok(());
    }
    db.execute("INSERT OR IGNORE INTO diary_model_jobs(entry_id,entry_revision,status,updated_at) VALUES(?1,?2,'queued',?3)",params![id,item.revision,now()]).api()?;
    db.execute("UPDATE diary_model_jobs SET status='queued',attempts=0,error=NULL,updated_at=?1 WHERE entry_id=?2 AND entry_revision=?3 AND status IN ('failed','blocked')",params![now(),id,item.revision]).api()?;
    Ok(())
}

pub fn retrieve(
    db: &Connection,
    owner: &str,
    query: &str,
    max_items: usize,
    max_bytes: usize,
) -> ApiResult<Vec<Entry>> {
    let page = list(
        db,
        &ListOptions {
            q: Some(clip(query, 192)),
            limit: max_items.clamp(1, 8),
            ..Default::default()
        },
        Some(owner),
    )?;
    let mut used = 0;
    Ok(page
        .items
        .into_iter()
        .filter(|e| {
            let size = e.content.len();
            if used + size > max_bytes {
                false
            } else {
                used += size;
                true
            }
        })
        .collect())
}
pub fn record_usage_in(db: &Connection, run: &str, entries: &[(String, i64)]) -> ApiResult<()> {
    let owner: Option<String> = db
        .query_row(
            "SELECT owner_id FROM runs WHERE id=?1 AND status='running'",
            [run],
            |r| r.get(0),
        )
        .optional()
        .api()?;
    let owner = owner.ok_or_else(ApiError::not_found)?;
    for (id, revision) in entries {
        if entry(db, id, Some(&owner))?.revision != *revision {
            return Err(ApiError::conflict());
        }
    }
    for (id, revision) in entries {
        db.execute(
            "INSERT OR REPLACE INTO diary_run_uses VALUES(?1,?2,?3)",
            params![run, id, revision],
        )
        .api()?;
    }
    Ok(())
}
pub fn status(db: &Connection) -> ApiResult<Value> {
    let tuple: (i64, i64, i64, i64) = db
        .query_row(
            "SELECT schema_version,deletion_epoch,last_tick,started_at FROM diary_meta WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .api()?;
    let pending: i64 = db
        .query_row(
            "SELECT count(*) FROM diary_model_jobs WHERE status IN ('queued','running')",
            [],
            |r| r.get(0),
        )
        .api()?;
    let blocked: i64 = db
        .query_row(
            "SELECT count(*) FROM diary_model_jobs WHERE status IN ('blocked','failed')",
            [],
            |r| r.get(0),
        )
        .api()?;
    let blocked_consolidations: i64 = db
        .query_row("SELECT count(*) FROM diary_consolidation_blocks", [], |r| {
            r.get(0)
        })
        .api()?;
    Ok(
        json!({"schema_version":tuple.0,"deletion_epoch":tuple.1,"last_tick":tuple.2,"started_at":tuple.3,"pending_model_jobs":pending,"failed_or_blocked_model_jobs":blocked,"blocked_consolidations":blocked_consolidations,"timezone":TIMEZONE,"automatic_sources":"explicit_policy_required","fallback_generator":"evidence_excerpt_v1","legacy_compatibility":"canonical_event_with_old_id_projection"}),
    )
}

#[cfg(test)]
#[path = "diary_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "diary_delivery_tests.rs"]
mod delivery_tests;
