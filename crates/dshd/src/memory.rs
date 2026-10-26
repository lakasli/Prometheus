//! Explicit memories belong to the assistant; source privacy is a separate ACL.
//!
//! This module never extracts memories or treats their content as instructions.
//! Forgetting clears live memory/version/index content, not messages or backups.
use crate::{
    error::{ApiError, ApiResult},
    state::now,
};
use axum::http::StatusCode;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_CONTENT_BYTES: usize = 4000;
pub const MAX_RECORDS: i64 = 5000;
pub const MAX_PRIVATE_RECORDS: i64 = 500;
const COLUMNS: &str = "m.id,m.assistant_id,m.content,m.revision,m.visibility,m.source_owner_id,m.source_conversation_id,m.source_message_id,m.created_by,m.created_at,m.updated_at";

#[derive(Clone, Copy)]
pub enum Access<'a> {
    Admin(&'a str),
    Owner(&'a str),
}
impl<'a> Access<'a> {
    fn actor(self) -> &'a str {
        match self {
            Self::Admin(actor) | Self::Owner(actor) => actor,
        }
    }
    fn can_mutate(self, record: &MemoryRecord) -> ApiResult<()> {
        match self {
            Self::Admin(_) => Ok(()),
            Self::Owner(owner)
                if record.visibility == Visibility::Private
                    && record.source_owner_id.as_deref() == Some(owner) =>
            {
                Ok(())
            }
            Self::Owner(_) => Err(ApiError::not_found()),
        }
    }
}

