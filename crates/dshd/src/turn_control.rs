//! Durable, conservative turn correction and publication fencing.
//!
//! These functions join the caller's SQLite transaction. They never cancel a
//! Run, mutate a prompt, release a network lease or erase a delivery attempt.
use anyhow::ensure;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize)]
pub struct InputDecision {
    pub input_seq: i64,
    pub predecessor_run_id: Option<String>,
    pub correction_of_run_id: Option<String>,
    pub predecessor_suppressed: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CorrectionContext {
    pub predecessor_run_id: String,
    pub original_input: String,
    pub correction_input: String,
    pub predecessor_decision: String,
    pub reason: String,
}

pub fn ensure_schema(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS conversation_input_sequences(
           conversation_id TEXT PRIMARY KEY REFERENCES conversations(id),
           owner_id TEXT NOT NULL,last_seq INTEGER NOT NULL CHECK(last_seq>=0));
         CREATE TABLE IF NOT EXISTS run_publication_decisions(
           run_id TEXT PRIMARY KEY REFERENCES runs(id),conversation_id TEXT NOT NULL,
           owner_id TEXT NOT NULL,speaker_scope TEXT NOT NULL,
           first_seq INTEGER NOT NULL,last_seq INTEGER NOT NULL,
           predecessor_run_id TEXT REFERENCES runs(id),
           eligible_plain_text INTEGER NOT NULL CHECK(eligible_plain_text IN (0,1)),
           effects_started INTEGER NOT NULL DEFAULT 0 CHECK(effects_started IN (0,1)),
           decision TEXT NOT NULL CHECK(decision IN ('active','suppressed')),
           revision INTEGER NOT NULL CHECK(revision>0),reason TEXT NOT NULL,
           created_ms INTEGER NOT NULL,updated_ms INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS conversation_inputs(
           conversation_id TEXT NOT NULL,owner_id TEXT NOT NULL,input_seq INTEGER NOT NULL,
           run_id TEXT NOT NULL REFERENCES runs(id),message_key TEXT NOT NULL,
           speaker_scope TEXT NOT NULL,content_digest TEXT NOT NULL,received_ms INTEGER NOT NULL,
           PRIMARY KEY(conversation_id,input_seq),UNIQUE(conversation_id,message_key));
         CREATE INDEX IF NOT EXISTS conversation_inputs_run ON conversation_inputs(run_id,input_seq);
         CREATE TABLE IF NOT EXISTS conversation_corrections(
           run_id TEXT PRIMARY KEY REFERENCES runs(id),
           predecessor_run_id TEXT REFERENCES runs(id),reason TEXT NOT NULL,
           predecessor_suppressed INTEGER NOT NULL CHECK(predecessor_suppressed IN (0,1)),
           created_ms INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS run_publication_events(
           run_id TEXT NOT NULL REFERENCES runs(id),revision INTEGER NOT NULL,
           decision TEXT NOT NULL,reason TEXT NOT NULL,successor_run_id TEXT REFERENCES runs(id),
           created_ms INTEGER NOT NULL,PRIMARY KEY(run_id,revision));",
    )?;
    Ok(())
}

fn transaction_required(db: &Connection) -> anyhow::Result<()> {
    ensure!(!db.is_autocommit(), "turn control requires a transaction");
    Ok(())
}

fn table_exists(db: &Connection, name: &str) -> anyhow::Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [name],
        |r| r.get(0),
    )?)
}

