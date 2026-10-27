//! Cheap host coordination. No model slot is needed for a direct wake request.
use crate::{
    rest_store as rest,
    state::{now, AppState},
};
use anyhow::ensure;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::json;

pub fn initialize(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS rest_control_candidates(
      run_id TEXT PRIMARY KEY REFERENCES runs(id),input_digest TEXT NOT NULL,
      received_at INTEGER NOT NULL,status TEXT NOT NULL CHECK(status IN('pending','completed','rejected')),
      operation_id TEXT,error_code TEXT,finished_at INTEGER);
      CREATE TABLE IF NOT EXISTS rest_schedule_receipts(
      policy_revision INTEGER NOT NULL,nominal_at INTEGER NOT NULL,status TEXT NOT NULL,
      operation_id TEXT,PRIMARY KEY(policy_revision,nominal_at));
      CREATE TABLE IF NOT EXISTS rest_deferred_presentations(
      run_id TEXT PRIMARY KEY REFERENCES runs(id),raw_generation TEXT NOT NULL,
      plan_json TEXT,media_inflight INTEGER NOT NULL DEFAULT 0,updated_at INTEGER NOT NULL);")?;
    Ok(())
}

/// Entirely absent schema supports old low-level fixtures. Partial schema is an
/// error; a production instance initializes all of it before starting workers.
fn initialized(db: &Connection) -> anyhow::Result<bool> {
    let count:i64=db.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN('rest_meta','rest_policies','assistant_rest_states','rest_control_grants','rest_operations','rest_cycles','rest_cycle_sources','rest_cycle_refs','rest_produced_refs','rest_steps','rest_ack_permits','consolidation_jobs','rest_job_sources','rest_job_attempts','rest_consolidation_profiles','rest_consolidation_targets','rest_consolidation_blocks','rest_management_requests','rest_control_candidates','rest_schedule_receipts','rest_deferred_presentations')",[],|r|r.get(0))?;
    ensure!(count == 0 || count == 21, "rest_schema_incomplete");
    let all:i64=db.query_row("SELECT count(*) FROM sqlite_master WHERE type IN('table','view') AND (name GLOB 'rest_*' OR name IN('assistant_rest_states','consolidation_jobs'))",[],|r|r.get(0))?;
    ensure!(all == count, "rest_schema_unrecognized_or_incomplete");
    Ok(count == 21)
}

/// A fresh schema is created atomically. An existing partial schema is never
/// repaired into a default awake state before its durable state is checked.
pub fn initialize_all(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch("SAVEPOINT rest_runtime_initialization")?;
    let result = (|| -> anyhow::Result<()> {
        if initialized(db)? {
            let seeds:i64=db.query_row("SELECT (SELECT count(*) FROM rest_meta)+(SELECT count(*) FROM rest_policies)+(SELECT count(*) FROM assistant_rest_states)",[],|r|r.get(0))?;
            ensure!(seeds == 3, "rest_initial_state_incomplete");
            rest::state_in(db)?;
            rest::policy_in(db)?;
        }
        rest::initialize(db)?;
        crate::rest_api::initialize(db)?;
        initialize(db)?;
        Ok(())
    })();
    match result {
        Ok(()) => db.execute_batch("RELEASE rest_runtime_initialization")?,
        Err(error) => {
            db.execute_batch(
                "ROLLBACK TO rest_runtime_initialization; RELEASE rest_runtime_initialization",
            )?;
            return Err(error);
        }
    }
    Ok(())
}
pub fn automatic_allowed_in(db: &Connection) -> anyhow::Result<bool> {
    if !initialized(db)? {
        return Ok(true);
    }
    Ok(matches!(
        rest::admission_in(db, rest::RestWork::Automatic, now())?,
        rest::RestAdmission::Allow { .. }
    ))
}
pub fn register_input_in(
    db: &Connection,
    run: &str,
    input: &str,
    eligible: bool,
) -> anyhow::Result<()> {
    if !eligible || rest::natural_control(input).is_none() {
        return Ok(());
    }
    if !initialized(db)? {
        return Ok(());
    }
    ensure!(!db.is_autocommit(), "rest_control_transaction_required");
    db.execute("INSERT OR IGNORE INTO rest_control_candidates(run_id,input_digest,received_at,status) VALUES(?1,?2,?3,'pending')",params![run,crate::trigger_store::digest(&json!({"input":input})),rest::state_in(db)?.logical_at.max(now())])?;
    Ok(())
}

