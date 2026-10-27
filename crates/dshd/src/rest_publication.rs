//! Rest checks belong to the existing queued -> sending transaction. They never
//! replay or alter an already claimed request, receipt, unknown or model lease.
use anyhow::ensure;
use rusqlite::{params, Connection};
use serde_json::json;

const TABLES: [&str; 14] = [
    "rest_meta",
    "rest_policies",
    "assistant_rest_states",
    "rest_control_grants",
    "rest_operations",
    "rest_cycles",
    "rest_cycle_sources",
    "rest_cycle_refs",
    "rest_produced_refs",
    "rest_steps",
    "rest_ack_permits",
    "consolidation_jobs",
    "rest_job_sources",
    "rest_job_attempts",
];
fn exists(db: &Connection, name: &str) -> anyhow::Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [name],
        |r| r.get(0),
    )?)
}
/// Old isolated sender fixtures may have no rest schema. Partial/newer schemas
/// are errors, never an implicit awake/allow decision.
pub(crate) fn schema_present_in(db: &Connection) -> anyhow::Result<bool> {
    ensure!(
        !db.is_autocommit(),
        "rest publication requires claim transaction"
    );
    let any:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE (name GLOB 'rest_*' OR name IN('assistant_rest_states','consolidation_jobs')) AND type IN('table','view'))",[],|r|r.get(0))?;
    if !any {
        return Ok(false);
    }
    for table in TABLES {
        ensure!(
            exists(db, table)?,
            "partial rest publication schema: {table}"
        );
    }
    let version: i64 =
        db.query_row("SELECT schema_version FROM rest_meta WHERE id=1", [], |r| {
            r.get(0)
        })?;
    ensure!(version == 1, "rest publication reader version required");
    Ok(true)
}
pub fn payload_digest(text: &str) -> String {
    crate::trigger_store::digest(&json!({"text":text}))
}
fn cancel_queued_in(db: &Connection, run: &str, code: &str, at: i64) -> anyhow::Result<()> {
    if exists(db, "outbox_reply_parts")? {
        db.execute("UPDATE outbox_reply_parts SET status='cancelled',error=?1 WHERE status='queued' AND outbox_id IN(SELECT id FROM outbox WHERE run_id=?2)",params![code,run])?;
    }
    if exists(db, "outbox")? {
        db.execute(
            "UPDATE outbox SET status='cancelled',error=?1 WHERE status='queued' AND run_id=?2",
            params![code, run],
        )?;
    }
    if exists(db, "weixin_outbox")? {
        db.execute("UPDATE weixin_outbox SET status='cancelled',error_code=?1,content=NULL,updated_at=?2 WHERE status='queued' AND run_id=?3",params![code,at,run])?;
    }
    Ok(())
}
pub fn claim_allowed_in(
    db: &Connection,
    run: &str,
    original_deadline: Option<i64>,
    content: &str,
) -> anyhow::Result<bool> {
    if !schema_present_in(db)? {
        return Ok(true);
    }
    let at = crate::state::now();
    let digest = payload_digest(content);
    let decision = crate::rest_store::admission_in(
        db,
        crate::rest_store::RestWork::Publication {
            run_id: run,
            original_deadline,
            payload_digest: Some(&digest),
        },
        at,
    )?;
    match decision {
        crate::rest_store::RestAdmission::Hold { .. } => Ok(false),
        crate::rest_store::RestAdmission::Reject { code, .. } => {
            cancel_queued_in(db, run, &code, at)?;
            Ok(false)
        }
        crate::rest_store::RestAdmission::Allow { .. } => {
            let ack: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM rest_ack_permits WHERE source_run_id=?1)",
                [run],
                |r| r.get(0),
            )?;
            if ack {
                // The host acknowledgement contract is one exact plain text
                // payload, never a model-authored collection of reply parts.
                let parts=exists(db,"outbox_reply_parts")? && db.query_row("SELECT EXISTS(SELECT 1 FROM outbox_reply_parts p JOIN outbox o ON o.id=p.outbox_id WHERE o.run_id=?1)",[run],|r|r.get::<_,bool>(0))?;
                if parts || !crate::rest_store::mark_ack_attempt_started_in(db, run, &digest, at)? {
                    cancel_queued_in(db, run, "rest_control_ack_not_permitted", at)?;
                    return Ok(false);
                }
            }
            Ok(true)
        }
        crate::rest_store::RestAdmission::PreserveInFlight { .. } => {
            anyhow::bail!("existing rest attempt cannot enter queued publication claim")
        }
    }
}

#[cfg(test)]
#[path = "rest_publication_tests.rs"]
mod tests;
