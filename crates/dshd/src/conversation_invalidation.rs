//! Exact invalidation of derived conversational state and its actual consumers.
//! Sending/accepted/unknown attempts and physical leases remain delivery facts.
//! History floors are owned by the original memory/diary invalidation paths.
use crate::{conversation_commitments as commitments, conversation_state as state};
use anyhow::ensure;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::BTreeSet;

const SCHEMA:&[(&str,&str)]=&[
    ("conversation_state_heads","scope_key,owner_id,conversation_id,epoch,revision"),
    ("conversation_state_entities","id,scope_key,kind,revision,status,content_json"),
    ("conversation_state_sources","entity_id,source_kind,source_id,source_revision,source_json"),
    ("conversation_state_observations","run_id,result_json"),
    ("conversation_state_uses","run_id,entity_id,entity_revision"),
    ("conversation_state_snapshots","run_id,scope_key,snapshot_json,invalidated_at"),
    ("conversation_state_source_tombstones","source_kind,source_id,through_revision,invalidated_at"),
    ("conversation_commitments","id,owner_id,conversation_id,revision,epoch,intent,time_json,sources_json,source_versions_json,participant_basis_json,subject_kind,counterparty_kind,invalidated_at"),
    ("conversation_commitment_epochs","owner_id,conversation_id,speaker_scope,epoch"),
    ("conversation_commitment_events","commitment_id,revision,actor_id,action,evidence_json"),
    ("conversation_commitment_usage","run_id,commitment_id,revision,epoch,digest,invalidated_at"),
    ("conversation_commitment_contexts","run_id,references_json,invalidated_at"),
    ("conversation_commitment_sources","commitment_id,source_kind,source_id,source_revision"),
    ("conversation_commitment_idempotency","scope_key,operation,request_key,request_digest,commitment_id,applied_revision"),
];

#[derive(Clone, Debug, Default, Serialize)]
pub struct InvalidationSummary {
    pub legacy_schema: bool,
    pub focus_entities: usize,
    pub commitments: usize,
    pub affected_run_ids: Vec<String>,
    pub cancelled_runs: usize,
    pub cancelled_qq_parts: usize,
    pub cancelled_qq_outboxes: usize,
    pub cancelled_weixin_outboxes: usize,
}

/// Absence of the entire feature is an ordinary legacy database. Any partial
/// feature schema is an error, even if the damaged table currently has no rows.
pub fn initialized_schema(db: &Connection) -> anyhow::Result<bool> {
    let mut found = 0;
    for (table, _) in SCHEMA {
        if state::table_exists(db, table)? {
            found += 1;
        }
    }
    if found == 0 {
        return Ok(false);
    }
    ensure!(
        found == SCHEMA.len(),
        "incomplete conversation state/commitment schema"
    );
    for (table, columns) in SCHEMA {
        // Both SQL identifier strings are fixed internal schema declarations.
        db.prepare(&format!("SELECT {columns} FROM {table} LIMIT 0"))?;
    }
    Ok(true)
}