pub fn original_deadline_in(db: &Connection, run: &str) -> anyhow::Result<Option<i64>> {
    let reply: Option<String> =
        db.query_row("SELECT reply_json FROM runs WHERE id=?1", [run], |r| {
            r.get(0)
        })?;
    let qq = reply
        .map(|v| serde_json::from_str::<crate::worker::ReplyTarget>(&v))
        .transpose()?
        .map(|v| v.deadline);
    let weixin: Option<i64> = db
        .query_row(
            "SELECT deadline FROM weixin_run_replies WHERE run_id=?1",
            [run],
            |r| r.get(0),
        )
        .optional()?;
    Ok(match (qq, weixin) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    })
}

/// Called only by an existing source-erasure/history-floor transaction. A held
/// result is a derived private body even if it used no Focus/Commitment entity.
pub fn erase_cached_through_in(
    db: &Connection,
    conversation: &str,
    through: i64,
) -> anyhow::Result<()> {
    ensure!(
        !db.is_autocommit(),
        "rest_cache_erasure_transaction_required"
    );
    let present:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='rest_deferred_presentations')",[],|r|r.get(0))?;
    if !present {
        return Ok(());
    }
    db.execute("UPDATE runs SET status='cancelled',error='conversation_source_invalidated',updated_at=?1 WHERE conversation_id=?2 AND rowid<=?3 AND status='queued' AND id IN(SELECT run_id FROM rest_deferred_presentations)",params![now(),conversation,through])?;
    db.execute("DELETE FROM rest_deferred_presentations WHERE run_id IN(SELECT id FROM runs WHERE conversation_id=?1 AND rowid<=?2)",params![conversation,through])?;
    Ok(())
}
pub fn ordinary_allowed_in(db: &Connection, run: &str) -> anyhow::Result<bool> {
    if !initialized(db)? {
        return Ok(true);
    }
    let decision = rest::admission_in(
        db,
        rest::RestWork::Ordinary {
            run_id: run,
            original_deadline: original_deadline_in(db, run)?,
        },
        now(),
    )?;
    match decision {
        rest::RestAdmission::Allow { .. } => Ok(true),
        rest::RestAdmission::Reject { code, .. } => {
            db.execute("UPDATE runs SET status='failed',error=?1,updated_at=?2 WHERE id=?3 AND status='queued'",params![code,now(),run])?;
            Ok(false)
        }
        _ => Ok(false),
    }
}

pub fn cached_generation_in(db: &Connection, run: &str) -> anyhow::Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT raw_generation FROM rest_deferred_presentations WHERE run_id=?1",
            [run],
            |r| r.get(0),
        )
        .optional()?)
}
pub fn cached_plan_in(
    db: &Connection,
    run: &str,
) -> anyhow::Result<Option<crate::chat_reply::ReplyPlan>> {
    let raw: Option<String> = db
        .query_row(
            "SELECT plan_json FROM rest_deferred_presentations WHERE run_id=?1",
            [run],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    raw.map(|v| serde_json::from_str(&v).map_err(Into::into))
        .transpose()
}
pub fn cache_generation(state: &AppState, run: &str, raw: &str) -> crate::error::ApiResult<bool> {
    state.db(|db|{
        let tx=db.transaction()?;
        let running:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND status='running')",[run],|r|r.get(0))?;
        let valid=running && crate::conversation_identity::validate_context_in(&tx,run)? && crate::conversation_context::validate_derived_in(&tx,run)?;
        if !valid{
            tx.execute("DELETE FROM rest_deferred_presentations WHERE run_id=?1",[run])?;
            tx.execute("UPDATE runs SET status='failed',error='rest_generation_sources_invalidated',updated_at=?1 WHERE id=?2 AND status='running'",params![now(),run])?;
            tx.commit()?;return Ok(false);
        }
        ensure!(raw.len()<=128*1024,"rest_generation_cache_bound");
        ensure!(tx.execute("INSERT INTO rest_deferred_presentations(run_id,raw_generation,updated_at) VALUES(?1,?2,?3) ON CONFLICT(run_id) DO UPDATE SET updated_at=excluded.updated_at WHERE rest_deferred_presentations.raw_generation=excluded.raw_generation",params![run,raw,now()])?==1,"rest_generation_checkpoint_conflict");
        tx.commit()?;Ok(true)
    })
}
/// Before each new stage, commit a recoverable queued state when rest holds it.
/// Caller returns from its future before the native physical lease is dropped.
pub fn stage_allowed(state: &AppState, run: &str) -> crate::error::ApiResult<bool> {
    state.db(|db| {
        let tx = db.transaction()?;
        let running: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND status='running')",
            [run],
            |r| r.get(0),
        )?;
        if !running {
            return Ok(false);
        }
        let allow = automatic_allowed_in(&tx)?;
        if !allow {
            let expired = original_deadline_in(&tx, run)?.is_some_and(|v| v <= now());
            tx.execute(
                "UPDATE runs SET status=?1,error=?2,updated_at=?3 WHERE id=?4 AND status='running'",
                params![
                    if expired { "failed" } else { "queued" },
                    if expired {
                        Some("original_reply_deadline_expired")
                    } else {
                        None
                    },
                    now(),
                    run
                ],
            )?;
            if expired {
                tx.execute(
                    "DELETE FROM rest_deferred_presentations WHERE run_id=?1",
                    [run],
                )?;
            }
        }
        tx.commit()?;
        Ok(allow)
    })
}
/// A completed media stage is checkpointed before considering a new one. This
/// stores already obtained asset IDs, never replays a transport or an upload.
pub fn checkpoint_plan(
    state: &AppState,
    run: &str,
    plan: &crate::chat_reply::ReplyPlan,
) -> Result<bool, String> {
    state
        .db(|db| {
            ensure!(db.execute(
                "UPDATE rest_deferred_presentations SET plan_json=?1,media_inflight=0,updated_at=?2 WHERE run_id=?3",
                params![serde_json::to_string(plan)?, now(), run],
            )?==1,"rest_presentation_checkpoint_missing");
            Ok(())
        })
        .map_err(|_| "回复整理检查点未能保存".to_owned())?;
    stage_allowed(state, run).map_err(|_| "睡眠准入暂时不可核验".to_owned())
}

