//! Versioned, fixed-point affinity policy and one-time downtime recovery.
use super::{Access, DbResult, DAY, MICRO};
use crate::{
    error::{ApiError, ApiResult},
    state::now,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    pub revision: i64,
    pub initial: i64,
    pub minimum: i64,
    pub maximum: i64,
    pub daily_decay: f64,
    pub grace_seconds: i64,
    pub downtime_cap_seconds: i64,
    pub effective_at: i64,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyInput {
    pub initial: i64,
    pub minimum: i64,
    pub maximum: i64,
    pub daily_decay: f64,
    pub grace_seconds: i64,
    pub downtime_cap_seconds: i64,
    pub confirmed: bool,
    pub reason: String,
}
pub fn initialize(db: &Connection) -> anyhow::Result<()> {
    let tx = db.unchecked_transaction()?;
    for (table, column, definition) in [
        (
            "relationship_affinity_states",
            "initial_policy_revision",
            "INTEGER NOT NULL DEFAULT 1",
        ),
        (
            "relationship_affinity_events",
            "requested_delta_micro",
            "INTEGER",
        ),
        (
            "relationship_affinity_events",
            "policy_revision",
            "INTEGER NOT NULL DEFAULT 1",
        ),
    ] {
        let mut st = tx.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = st
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(st);
        if !columns.iter().any(|c| c == column) {
            tx.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} {definition}"
            ))?;
        }
    }
    tx.execute_batch("CREATE TABLE IF NOT EXISTS relationship_affinity_policies(revision INTEGER PRIMARY KEY,initial INTEGER NOT NULL,minimum INTEGER NOT NULL,maximum INTEGER NOT NULL,daily_decay_micro INTEGER NOT NULL,grace_seconds INTEGER NOT NULL,downtime_cap_seconds INTEGER NOT NULL,effective_at INTEGER NOT NULL);
 INSERT OR IGNORE INTO relationship_affinity_policies VALUES(1,50,0,100,1000000,259200,604800,0);")?;
    recover_in(&tx, now())?;
    tx.commit()?;
    Ok(())
}
fn read(r: &rusqlite::Row<'_>) -> rusqlite::Result<Policy> {
    Ok(Policy {
        revision: r.get(0)?,
        initial: r.get(1)?,
        minimum: r.get(2)?,
        maximum: r.get(3)?,
        daily_decay: r.get::<_, i64>(4)? as f64 / MICRO as f64,
        grace_seconds: r.get(5)?,
        downtime_cap_seconds: r.get(6)?,
        effective_at: r.get(7)?,
    })
}
const COLS:&str="revision,initial,minimum,maximum,daily_decay_micro,grace_seconds,downtime_cap_seconds,effective_at";
pub fn current(db: &Connection) -> ApiResult<Policy> {
    db.query_row(
        &format!(
            "SELECT {COLS} FROM relationship_affinity_policies ORDER BY revision DESC LIMIT 1"
        ),
        [],
        read,
    )
    .api()
}
pub fn preview(
    db: &Connection,
    b: &PolicyInput,
    idle_seconds: i64,
) -> ApiResult<serde_json::Value> {
    validate(b)?;
    let current = current(db)?;
    let elapsed = idle_seconds
        .clamp(0, 3650 * DAY)
        .saturating_sub(b.grace_seconds);
    let normalized_rate = (b.daily_decay * MICRO as f64).round() / MICRO as f64;
    let loss = normalized_rate * elapsed as f64 / DAY as f64;
    Ok(
        serde_json::json!({"current_revision":current.revision,"preview_only":true,"idle_seconds":idle_seconds,"projected_from_initial":(b.initial as f64-loss).clamp(b.minimum as f64,b.maximum as f64),"contact_permission_changed":false}),
    )
}
fn validate(b: &PolicyInput) -> ApiResult<()> {
    if !(0..=100).contains(&b.minimum)
        || !(0..=100).contains(&b.maximum)
        || b.minimum >= b.maximum
        || b.initial < b.minimum
        || b.initial > b.maximum
        || !b.daily_decay.is_finite()
        || !(0.0..=10.0).contains(&b.daily_decay)
        || !(0..=30 * DAY).contains(&b.grace_seconds)
        || !(0..=30 * DAY).contains(&b.downtime_cap_seconds)
    {
        return Err(ApiError::bad(
            "invalid_affinity_policy",
            "量表须在0至100内，衰减率每日0至10，宽限和停机补算上限0至30天",
        ));
    }
    super::text(&b.reason, 2000, "策略变更依据")?;
    Ok(())
}
pub fn save(
    db: &mut Connection,
    access: Access<'_>,
    expected: i64,
    b: &PolicyInput,
) -> ApiResult<Policy> {
    super::administrative(access)?;
    validate(b)?;
    if !b.confirmed {
        return Err(ApiError::bad(
            "policy_confirmation_required",
            "好感策略变更须明确确认，不追溯改变旧区间",
        ));
    }
    let tx = db.transaction().api()?;
    let old = current(&tx)?;
    if old.revision != expected {
        return Err(ApiError::conflict());
    }
    let t = now();
    let ids = {
        let mut st = tx
            .prepare("SELECT person_id FROM relationship_affinity_states")
            .api()?;
        let ids = st
            .query_map([], |r| r.get::<_, String>(0))
            .api()?
            .collect::<rusqlite::Result<Vec<_>>>()
            .api()?;
        ids
    };
    for id in &ids {
        super::settle_in(&tx, id, t)?;
    }
    let effective: i64 = tx
        .query_row(
            "SELECT max(?1,coalesce(max(settled_at),?1)) FROM relationship_affinity_states",
            [t],
            |r| r.get(0),
        )
        .api()?;
    tx.execute(
        "INSERT INTO relationship_affinity_policies VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            old.revision + 1,
            b.initial,
            b.minimum,
            b.maximum,
            (b.daily_decay * MICRO as f64).round() as i64,
            b.grace_seconds,
            b.downtime_cap_seconds,
            effective
        ],
    )
    .api()?;
    for id in ids {
        let before: i64 = tx
            .query_row(
                "SELECT score_micro FROM relationship_affinity_states WHERE person_id=?1",
                [&id],
                |r| r.get(0),
            )
            .api()?;
        let after = before.clamp(b.minimum * MICRO, b.maximum * MICRO);
        tx.execute("UPDATE relationship_affinity_states SET score_micro=?1,policy_revision=?2 WHERE person_id=?3",params![after,old.revision+1,id]).api()?;
        if before != after {
            tx.execute("INSERT INTO relationship_affinity_events(id,person_id,event_key,kind,delta_micro,reason,occurred_at,created_at,policy_revision) VALUES(?1,?2,?3,'policy_bounds',?4,'新版策略上下界限制',?5,?5,?6)",params![uuid::Uuid::new_v4().to_string(),id,format!("policy:{}",old.revision+1),after-before,effective,old.revision+1]).api()?;
        }
    }
    super::audit(
        &tx,
        access.actor(),
        "relationships.affinity.policy",
        &format!("revision:{}", old.revision + 1),
    )?;
    let p = current(&tx)?;
    tx.commit().api()?;
    Ok(p)
}
/// The scheduler heartbeat distinguishes covered runtime from an unobserved stop.
/// An outage cap is applied exactly once, then its entire interval is fenced.
pub fn recover_in(db: &Connection, timestamp: i64) -> anyhow::Result<()> {
    let heartbeat: Option<i64> = db
        .query_row(
            "SELECT value FROM relationship_checkpoints WHERE key='heartbeat'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let Some(heartbeat) = heartbeat else {
        db.execute(
            "INSERT INTO relationship_checkpoints(key,value) VALUES('heartbeat',?1)",
            [timestamp],
        )?;
        return Ok(());
    };
    if timestamp <= heartbeat {
        return Ok(());
    }
    let policy = current(db).map_err(|_| anyhow::anyhow!("Affinity policy unavailable"))?;
    let ids = {
        let mut st = db.prepare("SELECT person_id FROM relationship_affinity_states")?;
        let ids = st
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids
    };
    for id in ids {
        super::settle_in(db, &id, heartbeat)
            .map_err(|_| anyhow::anyhow!("Unable to settle covered runtime"))?;
        let (water, last): (i64, Option<i64>) = db.query_row(
            "SELECT settled_at,last_direct_at FROM relationship_affinity_states WHERE person_id=?1",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let created: i64 = db.query_row(
            "SELECT created_at FROM relationship_people WHERE id=?1",
            [&id],
            |r| r.get(0),
        )?;
        let start = water.max(last.unwrap_or(created).saturating_add(policy.grace_seconds));
        let limited = timestamp
            .min(start.saturating_add(policy.downtime_cap_seconds))
            .max(water);
        super::settle_in(db, &id, limited)
            .map_err(|_| anyhow::anyhow!("Unable to settle downtime cap"))?;
        db.execute("UPDATE relationship_affinity_states SET settled_at=max(settled_at,?1) WHERE person_id=?2",params![timestamp,id])?;
        db.execute("INSERT OR IGNORE INTO relationship_affinity_events(id,person_id,event_key,kind,delta_micro,reason,occurred_at,created_at,policy_revision) VALUES(?1,?2,?3,'outage_checkpoint',0,?4,?5,?5,?6)",params![uuid::Uuid::new_v4().to_string(),id,format!("outage:{heartbeat}:{timestamp}:{}",policy.downtime_cap_seconds),format!("停机区间按上限补算；未观测秒数{}，舍弃空闲秒数{}",timestamp-heartbeat,(timestamp-start-policy.downtime_cap_seconds).max(0)),timestamp,policy.revision])?;
    }
    db.execute(
        "UPDATE relationship_checkpoints SET value=max(value,?1) WHERE key='heartbeat'",
        [timestamp],
    )?;
    Ok(())
}

#[derive(Clone)]
struct State {
    score: i64,
    water: i64,
    last: Option<i64>,
    remainder: i64,
    created: i64,
}
fn advance(s: &mut State, p: &Policy, end: i64) {
    let end = end.max(s.water);
    let start = s
        .water
        .max(s.last.unwrap_or(s.created).saturating_add(p.grace_seconds));
    let elapsed = (end - start).max(0);
    let numerator =
        (elapsed as i128) * (p.daily_decay * MICRO as f64).round() as i128 + s.remainder as i128;
    let loss = (numerator / DAY as i128).min(i64::MAX as i128) as i64;
    s.score = (s.score - loss).clamp(p.minimum * MICRO, p.maximum * MICRO);
    s.remainder = if s.score == p.minimum * MICRO {
        0
    } else {
        (numerator % DAY as i128) as i64
    };
    s.water = end;
}
pub fn replay_in(db: &Connection, id: &str) -> anyhow::Result<()> {
    let original_score: i64 = db.query_row(
        "SELECT score_micro FROM relationship_affinity_states WHERE person_id=?1",
        [id],
        |r| r.get(0),
    )?;
    let(created,initial_revision):(i64,i64)=db.query_row("SELECT p.created_at,s.initial_policy_revision FROM relationship_people p JOIN relationship_affinity_states s ON s.person_id=p.id WHERE p.id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let mut st = db.prepare(&format!(
        "SELECT {COLS} FROM relationship_affinity_policies WHERE revision>=?1 ORDER BY revision"
    ))?;
    let policies = st
        .query_map([initial_revision], read)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(st);
    let mut pi = 0usize;
    let first = policies
        .first()
        .ok_or_else(|| anyhow::anyhow!("Affinity initial policy missing"))?;
    let mut s = State {
        score: first.initial * MICRO,
        water: created,
        last: None,
        remainder: 0,
        created,
    };
    let mut st=db.prepare("SELECT coalesce(requested_delta_micro,delta_micro),kind,occurred_at,policy_revision,event_key FROM relationship_affinity_events WHERE person_id=?1 AND deleted_at IS NULL AND kind NOT IN ('decay','policy_bounds','source_replay') ORDER BY occurred_at,rowid")?;
    let events = st
        .query_map([id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(st);
    for (delta, kind, t, rev, key) in events {
        while pi + 1 < policies.len() && policies[pi + 1].revision <= rev {
            advance(&mut s, &policies[pi], policies[pi + 1].effective_at);
            pi += 1;
            s.score = s
                .score
                .clamp(policies[pi].minimum * MICRO, policies[pi].maximum * MICRO);
        }
        if kind == "outage_checkpoint" {
            let parts = key.split(':').collect::<Vec<_>>();
            if parts.len() == 4 {
                let prior = parts[1].parse::<i64>()?;
                let end = parts[2].parse::<i64>()?;
                let cap = parts[3].parse::<i64>()?;
                advance(&mut s, &policies[pi], prior);
                let start = s
                    .water
                    .max(s.last.unwrap_or(created) + policies[pi].grace_seconds);
                advance(&mut s, &policies[pi], end.min(start.saturating_add(cap)));
                s.water = s.water.max(end);
                continue;
            }
        }
        advance(&mut s, &policies[pi], t);
        s.score =
            (s.score + delta).clamp(policies[pi].minimum * MICRO, policies[pi].maximum * MICRO);
        if kind == "direct_interaction" {
            s.last = Some(t);
        }
    }
    while pi + 1 < policies.len() {
        advance(&mut s, &policies[pi], policies[pi + 1].effective_at);
        pi += 1;
        s.score = s
            .score
            .clamp(policies[pi].minimum * MICRO, policies[pi].maximum * MICRO);
    }
    advance(&mut s, &policies[pi], now());
    db.execute("DELETE FROM relationship_affinity_events WHERE person_id=?1 AND kind IN ('decay','policy_bounds')",[id])?;
    db.execute("UPDATE relationship_affinity_states SET score_micro=?1,settled_at=?2,last_direct_at=?3,decay_remainder=?4,policy_revision=?5 WHERE person_id=?6",params![s.score,s.water,s.last,s.remainder,policies[pi].revision,id])?;
    if original_score != s.score {
        db.execute("INSERT INTO relationship_affinity_events(id,person_id,event_key,kind,delta_micro,reason,occurred_at,created_at,policy_revision) VALUES(?1,?2,?3,'source_replay',?4,'来源更正或遗忘，按原策略版本重放有效事件',?5,?5,?6)",params![uuid::Uuid::new_v4().to_string(),id,format!("source-replay:{}",uuid::Uuid::new_v4()),s.score-original_score,now(),policies[pi].revision])?;
    }
    Ok(())
}
