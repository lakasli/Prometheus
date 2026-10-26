//! Trusted scope and provenance for short-lived conversation state.
//! This module grants no resource, scheduling, or message-sending permission.
use anyhow::{ensure, Context};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceRef {
    pub kind: String,
    pub id: String,
    pub revision: i64,
    pub epoch: Option<i64>,
    pub digest: Option<String>,
}

/// Only scope_for_run_in constructs this value. Public getters are for host
/// services, not fields the model or an HTTP caller can supply.
#[derive(Clone, Debug, PartialEq)]
pub struct VerifiedConversationScope {
    assistant_id: String,
    owner_id: String,
    conversation_id: String,
    speaker_scope: Option<String>,
    source_run_id: String,
    source_run_rowid: i64,
    input_seq: Option<i64>,
    created_at: i64,
    source_versions: Value,
    key: String,
    management_actor_id: Option<String>,
}
impl VerifiedConversationScope {
    pub fn assistant_id(&self) -> &str {
        &self.assistant_id
    }
    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }
    pub fn conversation_id(&self) -> &str {
        &self.conversation_id
    }
    pub fn speaker_scope(&self) -> Option<&str> {
        self.speaker_scope.as_deref()
    }
    pub fn source_run_id(&self) -> &str {
        &self.source_run_id
    }
    pub fn source_run_rowid(&self) -> i64 {
        self.source_run_rowid
    }
    pub fn input_seq(&self) -> Option<i64> {
        self.input_seq
    }
    pub fn created_at(&self) -> i64 {
        self.created_at
    }
    pub fn source_versions(&self) -> &Value {
        &self.source_versions
    }
    pub fn key(&self) -> &str {
        &self.key
    }
    pub fn management_actor_id(&self) -> Option<&str> {
        self.management_actor_id.as_deref()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EntityRef {
    pub kind: String,
    pub id: String,
    pub revision: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateEntity {
    pub reference: EntityRef,
    pub content: Value,
    pub created_at: i64,
    pub expires_at: i64,
    pub source_refs: Vec<SourceRef>,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct InvalidationResult {
    pub entities: usize,
    pub scopes: usize,
    pub affected_run_ids: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateUsageSnapshot {
    pub version: u32,
    pub scope_ref: String,
    pub epoch: i64,
    pub entities: Vec<EntityRef>,
    pub source_refs: Vec<SourceRef>,
    pub frozen_at: i64,
}

pub(crate) fn transaction_required(db: &Connection) -> anyhow::Result<()> {
    ensure!(
        !db.is_autocommit(),
        "conversation state requires a transaction"
    );
    Ok(())
}
pub(crate) fn table_exists(db: &Connection, name: &str) -> anyhow::Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
        [name],
        |r| r.get(0),
    )?)
}
pub(crate) fn digest(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}
pub(crate) fn bounded_text(text: &str, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}
fn versions_in(db: &Connection, owner: &str, conversation: &str) -> anyhow::Result<Value> {
    let deletion = if table_exists(db, "diary_meta")? {
        db.query_row(
            "SELECT deletion_epoch FROM diary_meta WHERE id=1",
            [],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
    } else {
        None
    };
    let policy = if table_exists(db, "diary_source_policies")? {
        db.query_row("SELECT revision,enabled,allow_model FROM diary_source_policies WHERE source_owner_id=?1",[owner],|r|Ok(json!({"revision":r.get::<_,i64>(0)?,"enabled":r.get::<_,bool>(1)?,"allow_model":r.get::<_,bool>(2)?}))).optional()?
    } else {
        None
    };
    let revocation = if table_exists(db, "diary_source_revocations")? {
        db.query_row("SELECT epoch,through_event_rowid FROM diary_source_revocations WHERE source_owner_id=?1",[owner],|r|Ok(json!({"epoch":r.get::<_,i64>(0)?,"through_event_rowid":r.get::<_,i64>(1)?}))).optional()?
    } else {
        None
    };
    let floor = if table_exists(db, "memory_context_watermarks")? {
        db.query_row(
            "SELECT through_run_rowid FROM memory_context_watermarks WHERE conversation_id=?1",
            [conversation],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
    } else {
        None
    };
    let identity = if table_exists(db, "relationship_identities")? {
        db.query_row("SELECT i.id,i.person_id,p.revision,i.confirmed_at FROM relationship_identities i JOIN relationship_people p ON p.id=i.person_id WHERE i.conversation_id=?1 AND i.source_owner_id=?2 AND i.deleted_at IS NULL AND p.deleted_at IS NULL AND p.merged_into IS NULL AND (p.visibility='shared' OR p.source_owner_id=?2)",params![conversation,owner],|r|Ok(json!({"id":r.get::<_,String>(0)?,"person_id":r.get::<_,String>(1)?,"person_revision":r.get::<_,i64>(2)?,"confirmed_at":r.get::<_,i64>(3)?}))).optional()?
    } else {
        None
    };
    Ok(
        json!({"deletion_epoch":deletion,"source_policy":policy,"source_revocation":revocation,"history_floor":floor,"identity_mapping":identity}),
    )
}

pub fn scope_for_run_in(db: &Connection, run: &str) -> anyhow::Result<VerifiedConversationScope> {
    build_scope_in(db, run, None)
}
/// Authentication/CSRF remain the management router's responsibility. This
/// restores source ownership for an actual administrator without minting a new
/// model prompt proof or changing that source's permissions.
pub fn management_scope_for_source_in(
    db: &Connection,
    admin: &crate::auth::Admin,
    run: &str,
) -> anyhow::Result<VerifiedConversationScope> {
    ensure!(
        admin.role == "admin"
            && db.query_row(
                "SELECT EXISTS(SELECT 1 FROM admins WHERE id=?1 AND username=?2)",
                params![admin.id, admin.username],
                |r| r.get::<_, bool>(0)
            )?,
        "management scope requires an authenticated instance administrator"
    );
    build_scope_in(db, run, Some(&admin.id))
}
fn build_scope_in(
    db: &Connection,
    run: &str,
    management_actor: Option<&str>,
) -> anyhow::Result<VerifiedConversationScope> {
    transaction_required(db)?;
    if management_actor.is_none() {
        crate::conversation_identity::freeze_run_in(db, run)?;
    }
    let(assistant,owner,conversation,rowid,created_at,kind,snapshot):(String,String,String,i64,i64,String,Option<String>)=db.query_row("SELECT s.value,r.owner_id,r.conversation_id,r.rowid,r.created_at,r.run_kind,r.character_snapshot_json FROM runs r JOIN conversations c ON c.id=r.conversation_id AND c.owner_id=r.owner_id JOIN settings s ON s.key='assistant.identity.v1' WHERE r.id=?1",[run],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)))?;
    ensure!(
        matches!(kind.as_str(), "model" | "skill_text"),
        "control or automatic run cannot create conversation state"
    );
    if management_actor.is_some() {
        let binding: String = db.query_row(
            "SELECT binding_json FROM run_conversation_bindings WHERE run_id=?1",
            [run],
            |r| r.get(0),
        )?;
        let binding: Value = serde_json::from_str(&binding)?;
        ensure!(
            binding["assistant_id"] == assistant
                && binding["source_owner_id"] == owner
                && binding["conversation_id"] == conversation,
            "saved management source binding is inconsistent"
        );
    }
    if let Some(snapshot) = snapshot {
        let snapshot: Value = serde_json::from_str(&snapshot)?;
        ensure!(
            snapshot["is_test"] != true,
            "draft test cannot create conversation state"
        );
    }
    if table_exists(db, "conversation_character_bindings")? {
        ensure!(!db.query_row("SELECT EXISTS(SELECT 1 FROM conversation_character_bindings WHERE conversation_id=?1 AND draft_card_json IS NOT NULL)",[&conversation],|r|r.get::<_,bool>(0))?,"draft conversation has no live focus");
    }
    let group = owner.starts_with("qq:")
        && (owner.contains(":group:") || owner.contains(":channel:") || owner.contains(":dms:"));
    let speaker_scope = if group {
        let speakers =
            crate::conversation_identity::message_speakers_in(db, run, &conversation, &owner)?;
        let known = speakers
            .iter()
            .filter_map(|v| v["speaker_ref"].as_str())
            .collect::<std::collections::BTreeSet<_>>();
        if !speakers.is_empty()
            && known.len() == 1
            && speakers.iter().all(|v| v["speaker_ref"].is_string())
        {
            known.first().map(|v| (*v).to_string())
        } else {
            None
        }
    } else {
        Some(format!("private:{}", digest(&owner)))
    };
    let input_seq = if table_exists(db, "conversation_inputs")? {
        db.query_row("SELECT max(input_seq) FROM conversation_inputs WHERE run_id=?1 AND owner_id=?2 AND conversation_id=?3",params![run,owner,conversation],|r|r.get::<_,Option<i64>>(0))?
    } else {
        None
    };
    let versions = versions_in(db, &owner, &conversation)?;
    let key = digest(&serde_json::to_string(&(
        &assistant,
        &owner,
        &conversation,
        &speaker_scope,
    ))?);
    Ok(VerifiedConversationScope {
        assistant_id: assistant,
        owner_id: owner,
        conversation_id: conversation,
        speaker_scope,
        source_run_id: run.to_string(),
        source_run_rowid: rowid,
        input_seq,
        created_at,
        source_versions: versions,
        key,
        management_actor_id: management_actor.map(str::to_owned),
    })
}
pub fn validate_scope_in(
    db: &Connection,
    scope: &VerifiedConversationScope,
) -> anyhow::Result<bool> {
    transaction_required(db)?;
    if let Some(actor) = scope.management_actor_id() {
        if !db.query_row(
            "SELECT EXISTS(SELECT 1 FROM admins WHERE id=?1)",
            [actor],
            |r| r.get::<_, bool>(0),
        )? {
            return Ok(false);
        }
    }
    Ok(
        build_scope_in(db, scope.source_run_id(), scope.management_actor_id())
            .is_ok_and(|current| current == *scope),
    )
}
pub fn run_input_source_in(
    db: &Connection,
    scope: &VerifiedConversationScope,
) -> anyhow::Result<SourceRef> {
    transaction_required(db)?;
    let content:String=db.query_row("SELECT coalesce(m.content,r.input) FROM runs r LEFT JOIN messages m ON m.run_id=r.id AND m.role='user' AND m.conversation_id=r.conversation_id WHERE r.id=?1 AND r.owner_id=?2 AND r.conversation_id=?3",params![scope.source_run_id(),scope.owner_id(),scope.conversation_id()],|r|r.get(0))?;
    // Input has no mutable revision column: v1 plus the exact content digest is
    // the evidence contract, not an invented platform revision.
    Ok(SourceRef {
        kind: "run_input".into(),
        id: scope.source_run_id().into(),
        revision: 1,
        epoch: None,
        digest: Some(digest(&content)),
    })
}
fn check_digest(source: &SourceRef, content: &str) -> bool {
    source
        .digest
        .as_ref()
        .is_none_or(|expected| expected == &digest(content))
}
pub fn validate_source_in(
    db: &Connection,
    scope: &VerifiedConversationScope,
    source: &SourceRef,
) -> anyhow::Result<bool> {
    transaction_required(db)?;
    if source.id.is_empty() || source.id.len() > 512 || source.revision < 0 {
        return Ok(false);
    }
    if table_exists(db,"conversation_state_source_tombstones")?&&db.query_row("SELECT EXISTS(SELECT 1 FROM conversation_state_source_tombstones WHERE source_kind=?1 AND source_id=?2 AND through_revision>=?3)",params![source.kind,source.id,source.revision],|r|r.get::<_,bool>(0))?{return Ok(false)}
    if let Some(epoch) = source.epoch {
        if versions_in(db, scope.owner_id(), scope.conversation_id())?["deletion_epoch"].as_i64()
            != Some(epoch)
        {
            return Ok(false);
        }
    }
    match source.kind.as_str() {
        "run_input" => {
            let row:Option<(i64,String,String,String)>=db.query_row("SELECT r.rowid,coalesce(m.content,r.input),r.status,r.run_kind FROM runs r JOIN conversations c ON c.id=r.conversation_id AND c.owner_id=r.owner_id LEFT JOIN messages m ON m.run_id=r.id AND m.role='user' AND m.conversation_id=r.conversation_id WHERE r.id=?1 AND r.owner_id=?2 AND r.conversation_id=?3 AND coalesce(CASE WHEN json_valid(r.character_snapshot_json) THEN json_extract(r.character_snapshot_json,'$.is_test') ELSE 0 END,0)=0",params![source.id,scope.owner_id(),scope.conversation_id()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
            let Some((rowid, text, status, kind)) = row else {
                return Ok(false);
            };
            let floor = versions_in(db, scope.owner_id(), scope.conversation_id())?
                ["history_floor"]
                .as_i64()
                .unwrap_or(0);
            Ok(source.revision == 1
                && source.digest.is_some()
                && check_digest(source, &text)
                && matches!(kind.as_str(), "model" | "skill_text")
                && rowid <= scope.source_run_rowid()
                && (rowid > floor
                    || (source.id == scope.source_run_id()
                        && matches!(status.as_str(), "queued" | "running"))))
        }
        "run_delivery" => {
            let real:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND owner_id=?2 AND conversation_id=?3 AND rowid<=?4 AND run_kind IN ('model','skill_text') AND coalesce(CASE WHEN json_valid(character_snapshot_json) THEN json_extract(character_snapshot_json,'$.is_test') ELSE 0 END,0)=0)",params![source.id,scope.owner_id(),scope.conversation_id(),scope.source_run_rowid()],|r|r.get(0))?;
            Ok(real
                && crate::delivery_context::project_run_in(
                    db,
                    scope.owner_id(),
                    scope.conversation_id(),
                    &source.id,
                )
                .is_ok_and(|p| {
                    p.delivery.delivery_revision == source.revision
                        && source.digest.as_deref() == Some(p.delivery.projection_digest.as_str())
                }))
        }
        "diary_event" => Ok(crate::diary::event(db, &source.id, Some(scope.owner_id()))
            .is_ok_and(|e| e.revision == source.revision && check_digest(source, &e.content))),
        "diary_entry" => Ok(crate::diary::entry(db, &source.id, Some(scope.owner_id()))
            .is_ok_and(|e| e.revision == source.revision && check_digest(source, &e.content))),
        "memory" => {
            let valid = crate::memory::get_visible(db, scope.owner_id(), &source.id)
                .is_ok_and(|m| m.revision == source.revision && check_digest(source, &m.content));
            Ok(valid
                && (!table_exists(db, "diary_events")?
                    || crate::diary::event(
                        db,
                        &format!("legacy:{}", source.id),
                        Some(scope.owner_id()),
                    )
                    .is_ok()))
        }
        "person_assertion" => {
            let value:Option<(i64,String)>=db.query_row("SELECT a.revision,a.value_json FROM relationship_assertions a JOIN relationship_people p ON p.id=a.person_id WHERE a.id=?1 AND a.deleted_at IS NULL AND a.status='approved' AND p.deleted_at IS NULL AND (a.visibility='shared' OR a.source_owner_id=?2) AND (p.visibility='shared' OR p.source_owner_id=?2) AND (a.valid_from IS NULL OR a.valid_from<=?3) AND (a.valid_until IS NULL OR a.valid_until>?3)",params![source.id,scope.owner_id(),crate::state::now()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            Ok(value.is_some_and(|(revision, value)| {
                revision == source.revision && check_digest(source, &value)
            }))
        }
        "person_identity" => {
            let value:Option<(i64,String,String,i64)>=db.query_row("SELECT p.revision,i.id,i.person_id,i.confirmed_at FROM relationship_identities i JOIN relationship_people p ON p.id=i.person_id WHERE i.id=?1 AND i.source_owner_id=?2 AND i.conversation_id=?3 AND i.deleted_at IS NULL AND p.deleted_at IS NULL AND p.merged_into IS NULL AND (p.visibility='shared' OR p.source_owner_id=?2)",params![source.id,scope.owner_id(),scope.conversation_id()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
            Ok(value.is_some_and(|(revision, id, person, at)| {
                revision == source.revision
                    && check_digest(
                        source,
                        &json!({"id":id,"person_id":person,"confirmed_at":at}).to_string(),
                    )
            }))
        }
        _ => Ok(false),
    }
}

pub fn initialize(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS conversation_state_heads(
      scope_key TEXT PRIMARY KEY,assistant_id TEXT NOT NULL,owner_id TEXT NOT NULL,conversation_id TEXT NOT NULL,speaker_scope TEXT,
      revision INTEGER NOT NULL DEFAULT 1,epoch INTEGER NOT NULL DEFAULT 0,last_input_seq INTEGER NOT NULL DEFAULT 0,last_run_rowid INTEGER NOT NULL DEFAULT 0);
      CREATE INDEX IF NOT EXISTS conversation_state_head_conversation ON conversation_state_heads(conversation_id,owner_id);
      CREATE TABLE IF NOT EXISTS conversation_state_entities(
      id TEXT PRIMARY KEY,kind TEXT NOT NULL CHECK(kind IN ('focus','correction')),scope_key TEXT NOT NULL REFERENCES conversation_state_heads(scope_key),revision INTEGER NOT NULL,
      status TEXT NOT NULL,content_json TEXT,created_at INTEGER NOT NULL,expires_at INTEGER NOT NULL,policy_version INTEGER NOT NULL);
      CREATE INDEX IF NOT EXISTS conversation_state_entity_scope ON conversation_state_entities(scope_key,kind,status,expires_at);
      CREATE TABLE IF NOT EXISTS conversation_state_sources(
      entity_id TEXT NOT NULL REFERENCES conversation_state_entities(id),source_kind TEXT NOT NULL,source_id TEXT NOT NULL,source_revision INTEGER NOT NULL,source_json TEXT NOT NULL,
      PRIMARY KEY(entity_id,source_kind,source_id));
      CREATE INDEX IF NOT EXISTS conversation_state_source_lookup ON conversation_state_sources(source_kind,source_id,source_revision);
      CREATE TABLE IF NOT EXISTS conversation_state_observations(run_id TEXT PRIMARY KEY,result_json TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS conversation_state_uses(run_id TEXT NOT NULL,entity_id TEXT NOT NULL,entity_revision INTEGER NOT NULL,PRIMARY KEY(run_id,entity_id));
      CREATE TABLE IF NOT EXISTS conversation_state_snapshots(run_id TEXT PRIMARY KEY,scope_key TEXT NOT NULL,snapshot_json TEXT NOT NULL,invalidated_at INTEGER);
      CREATE TABLE IF NOT EXISTS conversation_state_source_tombstones(source_kind TEXT NOT NULL,source_id TEXT NOT NULL,through_revision INTEGER NOT NULL,invalidated_at INTEGER NOT NULL,PRIMARY KEY(source_kind,source_id));")?;
    Ok(())
}
pub(crate) fn ensure_head_in(
    db: &Connection,
    scope: &VerifiedConversationScope,
) -> anyhow::Result<(i64, i64)> {
    transaction_required(db)?;
    db.execute("INSERT OR IGNORE INTO conversation_state_heads(scope_key,assistant_id,owner_id,conversation_id,speaker_scope) VALUES(?1,?2,?3,?4,?5)",params![scope.key(),scope.assistant_id(),scope.owner_id(),scope.conversation_id(),scope.speaker_scope()])?;
    Ok(db.query_row("SELECT revision,epoch FROM conversation_state_heads WHERE scope_key=?1 AND owner_id=?2 AND conversation_id=?3",params![scope.key(),scope.owner_id(),scope.conversation_id()],|r|Ok((r.get(0)?,r.get(1)?)))?)
}
pub(crate) fn insert_entity_in(
    db: &Connection,
    scope: &VerifiedConversationScope,
    id: &str,
    kind: &str,
    content: &Value,
    sources: &[SourceRef],
    validity: std::ops::Range<i64>,
) -> anyhow::Result<()> {
    let (at, expires) = (validity.start, validity.end);
    transaction_required(db)?;
    ensure_head_in(db, scope)?;
    ensure!(
        scope.speaker_scope().is_some(),
        "unknown speaker has no inheritable conversation state"
    );
    ensure!(
        matches!(kind, "focus" | "correction")
            && sources.len() <= 16
            && expires > at
            && expires - at <= 86400,
        "invalid bounded conversation entity"
    );
    let body = serde_json::to_string(content)?;
    ensure!(
        body.len() <= 4096,
        "conversation entity exceeds byte budget"
    );
    for source in sources {
        ensure!(
            validate_source_in(db, scope, source)?,
            "conversation source is stale or inaccessible"
        );
    }
    let inserted = db.execute(
        "INSERT OR IGNORE INTO conversation_state_entities VALUES(?1,?2,?3,1,'active',?4,?5,?6,1)",
        params![id, kind, scope.key(), body, at, expires],
    )?;
    if inserted != 0 {
        for source in sources {
            db.execute(
                "INSERT INTO conversation_state_sources VALUES(?1,?2,?3,?4,?5)",
                params![
                    id,
                    source.kind,
                    source.id,
                    source.revision,
                    serde_json::to_string(source)?
                ],
            )?;
        }
        db.execute(
            "UPDATE conversation_state_heads SET revision=revision+1 WHERE scope_key=?1",
            [scope.key()],
        )?;
    }
    Ok(())
}
pub(crate) fn entities_in(
    db: &Connection,
    scope: &VerifiedConversationScope,
    kind: &str,
    at: i64,
) -> anyhow::Result<Vec<StateEntity>> {
    transaction_required(db)?;
    if scope.speaker_scope().is_none() {
        return Ok(Vec::new());
    }
    let mut statement=db.prepare("SELECT id,revision,content_json,created_at,expires_at FROM conversation_state_entities WHERE scope_key=?1 AND kind=?2 AND status='active' AND content_json IS NOT NULL AND created_at<=?3 AND expires_at>?3 ORDER BY created_at DESC,rowid DESC LIMIT 16")?;
    let rows = statement
        .query_map(params![scope.key(), kind, at], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut result = Vec::new();
    for (id, revision, body, created_at, expires_at) in rows {
        let mut source_statement=db.prepare("SELECT source_json FROM conversation_state_sources WHERE entity_id=?1 ORDER BY source_kind,source_id")?;
        let texts = source_statement
            .query_map([&id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let sources = texts
            .into_iter()
            .map(|s| serde_json::from_str::<SourceRef>(&s))
            .collect::<Result<Vec<_>, _>>()?;
        let mut allowed = true;
        for source in &sources {
            if !validate_source_in(db, scope, source)? {
                allowed = false;
                break;
            }
        }
        if allowed {
            result.push(StateEntity {
                reference: EntityRef {
                    kind: kind.into(),
                    id,
                    revision,
                },
                content: serde_json::from_str(&body)?,
                created_at,
                expires_at,
                source_refs: sources,
            });
        }
    }
    Ok(result)
}
pub(crate) fn retire_entities_in(
    db: &Connection,
    scope: &VerifiedConversationScope,
    kind: &str,
    reason: &str,
    keep: usize,
) -> anyhow::Result<usize> {
    transaction_required(db)?;
    ensure!(
        matches!(reason, "closed" | "superseded") && keep <= 4,
        "invalid conversation retirement"
    );
    let changed=db.execute("UPDATE conversation_state_entities SET content_json=NULL,status=?1,revision=revision+1 WHERE id IN(SELECT id FROM conversation_state_entities WHERE scope_key=?2 AND kind=?3 AND status='active' ORDER BY created_at DESC,rowid DESC LIMIT -1 OFFSET ?4)",params![reason,scope.key(),kind,keep as i64])?;
    if changed > 0 {
        db.execute(
            "UPDATE conversation_state_heads SET revision=revision+1 WHERE scope_key=?1",
            [scope.key()],
        )?;
    }
    Ok(changed)
}
fn clear_entities_in(
    db: &Connection,
    ids: &[String],
    at: i64,
    whole_scope: bool,
) -> anyhow::Result<InvalidationResult> {
    transaction_required(db)?;
    let mut result = InvalidationResult::default();
    let mut scopes = std::collections::BTreeSet::new();
    let mut runs = std::collections::BTreeSet::new();
    for id in ids {
        let key:Option<String>=db.query_row("SELECT scope_key FROM conversation_state_entities WHERE id=?1 AND status!='invalidated'",[id],|r|r.get(0)).optional()?;
        if let Some(key) = key {
            scopes.insert(key);
            result.entities+=db.execute("UPDATE conversation_state_entities SET content_json=NULL,status='invalidated',revision=revision+1 WHERE id=?1",[id])?;
        }
        let mut statement =
            db.prepare("SELECT run_id FROM conversation_state_uses WHERE entity_id=?1")?;
        for run in statement.query_map([id], |r| r.get::<_, String>(0))? {
            runs.insert(run?);
        }
    }
    for key in &scopes {
        // A source correction fences only its actual consumers. Advancing the
        // scope epoch here would also reject unrelated frozen entity versions
        // from this conversation, even though they never used that source.
        db.execute("UPDATE conversation_state_heads SET epoch=epoch+?1,revision=revision+1 WHERE scope_key=?2",params![i64::from(whole_scope),key])?;
    }
    for run in &runs {
        db.execute(
            "UPDATE conversation_state_snapshots SET invalidated_at=?1 WHERE run_id=?2",
            params![at, run],
        )?;
        if table_exists(db, "run_prompt_snapshots")? {
            db.execute("DELETE FROM run_prompt_snapshots WHERE run_id=?1", [run])?;
        }
    }
    result.scopes = scopes.len();
    result.affected_run_ids = runs.into_iter().collect();
    Ok(result)
}
pub fn invalidate_source_in(
    db: &Connection,
    kind: &str,
    id: &str,
    through_revision: Option<i64>,
    at: i64,
) -> anyhow::Result<InvalidationResult> {
    transaction_required(db)?;
    let revision = through_revision.unwrap_or(i64::MAX);
    db.execute("INSERT INTO conversation_state_source_tombstones VALUES(?1,?2,?3,?4) ON CONFLICT(source_kind,source_id) DO UPDATE SET through_revision=max(through_revision,excluded.through_revision),invalidated_at=excluded.invalidated_at",params![kind,id,revision,at])?;
    let ids = {
        let mut s=db.prepare("SELECT DISTINCT entity_id FROM conversation_state_sources WHERE source_kind=?1 AND source_id=?2 AND source_revision<=?3")?;
        let ids = s
            .query_map(params![kind, id, revision], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids
    };
    clear_entities_in(db, &ids, at, false)
}
pub fn invalidate_scope_in(
    db: &Connection,
    scope: &VerifiedConversationScope,
    at: i64,
) -> anyhow::Result<InvalidationResult> {
    transaction_required(db)?;
    let ids = {
        let mut s=db.prepare("SELECT id FROM conversation_state_entities WHERE scope_key=?1 AND status!='invalidated'")?;
        let ids = s
            .query_map([scope.key()], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids
    };
    clear_entities_in(db, &ids, at, true)
}
pub fn invalidate_conversation_in(
    db: &Connection,
    owner: &str,
    conversation: &str,
    at: i64,
) -> anyhow::Result<InvalidationResult> {
    transaction_required(db)?;
    let ids = {
        let mut s=db.prepare("SELECT e.id FROM conversation_state_entities e JOIN conversation_state_heads h ON h.scope_key=e.scope_key WHERE h.owner_id=?1 AND h.conversation_id=?2 AND e.status!='invalidated'")?;
        let ids = s
            .query_map(params![owner, conversation], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids
    };
    clear_entities_in(db, &ids, at, true)
}
pub fn expire_in(db: &Connection, at: i64, limit: usize) -> anyhow::Result<usize> {
    transaction_required(db)?;
    ensure!((1..=128).contains(&limit), "invalid state expiry batch");
    Ok(db.execute("UPDATE conversation_state_entities SET content_json=NULL,status='expired',revision=revision+1 WHERE id IN(SELECT id FROM conversation_state_entities WHERE expires_at<=?1 AND content_json IS NOT NULL ORDER BY expires_at LIMIT ?2)",params![at,limit as i64])?)
}
pub fn record_usage_in(
    db: &Connection,
    run: &str,
    entities: &[EntityRef],
    at: i64,
) -> anyhow::Result<StateUsageSnapshot> {
    transaction_required(db)?;
    ensure!(
        entities.len() <= 12,
        "too many conversation state references"
    );
    let scope = scope_for_run_in(db, run)?;
    let (_, epoch) = ensure_head_in(db, &scope)?;
    if let Some(snapshot)=db.query_row("SELECT snapshot_json FROM conversation_state_snapshots WHERE run_id=?1 AND invalidated_at IS NULL",[run],|r|r.get::<_,String>(0)).optional()?{
        let snapshot:StateUsageSnapshot=serde_json::from_str(&snapshot)?;ensure!(snapshot.entities==entities,"cannot rewrite frozen conversation state usage");return Ok(snapshot)
    }
    ensure!(
        !db.query_row(
            "SELECT EXISTS(SELECT 1 FROM conversation_state_snapshots WHERE run_id=?1)",
            [run],
            |r| r.get::<_, bool>(0)
        )?,
        "conversation state snapshot was invalidated"
    );
    let mut sources = Vec::new();
    for entity in entities {
        let available = entities_in(db, &scope, &entity.kind, at)?
            .into_iter()
            .find(|e| e.reference == *entity)
            .context("conversation entity is stale or inaccessible")?;
        sources.extend(available.source_refs);
        db.execute(
            "INSERT INTO conversation_state_uses VALUES(?1,?2,?3)",
            params![run, entity.id, entity.revision],
        )?;
    }
    sources.sort_by(|a, b| (&a.kind, &a.id, a.revision).cmp(&(&b.kind, &b.id, b.revision)));
    sources.dedup();
    let snapshot = StateUsageSnapshot {
        version: 1,
        scope_ref: scope.key().into(),
        epoch,
        entities: entities.to_vec(),
        source_refs: sources,
        frozen_at: at,
    };
    db.execute(
        "INSERT INTO conversation_state_snapshots VALUES(?1,?2,?3,NULL)",
        params![run, scope.key(), serde_json::to_string(&snapshot)?],
    )?;
    Ok(snapshot)
}
pub fn validate_context_in(db: &Connection, run: &str) -> anyhow::Result<bool> {
    validate_context_at_in(db, run, crate::state::now())
}
pub fn validate_context_at_in(db: &Connection, run: &str, at: i64) -> anyhow::Result<bool> {
    transaction_required(db)?;
    let scope = scope_for_run_in(db, run)?;
    let snapshot:Option<String>=db.query_row("SELECT snapshot_json FROM conversation_state_snapshots WHERE run_id=?1 AND invalidated_at IS NULL",[run],|r|r.get(0)).optional()?;
    let Some(snapshot) = snapshot else {
        return Ok(false);
    };
    let snapshot: StateUsageSnapshot = serde_json::from_str(&snapshot)?;
    if snapshot.scope_ref != scope.key() {
        return Ok(false);
    }
    let (_, epoch) = ensure_head_in(db, &scope)?;
    if epoch != snapshot.epoch {
        return Ok(false);
    }
    if at < snapshot.frozen_at {
        return Ok(false);
    }
    for entity in &snapshot.entities {
        let live:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM conversation_state_entities WHERE id=?1 AND kind=?2 AND scope_key=?3 AND revision=?4 AND status='active' AND content_json IS NOT NULL AND created_at<=?5 AND expires_at>?5)",params![entity.id,entity.kind,scope.key(),entity.revision,at],|r|r.get(0))?;
        if !live {
            return Ok(false);
        }
    }
    for source in &snapshot.source_refs {
        if !validate_source_in(db, &scope, source)? {
            return Ok(false);
        }
    }
    Ok(true)
}
