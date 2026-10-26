//! Model history follows observed delivery evidence rather than generated text.
//!
//! Callers project, freeze the metadata below, and save their prompt or derived
//! source in one transaction. Late receipts change new projections only. Stored
//! snapshots contain hashes and references, never another copy of message text.
use anyhow::{ensure, Context};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const WEIXIN_OVERSIZE_REPLY: &str = "回复超过当前微信文本限制，请缩小问题范围后重试。";
const QQ_MEDIA_FALLBACK: &str = "媒体发送失败后已改发文字；原因：";

type ProjectionRunRow = (
    i64,
    String,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
);
type QqOutboxRow = (String, String, String, Option<String>, Option<String>);
type WeixinOutboxRow = (String, String, Option<String>, Option<String>, i64);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryPartEvidence {
    pub outbox_id: String,
    pub seq: Option<i64>,
    pub status: String,
    pub outbox_status: String,
    pub kind: String,
    pub platform_accepted: bool,
    pub receipt_digest: Option<String>,
    pub content_digest: Option<String>,
    pub finished_at_ms: Option<i64>,
    pub text_fallback: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryEvidence {
    pub run_id: String,
    pub run_rowid: i64,
    pub channel: String,
    pub delivery_revision: i64,
    pub run_status: String,
    pub publication_allowed: bool,
    pub publication_revision: Option<i64>,
    pub local_completion: bool,
    pub user_digest: String,
    pub assistant_digest: Option<String>,
    pub projection_digest: String,
    pub parts: Vec<DeliveryPartEvidence>,
}

#[derive(Clone, Debug)]
pub struct RunProjection {
    pub run_id: String,
    pub user_content: String,
    pub assistant_content: Option<String>,
    pub delivery: DeliveryEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryContextSnapshot {
    pub version: u32,
    pub kind: String,
    pub target_id: String,
    pub owner_id: String,
    pub conversation_id: String,
    pub history_floor: i64,
    pub evidence_cutoff_ms: i64,
    pub sources: Vec<DeliveryEvidence>,
    pub digest: String,
    pub invalidated_at: Option<i64>,
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn table_exists(db: &Connection, name: &str) -> anyhow::Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [name],
        |r| r.get(0),
    )?)
}