pub fn prompt_state_in(db: &Connection) -> anyhow::Result<String> {
    let state = rest::state_in(db)?;
    Ok(format!("\nDSH_REST_HOST_STATE_V1\n这是宿主实际状态，仅供如实表达，不授予全局控制权限。当前phase={}，generation={}，原计划唤醒时间={:?}。普通晚安、模型输出、引用或转发不改变状态；只有宿主实际受理的独立控制才可改变状态。不要声称因为一句晚安已经进入睡眠。",serde_json::to_value(state.phase)?,state.generation,state.wake_at))
}

fn complete_control_in(
    db: &Connection,
    run: &str,
    content: &str,
    permit: Option<&str>,
    requester: Option<&rest::VerifiedRequester>,
    operation: Option<&str>,
    code: Option<&str>,
) -> anyhow::Result<()> {
    let at = now();
    let conversation: String = db.query_row(
        "SELECT conversation_id FROM runs WHERE id=?1 AND status='queued'",
        [run],
        |r| r.get(0),
    )?;
    if let (Some(permit), Some(requester)) = (permit, requester) {
        rest::bind_ack_payload_in(
            db,
            requester,
            permit,
            &crate::rest_publication::payload_digest(content),
            at,
        )?;
    }
    db.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES(?1,?2,?3,'assistant',?4,?5)",params![uuid::Uuid::new_v4().to_string(),conversation,run,content,at])?;
    db.execute("UPDATE runs SET status='completed',response=?1,error=?2,updated_at=?3 WHERE id=?4 AND status='queued'",params![content,code,at,run])?;
    db.execute("UPDATE rest_control_candidates SET status=?1,operation_id=?2,error_code=?3,finished_at=?4 WHERE run_id=?5",params![if code.is_some(){"rejected"}else{"completed"},operation,code,at,run])?;
    db.execute("UPDATE qq_text_batches SET closed=1 WHERE run_id=?1", [run])?;
    if crate::turn_control::publication_allowed_in(db, run)? {
        let reply: Option<String> =
            db.query_row("SELECT reply_json FROM runs WHERE id=?1", [run], |r| {
                r.get(0)
            })?;
        if let Some(reply) = reply {
            let target: crate::worker::ReplyTarget = serde_json::from_str(&reply)?;
            db.execute("INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,deadline,created_at) VALUES(?1,?2,?3,?4,?5,?6,'queued',?7,?8)",params![uuid::Uuid::new_v4().to_string(),run,target.account_id,target.credential_ref,reply,content,target.deadline,at])?;
        } else if !crate::weixin_store::queue_reply_in(db, run, content)? && permit.is_some() {
            // A web result becomes available now; there is no network send to
            // grant later. Never let that permit authorize another payload.
            ensure!(
                rest::mark_ack_attempt_started_in(
                    db,
                    run,
                    &crate::rest_publication::payload_digest(content),
                    at
                )?,
                "rest_web_ack_not_permitted"
            );
        }
    }
    crate::trigger_store::observe_interactive_run_in(db, run, "completed")?;
    Ok(())
}