enum Selection<'a> {
    Source {
        kind: &'a str,
        id: &'a str,
        through: i64,
    },
    Scope {
        owner: &'a str,
        conversation: Option<&'a str>,
    },
}
fn affected_runs(db: &Connection, selection: &Selection<'_>) -> anyhow::Result<BTreeSet<String>> {
    let mut result = BTreeSet::new();
    let (state_sql,commitment_sql)=match selection{
        Selection::Source{..}=>(
            "SELECT DISTINCT u.run_id FROM conversation_state_uses u JOIN conversation_state_sources s ON s.entity_id=u.entity_id JOIN conversation_state_entities e ON e.id=u.entity_id JOIN conversation_state_heads h ON h.scope_key=e.scope_key JOIN runs r ON r.id=u.run_id AND r.owner_id=h.owner_id AND r.conversation_id=h.conversation_id WHERE s.source_kind=?1 AND s.source_id=?2 AND s.source_revision<=?3",
            "SELECT DISTINCT u.run_id FROM conversation_commitment_usage u JOIN conversation_commitment_sources s ON s.commitment_id=u.commitment_id JOIN conversation_commitments c ON c.id=u.commitment_id JOIN runs r ON r.id=u.run_id AND r.owner_id=c.owner_id AND r.conversation_id=c.conversation_id WHERE s.source_kind=?1 AND s.source_id=?2 AND s.source_revision<=?3"),
        Selection::Scope{..}=>(
            "SELECT DISTINCT u.run_id FROM conversation_state_uses u JOIN conversation_state_entities e ON e.id=u.entity_id JOIN conversation_state_heads h ON h.scope_key=e.scope_key JOIN runs r ON r.id=u.run_id AND r.owner_id=h.owner_id AND r.conversation_id=h.conversation_id WHERE h.owner_id=?1 AND (?2 IS NULL OR h.conversation_id=?2)",
            "SELECT DISTINCT u.run_id FROM conversation_commitment_usage u JOIN conversation_commitments c ON c.id=u.commitment_id JOIN runs r ON r.id=u.run_id AND r.owner_id=c.owner_id AND r.conversation_id=c.conversation_id WHERE c.owner_id=?1 AND (?2 IS NULL OR c.conversation_id=?2)"),
    };
    for sql in [state_sql, commitment_sql] {
        let mut statement = db.prepare(sql)?;
        let rows = match selection {
            Selection::Source { kind, id, through } => statement
                .query_map(params![kind, id, through], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?,
            Selection::Scope {
                owner,
                conversation,
            } => statement
                .query_map(params![owner, conversation], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        };
        result.extend(rows);
    }
    Ok(result)
}

fn fence_consumers(
    db: &Connection,
    runs: BTreeSet<String>,
    at: i64,
    summary: &mut InvalidationSummary,
) -> anyhow::Result<()> {
    let qq = state::table_exists(db, "outbox")?;
    let qq_parts = state::table_exists(db, "outbox_reply_parts")?;
    let weixin = state::table_exists(db, "weixin_outbox")?;
    let publication = state::table_exists(db, "run_publication_decisions")?;
    let publication_events = state::table_exists(db, "run_publication_events")?;
    let prompts = state::table_exists(db, "run_prompt_snapshots")?;
    let delivery = state::table_exists(db, "delivery_context_snapshots")?;
    let deferred = state::table_exists(db, "rest_deferred_presentations")?;
    for run in &runs {
        if deferred {
            db.execute(
                "DELETE FROM rest_deferred_presentations WHERE run_id=?1",
                [run],
            )?;
        }
        summary.cancelled_runs+=db.execute("UPDATE runs SET status='cancelled',error='conversation_source_invalidated',updated_at=?1 WHERE id=?2 AND status IN ('queued','running')",params![at,run])?;
        // This is a future-publication fence, not a claim that already accepted
        // content was withdrawn. The original effect-started flag is retained.
        if publication {
            let changed=db.execute("UPDATE run_publication_decisions SET decision='suppressed',revision=revision+1,reason='conversation_source_invalidated',updated_ms=?1 WHERE run_id=?2 AND decision='active'",params![at.saturating_mul(1000),run])?;
            if changed > 0 && publication_events {
                db.execute("INSERT INTO run_publication_events(run_id,revision,decision,reason,created_ms) SELECT run_id,revision,decision,reason,updated_ms FROM run_publication_decisions WHERE run_id=?1",[run])?;
            }
        }
        if qq_parts && qq {
            summary.cancelled_qq_parts+=db.execute("UPDATE outbox_reply_parts SET status='cancelled',error='conversation_source_invalidated' WHERE status='queued' AND outbox_id IN(SELECT id FROM outbox WHERE run_id=?1)",[run])?;
        }
        if qq {
            summary.cancelled_qq_outboxes+=db.execute("UPDATE outbox SET status='cancelled',error='conversation_source_invalidated' WHERE run_id=?1 AND status='queued'",[run])?;
        }
        if weixin {
            summary.cancelled_weixin_outboxes+=db.execute("UPDATE weixin_outbox SET status='cancelled',content=NULL,error_code='conversation_source_invalidated',updated_at=?1 WHERE run_id=?2 AND status='queued'",params![at,run])?;
        }
        db.execute(
            "UPDATE conversation_state_snapshots SET invalidated_at=?1 WHERE run_id=?2",
            params![at, run],
        )?;
        db.execute(
            "UPDATE conversation_commitment_contexts SET invalidated_at=?1 WHERE run_id=?2",
            params![at, run],
        )?;
        if prompts {
            db.execute("DELETE FROM run_prompt_snapshots WHERE run_id=?1", [run])?;
        }
        if delivery {
            db.execute("UPDATE delivery_context_snapshots SET invalidated_at=?1 WHERE kind='run' AND target_id=?2",params![at,run])?;
        }
    }
    summary.affected_run_ids = runs.into_iter().collect();
    Ok(())
}

fn apply_in(
    db: &Connection,
    selection: Selection<'_>,
    at: i64,
) -> anyhow::Result<InvalidationSummary> {
    if !initialized_schema(db)? {
        return Ok(InvalidationSummary {
            legacy_schema: true,
            ..Default::default()
        });
    }
    state::transaction_required(db)?;
    // Collect exact normalized references before either module mutates them.
    // Timestamps deliberately never select a consumer or its authorization.
    let runs = affected_runs(db, &selection)?;
    let mut summary = InvalidationSummary::default();
    match selection {
        Selection::Source { kind, id, through } => {
            summary.focus_entities =
                state::invalidate_source_in(db, kind, id, Some(through), at)?.entities;
            summary.commitments =
                commitments::invalidate_source_in(db, kind, id, Some(through), at)?;
        }
        Selection::Scope {
            owner,
            conversation,
        } => {
            if let Some(conversation) = conversation {
                summary.focus_entities =
                    state::invalidate_conversation_in(db, owner, conversation, at)?.entities;
            } else {
                let conversations = {
                    let mut statement=db.prepare("SELECT DISTINCT conversation_id FROM conversation_state_heads WHERE owner_id=?1")?;
                    let rows = statement
                        .query_map([owner], |r| r.get::<_, String>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    rows
                };
                for conversation in conversations {
                    summary.focus_entities +=
                        state::invalidate_conversation_in(db, owner, &conversation, at)?.entities;
                }
            }
            summary.commitments = commitments::invalidate_in(db, Some(owner), conversation, at)?;
        }
    }
    fence_consumers(db, runs, at, &mut summary)?;
    Ok(summary)
}

pub fn invalidate_source_in(
    db: &Connection,
    kind: &str,
    id: &str,
    through_revision: Option<i64>,
    at: i64,
) -> anyhow::Result<InvalidationSummary> {
    ensure!(
        !kind.is_empty()
            && kind.len() <= 64
            && !id.is_empty()
            && id.len() <= 512
            && through_revision.is_none_or(|revision| revision >= 0),
        "invalid conversation source selector"
    );
    apply_in(
        db,
        Selection::Source {
            kind,
            id,
            through: through_revision.unwrap_or(i64::MAX),
        },
        at,
    )
}
pub fn invalidate_conversation_in(
    db: &Connection,
    owner: &str,
    conversation: &str,
    at: i64,
) -> anyhow::Result<InvalidationSummary> {
    ensure!(
        !owner.is_empty() && !conversation.is_empty(),
        "exact source owner and conversation are required"
    );
    apply_in(
        db,
        Selection::Scope {
            owner,
            conversation: Some(conversation),
        },
        at,
    )
}
pub fn invalidate_owner_in(
    db: &Connection,
    owner: &str,
    at: i64,
) -> anyhow::Result<InvalidationSummary> {
    ensure!(!owner.is_empty(), "source owner is required");
    apply_in(
        db,
        Selection::Scope {
            owner,
            conversation: None,
        },
        at,
    )
}

#[cfg(test)]
#[path = "conversation_invalidation_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "conversation_invalidation_hook_tests.rs"]
mod hook_tests;