/// Run after the QQ/Weixin and context tables have been initialized. Repeating
/// this also installs the optional channel triggers added since the first call.
pub fn ensure_schema(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS delivery_context_revisions(
           run_id TEXT PRIMARY KEY REFERENCES runs(id),revision INTEGER NOT NULL CHECK(revision>0));
         CREATE TABLE IF NOT EXISTS delivery_context_snapshots(
           kind TEXT NOT NULL,target_id TEXT NOT NULL,owner_id TEXT NOT NULL,
           conversation_id TEXT NOT NULL REFERENCES conversations(id),
           snapshot_json TEXT NOT NULL,digest TEXT NOT NULL,invalidated_at INTEGER,
           PRIMARY KEY(kind,target_id));
         CREATE INDEX IF NOT EXISTS delivery_context_conversation
           ON delivery_context_snapshots(conversation_id,invalidated_at);",
    )?;
    for (table, prefix, run_expr) in [
        ("outbox", "qq_parent", "new.run_id"),
        ("weixin_outbox", "weixin", "new.run_id"),
        (
            "outbox_reply_parts",
            "qq_part",
            "(SELECT run_id FROM outbox WHERE id=new.outbox_id)",
        ),
        ("run_publication_decisions", "publication", "new.run_id"),
    ] {
        if !table_exists(db, table)? {
            continue;
        }
        let old_expr = run_expr.replace("new.", "old.");
        for (action, expr) in [
            ("INSERT", run_expr),
            ("UPDATE", run_expr),
            ("DELETE", &old_expr),
        ] {
            // Identifiers and expressions above are compile-time constants.
            db.execute_batch(&format!(
                "CREATE TRIGGER IF NOT EXISTS delivery_{prefix}_{} AFTER {action} ON {table}
                 BEGIN INSERT INTO delivery_context_revisions(run_id,revision)
                   SELECT {expr},1 WHERE {expr} IS NOT NULL
                   ON CONFLICT(run_id) DO UPDATE SET revision=revision+1; END;",
                action.to_ascii_lowercase()
            ))?;
        }
        let source = if table == "outbox_reply_parts" {
            "SELECT DISTINCT o.run_id,1 FROM outbox o JOIN outbox_reply_parts p ON p.outbox_id=o.id"
                .to_owned()
        } else {
            format!("SELECT run_id,1 FROM {table}")
        };
        db.execute_batch(&format!(
            "INSERT OR IGNORE INTO delivery_context_revisions(run_id,revision) {source};"
        ))?;
    }
    db.execute_batch(
        "CREATE TRIGGER IF NOT EXISTS delivery_run_changed AFTER UPDATE OF
           input,response,status,owner_id,conversation_id ON runs BEGIN
           INSERT INTO delivery_context_revisions(run_id,revision) VALUES(new.id,1)
           ON CONFLICT(run_id) DO UPDATE SET revision=revision+1; END;
         CREATE TRIGGER IF NOT EXISTS delivery_conversation_changed AFTER UPDATE OF
           owner_id,source_key ON conversations BEGIN
           INSERT INTO delivery_context_revisions(run_id,revision)
           SELECT id,1 FROM runs WHERE conversation_id=new.id
           ON CONFLICT(run_id) DO UPDATE SET revision=revision+1; END;",
    )?;
    for (action, reference) in [("INSERT", "new"), ("UPDATE", "new"), ("DELETE", "old")] {
        db.execute_batch(&format!(
            "CREATE TRIGGER IF NOT EXISTS delivery_message_{} AFTER {action} ON messages BEGIN
               INSERT INTO delivery_context_revisions(run_id,revision)
               SELECT {reference}.run_id,1 WHERE {reference}.run_id IS NOT NULL
               ON CONFLICT(run_id) DO UPDATE SET revision=revision+1; END;",
            action.to_ascii_lowercase()
        ))?;
    }
    if table_exists(db, "memory_context_watermarks")? {
        for action in ["INSERT", "UPDATE"] {
            db.execute_batch(&format!(
                "CREATE TRIGGER IF NOT EXISTS delivery_context_floor_{} AFTER {action}
                 ON memory_context_watermarks BEGIN
                   UPDATE delivery_context_snapshots SET invalidated_at=new.updated_at
                   WHERE conversation_id=new.conversation_id AND invalidated_at IS NULL;
                 END;",
                action.to_ascii_lowercase()
            ))?;
        }
    }
    Ok(())
}