trait DatabaseResult<T> {
    fn api(self) -> ApiResult<T>;
}
impl<T> DatabaseResult<T> for rusqlite::Result<T> {
    fn api(self) -> ApiResult<T> {
        self.map_err(|_| ApiError::internal())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Shared,
    Private,
}
impl Visibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Private => "private",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct MemoryRecord {
    pub id: String,
    pub assistant_id: String,
    pub content: String,
    pub revision: i64,
    pub visibility: Visibility,
    pub source_owner_id: Option<String>,
    pub source_conversation_id: Option<String>,
    pub source_message_id: Option<String>,
    pub source_type: &'static str,
    pub created_by: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateMemory {
    pub content: String,
    pub visibility: Visibility,
    pub source_conversation_id: Option<String>,
    pub source_message_id: Option<String>,
    #[serde(default)]
    pub publish: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateMemory {
    pub content: String,
    pub visibility: Option<Visibility>,
    #[serde(default)]
    pub publish: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ListOptions {
    pub q: Option<String>,
    pub visibility: Option<Visibility>,
    pub source_conversation_id: Option<String>,
    pub limit: usize,
    pub offset: usize,
}
impl Default for ListOptions {
    fn default() -> Self {
        Self {
            q: None,
            visibility: None,
            source_conversation_id: None,
            limit: 50,
            offset: 0,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct MemoryPage {
    pub assistant_id: String,
    pub items: Vec<MemoryRecord>,
    pub total: i64,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug, Serialize)]
pub struct Forgotten {
    pub id: String,
    pub revision: i64,
    pub deleted: bool,
    pub deleted_at: i64,
    pub deletion_sequence: i64,
    pub cleanup: Cleanup,
}

#[derive(Debug, Serialize)]
pub struct Cleanup {
    pub memory_content: &'static str,
    pub conversation_history: &'static str,
    pub offline_backups: &'static str,
}

pub fn initialize(db: &mut Connection) -> anyhow::Result<()> {
    let tx = db.transaction()?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_schema_migrations(version INTEGER PRIMARY KEY,applied_at INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS memory_records(
           id TEXT PRIMARY KEY,assistant_id TEXT NOT NULL,content TEXT,revision INTEGER NOT NULL CHECK(revision>0),
           visibility TEXT NOT NULL CHECK(visibility IN ('shared','private')),source_owner_id TEXT,search_owner_token TEXT NOT NULL,
           source_conversation_id TEXT REFERENCES conversations(id),source_message_id TEXT REFERENCES messages(id),
           source_type TEXT NOT NULL CHECK(source_type='explicit'),created_by TEXT NOT NULL,
           created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,deleted_at INTEGER,deletion_sequence INTEGER,
           CHECK(visibility='shared' OR source_owner_id IS NOT NULL),
           CHECK((deleted_at IS NULL AND content IS NOT NULL) OR (deleted_at IS NOT NULL AND content IS NULL)));
         CREATE INDEX IF NOT EXISTS memory_scope ON memory_records(assistant_id,deleted_at,visibility,source_owner_id,updated_at);
         CREATE INDEX IF NOT EXISTS memory_source ON memory_records(assistant_id,source_conversation_id,deleted_at);
         CREATE TABLE IF NOT EXISTS memory_versions(
           memory_id TEXT NOT NULL REFERENCES memory_records(id),revision INTEGER NOT NULL,content TEXT,
           visibility TEXT NOT NULL,actor_id TEXT NOT NULL,source_message_id TEXT,
           change_kind TEXT NOT NULL CHECK(change_kind IN ('created','corrected','forgotten')),
           created_at INTEGER NOT NULL,PRIMARY KEY(memory_id,revision));
         CREATE TABLE IF NOT EXISTS memory_deletion_watermarks(
           assistant_id TEXT PRIMARY KEY,sequence INTEGER NOT NULL,updated_at INTEGER NOT NULL);
         CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
           content,scope_token,owner_token,assistant_id UNINDEXED,visibility UNINDEXED,source_owner_id UNINDEXED,
           tokenize='trigram');
         CREATE TRIGGER IF NOT EXISTS memory_fts_insert AFTER INSERT ON memory_records WHEN new.deleted_at IS NULL BEGIN
           INSERT INTO memory_fts(rowid,content,scope_token,owner_token,assistant_id,visibility,source_owner_id)
           VALUES(new.rowid,new.content,new.assistant_id,new.search_owner_token,new.assistant_id,new.visibility,new.source_owner_id);
         END;
         CREATE TRIGGER IF NOT EXISTS memory_fts_update AFTER UPDATE ON memory_records BEGIN
           DELETE FROM memory_fts WHERE rowid=old.rowid;
           INSERT INTO memory_fts(rowid,content,scope_token,owner_token,assistant_id,visibility,source_owner_id)
           SELECT new.rowid,new.content,new.assistant_id,new.search_owner_token,new.assistant_id,new.visibility,new.source_owner_id
           WHERE new.deleted_at IS NULL;
         END;
         CREATE TRIGGER IF NOT EXISTS memory_fts_delete AFTER DELETE ON memory_records BEGIN
           DELETE FROM memory_fts WHERE rowid=old.rowid;
         END;",
    )?;
    let version: i64 = tx.query_row(
        "SELECT coalesce(max(version),0) FROM memory_schema_migrations",
        [],
        |r| r.get(0),
    )?;
    anyhow::ensure!(version <= 1, "Unsupported memory schema version");
    // FTS secure-delete removes obsolete postings when correcting or forgetting.
    // The bundled SQLite is pinned and supports this FTS5 option.
    tx.execute(
        "INSERT INTO memory_fts(memory_fts,rank) VALUES('secure-delete',1)",
        [],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO settings(key,value) VALUES('assistant.identity.v1',?1)",
        [uuid::Uuid::new_v4().to_string()],
    )?;
    let id: String = tx.query_row(
        "SELECT value FROM settings WHERE key='assistant.identity.v1'",
        [],
        |r| r.get(0),
    )?;
    anyhow::ensure!(
        uuid::Uuid::parse_str(&id).is_ok(),
        "Invalid assistant identity"
    );
    tx.execute(
        "INSERT OR IGNORE INTO memory_schema_migrations(version,applied_at) VALUES(1,?1)",
        [now()],
    )?;
    crate::memory_context::initialize(&tx)?;
    let used: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM memory_records) OR EXISTS(SELECT 1 FROM memory_context_watermarks)", [], |r|r.get(0),
    )?;
    if used {
        mark_feature_used(&tx)
            .map_err(|_| anyhow::anyhow!("Unable to persist memory compatibility requirement"))?;
    }
    tx.commit()?;
    Ok(())
}

pub fn identity(db: &Connection) -> ApiResult<String> {
    db.query_row(
        "SELECT value FROM settings WHERE key='assistant.identity.v1'",
        [],
        |r| r.get(0),
    )
    .api()
}

fn record(row: &Row<'_>) -> rusqlite::Result<MemoryRecord> {
    let visibility: String = row.get(4)?;
    Ok(MemoryRecord {
        id: row.get(0)?,
        assistant_id: row.get(1)?,
        content: row.get(2)?,
        revision: row.get(3)?,
        visibility: if visibility == "shared" {
            Visibility::Shared
        } else {
            Visibility::Private
        },
        source_owner_id: row.get(5)?,
        source_conversation_id: row.get(6)?,
        source_message_id: row.get(7)?,
        source_type: "explicit",
        created_by: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn validate_content(content: &str) -> ApiResult<()> {
    if content.trim().is_empty() || content.len() > MAX_CONTENT_BYTES || content.contains('\0') {
        return Err(ApiError::bad(
            "invalid_memory_content",
            "记忆内容须为1到4000字节，且不能包含空字符",
        ));
    }
    Ok(())
}

fn revision_conflict() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "memory_revision_conflict",
        "记忆已被修改，请重新加载后合并",
    )
}

fn source_owner(
    db: &Connection,
    conversation: Option<&str>,
    message: Option<&str>,
) -> ApiResult<Option<String>> {
    let Some(conversation) = conversation else {
        if message.is_some() {
            return Err(ApiError::bad(
                "invalid_memory_source",
                "来源消息需要来源会话",
            ));
        }
        return Ok(None);
    };
    let owner = db
        .query_row(
            "SELECT owner_id FROM conversations WHERE id=?1",
            [conversation],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .api()?
        .ok_or_else(ApiError::not_found)?;
    let has_bindings: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='conversation_character_bindings')",
        [], |r|r.get(0),
    ).api()?;
    if has_bindings {
        let is_test: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM conversation_character_bindings WHERE conversation_id=?1 AND draft_card_json IS NOT NULL)",
            [conversation], |r|r.get(0),
        ).api()?;
        if is_test {
            return Err(ApiError::bad(
                "memory_test_source",
                "角色草稿测试会话不能作为长期记忆来源",
            ));
        }
    }
    if let Some(message) = message {
        let exists: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE id=?1 AND conversation_id=?2)",
                params![message, conversation],
                |r| r.get(0),
            )
            .api()?;
        if !exists {
            return Err(ApiError::bad(
                "invalid_memory_source",
                "来源消息不属于来源会话",
            ));
        }
    }
    Ok(Some(owner))
}

