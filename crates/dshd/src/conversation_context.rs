//! Host-selected context evidence. Selection never grants access or rewrites a
//! frozen prompt; data and budget revisions have separate invalidation rules.
use crate::{
    context_budget::{self, BlockKind, ContextBlock},
    conversation_state::SourceRef,
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

#[derive(Debug)]
pub struct SourceUnavailable;
impl std::fmt::Display for SourceUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "上下文来源或授权账本暂时不可核验，未发送模型请求；这不表示资料不存在或已被遗忘",
        )
    }
}
impl std::error::Error for SourceUnavailable {}

pub fn source(kind: &str, id: &str, revision: i64, content: &str) -> SourceRef {
    SourceRef {
        kind: kind.into(),
        id: id.into(),
        revision,
        epoch: None,
        digest: Some(crate::character::digest(content.as_bytes())),
    }
}

/// Day/week/month records carry the same original events. Detect overlap in
/// SQL rather than copying an unbounded event list into the prompt or ledger.
pub fn deduplicate_lineage_in(db: &Connection, blocks: &mut [ContextBlock]) -> anyhow::Result<()> {
    for i in 0..blocks.len() {
        for j in i + 1..blocks.len() {
            let a = &blocks[i];
            let b = &blocks[j];
            let Some(ar) = a.source_refs.first() else {
                continue;
            };
            let Some(br) = b.source_refs.first() else {
                continue;
            };
            let overlap = match (a.kind, b.kind) {
                (BlockKind::Diary, BlockKind::Diary) => db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM diary_entry_sources a JOIN diary_entry_sources b ON b.event_id=a.event_id WHERE a.entry_id=?1 AND b.entry_id=?2)", params![ar.id,br.id], |r|r.get::<_,bool>(0))?,
                (BlockKind::Memory, BlockKind::Diary) => db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM diary_entry_sources WHERE entry_id=?2 AND event_id='legacy:'||?1)", params![ar.id,br.id], |r|r.get::<_,bool>(0))?,
                (BlockKind::Diary, BlockKind::Memory) => db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM diary_entry_sources WHERE entry_id=?1 AND event_id='legacy:'||?2)", params![ar.id,br.id], |r|r.get::<_,bool>(0))?,
                _ => false,
            };
            if overlap {
                let key = format!(
                    "lineage:{}",
                    crate::character::digest(format!("{}\n{}", a.id, b.id).as_bytes())
                );
                blocks[i].dedupe_keys.push(key.clone());
                blocks[j].dedupe_keys.push(key);
            }
        }
    }
    Ok(())
}

#[derive(Default)]
pub struct RecallSignals {
    pub lexical: bool,
    pub fallback: bool,
    pub incomplete: bool,
    pub ambiguous: bool,
    pub unavailable: bool,
    pub conflicting: bool,
    pub expired: bool,
}

pub fn recall_decision(s: &RecallSignals) -> Value {
    json!({"status":if s.unavailable {"unavailable"} else if s.conflicting {"conflicting"} else if s.ambiguous {"ambiguous"} else if s.expired {"expired"} else if s.incomplete {"incomplete"} else if s.lexical {"found"} else {"not_found"},
        "retrieval_basis":if s.lexical {"lexical_match"} else if s.fallback {"recent_visible_context"} else {"no_visible_match"},
        "fallback_is_question_match":false,"token_counts_exact":false,
        "conditions":{"unavailable":s.unavailable,"conflicting":s.conflicting,"ambiguous":s.ambiguous,"expired":s.expired,"incomplete":s.incomplete},
        "instruction":"未命中只表示本次有权限检索未找到匹配，不证明从未发生或没有记忆；近期回退不冒充问题命中。歧义只询问必要区别，部分资料不补写省略内容。unavailable只说明检索不可用，不能声称已遗忘；仍可依据单独核验的当前历史回答。授权账本不可核验时宿主停止请求。冲突保留双方来源，不靠自报信心选成真相；expired不当作当前事实。"})
}

