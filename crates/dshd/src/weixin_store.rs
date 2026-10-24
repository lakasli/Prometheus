//! Durable single-account iLink state. Private reply contexts never enter Runs.
//! All limits and deadlines here are DSH policies, not platform guarantees.
use crate::{
    state::now,
    weixin_protocol::{ApiBase, ContextToken},
};
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::fmt;

pub const MAX_BATCH: usize = 100;
pub const MAX_OPAQUE_BYTES: usize = 64 * 1024;
pub const MAX_INPUT_BYTES: usize = 16 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 4 * 1024;
pub const MAX_LIVE_CONTEXTS: i64 = 1000;
pub const REPLY_DEADLINE_SECONDS: i64 = 300;
const OVERSIZE_REPLY: &str = "回复超过当前微信文本限制，请缩小问题范围后重试。";
const CONTEXT_LIFETIME_SECONDS: i64 = 900;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    Invalid(&'static str),
    Conflict,
    NotConfigured,
    Disabled,
    StaleGeneration,
    StaleCursor,
    Capacity,
    EnqueueFailed,
    Storage,
}
impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "weixin store {self:?}")
    }
}
impl std::error::Error for StoreError {}
pub type Result<T> = std::result::Result<T, StoreError>;
trait SafeSql<T> {
    fn safe(self) -> Result<T>;
}
impl<T> SafeSql<T> for rusqlite::Result<T> {
    fn safe(self) -> Result<T> {
        self.map_err(|_| StoreError::Storage)
    }
}

#[derive(Clone, Debug)]
pub struct AccountInput {
    pub account_id: String,
    pub base_url: String,
    pub credential_ref: String,
    pub linked_user_id: String,
    pub enabled: bool,
}

/// Deliberately not Serialize. Management routes must select a safe DTO.
#[derive(Clone)]
pub struct AccountSnapshot {
    pub account_id: String,
    pub base_url: String,
    pub credential_ref: String,
    pub linked_user_id: String,
    pub revision: i64,
    pub generation: String,
    pub enabled: bool,
    pub cursor: String,
    pub connection_status: String,
    pub last_error_code: Option<String>,
    pub last_event_at: Option<i64>,
}
impl fmt::Debug for AccountSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountSnapshot")
            .field("account_id", &self.account_id)
            .field("revision", &self.revision)
            .field("generation", &self.generation)
            .field("enabled", &self.enabled)
            .field("connection_status", &self.connection_status)
            .field("cursor", &"[redacted]")
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub enum InboundEvent {
    Text {
        message_id: String,
        peer: String,
        text: String,
        context_token: ContextToken,
    },
    /// A rejected unfinished event must not consume the final text dedup key.
    Rejected {
        message_id: Option<String>,
        fingerprint: String,
        reason: String,
    },
}
impl fmt::Debug for InboundEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InboundEvent([private event redacted])")
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IngestResult {
    pub accepted: usize,
    pub duplicates: usize,
    pub rejected: usize,
}

#[derive(Clone)]
pub struct PendingSend {
    pub id: String,
    pub run_id: String,
    pub account_id: String,
    pub peer: String,
    pub base_url: String,
    pub credential_ref: String,
    pub generation: String,
    pub context_ref: String,
    pub client_id: String,
    pub attempt_id: String,
    pub content: String,
    pub deadline: i64,
}
impl fmt::Debug for PendingSend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingSend")
            .field("id", &self.id)
            .field("run_id", &self.run_id)
            .field("generation", &self.generation)
            .field("content", &"[redacted]")
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Debug)]
pub enum SendOutcome {
    Accepted { server_message_id: Option<String> },
    Failed { code: String },
    Unknown { code: String },
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && !value
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == ':')
}
fn valid_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}
fn valid_message_id(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|b| b.is_ascii_digit())
        && value.parse::<u64>().is_ok_and(|id| id.to_string() == value)
}
fn opaque(value: &str) -> bool {
    value.len() <= MAX_OPAQUE_BYTES && !value.chars().any(char::is_control)
}
fn exists(db: &Connection, table: &str) -> rusqlite::Result<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [table],
        |r| r.get(0),
    )
}
fn mark_used(db: &Connection) -> Result<()> {
    if exists(db, "dsh_schema_migrations").safe()? {
        db.execute(
            "INSERT OR IGNORE INTO dsh_schema_migrations(version,created_at) VALUES(4,?1)",
            [now()],
        )
        .safe()?;
    }
    Ok(())
}
fn audit(db: &Connection, actor: &str, action: &str, resource: &str) -> Result<()> {
    db.execute(
        "INSERT INTO audit_events(actor,action,resource,created_at) VALUES(?1,?2,?3,?4)",
        params![actor, action, resource, now()],
    )
    .safe()?;
    Ok(())
}
fn account_row(row: &Row<'_>) -> rusqlite::Result<AccountSnapshot> {
    Ok(AccountSnapshot {
        account_id: row.get(0)?,
        base_url: row.get(1)?,
        credential_ref: row.get(2)?,
        linked_user_id: row.get(3)?,
        revision: row.get(4)?,
        generation: row.get(5)?,
        enabled: row.get(6)?,
        cursor: row.get(7)?,
        connection_status: row.get(8)?,
        last_error_code: row.get(9)?,
        last_event_at: row.get(10)?,
    })
}
pub fn snapshot(db: &Connection) -> Result<Option<AccountSnapshot>> {
    db.query_row("SELECT account_id,base_url,credential_ref,linked_user_id,revision,generation,enabled,cursor,connection_status,last_error_code,last_event_at FROM weixin_account WHERE singleton=1", [], account_row).optional().safe()
}