fn check_quota(db: &Connection, assistant: &str, private_owner: Option<&str>) -> ApiResult<()> {
    let count: i64 = db
        .query_row(
            "SELECT count(*) FROM memory_records WHERE assistant_id=?1 AND deleted_at IS NULL",
            [assistant],
            |r| r.get(0),
        )
        .api()?;
    if count >= MAX_RECORDS {
        return Err(ApiError::bad("memory_limit", "有效记忆已达到5000条上限"));
    }
    check_private_quota(db, assistant, private_owner)
}

fn check_private_quota(db: &Connection, assistant: &str, owner: Option<&str>) -> ApiResult<()> {
    if let Some(owner) = owner {
        let count: i64 = db.query_row(
            "SELECT count(*) FROM memory_records WHERE assistant_id=?1 AND visibility='private' AND source_owner_id=?2 AND deleted_at IS NULL",
            params![assistant, owner], |r| r.get(0),
        ).api()?;
        if count >= MAX_PRIVATE_RECORDS {
            return Err(ApiError::bad(
                "memory_limit",
                "该私有来源域有效记忆已达到500条上限",
            ));
        }
    }
    Ok(())
}

fn audit(db: &Connection, actor: &str, action: &str, id: &str) -> ApiResult<()> {
    db.execute(
        "INSERT INTO audit_events(actor,action,resource,created_at) VALUES(?1,?2,?3,?4)",
        params![actor, action, id, now()],
    )
    .api()?;
    Ok(())
}

fn mark_feature_used(db: &Connection) -> ApiResult<()> {
    let has_migrations: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='dsh_schema_migrations')", [], |r|r.get(0),
    ).api()?;
    if has_migrations {
        db.execute(
            "INSERT OR IGNORE INTO dsh_schema_migrations(version,created_at) VALUES(2,?1)",
            [now()],
        )
        .api()?;
    }
    Ok(())
}