fn controls_in(db: &Connection, clock: &rest::ClockSample) -> anyhow::Result<()> {
    let mut stmt=db.prepare("SELECT c.run_id,c.input_digest,c.received_at,r.input FROM rest_control_candidates c JOIN runs r ON r.id=c.run_id WHERE c.status='pending' AND r.status='queued' ORDER BY r.rowid LIMIT 32")?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    for (run, digest, received, input) in rows {
        let deadline = original_deadline_in(db, &run)?;
        let at = rest::state_in(db)?.logical_at.max(now());
        if at.saturating_sub(received) > 120
            || deadline.is_some_and(|v| v <= at)
            || digest != crate::trigger_store::digest(&json!({"input":input}))
        {
            db.execute("UPDATE rest_control_candidates SET status='rejected',error_code='rest_control_expired_or_changed',finished_at=?1 WHERE run_id=?2",params![now(),run])?;
            db.execute("UPDATE runs SET status='failed',error='rest_control_expired_or_changed',updated_at=?1 WHERE id=?2 AND status='queued'",params![now(),run])?;
            continue;
        }
        // All actual adapter bookkeeping is committed before this pump runs.
        // A group with no trusted speaker can never become an instance control.
        let intent = rest::natural_control(&input);
        db.execute_batch("SAVEPOINT rest_direct_control")?;
        let accepted = (|| -> anyhow::Result<()> {
            let requester = rest::requester_for_run_in(db, &run)?;
            let generation = rest::state_in(db)?.generation;
            let op = match intent {
                Some(rest::ControlIntent::Sleep) => rest::sleep_in(
                    db,
                    &requester,
                    generation,
                    &format!("direct:{run}"),
                    &rest::SleepRequest { wake_at: None },
                    clock,
                )?,
                Some(rest::ControlIntent::Wake) => {
                    let op =
                        rest::wake_in(db, &requester, generation, &format!("direct:{run}"), clock)?;
                    if rest::state_in(db)?.phase == rest::RestPhase::Waking {
                        rest::advance_in(db, clock)?;
                    }
                    op
                }
                _ => anyhow::bail!("rest_direct_control_changed"),
            };
            let content = if matches!(intent, Some(rest::ControlIntent::Sleep)) {
                let zone: chrono_tz::Tz = op.time_zone.parse()?;
                let wake = chrono::DateTime::from_timestamp(
                    op.wake_at
                        .ok_or_else(|| anyhow::anyhow!("rest_wake_time_missing"))?,
                    0,
                )
                .ok_or_else(|| anyhow::anyhow!("rest_wake_time_invalid"))?
                .with_timezone(&zone);
                format!(
                    "已经受理入睡，计划在{}（{}）醒来，正在等待当前请求结束。",
                    wake.format("%Y-%m-%d %H:%M:%S"),
                    op.time_zone
                )
            } else {
                "现在已经醒来了。".to_owned()
            };
            complete_control_in(
                db,
                &run,
                &content,
                op.ack_permit_id.as_deref(),
                Some(&requester),
                Some(&op.id),
                None,
            )?;
            Ok(())
        })();
        if accepted.is_ok() {
            db.execute_batch("RELEASE rest_direct_control")?;
        } else {
            db.execute_batch("ROLLBACK TO rest_direct_control; RELEASE rest_direct_control")?;
            complete_control_in(
                db,
                &run,
                "这条睡眠控制未获受理，请在管理页面核对权限、作息和主机时间。",
                None,
                None,
                None,
                Some("rest_control_not_accepted"),
            )?;
        }
    }
    Ok(())
}