/// Can run within a surrounding startup transaction; no active account means no
/// new global reader requirement. In-flight sends become unknown, never retried.
pub fn initialize(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch("SAVEPOINT dsh_weixin_initialize")?;
    let result = (|| -> anyhow::Result<()> {
        db.execute_batch("CREATE TABLE IF NOT EXISTS weixin_schema_migrations(version INTEGER PRIMARY KEY,applied_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS weixin_account(singleton INTEGER PRIMARY KEY CHECK(singleton=1),account_id TEXT NOT NULL,base_url TEXT NOT NULL,credential_ref TEXT NOT NULL,linked_user_id TEXT NOT NULL,revision INTEGER NOT NULL CHECK(revision>0),generation TEXT NOT NULL,enabled INTEGER NOT NULL,cursor TEXT NOT NULL DEFAULT '',connection_status TEXT NOT NULL,last_error_code TEXT,last_event_at INTEGER,updated_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS weixin_inbox(account_id TEXT NOT NULL,message_key TEXT NOT NULL,message_id TEXT,disposition TEXT NOT NULL,reason_code TEXT,run_id TEXT REFERENCES runs(id),received_at INTEGER NOT NULL,PRIMARY KEY(account_id,message_key));
CREATE TABLE IF NOT EXISTS weixin_contexts(id TEXT PRIMARY KEY,account_id TEXT NOT NULL,peer TEXT NOT NULL,generation TEXT NOT NULL,token TEXT,created_at INTEGER NOT NULL,expires_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS weixin_run_replies(run_id TEXT PRIMARY KEY REFERENCES runs(id),account_id TEXT NOT NULL,peer TEXT NOT NULL,context_ref TEXT NOT NULL REFERENCES weixin_contexts(id),credential_ref TEXT NOT NULL,generation TEXT NOT NULL,deadline INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS weixin_outbox(id TEXT PRIMARY KEY,run_id TEXT NOT NULL UNIQUE REFERENCES runs(id),account_id TEXT NOT NULL,peer TEXT NOT NULL,context_ref TEXT NOT NULL REFERENCES weixin_contexts(id),credential_ref TEXT NOT NULL,generation TEXT NOT NULL,client_id TEXT NOT NULL UNIQUE,content TEXT,status TEXT NOT NULL CHECK(status IN ('queued','sending','accepted','failed','unknown','cancelled')),attempt_id TEXT,deadline INTEGER NOT NULL,error_code TEXT,server_message_id TEXT,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,CHECK(status NOT IN ('queued','sending') OR content IS NOT NULL));
CREATE INDEX IF NOT EXISTS weixin_send_queue ON weixin_outbox(status,created_at);
CREATE INDEX IF NOT EXISTS weixin_context_run ON weixin_run_replies(context_ref);
CREATE INDEX IF NOT EXISTS weixin_context_outbox ON weixin_outbox(context_ref,status);")?;
        let version: i64 = db.query_row(
            "SELECT coalesce(max(version),0) FROM weixin_schema_migrations",
            [],
            |r| r.get(0),
        )?;
        anyhow::ensure!(
            version <= 1,
            "Weixin storage schema is newer than this binary"
        );
        db.execute(
            "INSERT OR IGNORE INTO weixin_schema_migrations(version,applied_at) VALUES(1,?1)",
            [now()],
        )?;
        db.execute("UPDATE weixin_outbox SET status='unknown',error_code='restart_unknown',content=NULL,updated_at=?1 WHERE status='sending'",[now()])?;
        cleanup_in(db).map_err(|_| anyhow::anyhow!("Weixin context cleanup failed"))?;
        let used:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM weixin_account) OR EXISTS(SELECT 1 FROM weixin_inbox) OR EXISTS(SELECT 1 FROM weixin_outbox)",[],|r|r.get(0))?;
        if used {
            mark_used(db)?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            db.execute_batch("RELEASE dsh_weixin_initialize")?;
            Ok(())
        }
        Err(error) => {
            let _ = db
                .execute_batch("ROLLBACK TO dsh_weixin_initialize; RELEASE dsh_weixin_initialize");
            Err(error)
        }
    }
}

fn invalidate_old_sends(db: &Connection) -> Result<()> {
    db.execute("UPDATE weixin_outbox SET status='cancelled',error_code='account_generation_changed',content=NULL,updated_at=?1 WHERE status='queued'",[now()]).safe()?;
    db.execute("UPDATE weixin_outbox SET status='unknown',error_code='account_generation_changed',content=NULL,updated_at=?1 WHERE status='sending'",[now()]).safe()?;
    db.execute("UPDATE runs SET status='cancelled',error='微信账号已停用或凭证代次已变更',updated_at=?1 WHERE status IN ('queued','running') AND EXISTS(SELECT 1 FROM weixin_run_replies r WHERE r.run_id=runs.id)",[now()]).safe()?;
    Ok(())
}

pub fn replace_account(
    db: &mut Connection,
    expected_revision: i64,
    actor: &str,
    input: &AccountInput,
) -> Result<AccountSnapshot> {
    let tx = db.transaction().safe()?;
    let result = replace_account_in(&tx, expected_revision, actor, input)?;
    tx.commit().safe()?;
    Ok(result)
}

/// Login metadata, credential reference, account and job completion can share
/// one caller-owned transaction without leaking a partially rotated account.
pub fn replace_account_in(
    db: &Connection,
    expected_revision: i64,
    actor: &str,
    input: &AccountInput,
) -> Result<AccountSnapshot> {
    if db.is_autocommit() {
        return Err(StoreError::Storage);
    }
    db.execute_batch("SAVEPOINT weixin_replace_account")
        .safe()?;
    match replace_account_body(db, expected_revision, actor, input) {
        Ok(result) => {
            if db.execute_batch("RELEASE weixin_replace_account").is_err() {
                let _ = db.execute_batch(
                    "ROLLBACK TO weixin_replace_account; RELEASE weixin_replace_account",
                );
                return Err(StoreError::Storage);
            }
            Ok(result)
        }
        Err(error) => {
            let _ = db.execute_batch(
                "ROLLBACK TO weixin_replace_account; RELEASE weixin_replace_account",
            );
            Err(error)
        }
    }
}

fn replace_account_body(
    tx: &Connection,
    expected_revision: i64,
    actor: &str,
    input: &AccountInput,
) -> Result<AccountSnapshot> {
    if !valid_identity(&input.account_id) || !valid_identity(&input.linked_user_id) {
        return Err(StoreError::Invalid("account_identity"));
    }
    let base =
        ApiBase::trusted_https(&input.base_url).map_err(|_| StoreError::Invalid("base_url"))?;
    if uuid::Uuid::parse_str(&input.credential_ref).is_err() {
        return Err(StoreError::Invalid("credential_ref"));
    }
    let current = snapshot(tx)?;
    if current.as_ref().map_or(0, |c| c.revision) != expected_revision {
        return Err(StoreError::Conflict);
    }
    if current.as_ref().is_some_and(|a| {
        a.connection_status == "needs_login" && a.credential_ref == input.credential_ref
    }) {
        return Err(StoreError::Invalid("login_required"));
    }
    let valid_credential: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM secrets WHERE id=?1 AND kind='weixin_bot_token')",
            [&input.credential_ref],
            |r| r.get(0),
        )
        .safe()?;
    if !valid_credential {
        return Err(StoreError::Invalid("credential_ref"));
    }
    invalidate_old_sends(tx)?;
    tx.execute("INSERT INTO weixin_account(singleton,account_id,base_url,credential_ref,linked_user_id,revision,generation,enabled,cursor,connection_status,last_error_code,last_event_at,updated_at) VALUES(1,?1,?2,?3,?4,?5,?6,?7,'',?8,NULL,NULL,?9) ON CONFLICT(singleton) DO UPDATE SET account_id=excluded.account_id,base_url=excluded.base_url,credential_ref=excluded.credential_ref,linked_user_id=excluded.linked_user_id,revision=excluded.revision,generation=excluded.generation,enabled=excluded.enabled,cursor='',connection_status=excluded.connection_status,last_error_code=NULL,last_event_at=NULL,updated_at=excluded.updated_at",
        params![input.account_id,base.as_str(),input.credential_ref,input.linked_user_id,expected_revision+1,uuid::Uuid::new_v4().to_string(),input.enabled,if input.enabled{"connecting"}else{"disabled"},now()]).safe()?;
    mark_used(tx)?;
    audit(tx, actor, "weixin.account.replace", &input.account_id)?;
    cleanup_in(tx)?;
    snapshot(tx)?.ok_or(StoreError::NotConfigured)
}

pub fn set_enabled(
    db: &mut Connection,
    expected_revision: i64,
    actor: &str,
    enabled: bool,
) -> Result<AccountSnapshot> {
    let tx = db.transaction().safe()?;
    let current = snapshot(&tx)?.ok_or(StoreError::NotConfigured)?;
    if current.revision != expected_revision {
        return Err(StoreError::Conflict);
    }
    if enabled && current.connection_status == "needs_login" {
        return Err(StoreError::Invalid("login_required"));
    }
    invalidate_old_sends(&tx)?;
    let login_required = current.connection_status == "needs_login";
    tx.execute("UPDATE weixin_account SET revision=revision+1,generation=?1,enabled=?2,connection_status=?3,last_error_code=?4,updated_at=?5 WHERE singleton=1",
        params![uuid::Uuid::new_v4().to_string(),enabled,if login_required{"needs_login"}else if enabled{"connecting"}else{"disabled"},if login_required{current.last_error_code.as_deref()}else{None},now()]).safe()?;
    audit(
        &tx,
        actor,
        if enabled {
            "weixin.account.enable"
        } else {
            "weixin.account.disable"
        },
        &current.account_id,
    )?;
    cleanup_in(&tx)?;
    let result = snapshot(&tx)?.ok_or(StoreError::NotConfigured)?;
    tx.commit().safe()?;
    Ok(result)
}

pub fn invalidate_token(
    db: &mut Connection,
    expected: &AccountSnapshot,
) -> Result<Option<AccountSnapshot>> {
    require_login(db, expected, "token_expired")
}

pub fn require_login(
    db: &mut Connection,
    expected: &AccountSnapshot,
    code: &str,
) -> Result<Option<AccountSnapshot>> {
    if !valid_code(code) {
        return Err(StoreError::Invalid("error_code"));
    }
    let tx = db.transaction().safe()?;
    let Some(current) = snapshot(&tx)? else {
        return Ok(None);
    };
    if current.generation != expected.generation || current.account_id != expected.account_id {
        return Ok(None);
    }
    invalidate_old_sends(&tx)?;
    tx.execute("UPDATE weixin_account SET revision=revision+1,generation=?1,enabled=0,connection_status='needs_login',last_error_code=?2,updated_at=?3 WHERE singleton=1",params![uuid::Uuid::new_v4().to_string(),code,now()]).safe()?;
    audit(&tx, "system", "weixin.token.invalid", &current.account_id)?;
    cleanup_in(&tx)?;
    let result = snapshot(&tx)?;
    tx.commit().safe()?;
    Ok(result)
}

/// Operational status updates cannot overwrite a newer credential generation.
pub fn record_status(
    db: &Connection,
    expected: &AccountSnapshot,
    status: &str,
    code: Option<&str>,
) -> Result<bool> {
    if !matches!(status, "connecting" | "connected" | "backoff" | "disabled")
        || code.is_some_and(|c| !valid_code(c))
    {
        return Err(StoreError::Invalid("status_code"));
    }
    Ok(db.execute("UPDATE weixin_account SET connection_status=?1,last_error_code=?2,updated_at=?3 WHERE singleton=1 AND account_id=?4 AND generation=?5 AND enabled=1",
        params![status,code,now(),expected.account_id,expected.generation]).safe()?==1)
}

fn validate_events(events: &[InboundEvent]) -> Result<()> {
    if events.len() > MAX_BATCH {
        return Err(StoreError::Capacity);
    }
    for event in events {
        match event {
            InboundEvent::Text {
                message_id,
                peer,
                text,
                context_token,
            } => {
                if !valid_message_id(message_id) {
                    return Err(StoreError::Invalid("message_id"));
                }
                if !valid_identity(peer) {
                    return Err(StoreError::Invalid("peer"));
                }
                if text.trim().is_empty() || text.len() > MAX_INPUT_BYTES || text.contains('\0') {
                    return Err(StoreError::Invalid("text"));
                }
                if context_token.expose().is_empty() || !opaque(context_token.expose()) {
                    return Err(StoreError::Invalid("context_token"));
                }
            }
            InboundEvent::Rejected {
                message_id,
                fingerprint,
                reason,
            } => {
                if message_id.as_ref().is_some_and(|id| !valid_message_id(id)) {
                    return Err(StoreError::Invalid("message_id"));
                }
                if fingerprint.len() != 64
                    || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
                    || !valid_code(reason)
                {
                    return Err(StoreError::Invalid("rejection_metadata"));
                }
            }
        }
    }
    Ok(())
}

pub fn ingest_batch(
    db: &mut Connection,
    expected: &AccountSnapshot,
    new_cursor: &str,
    events: &[InboundEvent],
    mut enqueue: impl FnMut(&Connection, &str, &str, &str, &str) -> anyhow::Result<String>,
) -> Result<IngestResult> {
    validate_events(events)?;
    if !opaque(new_cursor) {
        return Err(StoreError::Invalid("cursor"));
    }
    let tx = db.transaction().safe()?;
    let current = snapshot(&tx)?.ok_or(StoreError::NotConfigured)?;
    if current.generation != expected.generation
        || current.account_id != expected.account_id
        || current.revision != expected.revision
    {
        return Err(StoreError::StaleGeneration);
    }
    if !current.enabled {
        return Err(StoreError::Disabled);
    }
    if current.cursor != expected.cursor {
        return Err(StoreError::StaleCursor);
    }
    cleanup_in(&tx)?;
    let mut result = IngestResult::default();
    for event in events {
        let (key, id) = match event {
            InboundEvent::Text { message_id, .. } => {
                (format!("text:{message_id}"), Some(message_id.as_str()))
            }
            InboundEvent::Rejected {
                message_id,
                fingerprint,
                reason,
            } => (
                format!(
                    "rejected:{}:{reason}:{fingerprint}",
                    message_id.as_deref().unwrap_or("none")
                ),
                message_id.as_deref(),
            ),
        };
        let duplicate: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM weixin_inbox WHERE account_id=?1 AND message_key=?2)",
                params![current.account_id, key],
                |r| r.get(0),
            )
            .safe()?;
        if duplicate {
            result.duplicates += 1;
            continue;
        }
        match event {
            InboundEvent::Rejected { reason, .. } => {
                tx.execute("INSERT INTO weixin_inbox(account_id,message_key,message_id,disposition,reason_code,received_at) VALUES(?1,?2,?3,'rejected',?4,?5)",params![current.account_id,key,id,reason,now()]).safe()?;
                result.rejected += 1;
            }
            InboundEvent::Text {
                message_id,
                peer,
                text,
                context_token,
            } => {
                let live: i64 = tx
                    .query_row(
                        "SELECT count(*) FROM weixin_contexts WHERE token IS NOT NULL",
                        [],
                        |r| r.get(0),
                    )
                    .safe()?;
                if live >= MAX_LIVE_CONTEXTS {
                    return Err(StoreError::Capacity);
                }
                let owner = format!("weixin:{}:c2c:{peer}", current.account_id);
                let conversation = tx
                    .query_row(
                        "SELECT id FROM conversations WHERE source_key=?1 AND owner_id=?1",
                        [&owner],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()
                    .safe()?
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                tx.execute("INSERT OR IGNORE INTO conversations(id,owner_id,title,source_key,created_at) VALUES(?1,?2,'微信私聊',?2,?3)",params![conversation,owner,now()]).safe()?;
                let run = enqueue(&tx, &conversation, &owner, message_id, text)
                    .map_err(|_| StoreError::EnqueueFailed)?;
                let valid_run:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM runs r WHERE r.id=?1 AND r.conversation_id=?2 AND r.owner_id=?3 AND r.client_message_id=?4 AND r.input=?5 AND r.reply_json IS NULL AND r.status='queued' AND EXISTS(SELECT 1 FROM messages m WHERE m.run_id=r.id AND m.conversation_id=r.conversation_id AND m.role='user' AND m.content=r.input))",params![run,conversation,owner,message_id,text],|r|r.get(0)).safe()?;
                if !valid_run {
                    return Err(StoreError::EnqueueFailed);
                }
                let context = uuid::Uuid::new_v4().to_string();
                tx.execute("INSERT INTO weixin_contexts(id,account_id,peer,generation,token,created_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![context,current.account_id,peer,current.generation,context_token.expose(),now(),now()+CONTEXT_LIFETIME_SECONDS]).safe()?;
                tx.execute("INSERT INTO weixin_run_replies(run_id,account_id,peer,context_ref,credential_ref,generation,deadline) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![run,current.account_id,peer,context,current.credential_ref,current.generation,now()+REPLY_DEADLINE_SECONDS]).safe()?;
                tx.execute("INSERT INTO weixin_inbox(account_id,message_key,message_id,disposition,run_id,received_at) VALUES(?1,?2,?3,'accepted',?4,?5)",params![current.account_id,key,message_id,run,now()]).safe()?;
                result.accepted += 1;
            }
        }
    }
    let cursor = if new_cursor.is_empty() {
        &current.cursor
    } else {
        new_cursor
    };
    tx.execute("UPDATE weixin_account SET cursor=?1,connection_status='connected',last_error_code=NULL,last_event_at=CASE WHEN ?2>0 THEN ?3 ELSE last_event_at END,updated_at=?3 WHERE singleton=1 AND generation=?4",params![cursor,result.accepted as i64,now(),current.generation]).safe()?;
    if !events.is_empty() {
        mark_used(&tx)?;
    }
    tx.commit().safe()?;
    Ok(result)
}

/// Call in the same transaction that persists the model/local-command result.
/// Returns false for non-Weixin Runs. Errors never leave a partial outbound row.
pub fn queue_reply_in(db: &Connection, run_id: &str, content: &str) -> Result<bool> {
    if db.is_autocommit() {
        return Err(StoreError::Storage);
    }
    db.execute_batch("SAVEPOINT weixin_queue_reply").safe()?;
    match queue_reply_body(db, run_id, content) {
        Ok(result) => {
            if db.execute_batch("RELEASE weixin_queue_reply").is_err() {
                let _ =
                    db.execute_batch("ROLLBACK TO weixin_queue_reply; RELEASE weixin_queue_reply");
                return Err(StoreError::Storage);
            }
            Ok(result)
        }
        Err(error) => {
            let _ = db.execute_batch("ROLLBACK TO weixin_queue_reply; RELEASE weixin_queue_reply");
            Err(error)
        }
    }
}

fn queue_reply_body(db: &Connection, run_id: &str, content: &str) -> Result<bool> {
    let reference:Option<(String,String,String,String,String,i64)>=db.query_row("SELECT account_id,peer,context_ref,credential_ref,generation,deadline FROM weixin_run_replies WHERE run_id=?1",[run_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional().safe()?;
    let Some((account, peer, context, credential, generation, deadline)) = reference else {
        return Ok(false);
    };
    let duplicate: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM weixin_outbox WHERE run_id=?1)",
            [run_id],
            |r| r.get(0),
        )
        .safe()?;
    if duplicate {
        return Ok(true);
    }
    let completed: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND status='completed' AND response=?2)",
            params![run_id, content],
            |r| r.get(0),
        )
        .safe()?;
    if !completed {
        return Err(StoreError::Invalid("run_result"));
    }
    let active = snapshot(db)?
        .is_some_and(|a| a.enabled && a.account_id == account && a.generation == generation);
    let context_valid:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM weixin_contexts WHERE id=?1 AND account_id=?2 AND peer=?3 AND generation=?4 AND token IS NOT NULL AND expires_at>?5)",params![context,account,peer,generation,now()],|r|r.get(0)).safe()?;
    let (status, code) = if !active {
        ("cancelled", Some("account_generation_changed"))
    } else if deadline <= now() || !context_valid {
        ("failed", Some("reply_context_expired"))
    } else if content.trim().is_empty() {
        ("failed", Some("reply_size_limit"))
    } else if content.len() > MAX_OUTPUT_BYTES {
        ("queued", Some("original_response_too_large"))
    } else {
        ("queued", None)
    };
    let send_content = if content.len() > MAX_OUTPUT_BYTES {
        OVERSIZE_REPLY
    } else {
        content
    };
    let body = (status == "queued").then_some(send_content);
    db.execute("INSERT INTO weixin_outbox(id,run_id,account_id,peer,context_ref,credential_ref,generation,client_id,content,status,deadline,error_code,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?13)",params![uuid::Uuid::new_v4().to_string(),run_id,account,peer,context,credential,generation,format!("dsh-{run_id}"),body,status,deadline,code,now()]).safe()?;
    mark_used(db)?;
    Ok(true)
}