fn mutation_in<T>(db: &Connection, operation: impl FnOnce() -> ApiResult<T>) -> ApiResult<T> {
    if db.is_autocommit() {
        return Err(ApiError::internal());
    }
    // Commands may commit an error reply in their outer transaction. Do not
    // leave a partially written memory or context invalidation behind on error.
    db.execute_batch("SAVEPOINT dsh_memory_mutation").api()?;
    match operation() {
        Ok(value) => {
            if db
                .execute_batch("RELEASE SAVEPOINT dsh_memory_mutation")
                .is_err()
            {
                let _ = db.execute_batch("ROLLBACK TO SAVEPOINT dsh_memory_mutation; RELEASE SAVEPOINT dsh_memory_mutation;");
                return Err(ApiError::internal());
            }
            Ok(value)
        }
        Err(error) => {
            db.execute_batch(
                "ROLLBACK TO SAVEPOINT dsh_memory_mutation; RELEASE SAVEPOINT dsh_memory_mutation;",
            )
            .api()?;
            Err(error)
        }
    }
}

pub fn create(db: &mut Connection, actor: &str, input: &CreateMemory) -> ApiResult<MemoryRecord> {
    let tx = db.transaction().api()?;
    let item = create_in(&tx, Access::Admin(actor), input)?;
    tx.commit().api()?;
    Ok(item)
}

/// Caller must hold a transaction when combining a command with its durable reply.
pub fn create_in(
    tx: &Connection,
    access: Access<'_>,
    input: &CreateMemory,
) -> ApiResult<MemoryRecord> {
    mutation_in(tx, || create_record(tx, access, input))
}

fn create_record(
    tx: &Connection,
    access: Access<'_>,
    input: &CreateMemory,
) -> ApiResult<MemoryRecord> {
    validate_content(&input.content)?;
    let actor = access.actor();
    if matches!(access, Access::Owner(_)) && input.visibility != Visibility::Private {
        return Err(ApiError::forbidden("只有实例管理员可以发布共享记忆"));
    }
    if input.visibility == Visibility::Shared && !input.publish {
        return Err(ApiError::bad(
            "publish_confirmation_required",
            "共享记忆需要明确确认发布",
        ));
    }
    let assistant = identity(tx)?;
    let owner = source_owner(
        tx,
        input.source_conversation_id.as_deref(),
        input.source_message_id.as_deref(),
    )?;
    if input.visibility == Visibility::Private && owner.is_none() {
        return Err(ApiError::bad(
            "private_source_required",
            "私有记忆需要来源会话",
        ));
    }
    if let Access::Owner(actor) = access {
        if owner.as_deref() != Some(actor) {
            return Err(ApiError::not_found());
        }
    }
    check_quota(
        tx,
        &assistant,
        if input.visibility == Visibility::Private {
            owner.as_deref()
        } else {
            None
        },
    )?;
    let id = uuid::Uuid::new_v4().to_string();
    let timestamp = now();
    tx.execute(
        "INSERT INTO memory_records(id,assistant_id,content,revision,visibility,source_owner_id,source_conversation_id,source_message_id,source_type,created_by,created_at,updated_at,search_owner_token) VALUES(?1,?2,?3,1,?4,?5,?6,?7,'explicit',?8,?9,?9,?10)",
        params![id, assistant, input.content, input.visibility.as_str(), owner, input.source_conversation_id, input.source_message_id, actor, timestamp, search_token(input.visibility, owner.as_deref())],
    ).api()?;
    tx.execute(
        "INSERT INTO memory_versions(memory_id,revision,content,visibility,actor_id,source_message_id,change_kind,created_at) VALUES(?1,1,?2,?3,?4,?5,'created',?6)",
        params![id,input.content,input.visibility.as_str(),actor,input.source_message_id,timestamp],
    ).api()?;
    audit(tx, actor, "memory.create", &id)?;
    if input.visibility == Visibility::Shared {
        audit(tx, actor, "memory.visibility.approve", &id)?;
    }
    mark_feature_used(tx)?;
    get_in(tx, &assistant, &id)
}