fn schedule_in(db: &Connection, clock: &rest::ClockSample) -> anyhow::Result<()> {
    let (revision, policy) = rest::policy_in(db)?;
    if !policy.schedule_enabled
        || policy.anticipation_enabled
        || rest::state_in(db)?.phase != rest::RestPhase::Awake
    {
        return Ok(());
    }
    let nominal = now().div_euclid(60) * 60;
    let zone: chrono_tz::Tz = policy.time_zone.parse()?;
    let local = chrono::DateTime::from_timestamp(nominal, 0)
        .ok_or_else(|| anyhow::anyhow!("invalid_rest_schedule_time"))?
        .with_timezone(&zone);
    // Neither fold of an ambiguous civil minute silently becomes a second
    // scheduled sleep. The same rule rejects a missing local minute.
    if !matches!(
        chrono::TimeZone::from_local_datetime(&zone, &local.naive_local()),
        chrono::LocalResult::Single(_)
    ) {
        return Ok(());
    }
    let Ok(requester) = rest::requester_for_schedule_in(db, revision, nominal, clock) else {
        return Ok(());
    };
    let fresh = db.execute(
        "INSERT OR IGNORE INTO rest_schedule_receipts VALUES(?1,?2,'considered',NULL)",
        params![revision, nominal],
    )?;
    if fresh == 0 {
        return Ok(());
    }
    let generation = rest::state_in(db)?.generation;
    db.execute_batch("SAVEPOINT rest_schedule_control")?;
    match rest::sleep_in(
        db,
        &requester,
        generation,
        &format!("schedule:{revision}:{nominal}"),
        &rest::SleepRequest { wake_at: None },
        clock,
    ) {
        Ok(op) => {
            db.execute("UPDATE rest_schedule_receipts SET status='accepted',operation_id=?1 WHERE policy_revision=?2 AND nominal_at=?3",params![op.id,revision,nominal])?;
            db.execute_batch("RELEASE rest_schedule_control")?;
        }
        Err(_) => {
            db.execute_batch("ROLLBACK TO rest_schedule_control; RELEASE rest_schedule_control")?;
            db.execute("UPDATE rest_schedule_receipts SET status='rejected' WHERE policy_revision=?1 AND nominal_at=?2",params![revision,nominal])?;
        }
    }
    Ok(())
}

/// Called before NTP/disk/model gates on every scheduler pass. Wake does not
/// await a model or a worker and never ends another subsystem's physical lease.
pub fn tick(state: &AppState) -> crate::error::ApiResult<()> {
    state.db(|db| {
        let tx = db.transaction()?;
        let clock = crate::rest_clock::sample_in(&tx)?;
        crate::daily_life::routine_tick(&tx, &clock, now())?;
        rest::advance_in(&tx, &clock)?;
        if rest::state_in(&tx)?.phase == rest::RestPhase::Waking {
            rest::advance_in(&tx, &clock)?;
        }
        controls_in(&tx, &clock)?;
        schedule_in(&tx, &clock)?;
        tx.commit()?;
        Ok(())
    })
}

/// Invoke only after the daemon has acquired its exclusive data-directory lock.
pub fn recover_after_confirmed_exit(state: &AppState) -> crate::error::ApiResult<()> {
    state.db(|db| {
        let tx = db.transaction()?;
        let clock = crate::rest_clock::sample_in(&tx)?;
        rest::recover_after_confirmed_process_exit_in(&tx, &clock)?;
        // Exclusive daemon lock proves the old process exited. Unknown media
        // is retained for diagnosis; completed stages never repeat the model.
        tx.execute("UPDATE runs SET status='failed',error='rest_media_stage_unknown_after_restart',updated_at=?1 WHERE status='interrupted' AND id IN(SELECT run_id FROM rest_deferred_presentations WHERE media_inflight=1)",[now()])?;
        tx.execute("UPDATE runs SET status='queued',error=NULL,updated_at=?1 WHERE status='interrupted' AND id IN(SELECT run_id FROM rest_deferred_presentations WHERE media_inflight=0)",[now()])?;
        tx.commit()?;
        Ok(())
    })
}

/// A started media stage is uncertain until its checkpoint is committed.
pub fn begin_media_stage(state: &AppState, run: &str) -> Result<bool, String> {
    state.db(|db|{
        let tx=db.transaction()?;
        let running:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND status='running')",[run],|r|r.get(0))?;
        if !running{return Ok(false)}
        if !automatic_allowed_in(&tx)?{
            tx.execute("UPDATE runs SET status='queued',error=NULL,updated_at=?1 WHERE id=?2 AND status='running'",params![now(),run])?;
            tx.commit()?;return Ok(false);
        }
        ensure!(tx.execute("UPDATE rest_deferred_presentations SET media_inflight=1,updated_at=?1 WHERE run_id=?2",params![now(),run])?==1,"rest_media_checkpoint_missing");
        tx.commit()?;Ok(true)
    }).map_err(|_|"媒体阶段检查点不可用，未启动请求".to_owned())
}

#[cfg(test)]
#[path = "rest_runtime_tests.rs"]
mod tests;