/// Join a caller-owned transaction when forgetting invalidates queued replies.
pub fn cleanup_in(db: &Connection) -> Result<()> {
    if db.is_autocommit() {
        return Err(StoreError::Storage);
    }
    db.execute("UPDATE weixin_outbox SET status='failed',error_code='reply_deadline',content=NULL,updated_at=?1 WHERE status='queued' AND deadline<=?1",[now()]).safe()?;
    db.execute("UPDATE weixin_contexts SET token=NULL WHERE token IS NOT NULL AND (expires_at<=?1 OR (NOT EXISTS(SELECT 1 FROM weixin_run_replies r JOIN runs s ON s.id=r.run_id WHERE r.context_ref=weixin_contexts.id AND s.status IN ('queued','running')) AND NOT EXISTS(SELECT 1 FROM weixin_outbox o WHERE o.context_ref=weixin_contexts.id AND o.status IN ('queued','sending'))))",[now()]).safe()?;
    Ok(())
}

/// Optional-table hook for memory invalidation; already-sending requests retain
/// their actual outcome. A new memory control confirmation is queued afterward.
pub fn cancel_queued_for_conversation_in(db: &Connection, conversation: &str) -> Result<usize> {
    if db.is_autocommit() {
        return Err(StoreError::Storage);
    }
    if !exists(db, "weixin_outbox").safe()? {
        return Ok(0);
    }
    let changed=db.execute("UPDATE weixin_outbox SET status='cancelled',error_code='context_invalidated',content=NULL,updated_at=?1 WHERE status='queued' AND run_id IN (SELECT id FROM runs WHERE conversation_id=?2)",params![now(),conversation]).safe()?;
    cleanup_in(db)?;
    Ok(changed)
}
pub fn cleanup(db: &mut Connection) -> Result<()> {
    let tx = db.transaction().safe()?;
    cleanup_in(&tx)?;
    tx.commit().safe()?;
    Ok(())
}