fn get_in(db: &Connection, assistant: &str, id: &str) -> ApiResult<MemoryRecord> {
    db.query_row(
        &format!("SELECT {COLUMNS} FROM memory_records m WHERE m.assistant_id=?1 AND m.id=?2 AND m.deleted_at IS NULL"),
        params![assistant,id], record,
    ).optional().api()?.ok_or_else(ApiError::not_found)
}

pub fn get(db: &Connection, id: &str) -> ApiResult<MemoryRecord> {
    get_in(db, &identity(db)?, id)
}

pub fn get_visible(db: &Connection, owner: &str, id: &str) -> ApiResult<MemoryRecord> {
    let item = get(db, id)?;
    if owner.is_empty()
        || (item.visibility == Visibility::Private
            && item.source_owner_id.as_deref() != Some(owner))
    {
        return Err(ApiError::not_found());
    }
    Ok(item)
}

pub fn update(
    db: &mut Connection,
    actor: &str,
    id: &str,
    expected: i64,
    input: &UpdateMemory,
) -> ApiResult<MemoryRecord> {
    let tx = db.transaction().api()?;
    let item = update_in(&tx, Access::Admin(actor), id, expected, input)?;
    tx.commit().api()?;
    Ok(item)
}

pub fn update_in(
    tx: &Connection,
    access: Access<'_>,
    id: &str,
    expected: i64,
    input: &UpdateMemory,
) -> ApiResult<MemoryRecord> {
    mutation_in(tx, || update_record(tx, access, id, expected, input))
}

fn update_record(
    tx: &Connection,
    access: Access<'_>,
    id: &str,
    expected: i64,
    input: &UpdateMemory,
) -> ApiResult<MemoryRecord> {
    validate_content(&input.content)?;
    let actor = access.actor();
    let assistant = identity(tx)?;
    let current = get_in(tx, &assistant, id)?;
    access.can_mutate(&current)?;
    if current.revision != expected {
        return Err(revision_conflict());
    }
    let visibility = input.visibility.unwrap_or(current.visibility);
    if matches!(access, Access::Owner(_)) && visibility != Visibility::Private {
        return Err(ApiError::forbidden("只有实例管理员可以发布共享记忆"));
    }
    if current.visibility == Visibility::Private
        && visibility == Visibility::Shared
        && !input.publish
    {
        return Err(ApiError::bad(
            "publish_confirmation_required",
            "将私有记忆转为共享需要明确确认发布",
        ));
    }
    if visibility == Visibility::Private {
        let owner = source_owner(
            tx,
            current.source_conversation_id.as_deref(),
            current.source_message_id.as_deref(),
        )?;
        if owner.is_none() || owner != current.source_owner_id {
            return Err(ApiError::bad(
                "private_source_required",
                "私有记忆需要有效的原始来源会话",
            ));
        }
        if current.visibility != visibility {
            check_private_quota(tx, &assistant, owner.as_deref())?;
        }
    }
    let timestamp = now();
    if input.content != current.content || visibility != current.visibility {
        crate::memory_context::invalidate(tx, &assistant, id).map_err(|_| ApiError::internal())?;
    }
    tx.execute(
        "UPDATE memory_records SET content=?1,visibility=?2,revision=revision+1,updated_at=?3,search_owner_token=?7 WHERE id=?4 AND assistant_id=?5 AND revision=?6 AND deleted_at IS NULL",
        params![input.content, visibility.as_str(), timestamp,id,assistant,expected,search_token(visibility,current.source_owner_id.as_deref())],
    ).api()?;
    tx.execute(
        "INSERT INTO memory_versions(memory_id,revision,content,visibility,actor_id,source_message_id,change_kind,created_at) VALUES(?1,?2,?3,?4,?5,?6,'corrected',?7)",
        params![id,expected+1,input.content,visibility.as_str(),actor,current.source_message_id,timestamp],
    ).api()?;
    audit(tx, actor, "memory.correct", id)?;
    if current.visibility != visibility {
        audit(
            tx,
            actor,
            if visibility == Visibility::Shared {
                "memory.visibility.approve"
            } else {
                "memory.visibility.restrict"
            },
            id,
        )?;
    }
    mark_feature_used(tx)?;
    get_in(tx, &assistant, id)
}

pub fn forget(db: &mut Connection, actor: &str, id: &str, expected: i64) -> ApiResult<Forgotten> {
    let tx = db.transaction().api()?;
    let item = forget_in(&tx, Access::Admin(actor), id, expected)?;
    tx.commit().api()?;
    Ok(item)
}