/// Only mutually exclusive, explicitly dated scalar assertions are compared.
/// Two hobbies/jobs/names or differently dated ages are not contradictions.
pub fn relevant_person_conflict(input: &str, data: &Value) -> bool {
    let age = input.contains("年龄") || input.contains("几岁");
    let birthday = input.contains("生日") || input.contains("出生");
    if !age && !birthday {
        return false;
    }
    let mut seen = std::collections::BTreeMap::<String, Value>::new();
    for person in data["people"].as_array().into_iter().flatten() {
        for assertion in person["assertions"].as_array().into_iter().flatten() {
            let field = assertion["field"].as_str().unwrap_or("");
            let value = &assertion["value"];
            let key = if field == "age" && age {
                format!("{}:age:{}", person["person_id"], value["stated_at"])
            } else if field == "birthday" && birthday {
                format!(
                    "{}:birthday:{}:{}",
                    person["person_id"], value["calendar"], value["precision"]
                )
            } else {
                continue;
            };
            let fact = if field == "age" {
                json!({"years":value["years"]})
            } else {
                match value["precision"].as_str() {
                    Some("year") => json!({"year":value["year"]}),
                    Some("month_day") => json!({"month":value["month"],"day":value["day"]}),
                    Some("date") => {
                        json!({"year":value["year"],"month":value["month"],"day":value["day"]})
                    }
                    _ => continue,
                }
            };
            if let Some(previous) = seen.insert(key, fact.clone()) {
                if previous != fact {
                    return true;
                }
            }
        }
    }
    false
}

/// Presentation cooldown is deliberately conservative: a short source must
/// have appeared verbatim in an actually delivered answer at the same revision.
/// Merely placing a candidate in an earlier prompt is not evidence of mention.
pub fn recently_presented_in(
    db: &Connection,
    owner: &str,
    conversation: &str,
    run: &str,
    reference: &SourceRef,
    text: &str,
) -> anyhow::Result<bool> {
    if text.len() < 12 || text.len() > 512 {
        return Ok(false);
    }
    let usage=match reference.kind.as_str(){
        "memory"=>"EXISTS(SELECT 1 FROM memory_run_uses u WHERE u.run_id=r.id AND u.memory_id=?4 AND u.revision=?5)",
        "diary_entry"=>"EXISTS(SELECT 1 FROM diary_run_uses u WHERE u.run_id=r.id AND u.entry_id=?4 AND u.revision=?5)",
        _=>return Ok(false),
    };
    let mut statement=db.prepare(&format!("SELECT r.id FROM runs r WHERE r.id IN(SELECT id FROM runs WHERE owner_id=?1 AND conversation_id=?2 AND status='completed' AND rowid<(SELECT rowid FROM runs WHERE id=?3) ORDER BY rowid DESC LIMIT 4) AND {usage}"))?;
    let runs = statement
        .query_map(
            params![owner, conversation, run, reference.id, reference.revision],
            |r| r.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for source_run in runs {
        if crate::delivery_context::project_run_in(db, owner, conversation, &source_run).is_ok_and(
            |p| {
                p.assistant_content
                    .as_ref()
                    .is_some_and(|content| content.contains(text))
            },
        ) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Validate only snapshots that actually existed for this run. Legacy prompts
/// cannot receive invented state or budget proof during a retry.
pub fn validate_derived_in(db: &Connection, run: &str) -> anyhow::Result<bool> {
    if !crate::daily_life::context_valid(db, run)? {
        return Ok(false);
    }
    if !crate::conversation_invalidation::initialized_schema(db)? {
        // A release predating the entire feature has no new evidence to check.
        // This read must never retrofit a snapshot onto historical results.
        return Ok(true);
    }
    let state: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM conversation_state_snapshots WHERE run_id=?1)",
        [run],
        |r| r.get(0),
    )?;
    let commitments: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM conversation_commitment_contexts WHERE run_id=?1)",
        [run],
        |r| r.get(0),
    )?;
    if state || commitments {
        // Reading an already frozen result must not reconstruct missing old
        // binding/context/Prompt evidence through fresh-scope constructors.
        for table in [
            "run_conversation_bindings",
            "run_conversation_contexts",
            "run_prompt_snapshots",
        ] {
            if !crate::conversation_state::table_exists(db, table)?
                || !db.query_row(
                    &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE run_id=?1)"),
                    [run],
                    |r| r.get::<_, bool>(0),
                )?
            {
                return Ok(false);
            }
        }
        if !crate::conversation_identity::validate_context_in(db, run)? {
            return Ok(false);
        }
    }
    if state && !crate::conversation_state::validate_context_in(db, run)? {
        return Ok(false);
    }
    if commitments
        && !crate::conversation_commitments::validate_context_in(db, run, crate::state::now())?
    {
        return Ok(false);
    }
    Ok(true)
}