/// Protect references held outside RuntimeConfig from the generic secret API.
pub fn credential_in_use(db: &Connection, id: &str) -> Result<bool> {
    let account = if exists(db, "weixin_account").safe()? {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM weixin_account WHERE credential_ref=?1)",
            [id],
            |r| r.get::<_, bool>(0),
        )
        .safe()?
    } else {
        false
    };
    let pending = if exists(db, "weixin_outbox").safe()? {
        db.query_row("SELECT EXISTS(SELECT 1 FROM weixin_outbox WHERE credential_ref=?1 AND status IN ('queued','sending'))",[id],|r|r.get::<_,bool>(0)).safe()?
    } else {
        false
    };
    Ok(account || pending)
}

pub fn claim_send(db: &mut Connection) -> Result<Option<PendingSend>> {
    let tx = db.transaction().safe()?;
    cleanup_in(&tx)?;
    tx.execute("UPDATE weixin_outbox SET status='cancelled',error_code='account_generation_changed',content=NULL,updated_at=?1 WHERE status='queued' AND NOT EXISTS(SELECT 1 FROM weixin_account a WHERE a.enabled=1 AND a.account_id=weixin_outbox.account_id AND a.generation=weixin_outbox.generation)",[now()]).safe()?;
    tx.execute("UPDATE weixin_outbox SET status='failed',error_code='reply_context_expired',content=NULL,updated_at=?1 WHERE status='queued' AND NOT EXISTS(SELECT 1 FROM weixin_contexts c WHERE c.id=weixin_outbox.context_ref AND c.account_id=weixin_outbox.account_id AND c.peer=weixin_outbox.peer AND c.generation=weixin_outbox.generation AND c.token IS NOT NULL AND c.expires_at>?1)",[now()]).safe()?;
    let active: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM weixin_outbox WHERE status='sending')",
            [],
            |r| r.get(0),
        )
        .safe()?;
    if active {
        tx.commit().safe()?;
        return Ok(None);
    }
    let item=tx.query_row("SELECT o.id,o.run_id,o.account_id,o.peer,a.base_url,o.credential_ref,o.generation,o.context_ref,o.client_id,o.content,o.deadline FROM weixin_outbox o JOIN weixin_account a ON a.account_id=o.account_id AND a.generation=o.generation WHERE o.status='queued' AND a.enabled=1 ORDER BY o.created_at,o.rowid LIMIT 1",[],|r|Ok(PendingSend{id:r.get(0)?,run_id:r.get(1)?,account_id:r.get(2)?,peer:r.get(3)?,base_url:r.get(4)?,credential_ref:r.get(5)?,generation:r.get(6)?,context_ref:r.get(7)?,client_id:r.get(8)?,attempt_id:uuid::Uuid::new_v4().to_string(),content:r.get(9)?,deadline:r.get(10)?})).optional().safe()?;
    if let Some(item) = &item {
        tx.execute("UPDATE weixin_outbox SET status='sending',attempt_id=?1,updated_at=?2 WHERE id=?3 AND status='queued'",params![item.attempt_id,now(),item.id]).safe()?;
    }
    tx.commit().safe()?;
    Ok(item)
}