pub fn forget_in(
    tx: &Connection,
    access: Access<'_>,
    id: &str,
    expected: i64,
) -> ApiResult<Forgotten> {
    mutation_in(tx, || forget_record(tx, access, id, expected))
}

fn forget_record(
    tx: &Connection,
    access: Access<'_>,
    id: &str,
    expected: i64,
) -> ApiResult<Forgotten> {
    let actor = access.actor();
    let assistant = identity(tx)?;
    let current = get_in(tx, &assistant, id)?;
    access.can_mutate(&current)?;
    if current.revision != expected {
        return Err(revision_conflict());
    }
    let timestamp = now();
    crate::memory_context::invalidate(tx, &assistant, id).map_err(|_| ApiError::internal())?;
    let sequence: i64 = tx.query_row(
        "INSERT INTO memory_deletion_watermarks(assistant_id,sequence,updated_at) VALUES(?1,1,?2) ON CONFLICT(assistant_id) DO UPDATE SET sequence=sequence+1,updated_at=excluded.updated_at RETURNING sequence",
        params![assistant,timestamp], |r|r.get(0),
    ).api()?;
    tx.execute(
        "UPDATE memory_records SET content=NULL,revision=revision+1,deleted_at=?1,updated_at=?1,deletion_sequence=?2 WHERE assistant_id=?3 AND id=?4 AND revision=?5 AND deleted_at IS NULL",
        params![timestamp,sequence,assistant,id,expected],
    ).api()?;
    tx.execute(
        "UPDATE memory_versions SET content=NULL WHERE memory_id=?1",
        [id],
    )
    .api()?;
    tx.execute(
        "INSERT INTO memory_versions(memory_id,revision,content,visibility,actor_id,source_message_id,change_kind,created_at) VALUES(?1,?2,NULL,?3,?4,?5,'forgotten',?6)",
        params![id,expected+1,current.visibility.as_str(),actor,current.source_message_id,timestamp],
    ).api()?;
    audit(tx, actor, "memory.forget", id)?;
    mark_feature_used(tx)?;
    Ok(Forgotten {
        id: id.into(),
        revision: expected + 1,
        deleted: true,
        deleted_at: timestamp,
        deletion_sequence: sequence,
        cleanup: Cleanup {
            memory_content: "removed",
            conversation_history: "retained",
            offline_backups: "retained",
        },
    })
}

fn quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn validate_options(options: &ListOptions) -> ApiResult<String> {
    if !(1..=100).contains(&options.limit) || options.offset > 1_000_000 {
        return Err(ApiError::bad(
            "invalid_pagination",
            "每页须为1到100条，偏移不得超过1000000",
        ));
    }
    let query = options.q.as_deref().unwrap_or("").trim();
    if query.len() > 256 || query.contains('\0') {
        return Err(ApiError::bad(
            "invalid_memory_query",
            "检索词不得超过256字节或包含空字符",
        ));
    }
    Ok(query.into())
}