fn history_floor(db: &Connection, conversation: &str) -> anyhow::Result<i64> {
    if !table_exists(db, "memory_context_watermarks")? {
        return Ok(0);
    }
    Ok(db
        .query_row(
            "SELECT through_run_rowid FROM memory_context_watermarks WHERE conversation_id=?1",
            [conversation],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

fn revision(db: &Connection, run: &str) -> anyhow::Result<i64> {
    Ok(db
        .query_row(
            "SELECT revision FROM delivery_context_revisions WHERE run_id=?1",
            [run],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

fn text_digest(text: Option<&str>) -> Option<String> {
    text.map(|value| hash(value.as_bytes()))
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|text| !text.trim().is_empty())
}

/// Projection requires the same read transaction used to persist its consumer.
/// User questions survive queued/partial/failed/unknown assistant delivery.
/// A local Web/CLI completion means locally available, never read by a person.
pub fn project_run_in(
    db: &Connection,
    owner: &str,
    conversation: &str,
    run: &str,
) -> anyhow::Result<RunProjection> {
    project_in(db, owner, conversation, run, true)
}

/// Diary capture has its own source grants, tombstones and revocation ledger.
/// Its caller must check those in this transaction. A conversational context
/// floor is not, by itself, deletion of the original authorized user source.
pub fn project_capture_run_in(
    db: &Connection,
    owner: &str,
    conversation: &str,
    run: &str,
) -> anyhow::Result<RunProjection> {
    project_in(db, owner, conversation, run, false)
}

fn project_in(
    db: &Connection,
    owner: &str,
    conversation: &str,
    run: &str,
    enforce_history_floor: bool,
) -> anyhow::Result<RunProjection> {
    ensure!(
        !db.is_autocommit(),
        "delivery projection requires a transaction"
    );
    let (rowid, status, response, input, source, reply): ProjectionRunRow = db.query_row(
        "SELECT r.rowid,r.status,r.response,coalesce(m.content,r.input),c.source_key,r.reply_json
         FROM runs r JOIN conversations c ON c.id=r.conversation_id
         LEFT JOIN messages m ON m.run_id=r.id AND m.role='user' AND m.conversation_id=c.id
         WHERE r.id=?1 AND r.owner_id=?2 AND c.owner_id=?2 AND c.id=?3",
        params![run, owner, conversation],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        },
    )?;
    ensure!(
        !enforce_history_floor || rowid > history_floor(db, conversation)?,
        "delivery source was invalidated"
    );
    let mut channel = if owner.starts_with("qq:")
        || source.as_deref().is_some_and(|s| s.starts_with("qq:"))
        || reply.is_some()
    {
        "qq"
    } else if owner.starts_with("weixin:")
        || source.as_deref().is_some_and(|s| s.starts_with("weixin:"))
    {
        "weixin"
    } else {
        "local"
    };
    let mut parts = Vec::new();
    let mut accepted = Vec::new();
    if table_exists(db, "outbox")? {
        let parent: Option<QqOutboxRow> = db
            .query_row(
                "SELECT id,status,content,platform_message_id,error FROM outbox WHERE run_id=?1",
                [run],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        if let Some((outbox_id, parent_status, content, receipt, _)) = parent {
            channel = "qq";
            if table_exists(db, "outbox_reply_parts")? {
                let mut statement = db.prepare(
                    "SELECT seq,status,payload_json,platform_message_id,error,finished_ms
                     FROM outbox_reply_parts WHERE outbox_id=?1 ORDER BY seq",
                )?;
                let rows = statement.query_map([&outbox_id], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, Option<i64>>(5)?,
                    ))
                })?;
                for row in rows {
                    let (seq, state, payload, receipt, error, finished) = row?;
                    let payload: serde_json::Value = serde_json::from_str(&payload)
                        .context("invalid durable delivery payload")?;
                    let kind = payload["type"].as_str().unwrap_or("unknown").to_owned();
                    let text = payload["text"].as_str();
                    let receipt = nonempty(receipt);
                    let confirmed = state == "sent" && receipt.is_some();
                    let fallback = error
                        .as_deref()
                        .is_some_and(|e| e.starts_with(QQ_MEDIA_FALLBACK));
                    let delivered = if !confirmed {
                        None
                    } else if fallback {
                        match kind.as_str() {
                            "voice" => text.map(|s| format!("（语音暂不可用）{s}")),
                            "image" => Some("图片发送暂不可用。".into()),
                            "video" => Some("视频发送暂不可用。".into()),
                            _ => None,
                        }
                    } else if error.is_none() {
                        match kind.as_str() {
                            "text" => text.map(str::to_owned),
                            "image" => text.map(|s| s.chars().take(1500).collect()),
                            // Voice/video have no text caption. A platform
                            // receipt does not prove the proposed words were
                            // spoken, played, transcribed, or read.
                            _ => None,
                        }
                    } else {
                        None
                    };
                    if let Some(text) = nonempty(delivered.clone()) {
                        accepted.push(text);
                    }
                    parts.push(DeliveryPartEvidence {
                        outbox_id: outbox_id.clone(),
                        seq: Some(seq),
                        status: state,
                        outbox_status: parent_status.clone(),
                        kind,
                        platform_accepted: confirmed,
                        receipt_digest: text_digest(receipt.as_deref()),
                        content_digest: text_digest(delivered.as_deref()),
                        finished_at_ms: finished,
                        text_fallback: fallback,
                    });
                }
            }
            if parts.is_empty() {
                let receipt = nonempty(receipt);
                let confirmed = parent_status == "sent" && receipt.is_some();
                if confirmed && !content.trim().is_empty() {
                    accepted.push(content.clone());
                }
                parts.push(DeliveryPartEvidence {
                    outbox_id,
                    seq: None,
                    status: parent_status.clone(),
                    outbox_status: parent_status,
                    kind: "text".into(),
                    platform_accepted: confirmed,
                    receipt_digest: text_digest(receipt.as_deref()),
                    content_digest: confirmed.then(|| hash(content.as_bytes())),
                    finished_at_ms: None,
                    text_fallback: false,
                });
            }
        }
    }
    if table_exists(db, "weixin_outbox")? {
        let row: Option<WeixinOutboxRow> = db
            .query_row(
                "SELECT id,status,error_code,server_message_id,updated_at FROM weixin_outbox WHERE run_id=?1",
                [run],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        if let Some((id, state, code, receipt, updated)) = row {
            ensure!(
                parts.is_empty(),
                "run has conflicting channel delivery evidence"
            );
            channel = "weixin";
            // iLink's successful business response is the acceptance evidence;
            // its protocol permits an absent server message ID.
            let confirmed = state == "accepted";
            let fallback = code.as_deref() == Some("original_response_too_large");
            let text = if confirmed && fallback {
                Some(WEIXIN_OVERSIZE_REPLY.to_owned())
            } else if confirmed && code.is_none() {
                response
                    .clone()
                    .filter(|v| v.len() <= crate::weixin_store::MAX_OUTPUT_BYTES)
            } else {
                None
            };
            if let Some(text) = nonempty(text.clone()) {
                accepted.push(text);
            }
            parts.push(DeliveryPartEvidence {
                outbox_id: id,
                seq: None,
                status: state.clone(),
                outbox_status: state,
                kind: "text".into(),
                platform_accepted: confirmed,
                receipt_digest: text_digest(nonempty(receipt).as_deref()),
                content_digest: text_digest(text.as_deref()),
                finished_at_ms: Some(updated.saturating_mul(1000)),
                text_fallback: fallback,
            });
        }
    }
    // No channel outbox does not prove a QQ/Weixin reply was ever attempted.
    let publication: Option<(String, i64)> = if table_exists(db, "run_publication_decisions")? {
        let row: Option<(String,i64,String,String)> = db.query_row(
            "SELECT decision,revision,owner_id,conversation_id FROM run_publication_decisions WHERE run_id=?1",
            [run], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
        ).optional()?;
        if let Some((decision, revision, bound_owner, bound_conversation)) = row {
            ensure!(
                bound_owner == owner && bound_conversation == conversation,
                "publication scope mismatch"
            );
            Some((decision, revision))
        } else {
            None
        }
    } else {
        None
    };
    let publication_allowed = publication
        .as_ref()
        .is_none_or(|(state, _)| state == "active");
    let local_completion = channel == "local" && status == "completed" && publication_allowed;
    let assistant_content = if local_completion {
        nonempty(response)
    } else {
        nonempty((!accepted.is_empty()).then(|| accepted.join("\n")))
    };
    let mut evidence = DeliveryEvidence {
        run_id: run.to_owned(),
        run_rowid: rowid,
        channel: channel.into(),
        delivery_revision: revision(db, run)?,
        run_status: status,
        publication_allowed,
        publication_revision: publication.map(|(_, revision)| revision),
        local_completion,
        user_digest: hash(input.as_bytes()),
        assistant_digest: text_digest(assistant_content.as_deref()),
        projection_digest: String::new(),
        parts,
    };
    evidence.projection_digest = hash(&serde_json::to_vec(&(
        &evidence,
        owner,
        conversation,
        &source,
    ))?);
    Ok(RunProjection {
        run_id: run.to_owned(),
        user_content: input,
        assistant_content,
        delivery: evidence,
    })
}

/// Useful for summary/diary commit fencing and for detecting that a cached
/// derived record predates a late receipt, deletion, or authorization watermark.
pub fn validate_sources_in(
    db: &Connection,
    owner: &str,
    conversation: &str,
    sources: &[DeliveryEvidence],
) -> anyhow::Result<bool> {
    validate_in(db, owner, conversation, sources, true)
}

pub fn validate_capture_sources_in(
    db: &Connection,
    owner: &str,
    conversation: &str,
    sources: &[DeliveryEvidence],
) -> anyhow::Result<bool> {
    validate_in(db, owner, conversation, sources, false)
}

fn validate_in(
    db: &Connection,
    owner: &str,
    conversation: &str,
    sources: &[DeliveryEvidence],
    enforce_history_floor: bool,
) -> anyhow::Result<bool> {
    ensure!(
        !db.is_autocommit(),
        "delivery validation requires a transaction"
    );
    for evidence in sources {
        let current = project_in(
            db,
            owner,
            conversation,
            &evidence.run_id,
            enforce_history_floor,
        );
        if !matches!(current, Ok(ref p) if p.delivery == *evidence) {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn snapshot_in(
    db: &Connection,
    kind: &str,
    target_id: &str,
) -> anyhow::Result<Option<DeliveryContextSnapshot>> {
    let row: Option<(String, Option<i64>)> = db.query_row(
        "SELECT snapshot_json,invalidated_at FROM delivery_context_snapshots WHERE kind=?1 AND target_id=?2",
        params![kind, target_id], |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional()?;
    row.map(|(text, invalidated)| {
        let mut snapshot: DeliveryContextSnapshot = serde_json::from_str(&text)?;
        snapshot.invalidated_at = invalidated;
        Ok(snapshot)
    })
    .transpose()
}

/// Repeating the same target returns its immutable original snapshot, even if
/// delivery advanced. The caller must also reuse its original frozen prompt.
/// An explicitly invalidated target may not be silently rebuilt or resurrected.
pub fn snapshot_sources_in(
    db: &Connection,
    kind: &str,
    target_id: &str,
    owner: &str,
    conversation: &str,
    projections: &[RunProjection],
    evidence_cutoff_ms: i64,
) -> anyhow::Result<DeliveryContextSnapshot> {
    ensure!(
        !db.is_autocommit(),
        "delivery snapshot requires a transaction"
    );
    ensure!(
        !kind.is_empty() && kind.len() <= 40 && !target_id.is_empty() && target_id.len() <= 512,
        "invalid delivery snapshot target"
    );
    ensure!(
        projections.len() <= 256,
        "too many delivery snapshot sources"
    );
    let owns: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM conversations WHERE id=?1 AND owner_id=?2)",
        params![conversation, owner],
        |r| r.get(0),
    )?;
    ensure!(owns, "delivery snapshot owner mismatch");
    if let Some(snapshot) = snapshot_in(db, kind, target_id)? {
        ensure!(
            snapshot.owner_id == owner && snapshot.conversation_id == conversation,
            "delivery snapshot scope mismatch"
        );
        ensure!(
            snapshot.invalidated_at.is_none(),
            "delivery snapshot was invalidated"
        );
        return Ok(snapshot);
    }
    let sources = projections
        .iter()
        .map(|p| p.delivery.clone())
        .collect::<Vec<_>>();
    for projection in projections {
        ensure!(
            hash(projection.user_content.as_bytes()) == projection.delivery.user_digest
                && text_digest(projection.assistant_content.as_deref())
                    == projection.delivery.assistant_digest,
            "delivery projection content changed"
        );
    }
    ensure!(
        validate_in(db, owner, conversation, &sources, kind != "diary_capture")?,
        "delivery sources changed"
    );
    let mut snapshot = DeliveryContextSnapshot {
        version: 1,
        kind: kind.to_owned(),
        target_id: target_id.to_owned(),
        owner_id: owner.to_owned(),
        conversation_id: conversation.to_owned(),
        history_floor: history_floor(db, conversation)?,
        evidence_cutoff_ms,
        sources,
        digest: String::new(),
        invalidated_at: None,
    };
    snapshot.digest = hash(&serde_json::to_vec(&snapshot)?);
    db.execute(
        "INSERT INTO delivery_context_snapshots(kind,target_id,owner_id,conversation_id,snapshot_json,digest)
         VALUES(?1,?2,?3,?4,?5,?6)",
        params![kind, target_id, owner, conversation, serde_json::to_string(&snapshot)?, snapshot.digest],
    )?;
    Ok(snapshot)
}

pub fn freeze_snapshot_in(
    db: &Connection,
    target_run_id: &str,
    projections: &[RunProjection],
    evidence_cutoff_ms: i64,
) -> anyhow::Result<DeliveryContextSnapshot> {
    let (owner, conversation, state): (String, String, String) = db.query_row(
        "SELECT owner_id,conversation_id,status FROM runs WHERE id=?1",
        [target_run_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    ensure!(
        state == "running",
        "delivery snapshot run is no longer active"
    );
    snapshot_sources_in(
        db,
        "run",
        target_run_id,
        &owner,
        &conversation,
        projections,
        evidence_cutoff_ms,
    )
}

/// Invalidation is separate from delivery facts. Do not erase sent receipts or
/// label an in-flight platform attempt as never attempted during a forget.
pub fn invalidate_conversation_in(
    db: &Connection,
    conversation: &str,
    at: i64,
) -> anyhow::Result<()> {
    ensure!(
        !db.is_autocommit(),
        "delivery invalidation requires a transaction"
    );
    db.execute(
        "UPDATE delivery_context_snapshots SET invalidated_at=?1 WHERE conversation_id=?2 AND invalidated_at IS NULL",
        params![at, conversation],
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "delivery_context_tests.rs"]
mod tests;