/// The exact account, peer, credential generation and attempt must still match.
pub fn load_context(db: &Connection, pending: &PendingSend) -> Result<ContextToken> {
    let value:Option<String>=db.query_row("SELECT c.token FROM weixin_contexts c JOIN weixin_outbox o ON o.context_ref=c.id JOIN weixin_account a ON a.account_id=o.account_id AND a.generation=o.generation WHERE o.id=?1 AND o.run_id=?2 AND o.attempt_id=?3 AND o.status='sending' AND o.account_id=?4 AND o.peer=?5 AND o.generation=?6 AND o.context_ref=?7 AND o.credential_ref=?8 AND c.account_id=o.account_id AND c.peer=o.peer AND c.generation=o.generation AND c.token IS NOT NULL AND c.expires_at>?9 AND o.deadline>?9 AND a.enabled=1 AND a.credential_ref=o.credential_ref AND a.base_url=?10 AND o.client_id=?11 AND o.content=?12",params![pending.id,pending.run_id,pending.attempt_id,pending.account_id,pending.peer,pending.generation,pending.context_ref,pending.credential_ref,now(),pending.base_url,pending.client_id,pending.content],|r|r.get(0)).optional().safe()?.flatten();
    ContextToken::new(value.ok_or(StoreError::StaleGeneration)?).map_err(|_| StoreError::Storage)
}