fn list_in(db: &Connection, owner: Option<&str>, options: &ListOptions) -> ApiResult<MemoryPage> {
    let query = validate_options(options)?;
    let assistant = identity(db)?;
    let mut conditions = String::from("m.assistant_id=?1 AND m.deleted_at IS NULL");
    let mut values: Vec<rusqlite::types::Value> = vec![assistant.clone().into()];
    if let Some(owner) = owner {
        values.push(owner.to_string().into());
        conditions.push_str(&format!(
            " AND (m.visibility='shared' OR (m.visibility='private' AND m.source_owner_id=?{}))",
            values.len()
        ));
    }
    if let Some(visibility) = options.visibility {
        values.push(visibility.as_str().to_string().into());
        conditions.push_str(&format!(" AND m.visibility=?{}", values.len()));
    }
    if let Some(source) = &options.source_conversation_id {
        values.push(source.clone().into());
        conditions.push_str(&format!(" AND m.source_conversation_id=?{}", values.len()));
    }
    let mut join = "";
    if query.chars().count() >= 3 {
        join = " JOIN memory_fts f ON f.rowid=m.rowid";
        // The assistant scope is part of MATCH, not a post-retrieval filter.
        // Runtime ACL also participates in SQL before ORDER/LIMIT/OFFSET.
        let mut expression = format!(
            "scope_token : {} AND content : {}",
            quoted(&assistant),
            quoted(&query)
        );
        if let Some(owner) = owner {
            // An exact hash is indexed separately so short owner IDs cannot weaken the FTS scope.
            let token = owner_token(owner);
            expression = format!(
                "({expression}) AND (owner_token : {} OR owner_token : {})",
                quoted(&token),
                quoted("publicmemory")
            );
        }
        values.push(expression.into());
        conditions.push_str(&format!(
            " AND memory_fts MATCH ?{} AND f.assistant_id=?1",
            values.len()
        ));
        if let Some(owner) = owner {
            values.push(owner.to_string().into());
            conditions.push_str(&format!(" AND (f.visibility='shared' OR (f.visibility='private' AND f.source_owner_id=?{}))",values.len()));
        }
    } else if !query.is_empty() {
        values.push(query.into());
        conditions.push_str(&format!(
            " AND instr(lower(m.content),lower(?{}))>0",
            values.len()
        ));
    }
    let total: i64 = db
        .query_row(
            &format!("SELECT count(*) FROM memory_records m{join} WHERE {conditions}"),
            rusqlite::params_from_iter(values.iter()),
            |r| r.get(0),
        )
        .api()?;
    let mut statement = db.prepare(&format!("SELECT {COLUMNS} FROM memory_records m{join} WHERE {conditions} ORDER BY m.updated_at DESC,m.rowid DESC LIMIT ?{} OFFSET ?{}",values.len()+1,values.len()+2)).api()?;
    values.push((options.limit as i64).into());
    values.push((options.offset as i64).into());
    let items = statement
        .query_map(rusqlite::params_from_iter(values.iter()), record)
        .api()?
        .collect::<rusqlite::Result<Vec<_>>>()
        .api()?;
    Ok(MemoryPage {
        assistant_id: assistant,
        items,
        total,
        limit: options.limit,
        offset: options.offset,
    })
}

pub fn admin_list(db: &Connection, options: &ListOptions) -> ApiResult<MemoryPage> {
    list_in(db, None, options)
}

/// Runtime reads are separate from instance-administrator management reads.
pub fn visible_list(db: &Connection, owner: &str, options: &ListOptions) -> ApiResult<MemoryPage> {
    if owner.is_empty() {
        return Err(ApiError::forbidden("记忆检索需要真实来源主体"));
    }
    list_in(db, Some(owner), options)
}

/// Bounded runtime retrieval. Callers must project these records as untrusted data.
pub fn retrieve(
    db: &Connection,
    owner: &str,
    query: &str,
    limit: usize,
    byte_budget: usize,
) -> ApiResult<Vec<MemoryRecord>> {
    let options = ListOptions {
        q: Some(query.into()),
        limit,
        ..Default::default()
    };
    let page = visible_list(db, owner, &options)?;
    let mut remaining = byte_budget;
    let mut result = Vec::new();
    for item in page.items {
        if item.content.len() <= remaining {
            remaining -= item.content.len();
            result.push(item);
        }
    }
    Ok(result)
}

pub fn export(db: &Connection) -> ApiResult<serde_json::Value> {
    let assistant = identity(db)?;
    let mut statement = db.prepare(&format!("SELECT {COLUMNS} FROM memory_records m WHERE m.assistant_id=?1 AND m.deleted_at IS NULL ORDER BY m.created_at,m.rowid")).api()?;
    let items = statement
        .query_map([&assistant], record)
        .api()?
        .collect::<rusqlite::Result<Vec<_>>>()
        .api()?;
    Ok(serde_json::json!({
        "schema":"dsh.assistant-memory.v1", "generated_at":now(),
        "scope":{"kind":"assistant","assistant_id":assistant,"access":"instance_admin"},
        "items":items,
    }))
}

fn owner_token(owner: &str) -> String {
    format!("o{}", hex::encode(Sha256::digest(owner.as_bytes())))
}

fn search_token(visibility: Visibility, owner: Option<&str>) -> String {
    if visibility == Visibility::Shared {
        "publicmemory".into()
    } else {
        owner_token(owner.unwrap_or(""))
    }
}

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;