/// Closed affirmative forms only. Quoted examples, negation, ordinary thanks,
/// supplements and another question must not retract a prior answer.
pub fn is_explicit_correction(text: &str) -> bool {
    let text = text.trim().trim_end_matches(['。', '！', '!']);
    if text.is_empty()
        || text.len() > 2048
        || [
            "如果",
            "假设",
            "举例",
            "例如",
            "不要取消",
            "不是要取消",
            "别取消",
            "不要改",
            "别改",
            "另外",
            "顺便",
            "?",
            "？",
            "是不是",
            "要不要",
            "能不能",
            "可不可以",
            "好不好",
            "行不行",
            "对不对",
            "为什么",
            "什么",
            "\"",
            "“",
            "”",
            "「",
            "」",
        ]
        .iter()
        .any(|s| text.contains(s))
        || text.ends_with(['吗', '么', '呢'])
    {
        return false;
    }
    if [
        "取消刚才的问题",
        "撤回刚才的问题",
        "刚才的问题不用回答了",
        "上一条不用回答了",
        "先别回答上一条",
        "等等，先别回答",
        "等等，先不要回答",
        "等等，取消",
        "等等,取消",
    ]
    .contains(&text)
    {
        return true;
    }
    [
        "等等，改成",
        "等等,改成",
        "等等 改成",
        "等一下，改成",
        "等一下,改成",
        "刚才那个数字是",
        "刚才的数字是",
        "刚才那个日期是",
        "刚才的日期是",
        "刚才那个时间是",
        "刚才的时间是",
        "刚才那个改成",
        "刚才那条改成",
        "刚才的问题改成",
        "刚才那句改成",
        "刚才说错了，",
        "刚才写错了，",
    ]
    .iter()
    .any(|prefix| {
        text.strip_prefix(prefix)
            .is_some_and(|tail| !tail.trim().is_empty())
    })
}

fn private_speaker(owner: &str, speaker: &str) -> bool {
    !speaker.is_empty()
        && owner == speaker
        && (!owner.starts_with("qq:") || owner.contains(":c2c:"))
        && !owner.contains(":group:")
        && !owner.contains(":channel:")
        && !owner.contains(":dms:")
}

fn visible_predecessor(
    db: &Connection,
    conversation: &str,
    predecessor: &str,
) -> anyhow::Result<bool> {
    if !table_exists(db, "memory_context_watermarks")? {
        return Ok(true);
    }
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM runs r WHERE r.id=?1 AND r.conversation_id=?2
         AND r.rowid>coalesce((SELECT through_run_rowid FROM memory_context_watermarks WHERE conversation_id=?2),0))",
        params![predecessor,conversation], |r| r.get(0),
    )?)
}