/// Cancel future delivery only. Physical leases and already claimed, accepted
/// or unknown attempts remain facts; this is not a correction or history erase.
pub fn cancel_queued_delivery_in(
    db: &Connection,
    run: &str,
    reason: &'static str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !db.is_autocommit(),
        "delivery fencing requires a transaction"
    );
    let exists = crate::conversation_state::table_exists;
    if exists(db, "outbox")? {
        if exists(db, "outbox_reply_parts")? {
            db.execute("UPDATE outbox_reply_parts SET status='cancelled',error=?1 WHERE status='queued' AND outbox_id IN(SELECT id FROM outbox WHERE run_id=?2)",params![reason,run])?;
        }
        db.execute(
            "UPDATE outbox SET status='cancelled',error=?1 WHERE run_id=?2 AND status='queued'",
            params![reason, run],
        )?;
    }
    if exists(db, "weixin_outbox")? {
        db.execute("UPDATE weixin_outbox SET status='cancelled',content=NULL,error_code=?1,updated_at=?2 WHERE run_id=?3 AND status='queued'",params![reason,crate::state::now(),run])?;
    }
    Ok(())
}

/// Call inside the same transaction that changes queued -> sending. A failed
/// source check becomes a durable, narrowly scoped queue fence, not a retry.
pub fn claim_delivery_allowed_in(db: &Connection, run: &str) -> anyhow::Result<bool> {
    anyhow::ensure!(!db.is_autocommit(), "delivery proof requires a transaction");
    let reason = match validate_derived_in(db, run) {
        Ok(true) => return Ok(true),
        Ok(false) => "conversation_context_stale",
        Err(_) => "conversation_context_unavailable",
    };
    cancel_queued_delivery_in(db, run, reason)?;
    Ok(false)
}

/// Sweep only a bounded set of still-unclaimed derived deliveries, including
/// remaining queued QQ parts behind an unknown/sending parent. Preserve that
/// parent's uncertain outcome; only its future parts are fenced.
pub fn fence_queued_delivery_candidates_in(db: &Connection, qq: bool) -> anyhow::Result<()> {
    anyhow::ensure!(
        !db.is_autocommit(),
        "delivery fencing requires a transaction"
    );
    let initialized = crate::conversation_invalidation::initialized_schema(db);
    if matches!(initialized, Ok(false)) {
        return Ok(());
    }
    let mut sql = if qq {
        if crate::conversation_state::table_exists(db, "outbox_reply_parts")? {
            "SELECT o.run_id FROM outbox o WHERE (o.status='queued' OR EXISTS(SELECT 1 FROM outbox_reply_parts p WHERE p.outbox_id=o.id AND p.status='queued'))".to_owned()
        } else {
            "SELECT o.run_id FROM outbox o WHERE o.status='queued'".to_owned()
        }
    } else {
        "SELECT o.run_id FROM weixin_outbox o WHERE o.status='queued'".to_owned()
    };
    if initialized.is_ok() {
        sql.push_str(" AND (EXISTS(SELECT 1 FROM conversation_state_snapshots s WHERE s.run_id=o.run_id) OR EXISTS(SELECT 1 FROM conversation_commitment_contexts c WHERE c.run_id=o.run_id))");
    }
    sql.push_str(" GROUP BY o.run_id ORDER BY min(o.rowid) LIMIT 64");
    let mut statement = db.prepare(&sql)?;
    let runs = statement
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for run in runs {
        claim_delivery_allowed_in(db, &run)?;
    }
    Ok(())
}

pub fn validate_request_in(
    db: &Connection,
    run: &str,
    model: &str,
    messages: &[Value],
    response_tokens: u64,
) -> anyhow::Result<bool> {
    if !validate_derived_in(db, run)? {
        return Ok(false);
    }
    if context_budget::snapshot_in(db, run)?.is_some() {
        return context_budget::validate_in(db, run, model, messages, response_tokens);
    }
    Ok(true)
}

#[cfg(test)]
#[path = "conversation_context_tests.rs"]
mod tests;