/// No retry path is provided. A late explicit ACK may resolve an unknown old
/// generation attempt; it records evidence rather than issuing another request.
pub fn complete_send(
    db: &mut Connection,
    pending: &PendingSend,
    outcome: SendOutcome,
) -> Result<bool> {
    let (status, code, server) = match outcome {
        SendOutcome::Accepted { server_message_id } => {
            if server_message_id
                .as_deref()
                .is_some_and(|id| !valid_message_id(id))
            {
                return Err(StoreError::Invalid("server_message_id"));
            }
            ("accepted", None, server_message_id)
        }
        SendOutcome::Failed { code } => {
            if !valid_code(&code) {
                return Err(StoreError::Invalid("error_code"));
            }
            ("failed", Some(code), None)
        }
        SendOutcome::Unknown { code } => {
            if !valid_code(&code) {
                return Err(StoreError::Invalid("error_code"));
            }
            ("unknown", Some(code), None)
        }
    };
    let tx = db.transaction().safe()?;
    let changed=tx.execute("UPDATE weixin_outbox SET status=?1,error_code=CASE WHEN ?2 IS NOT NULL THEN ?2 WHEN error_code='original_response_too_large' THEN error_code ELSE NULL END,server_message_id=?3,content=NULL,updated_at=?4 WHERE id=?5 AND run_id=?6 AND account_id=?7 AND generation=?8 AND client_id=?9 AND attempt_id=?10 AND status IN ('sending','unknown')",params![status,code,server,now(),pending.id,pending.run_id,pending.account_id,pending.generation,pending.client_id,pending.attempt_id]).safe()?;
    cleanup_in(&tx)?;
    tx.commit().safe()?;
    Ok(changed == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AppState, Options};
    use tempfile::TempDir;

    fn database() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        db.execute_batch(include_str!("../schema.sql")).unwrap();
        db.execute_batch("CREATE TABLE dsh_schema_migrations(version INTEGER PRIMARY KEY,created_at INTEGER NOT NULL); INSERT INTO dsh_schema_migrations VALUES(1,0);").unwrap();
        initialize(&db).unwrap();
        db
    }
    fn account(db: &mut Connection, name: &str) -> AccountSnapshot {
        let credential = uuid::Uuid::new_v4().to_string();
        db.execute(
            "INSERT INTO secrets(id,kind,created_at) VALUES(?1,'weixin_bot_token',0)",
            [&credential],
        )
        .unwrap();
        let revision = snapshot(db).unwrap().map_or(0, |s| s.revision);
        replace_account(
            db,
            revision,
            "admin",
            &AccountInput {
                account_id: name.into(),
                base_url: "https://ilinkai.weixin.qq.com/".into(),
                credential_ref: credential,
                linked_user_id: "user@im.wechat".into(),
                enabled: true,
            },
        )
        .unwrap()
    }
    fn event(id: &str, peer: &str, token: &str) -> InboundEvent {
        InboundEvent::Text {
            message_id: id.into(),
            peer: peer.into(),
            text: format!("synthetic text {id}"),
            context_token: ContextToken::new(token).unwrap(),
        }
    }
    fn enqueue(
        db: &Connection,
        conversation: &str,
        owner: &str,
        client: &str,
        text: &str,
    ) -> anyhow::Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,'{}','queued',0,0)",params![id,conversation,owner,client,text])?;
        db.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES(?1,?2,?3,'user',?4,0)",params![uuid::Uuid::new_v4().to_string(),conversation,id,text])?;
        Ok(id)
    }
    fn scalar(db: &Connection, sql: &str) -> i64 {
        db.query_row(sql, [], |r| r.get(0)).unwrap()
    }
    fn run(db: &Connection, id: &str) -> String {
        db.query_row(
            "SELECT run_id FROM weixin_inbox WHERE message_id=?1 AND disposition='accepted'",
            [id],
            |r| r.get(0),
        )
        .unwrap()
    }
    fn finish(db: &mut Connection, run: &str) {
        let tx = db.transaction().unwrap();
        tx.execute(
            "UPDATE runs SET status='completed',response='synthetic response' WHERE id=?1",
            [run],
        )
        .unwrap();
        assert!(queue_reply_in(&tx, run, "synthetic response").unwrap());
        assert!(queue_reply_in(&tx, run, "same retry").unwrap());
        tx.commit().unwrap();
    }

    #[test]
    fn initialization_is_additive_future_schema_fails_and_account_revisions_rotate_safely() {
        let mut db = database();
        assert_eq!(
            scalar(&db, "SELECT max(version) FROM dsh_schema_migrations"),
            1
        );
        let first = account(&mut db, "bot@im.bot");
        assert_eq!(first.revision, 1);
        assert_eq!(
            scalar(&db, "SELECT max(version) FROM dsh_schema_migrations"),
            4
        );
        assert!(!format!("{first:?}").contains("ilinkai"));
        assert_eq!(
            set_enabled(&mut db, 0, "admin", false).unwrap_err(),
            StoreError::Conflict
        );
        let disabled = set_enabled(&mut db, first.revision, "admin", false).unwrap();
        assert_ne!(disabled.generation, first.generation);
        assert!(!disabled.enabled);
        let enabled = set_enabled(&mut db, disabled.revision, "admin", true).unwrap();
        assert!(enabled.enabled);
        assert_eq!(enabled.revision, 3);
        db.execute("INSERT INTO weixin_schema_migrations VALUES(2,0)", [])
            .unwrap();
        assert!(initialize(&db).is_err());
    }

    #[test]
    fn batch_cursor_inbox_run_context_and_u64_identity_commit_together() {
        let mut db = database();
        let initial = account(&mut db, "bot@im.bot");
        let rejected = InboundEvent::Rejected {
            message_id: Some("18446744073709551615".into()),
            fingerprint: "a".repeat(64),
            reason: "not_finished".into(),
        };
        let result = ingest_batch(
            &mut db,
            &initial,
            "cursor-private",
            &[
                rejected,
                event("18446744073709551615", "peer@im.wechat", "secret-context"),
            ],
            enqueue,
        )
        .unwrap();
        assert_eq!(
            result,
            IngestResult {
                accepted: 1,
                rejected: 1,
                duplicates: 0
            }
        );
        let after = snapshot(&db).unwrap().unwrap();
        assert_eq!(after.cursor, "cursor-private");
        assert!(!format!("{after:?}").contains("cursor-private"));
        let repeated = ingest_batch(
            &mut db,
            &after,
            "next-cursor",
            &[event(
                "18446744073709551615",
                "peer@im.wechat",
                "new-context-ignored",
            )],
            enqueue,
        )
        .unwrap();
        assert_eq!(repeated.duplicates, 1);
        assert_eq!(scalar(&db, "SELECT count(*) FROM runs"), 1);
        assert_eq!(scalar(&db, "SELECT count(*) FROM weixin_contexts"), 1);
        let owner: String = db
            .query_row("SELECT owner_id FROM runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(owner, "weixin:bot@im.bot:c2c:peer@im.wechat");
        assert_eq!(
            scalar(
                &db,
                "SELECT count(*) FROM runs WHERE reply_json IS NOT NULL"
            ),
            0
        );
        assert_eq!(
            ingest_batch(&mut db, &initial, "stale", &[], enqueue).unwrap_err(),
            StoreError::StaleCursor
        );
    }

    #[test]
    fn later_enqueue_failure_and_invalid_batch_leave_no_partial_acknowledgement() {
        let mut db = database();
        let current = account(&mut db, "bot@im.bot");
        let mut count = 0;
        let error = ingest_batch(
            &mut db,
            &current,
            "lost-cursor",
            &[
                event("1", "one@im.wechat", "one-secret"),
                event("2", "two@im.wechat", "two-secret"),
            ],
            |db, c, o, i, t| {
                count += 1;
                if count == 2 {
                    anyhow::bail!("synthetic queue failure");
                }
                enqueue(db, c, o, i, t)
            },
        )
        .unwrap_err();
        assert_eq!(error, StoreError::EnqueueFailed);
        assert_eq!(scalar(&db, "SELECT count(*) FROM runs"), 0);
        assert_eq!(scalar(&db, "SELECT count(*) FROM weixin_inbox"), 0);
        assert_eq!(scalar(&db, "SELECT count(*) FROM conversations"), 0);
        assert_eq!(scalar(&db, "SELECT count(*) FROM weixin_contexts"), 0);
        assert_eq!(snapshot(&db).unwrap().unwrap().cursor, "");
        let invalid = event("9007199254740993", "bad:peer", "secret");
        assert!(ingest_batch(
            &mut db,
            &current,
            "bad",
            &[event("1", "valid@im.wechat", "secret"), invalid],
            enqueue
        )
        .is_err());
        assert_eq!(scalar(&db, "SELECT count(*) FROM runs"), 0);
    }

    #[test]
    fn exact_run_reply_context_is_frozen_and_send_acceptance_releases_private_tokens() {
        let mut db = database();
        let current = account(&mut db, "bot@im.bot");
        ingest_batch(
            &mut db,
            &current,
            "",
            &[
                event("1", "one@im.wechat", "context-one"),
                event("2", "two@im.wechat", "context-two"),
            ],
            enqueue,
        )
        .unwrap();
        let one = run(&db, "1");
        finish(&mut db, &one);
        let pending = claim_send(&mut db).unwrap().unwrap();
        assert_eq!(pending.run_id, one);
        assert_eq!(load_context(&db, &pending).unwrap().expose(), "context-one");
        assert!(!format!("{pending:?}").contains("synthetic response"));
        let mut forged = pending.clone();
        forged.peer = "two@im.wechat".into();
        assert!(load_context(&db, &forged).is_err());
        assert!(complete_send(
            &mut db,
            &pending,
            SendOutcome::Accepted {
                server_message_id: Some(u64::MAX.to_string())
            }
        )
        .unwrap());
        assert_eq!(
            scalar(
                &db,
                "SELECT count(*) FROM weixin_outbox WHERE status='accepted'"
            ),
            1
        );
        assert_eq!(
            scalar(
                &db,
                "SELECT count(*) FROM weixin_contexts WHERE token IS NOT NULL"
            ),
            1
        );
        assert!(claim_send(&mut db).unwrap().is_none());
    }

    #[test]
    fn generation_rotation_cancels_unsent_and_preserves_ambiguous_or_late_accepted_evidence() {
        let mut db = database();
        let current = account(&mut db, "bot@im.bot");
        ingest_batch(
            &mut db,
            &current,
            "",
            &[
                event("1", "one@im.wechat", "one"),
                event("2", "two@im.wechat", "two"),
            ],
            enqueue,
        )
        .unwrap();
        let one = run(&db, "1");
        let two = run(&db, "2");
        finish(&mut db, &one);
        finish(&mut db, &two);
        let pending = claim_send(&mut db).unwrap().unwrap();
        let replacement = account(&mut db, "replacement@im.bot");
        assert_ne!(replacement.generation, current.generation);
        assert_eq!(
            scalar(
                &db,
                "SELECT count(*) FROM weixin_outbox WHERE status='cancelled'"
            ),
            1
        );
        assert_eq!(
            scalar(
                &db,
                "SELECT count(*) FROM weixin_outbox WHERE status='unknown'"
            ),
            1
        );
        assert!(load_context(&db, &pending).is_err());
        assert!(complete_send(
            &mut db,
            &pending,
            SendOutcome::Accepted {
                server_message_id: None
            }
        )
        .unwrap());
        assert_eq!(
            scalar(
                &db,
                "SELECT count(*) FROM weixin_outbox WHERE status='accepted'"
            ),
            1
        );
        assert!(claim_send(&mut db).unwrap().is_none());
        assert_eq!(
            ingest_batch(&mut db, &current, "", &[], enqueue).unwrap_err(),
            StoreError::StaleGeneration
        );
    }

    #[test]
    fn restart_and_invalid_token_never_blindly_replay_a_send() {
        let mut db = database();
        let current = account(&mut db, "bot@im.bot");
        ingest_batch(
            &mut db,
            &current,
            "",
            &[event("1", "one@im.wechat", "private-token")],
            enqueue,
        )
        .unwrap();
        let id = run(&db, "1");
        finish(&mut db, &id);
        let pending = claim_send(&mut db).unwrap().unwrap();
        initialize(&db).unwrap();
        assert_eq!(
            scalar(
                &db,
                "SELECT count(*) FROM weixin_outbox WHERE status='unknown'"
            ),
            1
        );
        assert!(claim_send(&mut db).unwrap().is_none());
        assert!(load_context(&db, &pending).is_err());
        let expired = invalidate_token(&mut db, &current).unwrap().unwrap();
        assert!(!expired.enabled);
        assert_eq!(expired.connection_status, "needs_login");
        assert!(invalidate_token(&mut db, &current).unwrap().is_none());
        assert_eq!(
            set_enabled(&mut db, expired.revision, "admin", true).unwrap_err(),
            StoreError::Invalid("login_required")
        );
        let disabled_again = set_enabled(&mut db, expired.revision, "admin", false).unwrap();
        assert_eq!(disabled_again.connection_status, "needs_login");
        assert_eq!(
            disabled_again.last_error_code.as_deref(),
            Some("token_expired")
        );
        assert_eq!(
            set_enabled(&mut db, disabled_again.revision, "admin", true).unwrap_err(),
            StoreError::Invalid("login_required")
        );
        assert!(!record_status(&db, &current, "connected", None).unwrap());
    }

    #[test]
    fn deadlines_and_size_limits_fail_explicitly_and_domain_quota_rolls_back_cursor() {
        let mut db = database();
        let current = account(&mut db, "bot@im.bot");
        ingest_batch(
            &mut db,
            &current,
            "",
            &[event("1", "one@im.wechat", "one")],
            enqueue,
        )
        .unwrap();
        let id = run(&db, "1");
        let tx = db.transaction().unwrap();
        let original = "x".repeat(MAX_OUTPUT_BYTES + 1);
        tx.execute(
            "UPDATE runs SET status='completed',response=?1 WHERE id=?2",
            params![original, id],
        )
        .unwrap();
        queue_reply_in(&tx, &id, &original).unwrap();
        tx.commit().unwrap();
        assert_eq!(scalar(&db,"SELECT count(*) FROM weixin_outbox WHERE status='queued' AND error_code='original_response_too_large'"),1);
        let pending = claim_send(&mut db).unwrap().unwrap();
        assert_eq!(pending.content, OVERSIZE_REPLY);
        assert_eq!(
            db.query_row("SELECT response FROM runs WHERE id=?1", [&id], |r| r
                .get::<_, String>(0))
                .unwrap(),
            original
        );
        complete_send(
            &mut db,
            &pending,
            SendOutcome::Accepted {
                server_message_id: None,
            },
        )
        .unwrap();
        assert_eq!(scalar(&db,"SELECT count(*) FROM weixin_outbox WHERE status='accepted' AND error_code='original_response_too_large'"),1);
        let many = vec![event("2", "peer@im.wechat", "context"); MAX_BATCH + 1];
        assert_eq!(
            ingest_batch(&mut db, &current, "too-many", &many, enqueue).unwrap_err(),
            StoreError::Capacity
        );
        assert_eq!(snapshot(&db).unwrap().unwrap().cursor, "");
        assert!(ingest_batch(
            &mut db,
            &current,
            &"x".repeat(MAX_OPAQUE_BYTES + 1),
            &[],
            enqueue
        )
        .is_err());
    }

    #[test]
    fn safe_errors_and_events_never_debug_expose_cursor_or_context_and_unknown_reasons_reject() {
        let mut db = database();
        let current = account(&mut db, "bot@im.bot");
        let text = event("1", "peer@im.wechat", "private-do-not-debug");
        assert!(!format!("{text:?}").contains("private-do-not-debug"));
        let bad = InboundEvent::Rejected {
            message_id: None,
            fingerprint: "f".repeat(64),
            reason: "raw private body".into(),
        };
        let error = ingest_batch(&mut db, &current, "", &[bad], enqueue).unwrap_err();
        assert_eq!(error, StoreError::Invalid("rejection_metadata"));
        assert!(!format!("{error}").contains("private body"));
        assert_eq!(scalar(&db, "SELECT count(*) FROM weixin_inbox"), 0);
    }

    #[test]
    fn outbound_savepoint_and_account_audit_failure_do_not_leave_partial_changes() {
        let mut db = database();
        let current = account(&mut db, "bot@im.bot");
        ingest_batch(
            &mut db,
            &current,
            "",
            &[event("1", "peer@im.wechat", "private-context")],
            enqueue,
        )
        .unwrap();
        let id = run(&db, "1");
        db.execute_batch("CREATE TRIGGER deny_reader BEFORE INSERT ON dsh_schema_migrations WHEN NEW.version=4 BEGIN SELECT RAISE(ABORT,'synthetic fault'); END;").unwrap();
        let tx = db.transaction().unwrap();
        tx.execute(
            "UPDATE runs SET status='completed',response='synthetic response' WHERE id=?1",
            [&id],
        )
        .unwrap();
        assert_eq!(
            queue_reply_in(&tx, &id, "synthetic response").unwrap_err(),
            StoreError::Storage
        );
        tx.commit().unwrap();
        assert_eq!(scalar(&db, "SELECT count(*) FROM weixin_outbox"), 0);
        assert_eq!(
            scalar(&db, "SELECT count(*) FROM runs WHERE status='completed'"),
            1
        );
        db.execute_batch("DROP TRIGGER deny_reader").unwrap();
        let tx = db.transaction().unwrap();
        queue_reply_in(&tx, &id, "synthetic response").unwrap();
        tx.commit().unwrap();
        db.execute_batch("CREATE TRIGGER deny_audit BEFORE INSERT ON audit_events WHEN NEW.action='weixin.account.disable' BEGIN SELECT RAISE(ABORT,'synthetic fault'); END;").unwrap();
        assert_eq!(
            set_enabled(&mut db, current.revision, "admin", false).unwrap_err(),
            StoreError::Storage
        );
        let after = snapshot(&db).unwrap().unwrap();
        assert_eq!(after.generation, current.generation);
        assert!(after.enabled);
        assert_eq!(
            scalar(
                &db,
                "SELECT count(*) FROM weixin_outbox WHERE status='queued'"
            ),
            1
        );
    }

    #[test]
    fn expired_context_or_deadline_cannot_be_claimed_or_replaced_with_another_peers_context() {
        let mut db = database();
        let current = account(&mut db, "bot@im.bot");
        ingest_batch(
            &mut db,
            &current,
            "",
            &[
                event("1", "peer@im.wechat", "context-one"),
                event("2", "other@im.wechat", "context-two"),
            ],
            enqueue,
        )
        .unwrap();
        let first = run(&db, "1");
        let second = run(&db, "2");
        finish(&mut db, &first);
        finish(&mut db, &second);
        let pending = claim_send(&mut db).unwrap().unwrap();
        assert!(claim_send(&mut db).unwrap().is_none());
        let mut tampered = pending.clone();
        tampered.base_url = "https://other.weixin.qq.com/".into();
        assert!(load_context(&db, &tampered).is_err());
        tampered = pending.clone();
        tampered.content = "different reply".into();
        assert!(load_context(&db, &tampered).is_err());
        complete_send(
            &mut db,
            &pending,
            SendOutcome::Unknown {
                code: "transport_unknown".into(),
            },
        )
        .unwrap();
        db.execute("UPDATE weixin_contexts SET expires_at=?1 WHERE id=(SELECT context_ref FROM weixin_outbox WHERE run_id=?2)",params![now()-1,second]).unwrap();
        assert!(claim_send(&mut db).unwrap().is_none());
        assert_eq!(scalar(&db,"SELECT count(*) FROM weixin_outbox WHERE status='failed' AND error_code='reply_context_expired'"),1);
        assert_eq!(
            scalar(
                &db,
                "SELECT count(*) FROM weixin_contexts WHERE token IS NOT NULL"
            ),
            0
        );
    }

    #[test]
    fn context_invalidation_cancels_only_unsent_and_scrubs_its_private_token() {
        let mut db = database();
        let current = account(&mut db, "bot@im.bot");
        ingest_batch(
            &mut db,
            &current,
            "",
            &[
                event("1", "peer@im.wechat", "context-one"),
                event("2", "peer@im.wechat", "context-two"),
            ],
            enqueue,
        )
        .unwrap();
        let one = run(&db, "1");
        let two = run(&db, "2");
        finish(&mut db, &one);
        finish(&mut db, &two);
        let sending = claim_send(&mut db).unwrap().unwrap();
        let conversation: String = db
            .query_row(
                "SELECT conversation_id FROM runs WHERE id=?1",
                [&one],
                |r| r.get(0),
            )
            .unwrap();
        let tx = db.transaction().unwrap();
        assert_eq!(
            cancel_queued_for_conversation_in(&tx, &conversation).unwrap(),
            1
        );
        tx.commit().unwrap();
        assert_eq!(
            scalar(
                &db,
                "SELECT count(*) FROM weixin_outbox WHERE status='sending'"
            ),
            1
        );
        assert_eq!(scalar(&db,"SELECT count(*) FROM weixin_outbox WHERE status='cancelled' AND error_code='context_invalidated'"),1);
        assert_eq!(
            scalar(
                &db,
                "SELECT count(*) FROM weixin_contexts WHERE token IS NOT NULL"
            ),
            1
        );
        complete_send(
            &mut db,
            &sending,
            SendOutcome::Accepted {
                server_message_id: None,
            },
        )
        .unwrap();
        assert_eq!(
            scalar(
                &db,
                "SELECT count(*) FROM weixin_contexts WHERE token IS NOT NULL"
            ),
            0
        );
        let mut absent = Connection::open_in_memory().unwrap();
        let tx = absent.transaction().unwrap();
        assert_eq!(cancel_queued_for_conversation_in(&tx, "unused").unwrap(), 0);
    }

    #[test]
    fn credential_guards_include_disabled_accounts_and_outer_account_replacement_is_atomic() {
        let absent = Connection::open_in_memory().unwrap();
        assert!(!credential_in_use(&absent, "unused").unwrap());
        initialize(&absent).unwrap_err(); // Base host tables are a required dependency.
        let mut db = database();
        let current = account(&mut db, "bot@im.bot");
        assert!(credential_in_use(&db, &current.credential_ref).unwrap());
        let disabled = set_enabled(&mut db, current.revision, "admin", false).unwrap();
        assert!(credential_in_use(&db, &disabled.credential_ref).unwrap());
        let replacement = uuid::Uuid::new_v4().to_string();
        let tx = db.transaction().unwrap();
        tx.execute(
            "INSERT INTO secrets(id,kind,created_at) VALUES(?1,'weixin_bot_token',0)",
            [&replacement],
        )
        .unwrap();
        let next = replace_account_in(
            &tx,
            disabled.revision,
            "admin",
            &AccountInput {
                account_id: "replacement@im.bot".into(),
                base_url: "https://ilinkai.weixin.qq.com/".into(),
                credential_ref: replacement.clone(),
                linked_user_id: "user@im.wechat".into(),
                enabled: true,
            },
        )
        .unwrap();
        assert_eq!(next.credential_ref, replacement);
        tx.rollback().unwrap();
        assert_eq!(
            snapshot(&db).unwrap().unwrap().generation,
            disabled.generation
        );
        assert!(!credential_in_use(&db, &replacement).unwrap());
        assert_eq!(scalar(&db,"SELECT count(*) FROM secrets WHERE id NOT IN (SELECT credential_ref FROM weixin_account)"),0);
        let input = AccountInput {
            account_id: "outside@im.bot".into(),
            base_url: "https://ilinkai.weixin.qq.com/".into(),
            credential_ref: disabled.credential_ref,
            linked_user_id: "user@im.wechat".into(),
            enabled: true,
        };
        assert_eq!(
            replace_account_in(&db, disabled.revision, "admin", &input).unwrap_err(),
            StoreError::Storage
        );
    }

    #[test]
    fn real_app_fixture_can_initialize_without_changing_current_reader_until_account_persisted() {
        let temporary = TempDir::new().unwrap();
        let state = AppState::open(Options {
            data_dir: temporary.path().to_owned(),
            origin: "http://127.0.0.1:18419".into(),
            allow_private_model: false,
            cookie_secure: false,
            web_dir: temporary.path().join("web"),
        })
        .unwrap();
        state
            .db(|db| {
                initialize(db)?;
                assert_eq!(
                    scalar(db, "SELECT max(version) FROM dsh_schema_migrations"),
                    1
                );
                account(db, "bot@im.bot");
                assert_eq!(
                    scalar(db, "SELECT max(version) FROM dsh_schema_migrations"),
                    4
                );
                Ok(())
            })
            .unwrap();
    }
}