/// The only target is the immediately preceding Run, with the same verified
/// private speaker. Every input, including commands and media, must register so
/// such boundaries cannot accidentally be skipped.
#[allow(clippy::too_many_arguments)]
pub fn register_input_in(
    db: &Connection,
    run: &str,
    message_key: &str,
    text: &str,
    speaker_scope: &str,
    eligible_plain_text: bool,
    at_ms: i64,
) -> anyhow::Result<InputDecision> {
    transaction_required(db)?;
    ensure!(
        !message_key.is_empty() && message_key.len() <= 4096 && !message_key.contains('\0'),
        "invalid input identity"
    );
    ensure!(
        !text.is_empty() && text.len() <= 65536 && !text.contains('\0'),
        "invalid input text"
    );
    ensure!(
        speaker_scope.len() <= 1024 && !speaker_scope.contains('\0'),
        "invalid speaker identity"
    );
    let (conversation,owner,kind):(String,String,String)=db.query_row(
        "SELECT r.conversation_id,r.owner_id,r.run_kind FROM runs r JOIN conversations c ON c.id=r.conversation_id AND c.owner_id=r.owner_id WHERE r.id=?1",
        [run],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    )?;
    let content_digest = hex::encode(Sha256::digest(text.as_bytes()));
    if let Some((prior_run,prior_digest,prior_speaker,seq))=db.query_row(
        "SELECT run_id,content_digest,speaker_scope,input_seq FROM conversation_inputs WHERE conversation_id=?1 AND message_key=?2",
        params![conversation,message_key],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?)),
    ).optional()? {
        ensure!(prior_run==run && prior_digest==content_digest && prior_speaker==speaker_scope,"input replay mismatch");
        return input_decision_in(db,run,seq);
    }
    db.execute("INSERT INTO conversation_input_sequences(conversation_id,owner_id,last_seq) VALUES(?1,?2,1) ON CONFLICT(conversation_id) DO UPDATE SET last_seq=last_seq+1 WHERE owner_id=excluded.owner_id",params![conversation,owner])?;
    let seq:i64=db.query_row("SELECT last_seq FROM conversation_input_sequences WHERE conversation_id=?1 AND owner_id=?2",params![conversation,owner],|r|r.get(0))?;
    let predecessor:Option<String>=db.query_row(
        "SELECT id FROM runs WHERE conversation_id=?1 AND owner_id=?2 AND rowid<(SELECT rowid FROM runs WHERE id=?3) ORDER BY rowid DESC LIMIT 1",
        params![conversation,owner,run],|r|r.get(0),
    ).optional()?;
    let eligible = eligible_plain_text && kind == "model" && private_speaker(&owner, speaker_scope);
    let fresh=db.execute(
        "INSERT OR IGNORE INTO run_publication_decisions(run_id,conversation_id,owner_id,speaker_scope,first_seq,last_seq,predecessor_run_id,eligible_plain_text,decision,revision,reason,created_ms,updated_ms) VALUES(?1,?2,?3,?4,?5,?5,?6,?7,'active',1,'input_registered',?8,?8)",
        params![run,conversation,owner,speaker_scope,seq,predecessor,eligible,at_ms],
    )?!=0;
    if !fresh {
        let (saved_speaker, saved_decision): (String, String) = db.query_row(
            "SELECT speaker_scope,decision FROM run_publication_decisions WHERE run_id=?1",
            [run],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        ensure!(
            saved_speaker == speaker_scope && saved_decision == "active",
            "cannot append to a different or suppressed turn"
        );
        db.execute("UPDATE run_publication_decisions SET last_seq=?1,eligible_plain_text=eligible_plain_text AND ?2,updated_ms=?3 WHERE run_id=?4",params![seq,eligible,at_ms,run])?;
    } else {
        db.execute("INSERT INTO run_publication_events(run_id,revision,decision,reason,created_ms) VALUES(?1,1,'active','input_registered',?2)",params![run,at_ms])?;
    }
    db.execute("INSERT INTO conversation_inputs(conversation_id,owner_id,input_seq,run_id,message_key,speaker_scope,content_digest,received_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![conversation,owner,seq,run,message_key,speaker_scope,content_digest,at_ms])?;
    if fresh && is_explicit_correction(text) {
        let mut reason = "no_unique_predecessor".to_string();
        let mut suppressed = false;
        let mut linked = None;
        if eligible {
            if let Some(previous) = predecessor.as_deref() {
                let candidate:Option<(String,bool,String,i64)>=db.query_row(
                    "SELECT speaker_scope,eligible_plain_text,decision,revision FROM run_publication_decisions WHERE run_id=?1 AND conversation_id=?2 AND owner_id=?3",
                    params![previous,conversation,owner],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
                ).optional()?;
                if let Some((speaker, plain, decision, revision)) = candidate {
                    if speaker == speaker_scope && visible_predecessor(db, &conversation, previous)?
                    {
                        linked = Some(previous.to_string());
                        reason = if !plain {
                            "predecessor_not_plain_text".into()
                        } else if decision != "active" {
                            "predecessor_already_suppressed".into()
                        } else {
                            suppression_guard_in(db, previous)?
                        };
                        if reason == "safe_to_suppress" {
                            let changed=db.execute("UPDATE run_publication_decisions SET decision='suppressed',revision=revision+1,reason='explicit_correction',updated_ms=?1 WHERE run_id=?2 AND revision=?3 AND decision='active' AND effects_started=0",params![at_ms,previous,revision])?;
                            ensure!(changed == 1, "publication decision changed");
                            db.execute("INSERT INTO run_publication_events(run_id,revision,decision,reason,successor_run_id,created_ms) VALUES(?1,?2,'suppressed','explicit_correction',?3,?4)",params![previous,revision+1,run,at_ms])?;
                            suppress_queued_in(db, previous, at_ms)?;
                            suppressed = true;
                            reason = "explicit_correction".into();
                        }
                    } else {
                        reason = "speaker_or_visibility_boundary".into();
                    }
                }
            }
        } else {
            reason = "current_turn_not_plain_private_text".into();
        }
        db.execute("INSERT INTO conversation_corrections(run_id,predecessor_run_id,reason,predecessor_suppressed,created_ms) VALUES(?1,?2,?3,?4,?5)",params![run,linked,reason,suppressed,at_ms])?;
    }
    input_decision_in(db, run, seq)
}

fn input_decision_in(db: &Connection, run: &str, seq: i64) -> anyhow::Result<InputDecision> {
    Ok(db.query_row("SELECT p.predecessor_run_id,c.predecessor_run_id,coalesce(c.predecessor_suppressed,0),coalesce(c.reason,'ordinary_successor') FROM run_publication_decisions p LEFT JOIN conversation_corrections c ON c.run_id=p.run_id WHERE p.run_id=?1",[run],|r|Ok(InputDecision{input_seq:seq,predecessor_run_id:r.get(0)?,correction_of_run_id:r.get(1)?,predecessor_suppressed:r.get(2)?,reason:r.get(3)?}))?)
}

fn suppression_guard_in(db: &Connection, run: &str) -> anyhow::Result<String> {
    let effects: bool = db.query_row(
        "SELECT effects_started FROM run_publication_decisions WHERE run_id=?1",
        [run],
        |r| r.get(0),
    )?;
    if effects {
        return Ok("effects_started".into());
    }
    let status: String =
        db.query_row("SELECT status FROM runs WHERE id=?1", [run], |r| r.get(0))?;
    if !["queued", "running", "completed"].contains(&status.as_str()) {
        return Ok("predecessor_not_active".into());
    }
    let mut queued = false;
    if table_exists(db, "outbox")? {
        let outbox: Option<(String, String)> = db
            .query_row("SELECT id,status FROM outbox WHERE run_id=?1", [run], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        if let Some((id, state)) = outbox {
            if ["unknown", "sending"].contains(&state.as_str()) {
                return Ok("delivery_in_flight_or_unknown".into());
            }
            queued = state == "queued";
            if table_exists(db, "outbox_reply_parts")? {
                let unsafe_part:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM outbox_reply_parts WHERE outbox_id=?1 AND status IN ('sending','unknown'))",[&id],|r|r.get(0))?;
                if unsafe_part {
                    return Ok("delivery_in_flight_or_unknown".into());
                }
                let mut statement =
                    db.prepare("SELECT payload_json FROM outbox_reply_parts WHERE outbox_id=?1")?;
                for payload in statement.query_map([&id], |r| r.get::<_, String>(0))? {
                    let payload: serde_json::Value = serde_json::from_str(&payload?)?;
                    if payload.get("type").and_then(|v| v.as_str()) != Some("text") {
                        return Ok("predecessor_media_reply".into());
                    }
                }
            }
            if !queued {
                return Ok("already_delivered_or_terminal".into());
            }
        }
    }
    if table_exists(db, "weixin_outbox")? {
        let state: Option<String> = db
            .query_row(
                "SELECT status FROM weixin_outbox WHERE run_id=?1",
                [run],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(state) = state {
            if ["unknown", "sending"].contains(&state.as_str()) {
                return Ok("delivery_in_flight_or_unknown".into());
            }
            if state != "queued" {
                return Ok("already_delivered_or_terminal".into());
            }
            queued = true;
        }
    }
    if status == "completed" && !queued {
        return Ok("completed_without_pending_delivery".into());
    }
    Ok("safe_to_suppress".into())
}

/// Missing rows are legacy turns and retain revision zero. A suppressed turn
/// has no active revision; its result remains available for restricted audit.
pub fn active_revision_in(db: &Connection, run: &str) -> anyhow::Result<Option<i64>> {
    transaction_required(db)?;
    if !table_exists(db, "run_publication_decisions")? {
        return Ok(Some(0));
    }
    let decision: Option<(String, i64)> = db
        .query_row(
            "SELECT decision,revision FROM run_publication_decisions WHERE run_id=?1",
            [run],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(match decision {
        None => Some(0),
        Some((state, revision)) => (state == "active").then_some(revision),
    })
}

pub fn claim_allowed_in(
    db: &Connection,
    run: &str,
    expected_revision: i64,
) -> anyhow::Result<bool> {
    Ok(active_revision_in(db, run)? == Some(expected_revision))
}

pub fn publication_allowed_in(db: &Connection, run: &str) -> anyhow::Result<bool> {
    Ok(active_revision_in(db, run)?.is_some())
}

/// Call before starting a tool, synthesis or image generation. A correction
/// committed first forbids starting it; a committed effect blocks superseding.
pub fn mark_effects_started_in(db: &Connection, run: &str, at_ms: i64) -> anyhow::Result<bool> {
    transaction_required(db)?;
    let Some(revision) = active_revision_in(db, run)? else {
        return Ok(false);
    };
    if revision == 0 {
        return Ok(true);
    }
    let changed=db.execute("UPDATE run_publication_decisions SET effects_started=1,revision=revision+1,reason='effects_started',updated_ms=?1 WHERE run_id=?2 AND revision=?3 AND decision='active' AND effects_started=0",params![at_ms,run,revision])?;
    if changed != 0 {
        db.execute("INSERT INTO run_publication_events(run_id,revision,decision,reason,created_ms) VALUES(?1,?2,'active','effects_started',?3)",params![run,revision+1,at_ms])?;
    }
    Ok(true)
}

/// Cancel only queued work. Claimed/sent/unknown attempts retain their state,
/// IDs and timestamps. The sender must re-read the revision in its claim txn.
pub fn suppress_queued_in(db: &Connection, run: &str, at_ms: i64) -> anyhow::Result<usize> {
    transaction_required(db)?;
    ensure!(
        !publication_allowed_in(db, run)?,
        "cannot suppress an active publication"
    );
    let mut changed = 0;
    if table_exists(db, "outbox_reply_parts")? {
        changed+=db.execute("UPDATE outbox_reply_parts SET status='cancelled',error='明确更正，后续未发送句段已停止' WHERE status='queued' AND outbox_id IN (SELECT id FROM outbox WHERE run_id=?1)",[run])?;
    }
    if table_exists(db, "outbox")? {
        changed+=db.execute("UPDATE outbox SET status='cancelled',error='明确更正，未发送回复已停止' WHERE run_id=?1 AND status='queued'",[run])?;
    }
    if table_exists(db, "weixin_outbox")? {
        changed+=db.execute("UPDATE weixin_outbox SET status='cancelled',error_code='explicit_correction',content=NULL,updated_at=?1 WHERE run_id=?2 AND status='queued'",params![at_ms/1000,run])?;
    }
    Ok(changed)
}

/// Extra user-role source material for the successor, never a prompt rewrite.
/// Recheck the visibility watermark so forgetting cannot be bypassed by links.
pub fn correction_context_in(
    db: &Connection,
    run: &str,
) -> anyhow::Result<Option<CorrectionContext>> {
    transaction_required(db)?;
    let value:Option<(String,CorrectionContext)>=db.query_row(
        "SELECT current.conversation_id,old.id,old.input,current.input,p.decision,c.reason FROM conversation_corrections c JOIN runs current ON current.id=c.run_id JOIN runs old ON old.id=c.predecessor_run_id AND old.conversation_id=current.conversation_id AND old.owner_id=current.owner_id JOIN run_publication_decisions p ON p.run_id=old.id JOIN run_publication_decisions latest ON latest.run_id=current.id AND latest.speaker_scope=p.speaker_scope WHERE c.run_id=?1",
        [run],|r|Ok((r.get(0)?,CorrectionContext{predecessor_run_id:r.get(1)?,original_input:r.get(2)?,correction_input:r.get(3)?,predecessor_decision:r.get(4)?,reason:r.get(5)?})),
    ).optional()?;
    match value {
        Some((conversation, value))
            if visible_predecessor(db, &conversation, &value.predecessor_run_id)? =>
        {
            Ok(Some(value))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
#[path = "turn_control_tests.rs"]
mod tests;
