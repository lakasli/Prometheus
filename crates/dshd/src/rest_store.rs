//! Persistent host-owned rest coordination. This module never stops receivers,
//! performs model work, sends messages, or releases another worker's lease.
use crate::auth::Admin;
use anyhow::{ensure, Context};
use chrono::{NaiveTime, TimeZone, Timelike};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: i64 = 1;

fn tx(db: &Connection) -> anyhow::Result<()> {
    ensure!(!db.is_autocommit(), "rest_transaction_required");
    Ok(())
}
fn exists(db: &Connection, table: &str) -> anyhow::Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
        [table],
        |r| r.get(0),
    )?)
}
fn hash(value: impl AsRef<[u8]>) -> String {
    crate::character::digest(value.as_ref())
}
fn assistant(db: &Connection) -> anyhow::Result<String> {
    Ok(db.query_row(
        "SELECT value FROM settings WHERE key='assistant.identity.v1'",
        [],
        |r| r.get(0),
    )?)
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_:-./".contains(c))
}
fn digest_ok(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RestPhase {
    Awake,
    SleepPending,
    Consolidating,
    Sleeping,
    Waking,
}
impl RestPhase {
    fn text(self) -> &'static str {
        match self {
            Self::Awake => "awake",
            Self::SleepPending => "sleep_pending",
            Self::Consolidating => "consolidating",
            Self::Sleeping => "sleeping",
            Self::Waking => "waking",
        }
    }
    fn parse(text: &str) -> anyhow::Result<Self> {
        Ok(match text {
            "awake" => Self::Awake,
            "sleep_pending" => Self::SleepPending,
            "consolidating" => Self::Consolidating,
            "sleeping" => Self::Sleeping,
            "waking" => Self::Waking,
            _ => anyhow::bail!("invalid_rest_phase"),
        })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestPolicy {
    pub natural_enabled: bool,
    pub schedule_enabled: bool,
    pub time_zone: String,
    pub sleep_local: Option<String>,
    pub wake_local: Option<String>,
    pub default_sleep_seconds: Option<i64>,
    pub max_sleep_seconds: i64,
    pub drain_seconds: i64,
    pub consolidation_seconds: i64,
    pub queue_limit: usize,
    pub max_job_attempts: usize,
    pub max_cycle_calls: usize,
    pub max_cycle_reserved_tokens: i64,
    pub ack_seconds: i64,
}
impl Default for RestPolicy {
    fn default() -> Self {
        Self {
            natural_enabled: false,
            schedule_enabled: false,
            time_zone: "Asia/Shanghai".into(),
            sleep_local: None,
            wake_local: None,
            default_sleep_seconds: None,
            max_sleep_seconds: 86400,
            drain_seconds: 30,
            consolidation_seconds: 300,
            queue_limit: 128,
            max_job_attempts: 3,
            max_cycle_calls: 8,
            max_cycle_reserved_tokens: 24000,
            ack_seconds: 30,
        }
    }
}
impl RestPolicy {
    pub fn validate(&self) -> anyhow::Result<()> {
        self.time_zone
            .parse::<chrono_tz::Tz>()
            .context("invalid_rest_timezone")?;
        for value in [&self.sleep_local, &self.wake_local].into_iter().flatten() {
            NaiveTime::parse_from_str(value, "%H:%M").context("invalid_rest_local_time")?;
            ensure!(value.len() == 5, "invalid_rest_local_time");
        }
        ensure!(
            (1..=86400).contains(&self.max_sleep_seconds)
                && self
                    .default_sleep_seconds
                    .is_none_or(|v| v > 0 && v <= self.max_sleep_seconds),
            "invalid_rest_duration"
        );
        ensure!(
            (0..=120).contains(&self.drain_seconds)
                && (0..=1800).contains(&self.consolidation_seconds)
                && (1..=512).contains(&self.queue_limit)
                && (1..=3).contains(&self.max_job_attempts)
                && (1..=24).contains(&self.max_cycle_calls)
                && (1..=96000).contains(&self.max_cycle_reserved_tokens)
                && (1..=120).contains(&self.ack_seconds),
            "invalid_rest_bounds"
        );
        ensure!(
            !self.schedule_enabled || self.sleep_local.is_some(),
            "rest_schedule_time_required"
        );
        if self.natural_enabled {
            ensure!(self.drain_seconds > 0, "rest_control_ack_window_required");
        }
        Ok(())
    }
}
/// Samples are supplied by the host, never deserialized from a model or API.
/// continuity_id identifies one monotonic clock epoch, preferably host boot ID.
#[derive(Debug, Clone)]
pub struct ClockSample {
    wall_at: i64,
    monotonic_ms: i64,
    continuity_id: String,
    trusted_for_new_sleep: bool,
}
impl ClockSample {
    pub fn new(
        wall_at: i64,
        monotonic_ms: i64,
        continuity_id: &str,
        trusted_for_new_sleep: bool,
    ) -> anyhow::Result<Self> {
        ensure!(
            wall_at > 0 && monotonic_ms >= 0 && identifier(continuity_id),
            "invalid_rest_clock_sample"
        );
        Ok(Self {
            wall_at,
            monotonic_ms,
            continuity_id: continuity_id.into(),
            trusted_for_new_sleep,
        })
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct RestState {
    pub assistant_id: String,
    pub generation: i64,
    pub revision: i64,
    pub phase: RestPhase,
    pub cycle_id: Option<String>,
    pub wake_at: Option<i64>,
    pub drain_at: Option<i64>,
    pub consolidate_at: Option<i64>,
    pub logical_at: i64,
    pub reason: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
enum Actor {
    Admin {
        id: String,
        username: String,
    },
    Source {
        run: String,
        owner: String,
        conversation: String,
        speaker: String,
        input_digest: String,
    },
    Schedule {
        policy_revision: i64,
        nominal_at: i64,
    },
}
/// Only verified host ledger constructors can produce a requester.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRequester {
    assistant: String,
    actor: Actor,
    actor_ref: String,
}
impl VerifiedRequester {
    pub fn owner_id(&self) -> Option<&str> {
        if let Actor::Source { owner, .. } = &self.actor {
            Some(owner)
        } else {
            None
        }
    }
    pub fn conversation_id(&self) -> Option<&str> {
        if let Actor::Source { conversation, .. } = &self.actor {
            Some(conversation)
        } else {
            None
        }
    }
    pub fn speaker_ref(&self) -> Option<&str> {
        if let Actor::Source { speaker, .. } = &self.actor {
            Some(speaker)
        } else {
            None
        }
    }
    pub fn source_run_id(&self) -> Option<&str> {
        if let Actor::Source { run, .. } = &self.actor {
            Some(run)
        } else {
            None
        }
    }
    pub fn audit_ref(&self) -> &str {
        &self.actor_ref
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlIntent {
    Sleep,
    Wake,
}
pub fn natural_control(text: &str) -> Option<ControlIntent> {
    if text.len() > 128
        || ['"', '\'', '“', '”', '「', '」', '\n']
            .iter()
            .any(|c| text.contains(*c))
        || ["如果", "假设", "转发", "引用", "例如", "有人说"]
            .iter()
            .any(|s| text.contains(s))
    {
        return None;
    }
    let text = text.trim().trim_end_matches(['。', '！', '!', '～', '~']);
    if [
        "快去睡觉啦",
        "去睡觉吧",
        "现在去睡觉",
        "请去睡觉",
        "请求入睡",
    ]
    .contains(&text)
    {
        Some(ControlIntent::Sleep)
    } else if ["醒醒", "起床啦", "现在醒来", "请醒来", "请求唤醒"].contains(&text) {
        Some(ControlIntent::Wake)
    } else {
        None
    }
}
pub fn requester_for_admin_in(db: &Connection, admin: &Admin) -> anyhow::Result<VerifiedRequester> {
    tx(db)?;
    ensure!(
        admin.role == "admin"
            && db.query_row(
                "SELECT EXISTS(SELECT 1 FROM admins WHERE id=?1 AND username=?2)",
                params![admin.id, admin.username],
                |r| r.get::<_, bool>(0)
            )?,
        "rest_admin_required"
    );
    Ok(VerifiedRequester {
        assistant: assistant(db)?,
        actor: Actor::Admin {
            id: admin.id.clone(),
            username: admin.username.clone(),
        },
        actor_ref: hash(format!("admin\n{}", admin.id)),
    })
}
pub fn requester_for_run_in(db: &Connection, run: &str) -> anyhow::Result<VerifiedRequester> {
    tx(db)?;
    crate::conversation_identity::freeze_run_in(db, run)?;
    let(owner,conversation,input,kind,snapshot):(String,String,String,String,Option<String>)=db.query_row("SELECT r.owner_id,r.conversation_id,r.input,r.run_kind,r.character_snapshot_json FROM runs r JOIN conversations c ON c.id=r.conversation_id AND c.owner_id=r.owner_id WHERE r.id=?1",[run],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
    ensure!(kind == "model", "rest_direct_user_source_required");
    if let Some(snapshot) = snapshot {
        ensure!(
            serde_json::from_str::<serde_json::Value>(&snapshot)?["is_test"] != true,
            "rest_draft_source_denied"
        );
    }
    if exists(db, "conversation_character_bindings")? {
        ensure!(!db.query_row("SELECT EXISTS(SELECT 1 FROM conversation_character_bindings WHERE conversation_id=?1 AND draft_card_json IS NOT NULL)",[&conversation],|r|r.get::<_,bool>(0))?,"rest_draft_source_denied");
    }
    if exists(db, "conversation_images")? {
        ensure!(
            !db.query_row(
                "SELECT EXISTS(SELECT 1 FROM conversation_images WHERE run_id=?1)",
                [run],
                |r| r.get::<_, bool>(0)
            )?,
            "rest_plain_direct_input_required"
        );
    }
    let binding: String = db.query_row(
        "SELECT binding_json FROM run_conversation_bindings WHERE run_id=?1",
        [run],
        |r| r.get(0),
    )?;
    let binding: serde_json::Value = serde_json::from_str(&binding)?;
    let group = matches!(
        binding["conversation_kind"].as_str(),
        Some("group" | "channel" | "dms")
    );
    let speaker = if group {
        let speakers =
            crate::conversation_identity::message_speakers_in(db, run, &conversation, &owner)?;
        let known = speakers
            .iter()
            .filter_map(|v| v["speaker_ref"].as_str())
            .collect::<std::collections::BTreeSet<_>>();
        ensure!(
            !speakers.is_empty()
                && known.len() == 1
                && speakers.iter().all(|v| v["speaker_ref"].is_string()),
            "rest_unknown_or_multiple_speakers"
        );
        (*known.first().context("rest_unknown_speaker")?).to_owned()
    } else {
        format!("private:{}", hash(&owner))
    };
    let actor_ref = hash(serde_json::to_vec(&(&owner, &conversation, &speaker))?);
    Ok(VerifiedRequester {
        assistant: assistant(db)?,
        actor: Actor::Source {
            run: run.into(),
            owner,
            conversation,
            speaker,
            input_digest: hash(input),
        },
        actor_ref,
    })
}
fn validate_requester(db: &Connection, requester: &VerifiedRequester) -> anyhow::Result<()> {
    ensure!(
        requester.assistant == assistant(db)?,
        "rest_instance_changed"
    );
    let current = match &requester.actor {
        Actor::Admin { id, username } => requester_for_admin_in(
            db,
            &Admin {
                id: id.clone(),
                username: username.clone(),
                role: "admin".into(),
                csrf_token: String::new(),
            },
        )?,
        Actor::Source { run, .. } => requester_for_run_in(db, run)?,
        Actor::Schedule {
            policy_revision, ..
        } => {
            let (rev, policy) = policy_in(db)?;
            ensure!(
                rev == *policy_revision && policy.schedule_enabled,
                "rest_schedule_grant_stale"
            );
            requester.clone()
        }
    };
    ensure!(current == *requester, "rest_requester_binding_changed");
    Ok(())
}
fn require_admin(db: &Connection, requester: &VerifiedRequester) -> anyhow::Result<()> {
    validate_requester(db, requester)?;
    ensure!(
        matches!(requester.actor, Actor::Admin { .. }),
        "rest_admin_required"
    );
    Ok(())
}

pub fn initialize(db: &Connection) -> anyhow::Result<()> {
    if exists(db, "rest_meta")? {
        let version: i64 =
            db.query_row("SELECT schema_version FROM rest_meta WHERE id=1", [], |r| {
                r.get(0)
            })?;
        ensure!(version == SCHEMA_VERSION, "rest_reader_version_required");
    }
    db.execute_batch("CREATE TABLE IF NOT EXISTS rest_meta(id INTEGER PRIMARY KEY CHECK(id=1),schema_version INTEGER NOT NULL);
    INSERT OR IGNORE INTO rest_meta VALUES(1,1);
    CREATE TABLE IF NOT EXISTS rest_policies(assistant_id TEXT PRIMARY KEY,revision INTEGER NOT NULL,policy_json TEXT NOT NULL,actor_ref TEXT NOT NULL,updated_at INTEGER NOT NULL);
    CREATE TABLE IF NOT EXISTS assistant_rest_states(assistant_id TEXT PRIMARY KEY,generation INTEGER NOT NULL,revision INTEGER NOT NULL,phase TEXT NOT NULL,cycle_id TEXT,wake_at INTEGER,drain_at INTEGER,consolidate_at INTEGER,logical_at INTEGER NOT NULL,clock_epoch TEXT,mono_ms INTEGER,reason TEXT);
    CREATE TABLE IF NOT EXISTS rest_control_grants(assistant_id TEXT NOT NULL,scope_ref TEXT NOT NULL,owner_id TEXT NOT NULL,conversation_id TEXT NOT NULL,speaker_ref TEXT NOT NULL,revision INTEGER NOT NULL,can_sleep INTEGER NOT NULL,can_wake INTEGER NOT NULL,valid_until INTEGER NOT NULL,actor_ref TEXT NOT NULL,updated_at INTEGER NOT NULL,PRIMARY KEY(assistant_id,scope_ref));
    CREATE TABLE IF NOT EXISTS rest_operations(id TEXT PRIMARY KEY,assistant_id TEXT NOT NULL,actor_ref TEXT NOT NULL,idempotency_key TEXT NOT NULL,request_digest TEXT NOT NULL,result_json TEXT NOT NULL,created_at INTEGER NOT NULL,source_run_id TEXT,UNIQUE(assistant_id,actor_ref,idempotency_key),UNIQUE(assistant_id,source_run_id));
    CREATE TABLE IF NOT EXISTS rest_cycles(id TEXT PRIMARY KEY,assistant_id TEXT NOT NULL,generation INTEGER NOT NULL,trigger_kind TEXT NOT NULL,trigger_ref TEXT NOT NULL,source_run_id TEXT,policy_revision INTEGER NOT NULL,policy_json TEXT NOT NULL,wake_at INTEGER NOT NULL,drain_at INTEGER NOT NULL,consolidate_at INTEGER NOT NULL,run_cutoff INTEGER NOT NULL,event_cutoff INTEGER NOT NULL,entry_cutoff INTEGER NOT NULL,person_cutoff INTEGER NOT NULL,deletion_epoch INTEGER NOT NULL,calls INTEGER NOT NULL DEFAULT 0,reserved_tokens INTEGER NOT NULL DEFAULT 0,created_at INTEGER NOT NULL,finished_at INTEGER,partial INTEGER NOT NULL DEFAULT 0,error_code TEXT,UNIQUE(assistant_id,generation));
    CREATE TABLE IF NOT EXISTS rest_cycle_sources(cycle_id TEXT NOT NULL,owner_id TEXT NOT NULL,policy_revision INTEGER NOT NULL,allow_model INTEGER NOT NULL,policy_digest TEXT NOT NULL,revocation_epoch INTEGER NOT NULL,revocation_watermark INTEGER NOT NULL,PRIMARY KEY(cycle_id,owner_id));
    CREATE TABLE IF NOT EXISTS rest_cycle_refs(cycle_id TEXT NOT NULL,source_kind TEXT NOT NULL,source_id TEXT NOT NULL,source_json TEXT NOT NULL,PRIMARY KEY(cycle_id,source_kind,source_id));
    CREATE TABLE IF NOT EXISTS rest_produced_refs(cycle_id TEXT NOT NULL,source_kind TEXT NOT NULL,source_id TEXT NOT NULL,revision INTEGER NOT NULL,owner_id TEXT NOT NULL,source_json TEXT NOT NULL,job_id TEXT NOT NULL,attempt_id TEXT NOT NULL,job_fence INTEGER NOT NULL,stage INTEGER NOT NULL,parent_digest TEXT NOT NULL,parent_refs_json TEXT NOT NULL,PRIMARY KEY(cycle_id,source_kind,source_id,revision));
    CREATE TABLE IF NOT EXISTS rest_steps(cycle_id TEXT NOT NULL,ordinal INTEGER NOT NULL,kind TEXT NOT NULL,status TEXT NOT NULL,checkpoint_json TEXT NOT NULL,result_digest TEXT,error_code TEXT,updated_at INTEGER NOT NULL,PRIMARY KEY(cycle_id,kind));
    CREATE TABLE IF NOT EXISTS rest_ack_permits(id TEXT PRIMARY KEY,cycle_id TEXT NOT NULL,generation INTEGER NOT NULL,source_run_id TEXT NOT NULL,scope_ref TEXT NOT NULL,grant_revision INTEGER NOT NULL,kind TEXT NOT NULL,payload_digest TEXT,expires_at INTEGER NOT NULL,used_at INTEGER);
    CREATE TABLE IF NOT EXISTS consolidation_jobs(id TEXT PRIMARY KEY,assistant_id TEXT NOT NULL,cycle_id TEXT NOT NULL,owner_id TEXT NOT NULL,kind TEXT NOT NULL,period_start TEXT NOT NULL,period_end TEXT NOT NULL,target_revision INTEGER NOT NULL,status TEXT NOT NULL,fence INTEGER NOT NULL,attempts INTEGER NOT NULL,snapshot_digest TEXT NOT NULL,uses_model INTEGER NOT NULL,checkpoint_json TEXT NOT NULL,result_digest TEXT,error_code TEXT,UNIQUE(assistant_id,owner_id,kind,period_start,period_end,target_revision));
    CREATE TABLE IF NOT EXISTS rest_job_sources(job_id TEXT NOT NULL,source_kind TEXT NOT NULL,source_id TEXT NOT NULL,source_json TEXT NOT NULL,PRIMARY KEY(job_id,source_kind,source_id));
    CREATE TABLE IF NOT EXISTS rest_job_attempts(id TEXT PRIMARY KEY,job_id TEXT NOT NULL,cycle_id TEXT NOT NULL,fence INTEGER NOT NULL,reserved_tokens INTEGER NOT NULL,started_at INTEGER NOT NULL,finished_at INTEGER,outcome TEXT);
    CREATE INDEX IF NOT EXISTS rest_jobs_cycle ON consolidation_jobs(cycle_id,status);
    CREATE INDEX IF NOT EXISTS rest_attempt_physical ON rest_job_attempts(finished_at);")?;
    let id = assistant(db)?;
    db.execute(
        "INSERT OR IGNORE INTO rest_policies VALUES(?1,1,?2,'initial_disabled',0)",
        params![id, serde_json::to_string(&RestPolicy::default())?],
    )?;
    db.execute("INSERT OR IGNORE INTO assistant_rest_states VALUES(?1,0,1,'awake',NULL,NULL,NULL,NULL,0,NULL,NULL,NULL)",[id])?;
    crate::rest_consolidation::initialize(db)?;
    Ok(())
}
pub fn policy_in(db: &Connection) -> anyhow::Result<(i64, RestPolicy)> {
    let (rev, text): (i64, String) = db.query_row(
        "SELECT revision,policy_json FROM rest_policies WHERE assistant_id=?1",
        [assistant(db)?],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let policy: RestPolicy = serde_json::from_str(&text)?;
    policy.validate()?;
    Ok((rev, policy))
}
pub fn save_policy_in(
    db: &Connection,
    admin: &VerifiedRequester,
    expected_revision: i64,
    policy: &RestPolicy,
    at: i64,
) -> anyhow::Result<i64> {
    tx(db)?;
    require_admin(db, admin)?;
    policy.validate()?;
    ensure!(db.execute("UPDATE rest_policies SET revision=revision+1,policy_json=?1,actor_ref=?2,updated_at=?3 WHERE assistant_id=?4 AND revision=?5",params![serde_json::to_string(policy)?,admin.actor_ref,at,admin.assistant,expected_revision])?==1,"rest_policy_revision_conflict");
    Ok(expected_revision + 1)
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantPermissions {
    pub can_sleep: bool,
    pub can_wake: bool,
    pub valid_until: i64,
}
pub fn set_source_grant_in(
    db: &Connection,
    admin: &VerifiedRequester,
    source: &VerifiedRequester,
    expected_revision: i64,
    permissions: &GrantPermissions,
    at: i64,
) -> anyhow::Result<i64> {
    tx(db)?;
    require_admin(db, admin)?;
    validate_requester(db, source)?;
    let Actor::Source {
        owner,
        conversation,
        speaker,
        ..
    } = &source.actor
    else {
        anyhow::bail!("rest_real_source_required")
    };
    ensure!(
        permissions.valid_until > at && permissions.valid_until <= at.saturating_add(366 * 86400),
        "invalid_rest_grant_expiry"
    );
    let current: Option<i64> = db
        .query_row(
            "SELECT revision FROM rest_control_grants WHERE assistant_id=?1 AND scope_ref=?2",
            params![source.assistant, source.actor_ref],
            |r| r.get(0),
        )
        .optional()?;
    ensure!(
        current.unwrap_or(0) == expected_revision,
        "rest_grant_revision_conflict"
    );
    let revision = expected_revision + 1;
    db.execute("INSERT INTO rest_control_grants VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT(assistant_id,scope_ref) DO UPDATE SET revision=excluded.revision,can_sleep=excluded.can_sleep,can_wake=excluded.can_wake,valid_until=excluded.valid_until,actor_ref=excluded.actor_ref,updated_at=excluded.updated_at",params![source.assistant,source.actor_ref,owner,conversation,speaker,revision,permissions.can_sleep,permissions.can_wake,permissions.valid_until,admin.actor_ref,at])?;
    Ok(revision)
}
fn authorize(
    db: &Connection,
    requester: &VerifiedRequester,
    intent: ControlIntent,
    at: i64,
) -> anyhow::Result<i64> {
    validate_requester(db, requester)?;
    match &requester.actor {
        Actor::Admin { .. } => Ok(0),
        Actor::Schedule {
            policy_revision,
            nominal_at,
        } => {
            ensure!(intent == ControlIntent::Sleep, "rest_schedule_cannot_wake");
            ensure!(
                at.abs_diff(*nominal_at) <= 60,
                "rest_schedule_window_expired"
            );
            Ok(*policy_revision)
        }
        Actor::Source { run, .. } => {
            ensure!(
                policy_in(db)?.1.natural_enabled,
                "rest_natural_control_disabled"
            );
            let input: String =
                db.query_row("SELECT input FROM runs WHERE id=?1", [run], |r| r.get(0))?;
            ensure!(
                natural_control(&input) == Some(intent),
                "rest_direct_control_not_recognized"
            );
            let(rev,sleep,wake,until):(i64,bool,bool,i64)=db.query_row("SELECT revision,can_sleep,can_wake,valid_until FROM rest_control_grants WHERE assistant_id=?1 AND scope_ref=?2",params![requester.assistant,requester.actor_ref],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?.context("rest_source_grant_required")?;
            ensure!(
                until > at
                    && if intent == ControlIntent::Sleep {
                        sleep
                    } else {
                        wake
                    },
                "rest_source_grant_denied"
            );
            Ok(rev)
        }
    }
}
pub fn state_in(db: &Connection) -> anyhow::Result<RestState> {
    let mut s=db.query_row("SELECT assistant_id,generation,revision,phase,cycle_id,wake_at,drain_at,consolidate_at,logical_at,reason FROM assistant_rest_states WHERE assistant_id=?1",[assistant(db)?],|r|Ok((r.get::<_,String>(0)?,r.get(1)?,r.get(2)?,r.get::<_,String>(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?)))?;
    Ok(RestState {
        assistant_id: std::mem::take(&mut s.0),
        generation: s.1,
        revision: s.2,
        phase: RestPhase::parse(&s.3)?,
        cycle_id: s.4,
        wake_at: s.5,
        drain_at: s.6,
        consolidate_at: s.7,
        logical_at: s.8,
        reason: s.9,
    })
}
fn sample_in(db: &Connection, clock: &ClockSample) -> anyhow::Result<(i64, bool)> {
    tx(db)?;
    let state = state_in(db)?;
    let (epoch, mono): (Option<String>, Option<i64>) = db.query_row(
        "SELECT clock_epoch,mono_ms FROM assistant_rest_states WHERE assistant_id=?1",
        [&state.assistant_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let continuous = epoch.as_deref() == Some(&clock.continuity_id)
        && mono.is_some_and(|v| clock.monotonic_ms >= v);
    let continuity_lost = state.phase != RestPhase::Awake && epoch.is_some() && !continuous;
    // Keep fractional monotonic remainder; frequent polling must not freeze time.
    let (at, stored_mono) = if continuous {
        let previous = mono.context("rest_monotonic_sample_missing")?;
        let seconds = (clock.monotonic_ms - previous) / 1000;
        let projected = state.logical_at.saturating_add(seconds);
        if clock.wall_at > projected {
            (clock.wall_at, clock.monotonic_ms)
        } else {
            (projected, previous + seconds * 1000)
        }
    } else {
        (state.logical_at.max(clock.wall_at), clock.monotonic_ms)
    };
    db.execute("UPDATE assistant_rest_states SET logical_at=?1,clock_epoch=?2,mono_ms=?3 WHERE assistant_id=?4",params![at,clock.continuity_id,stored_mono,state.assistant_id])?;
    Ok((at, continuity_lost))
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SleepRequest {
    pub wake_at: Option<i64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestOperation {
    pub id: String,
    pub cycle_id: Option<String>,
    pub generation: i64,
    pub phase: RestPhase,
    pub wake_at: Option<i64>,
    pub time_zone: String,
    pub ack_permit_id: Option<String>,
}
fn prior_operation(
    db: &Connection,
    requester: &VerifiedRequester,
    key: &str,
    digest: &str,
) -> anyhow::Result<Option<RestOperation>> {
    ensure!(identifier(key), "invalid_rest_idempotency_key");
    let prior:Option<(String,String)>=db.query_row("SELECT request_digest,result_json FROM rest_operations WHERE assistant_id=?1 AND ((actor_ref=?2 AND idempotency_key=?3) OR source_run_id=?4)",params![requester.assistant,requester.actor_ref,key,requester.source_run_id()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    prior
        .map(|(old, result)| {
            ensure!(old == digest, "rest_idempotency_conflict");
            Ok(serde_json::from_str(&result)?)
        })
        .transpose()
}
fn save_operation(
    db: &Connection,
    requester: &VerifiedRequester,
    key: &str,
    digest: &str,
    result: &RestOperation,
    at: i64,
) -> anyhow::Result<()> {
    db.execute(
        "INSERT INTO rest_operations VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            result.id,
            requester.assistant,
            requester.actor_ref,
            key,
            digest,
            serde_json::to_string(result)?,
            at,
            requester.source_run_id()
        ],
    )?;
    Ok(())
}
fn next_wake(policy: &RestPolicy, requested: Option<i64>, at: i64) -> anyhow::Result<i64> {
    let until = if let Some(until) = requested {
        until
    } else if let Some(local) = &policy.wake_local {
        let zone: chrono_tz::Tz = policy.time_zone.parse()?;
        let now = chrono::DateTime::from_timestamp(at, 0)
            .context("invalid_rest_time")?
            .with_timezone(&zone);
        let time = NaiveTime::parse_from_str(local, "%H:%M")?;
        let mut date = now.date_naive();
        let mut candidate = zone
            .from_local_datetime(&date.and_time(time))
            .single()
            .context("rest_wake_local_time_ambiguous_or_missing")?
            .timestamp();
        if candidate <= at {
            date = date.succ_opt().context("invalid_rest_time")?;
            candidate = zone
                .from_local_datetime(&date.and_time(time))
                .single()
                .context("rest_wake_local_time_ambiguous_or_missing")?
                .timestamp();
        }
        candidate
    } else {
        at.checked_add(
            policy
                .default_sleep_seconds
                .context("rest_wake_time_required")?,
        )
        .context("rest_wake_overflow")?
    };
    ensure!(
        until > at && until - at <= policy.max_sleep_seconds,
        "rest_wake_outside_maximum"
    );
    Ok(until)
}
pub fn requester_for_schedule_in(
    db: &Connection,
    expected_policy_revision: i64,
    nominal_at: i64,
    clock: &ClockSample,
) -> anyhow::Result<VerifiedRequester> {
    tx(db)?;
    let (revision, policy) = policy_in(db)?;
    ensure!(
        clock.trusted_for_new_sleep
            && revision == expected_policy_revision
            && policy.schedule_enabled,
        "rest_schedule_disabled_or_stale"
    );
    ensure!(
        clock.wall_at.abs_diff(nominal_at) <= 60,
        "rest_schedule_window_expired"
    );
    let zone: chrono_tz::Tz = policy.time_zone.parse()?;
    let local = chrono::DateTime::from_timestamp(nominal_at, 0)
        .context("invalid_rest_time")?
        .with_timezone(&zone);
    ensure!(
        Some(format!("{:02}:{:02}", local.hour(), local.minute())) == policy.sleep_local
            && local.second() == 0,
        "rest_schedule_time_mismatch"
    );
    Ok(VerifiedRequester {
        assistant: assistant(db)?,
        actor: Actor::Schedule {
            policy_revision: revision,
            nominal_at,
        },
        actor_ref: hash(format!("schedule:{revision}:{nominal_at}")),
    })
}
fn cutoff(db: &Connection, table: &str) -> anyhow::Result<i64> {
    if exists(db, table)? {
        Ok(db.query_row(
            &format!("SELECT coalesce(max(rowid),0) FROM {table}"),
            [],
            |r| r.get(0),
        )?)
    } else {
        Ok(0)
    }
}
fn deletion_epoch(db: &Connection) -> anyhow::Result<i64> {
    if exists(db, "diary_meta")? {
        Ok(db.query_row(
            "SELECT deletion_epoch FROM diary_meta WHERE id=1",
            [],
            |r| r.get(0),
        )?)
    } else {
        Ok(0)
    }
}
fn freeze_source_scopes(db: &Connection, cycle: &str) -> anyhow::Result<bool> {
    if !exists(db, "diary_source_policies")? {
        return Ok(true);
    }
    let mut statement=db.prepare("SELECT source_owner_id,revision,allow_model,enabled,timezone,authorized_at FROM diary_source_policies WHERE enabled=1 ORDER BY source_owner_id LIMIT 65")?;
    let mut rows = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, bool>(2)?,
                r.get::<_, bool>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let complete = rows.len() <= 64;
    rows.truncate(64);
    for (owner, revision, allow_model, enabled, zone, authorized_at) in rows {
        let (epoch, watermark) = if exists(db, "diary_source_revocations")? {
            db.query_row("SELECT epoch,through_event_rowid FROM diary_source_revocations WHERE source_owner_id=?1",[&owner],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?))).optional()?.unwrap_or((0,0))
        } else {
            (0, 0)
        };
        db.execute(
            "INSERT INTO rest_cycle_sources VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                cycle,
                owner,
                revision,
                allow_model,
                hash(serde_json::to_vec(&(
                    revision,
                    allow_model,
                    enabled,
                    zone,
                    authorized_at
                ))?),
                epoch,
                watermark
            ],
        )?;
    }
    Ok(complete)
}
fn ack_permit(
    db: &Connection,
    requester: &VerifiedRequester,
    cycle: &str,
    generation: i64,
    grant_revision: i64,
    kind: &str,
    expires: i64,
) -> anyhow::Result<Option<String>> {
    let Some(run) = requester.source_run_id() else {
        return Ok(None);
    };
    let id = uuid::Uuid::new_v4().to_string();
    db.execute(
        "INSERT INTO rest_ack_permits VALUES(?1,?2,?3,?4,?5,?6,?7,NULL,?8,NULL)",
        params![
            id,
            cycle,
            generation,
            run,
            requester.actor_ref,
            grant_revision,
            kind,
            expires
        ],
    )?;
    Ok(Some(id))
}
pub fn sleep_in(
    db: &Connection,
    requester: &VerifiedRequester,
    expected_generation: i64,
    key: &str,
    request: &SleepRequest,
    clock: &ClockSample,
) -> anyhow::Result<RestOperation> {
    tx(db)?;
    let (at, lost) = sample_in(db, clock)?;
    let grant_revision = authorize(db, requester, ControlIntent::Sleep, at)?;
    let digest = hash(serde_json::to_vec(&(
        "sleep",
        expected_generation,
        request,
        requester.source_run_id(),
    ))?);
    if let Some(prior) = prior_operation(db, requester, key, &digest)? {
        return Ok(prior);
    }
    let state = state_in(db)?;
    ensure!(
        clock.trusted_for_new_sleep && !lost,
        "rest_clock_not_trusted_for_sleep"
    );
    ensure!(
        state.phase == RestPhase::Awake && state.generation == expected_generation,
        "rest_generation_or_phase_conflict"
    );
    ensure!(
        !matches!(requester.actor, Actor::Source { .. }) || request.wake_at.is_none(),
        "rest_source_duration_requires_explicit_host_parser"
    );
    let (policy_revision, policy) = policy_in(db)?;
    let wake = next_wake(&policy, request.wake_at, at)?;
    let drain = at.saturating_add(policy.drain_seconds).min(wake);
    let consolidate = drain.saturating_add(policy.consolidation_seconds).min(wake);
    let cycle = uuid::Uuid::new_v4().to_string();
    let generation = state
        .generation
        .checked_add(1)
        .context("rest_generation_overflow")?;
    let trigger = match requester.actor {
        Actor::Admin { .. } => "admin",
        Actor::Source { .. } => "direct_source",
        Actor::Schedule { .. } => "schedule",
    };
    db.execute("INSERT INTO rest_cycles(id,assistant_id,generation,trigger_kind,trigger_ref,source_run_id,policy_revision,policy_json,wake_at,drain_at,consolidate_at,run_cutoff,event_cutoff,entry_cutoff,person_cutoff,deletion_epoch,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",params![cycle,state.assistant_id,generation,trigger,requester.actor_ref,requester.source_run_id(),policy_revision,serde_json::to_string(&policy)?,wake,drain,consolidate,cutoff(db,"runs")?,cutoff(db,"diary_events")?,cutoff(db,"diary_entries")?,cutoff(db,"relationship_people")?,deletion_epoch(db)?,at])?;
    // The cycle binds the actual active model profile before sources leave the
    // host. A later profile switch never silently changes this cycle's provider.
    if let Ok((revision, config)) = crate::character::current_config_in(db) {
        db.execute(
            "INSERT INTO rest_consolidation_profiles VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                cycle,
                revision,
                hash(serde_json::to_vec(&config)?),
                config.model.base_url,
                config.model.model,
                config.model.credential_ref
            ],
        )?;
    }
    let scopes_complete = freeze_source_scopes(db, &cycle)?;
    let refs_complete = freeze_cycle_references(db, &cycle)?;
    let snapshot_complete = scopes_complete && refs_complete;
    for (index, kind) in [
        "freeze_sources",
        "diary_day",
        "closed_periods",
        "relationships",
        "affinity_candidates",
        "commit_index",
        "checkpoint",
    ]
    .into_iter()
    .enumerate()
    {
        db.execute(
            "INSERT INTO rest_steps VALUES(?1,?2,?3,'pending','{}',NULL,NULL,?4)",
            params![cycle, index as i64, kind, at],
        )?;
    }
    if !snapshot_complete {
        db.execute("UPDATE rest_steps SET status='partial',error_code='source_snapshot_page_limit' WHERE cycle_id=?1 AND kind='freeze_sources'",[&cycle])?;
        db.execute(
            "UPDATE rest_cycles SET partial=1,error_code='source_snapshot_page_limit' WHERE id=?1",
            [&cycle],
        )?;
    }
    db.execute("UPDATE assistant_rest_states SET generation=?1,revision=revision+1,phase='sleep_pending',cycle_id=?2,wake_at=?3,drain_at=?4,consolidate_at=?5,reason=NULL WHERE assistant_id=?6",params![generation,cycle,wake,drain,consolidate,state.assistant_id])?;
    let permit = ack_permit(
        db,
        requester,
        &cycle,
        generation,
        grant_revision,
        "sleep_accepted",
        at.saturating_add(policy.ack_seconds).min(drain),
    )?;
    let result = RestOperation {
        id: uuid::Uuid::new_v4().to_string(),
        cycle_id: Some(cycle),
        generation,
        phase: RestPhase::SleepPending,
        wake_at: Some(wake),
        time_zone: policy.time_zone,
        ack_permit_id: permit,
    };
    save_operation(db, requester, key, &digest, &result, at)?;
    Ok(result)
}
fn transition(db: &Connection, phase: RestPhase, reason: Option<&str>) -> anyhow::Result<()> {
    db.execute("UPDATE assistant_rest_states SET phase=?1,revision=revision+1,reason=?2 WHERE assistant_id=?3",params![phase.text(),reason,assistant(db)?])?;
    Ok(())
}
fn stop_unfinished(db: &Connection, cycle: &str, code: &str, at: i64) -> anyhow::Result<()> {
    let jobs=db.execute("UPDATE consolidation_jobs SET status=CASE WHEN status='running' THEN 'unknown' ELSE 'failure' END,error_code=?1,result_digest=NULL,fence=fence+1 WHERE cycle_id=?2 AND status IN('pending','running')",params![code,cycle])?;
    let steps=db.execute("UPDATE rest_steps SET status=CASE WHEN status='running' THEN 'unknown' ELSE 'failure' END,error_code=?1,updated_at=?2 WHERE cycle_id=?3 AND status IN('pending','running')",params![code,at,cycle])?;
    if jobs + steps > 0 {
        db.execute(
            "UPDATE rest_cycles SET partial=1,error_code=coalesce(error_code,?1) WHERE id=?2",
            params![code, cycle],
        )?;
    }
    Ok(())
}
pub fn wake_in(
    db: &Connection,
    requester: &VerifiedRequester,
    expected_generation: i64,
    key: &str,
    clock: &ClockSample,
) -> anyhow::Result<RestOperation> {
    tx(db)?;
    let (at, _) = sample_in(db, clock)?;
    let grant_revision = authorize(db, requester, ControlIntent::Wake, at)?;
    let digest = hash(serde_json::to_vec(&(
        "wake",
        expected_generation,
        requester.source_run_id(),
    ))?);
    if let Some(prior) = prior_operation(db, requester, key, &digest)? {
        return Ok(prior);
    }
    let state = state_in(db)?;
    ensure!(
        state.generation == expected_generation,
        "rest_generation_conflict"
    );
    let mut permit = None;
    if let Some(cycle) = &state.cycle_id {
        if !matches!(state.phase, RestPhase::Awake | RestPhase::Waking) {
            stop_unfinished(db, cycle, "explicit_wake", at)?;
            transition(db, RestPhase::Waking, Some("explicit_wake"))?;
        }
        permit = ack_permit(
            db,
            requester,
            cycle,
            state.generation,
            grant_revision,
            "awake",
            at.saturating_add(policy_in(db)?.1.ack_seconds),
        )?;
    }
    let result = RestOperation {
        id: uuid::Uuid::new_v4().to_string(),
        cycle_id: state.cycle_id,
        generation: state.generation,
        phase: state_in(db)?.phase,
        wake_at: state.wake_at,
        time_zone: policy_in(db)?.1.time_zone,
        ack_permit_id: permit,
    };
    save_operation(db, requester, key, &digest, &result, at)?;
    Ok(result)
}
pub fn physical_work_in(db: &Connection) -> anyhow::Result<i64> {
    // Native running_count includes diary/schedule/relationship and Rest leases.
    let mut count = crate::worker::running_count(db)?
        + crate::memory_candidates::running_count(db)?
        + crate::image_analysis::running_count(db)?
        + crate::wasm_workbench::running_count(db)?
        + crate::plugin_factory::running_count(db)?;
    for table in ["outbox", "weixin_outbox"] {
        if exists(db, table)? {
            count += db.query_row(
                &format!("SELECT count(*) FROM {table} WHERE status='sending'"),
                [],
                |r| r.get::<_, i64>(0),
            )?;
        }
    }
    Ok(count)
}
pub fn running_count(db: &Connection) -> anyhow::Result<i64> {
    if exists(db, "rest_job_attempts")? {
        Ok(db.query_row(
            "SELECT count(*) FROM rest_job_attempts WHERE finished_at IS NULL",
            [],
            |r| r.get(0),
        )?)
    } else {
        Ok(0)
    }
}
/// Called before other scheduler health gates. No clock-health/disk-health
/// condition can suppress waking; storage failure must remain visible upstream.
pub fn advance_in(db: &Connection, clock: &ClockSample) -> anyhow::Result<RestState> {
    tx(db)?;
    let (at, lost) = sample_in(db, clock)?;
    let state = state_in(db)?;
    if state.phase == RestPhase::Awake {
        return Ok(state);
    }
    let cycle = state.cycle_id.as_deref().context("rest_cycle_missing")?;
    if state.phase == RestPhase::Waking {
        db.execute(
            "UPDATE rest_cycles SET finished_at=coalesce(finished_at,?1) WHERE id=?2",
            params![at, cycle],
        )?;
        transition(db, RestPhase::Awake, state.reason.as_deref())?;
    } else if lost || state.wake_at.is_some_and(|wake| at >= wake) {
        let code = if lost {
            "clock_continuity_lost"
        } else {
            "wake_deadline"
        };
        stop_unfinished(db, cycle, code, at)?;
        transition(db, RestPhase::Waking, Some(code))?;
    } else if state.phase == RestPhase::SleepPending {
        let pending_ack:i64=db.query_row("SELECT count(*) FROM rest_ack_permits WHERE cycle_id=?1 AND kind='sleep_accepted' AND used_at IS NULL AND expires_at>?2",params![cycle,at],|r|r.get(0))?;
        let active = physical_work_in(db)? + pending_ack;
        if active == 0 || state.drain_at.is_some_and(|deadline| at >= deadline) {
            transition(
                db,
                RestPhase::Consolidating,
                if active > 0 {
                    Some("drain_deadline_with_inflight")
                } else {
                    None
                },
            )?;
            db.execute("UPDATE rest_steps SET status='success',updated_at=?1 WHERE cycle_id=?2 AND kind='freeze_sources' AND status='pending'",params![at,cycle])?;
        }
    } else if state.phase == RestPhase::Consolidating {
        let pending:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM rest_steps WHERE cycle_id=?1 AND status IN('pending','running'))",[cycle],|r|r.get(0))?;
        if !pending || state.consolidate_at.is_some_and(|deadline| at >= deadline) {
            if pending {
                stop_unfinished(db, cycle, "consolidation_deadline", at)?;
            }
            transition(
                db,
                RestPhase::Sleeping,
                if pending {
                    Some("consolidation_partial")
                } else {
                    None
                },
            )?;
        }
    }
    state_in(db)
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum RestAdmission {
    Allow {
        generation: i64,
    },
    Hold {
        generation: i64,
        wake_at: Option<i64>,
    },
    Reject {
        generation: i64,
        code: String,
    },
    PreserveInFlight {
        generation: i64,
    },
}
pub enum RestWork<'a> {
    Receiver,
    Management,
    WakeControl,
    ExistingAttempt,
    Ordinary {
        run_id: &'a str,
        original_deadline: Option<i64>,
    },
    Automatic,
    Publication {
        run_id: &'a str,
        original_deadline: Option<i64>,
        payload_digest: Option<&'a str>,
    },
    Consolidation {
        cycle_id: &'a str,
    },
}
pub fn admission_in(db: &Connection, work: RestWork<'_>, at: i64) -> anyhow::Result<RestAdmission> {
    tx(db)?;
    let state = state_in(db)?;
    let at = at.max(state.logical_at);
    let generation = state.generation;
    if matches!(
        work,
        RestWork::Receiver | RestWork::Management | RestWork::WakeControl
    ) {
        return Ok(RestAdmission::Allow { generation });
    }
    if matches!(work, RestWork::ExistingAttempt) {
        return Ok(RestAdmission::PreserveInFlight { generation });
    }
    let deadline = match &work {
        RestWork::Ordinary {
            original_deadline, ..
        }
        | RestWork::Publication {
            original_deadline, ..
        } => *original_deadline,
        _ => None,
    };
    if deadline.is_some_and(|v| v <= at) {
        return Ok(RestAdmission::Reject {
            generation,
            code: "original_reply_deadline_expired".into(),
        });
    }
    if let RestWork::Publication {
        run_id,
        payload_digest,
        ..
    } = &work
    {
        let control: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM rest_ack_permits WHERE source_run_id=?1)",
            [run_id],
            |r| r.get(0),
        )?;
        if control {
            if let Some(digest) = payload_digest {
                if valid_ack(db, run_id, digest, at, &state)? {
                    return Ok(RestAdmission::Allow { generation });
                }
            }
            // An old control acknowledgement must not become an ordinary
            // queued publication after waking, even if its platform window is
            // longer than the rest period.
            return Ok(RestAdmission::Reject {
                generation,
                code: "rest_control_ack_not_permitted".into(),
            });
        }
    }
    if state.phase == RestPhase::Awake {
        return Ok(RestAdmission::Allow { generation });
    }
    if let RestWork::Consolidation { cycle_id } = &work {
        if state.phase == RestPhase::Consolidating && state.cycle_id.as_deref() == Some(cycle_id) {
            return Ok(RestAdmission::Allow { generation });
        }
    }
    if matches!(work, RestWork::Ordinary { .. }) {
        let count: i64 = db.query_row(
            "SELECT count(*) FROM runs WHERE status='queued' AND run_kind IN('model','skill_text')",
            [],
            |r| r.get(0),
        )?;
        if count > policy_in(db)?.1.queue_limit as i64 {
            return Ok(RestAdmission::Reject {
                generation,
                code: "rest_pending_queue_full".into(),
            });
        }
    }
    Ok(RestAdmission::Hold {
        generation,
        wake_at: state.wake_at,
    })
}
/// Binds a host-built fixed acknowledgement to its original control Run. The
/// payload is a digest, not model text. This function grants no reply credential.
pub fn bind_ack_payload_in(
    db: &Connection,
    requester: &VerifiedRequester,
    permit: &str,
    payload_digest: &str,
    at: i64,
) -> anyhow::Result<()> {
    tx(db)?;
    validate_requester(db, requester)?;
    let at = at.max(state_in(db)?.logical_at);
    ensure!(digest_ok(payload_digest), "invalid_rest_ack_digest");
    ensure!(db.execute("UPDATE rest_ack_permits SET payload_digest=?1 WHERE id=?2 AND source_run_id=?3 AND scope_ref=?4 AND expires_at>?5 AND used_at IS NULL AND (payload_digest IS NULL OR payload_digest=?1)",params![payload_digest,permit,requester.source_run_id(),requester.actor_ref,at])?==1,"rest_ack_permit_unavailable");
    Ok(())
}
fn valid_ack(
    db: &Connection,
    run: &str,
    digest: &str,
    at: i64,
    state: &RestState,
) -> anyhow::Result<bool> {
    let row:Option<(String,String,i64,String,i64)>=db.query_row("SELECT id,scope_ref,grant_revision,kind,expires_at FROM rest_ack_permits WHERE source_run_id=?1 AND generation=?2 AND payload_digest=?3 AND expires_at>?4 AND used_at IS NULL",params![run,state.generation,digest,at],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let Some((_, scope, grant, kind, _)) = row else {
        return Ok(false);
    };
    if (kind == "sleep_accepted" && state.phase != RestPhase::SleepPending)
        || (kind == "awake" && state.phase != RestPhase::Awake)
    {
        return Ok(false);
    }
    let requester = requester_for_run_in(db, run)?;
    if requester.actor_ref != scope {
        return Ok(false);
    }
    let intent = if kind == "sleep_accepted" {
        ControlIntent::Sleep
    } else {
        ControlIntent::Wake
    };
    Ok(authorize(db, &requester, intent, at).is_ok_and(|revision| revision == grant))
}
pub fn mark_ack_attempt_started_in(
    db: &Connection,
    run: &str,
    payload_digest: &str,
    at: i64,
) -> anyhow::Result<bool> {
    tx(db)?;
    let state = state_in(db)?;
    let at = at.max(state.logical_at);
    if !valid_ack(db, run, payload_digest, at, &state)? {
        return Ok(false);
    }
    Ok(db.execute("UPDATE rest_ack_permits SET used_at=?1 WHERE source_run_id=?2 AND generation=?3 AND payload_digest=?4 AND used_at IS NULL",params![at,run,state.generation,payload_digest])?==1)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    DiaryEvent,
    DiaryEntry,
    Person,
    PersonAssertion,
}
impl SourceKind {
    fn text(self) -> &'static str {
        match self {
            Self::DiaryEvent => "diary_event",
            Self::DiaryEntry => "diary_entry",
            Self::Person => "person",
            Self::PersonAssertion => "person_assertion",
        }
    }
    fn table(self) -> &'static str {
        match self {
            Self::DiaryEvent => "diary_events",
            Self::DiaryEntry => "diary_entries",
            Self::Person => "relationship_people",
            Self::PersonAssertion => "relationship_assertions",
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestSourceRef {
    pub kind: SourceKind,
    pub id: String,
    pub owner_id: String,
    pub revision: i64,
    pub rowid: i64,
    pub deletion_epoch: i64,
    pub digest: String,
}
fn source_scope_valid(
    db: &Connection,
    cycle: &str,
    owner: &str,
    uses_model: bool,
) -> anyhow::Result<bool> {
    let frozen:Option<(i64,bool,String,i64,i64)>=db.query_row("SELECT policy_revision,allow_model,policy_digest,revocation_epoch,revocation_watermark FROM rest_cycle_sources WHERE cycle_id=?1 AND owner_id=?2",params![cycle,owner],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let Some((revision, allow_model, digest, epoch, watermark)) = frozen else {
        return Ok(false);
    };
    if uses_model && !allow_model {
        return Ok(false);
    }
    let current:Option<(i64,bool,bool,String,i64)>=db.query_row("SELECT revision,allow_model,enabled,timezone,authorized_at FROM diary_source_policies WHERE source_owner_id=?1",[owner],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let Some(current) = current else {
        return Ok(false);
    };
    if !current.2 || current.0 != revision || hash(serde_json::to_vec(&current)?) != digest {
        return Ok(false);
    }
    let live = if exists(db, "diary_source_revocations")? {
        db.query_row("SELECT epoch,through_event_rowid FROM diary_source_revocations WHERE source_owner_id=?1",[owner],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?))).optional()?.unwrap_or((0,0))
    } else {
        (0, 0)
    };
    Ok(live == (epoch, watermark))
}
fn live_source(
    db: &Connection,
    cycle: &str,
    kind: SourceKind,
    id: &str,
) -> anyhow::Result<Option<RestSourceRef>> {
    live_source_with_output(db, cycle, kind, id, false)
}
fn live_source_with_output(
    db: &Connection,
    cycle: &str,
    kind: SourceKind,
    id: &str,
    registering_output: bool,
) -> anyhow::Result<Option<RestSourceRef>> {
    let(epoch,event_cutoff,entry_cutoff,person_cutoff,created):(i64,i64,i64,i64,i64)=db.query_row("SELECT deletion_epoch,event_cutoff,entry_cutoff,person_cutoff,created_at FROM rest_cycles WHERE id=?1",[cycle],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
    if epoch != deletion_epoch(db)? {
        return Ok(None);
    }
    let sql=match kind{
        SourceKind::DiaryEvent=>"SELECT rowid,source_owner_id,revision,content FROM diary_events WHERE id=?1 AND deleted_at IS NULL AND content IS NOT NULL",
        SourceKind::DiaryEntry=>"SELECT rowid,source_owner_id,revision,content FROM diary_entries WHERE id=?1 AND deleted_at IS NULL AND status!='invalidated'",
        SourceKind::Person=>"SELECT rowid,source_owner_id,revision,json_array(display_name,visibility,source_conversation_id,merged_into) FROM relationship_people WHERE id=?1 AND deleted_at IS NULL AND merged_into IS NULL",
        SourceKind::PersonAssertion=>"SELECT a.rowid,a.source_owner_id,a.revision,json_array(a.person_id,a.field,a.value_json,a.status,a.source_type,a.source_id,a.source_revision,a.valid_from,a.valid_until) FROM relationship_assertions a JOIN relationship_people p ON p.id=a.person_id WHERE a.id=?1 AND a.deleted_at IS NULL AND a.status='approved' AND a.value_json IS NOT NULL AND p.deleted_at IS NULL AND p.merged_into IS NULL AND (p.visibility='shared' OR p.source_owner_id=a.source_owner_id)",
    };
    let row: Option<(i64, Option<String>, i64, String)> = db
        .query_row(sql, [id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()?;
    let Some((rowid, Some(owner), revision, content)) = row else {
        return Ok(None);
    };
    if !source_scope_valid(db, cycle, &owner, false)? {
        return Ok(None);
    }
    let through = match kind {
        SourceKind::DiaryEvent => event_cutoff,
        SourceKind::DiaryEntry => entry_cutoff,
        SourceKind::Person => person_cutoff,
        SourceKind::PersonAssertion => i64::MAX,
    };
    let produced = kind == SourceKind::DiaryEntry && (registering_output || db.query_row("SELECT EXISTS(SELECT 1 FROM rest_produced_refs WHERE cycle_id=?1 AND source_kind='diary_entry' AND source_id=?2 AND revision=?3)",params![cycle,id,revision],|r|r.get::<_,bool>(0))?);
    if rowid > through && !produced {
        return Ok(None);
    }
    let visible = match kind {
        SourceKind::DiaryEvent => crate::diary::event(db, id, Some(&owner)).is_ok(),
        SourceKind::DiaryEntry => {
            let late:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM diary_entry_sources s JOIN diary_events e ON e.id=s.event_id WHERE s.entry_id=?1 AND (e.rowid>?2 OR e.revision!=s.event_revision OR e.deleted_at IS NOT NULL OR e.source_owner_id!=?3))",params![id,event_cutoff,owner],|r|r.get(0))?;
            !late && crate::diary::entry(db, id, Some(&owner)).is_ok()
        }
        SourceKind::Person => true,
        SourceKind::PersonAssertion => {
            // New assertions cannot enter this batch even if their parent is old.
            db.query_row("SELECT created_at<=?2 AND updated_at<=?2 AND (valid_from IS NULL OR valid_from<=?2) AND (valid_until IS NULL OR valid_until>?3) FROM relationship_assertions WHERE id=?1",params![id,created,state_in(db)?.logical_at],|r|r.get::<_,bool>(0))?
        }
    };
    Ok(visible.then(|| RestSourceRef {
        kind,
        id: id.into(),
        owner_id: owner,
        revision,
        rowid,
        deletion_epoch: epoch,
        digest: hash(content),
    }))
}
fn freeze_cycle_references(db: &Connection, cycle: &str) -> anyhow::Result<bool> {
    let owners = {
        let mut s = db.prepare(
            "SELECT owner_id FROM rest_cycle_sources WHERE cycle_id=?1 ORDER BY owner_id",
        )?;
        let values = s
            .query_map([cycle], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        values
    };
    let mut count = 0;
    let mut complete = true;
    for owner in owners {
        for kind in [
            SourceKind::DiaryEvent,
            SourceKind::DiaryEntry,
            SourceKind::Person,
            SourceKind::PersonAssertion,
        ] {
            if !exists(db, kind.table())? {
                continue;
            }
            let ids = {
                let mut s=db.prepare(&format!("SELECT id FROM {} WHERE source_owner_id=?1 AND deleted_at IS NULL ORDER BY rowid LIMIT 513",kind.table()))?;
                let values = s
                    .query_map([&owner], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                values
            };
            if ids.len() == 513 {
                complete = false;
            }
            for id in ids {
                if let Some(reference) = live_source(db, cycle, kind, &id)? {
                    if count == 512 {
                        return Ok(false);
                    }
                    db.execute(
                        "INSERT INTO rest_cycle_refs VALUES(?1,?2,?3,?4)",
                        params![cycle, kind.text(), id, serde_json::to_string(&reference)?],
                    )?;
                    count += 1;
                }
            }
        }
    }
    Ok(complete)
}
/// Reads only the versions frozen when the cycle was accepted, never latest
/// records discovered later in the same cycle. No source body is returned here.
pub fn frozen_source_in(
    db: &Connection,
    cycle: &str,
    kind: SourceKind,
    id: &str,
) -> anyhow::Result<RestSourceRef> {
    tx(db)?;
    let text:String=db.query_row("SELECT source_json FROM rest_cycle_refs WHERE cycle_id=?1 AND source_kind=?2 AND source_id=?3",params![cycle,kind.text(),id],|r|r.get(0))?;
    Ok(serde_json::from_str(&text)?)
}
pub fn validate_source_in(
    db: &Connection,
    cycle: &str,
    reference: &RestSourceRef,
    uses_model: bool,
) -> anyhow::Result<bool> {
    tx(db)?;
    validate_lineage(db, cycle, reference, uses_model, 4)
}

// Four levels cover event -> day -> week/month. Parent stages strictly decrease;
// callers cannot turn an arbitrary latest row into a source by knowing its ID.
fn validate_lineage(
    db: &Connection,
    cycle: &str,
    reference: &RestSourceRef,
    uses_model: bool,
    depth: usize,
) -> anyhow::Result<bool> {
    if !source_scope_valid(db, cycle, &reference.owner_id, uses_model)?
        || live_source(db, cycle, reference.kind, &reference.id)?.as_ref() != Some(reference)
    {
        return Ok(false);
    }
    if frozen_source_in(db, cycle, reference.kind, &reference.id).is_ok_and(|f| f == *reference) {
        return Ok(true);
    }
    if depth == 0 || reference.kind != SourceKind::DiaryEntry {
        return Ok(false);
    }
    let row:Option<(String,String,i64,String,String,i64)>=db.query_row("SELECT source_json,parent_refs_json,stage,parent_digest,job_id,job_fence FROM rest_produced_refs WHERE cycle_id=?1 AND source_kind='diary_entry' AND source_id=?2 AND revision=?3",params![cycle,reference.id,reference.revision],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
    let Some((source, parents, stage, digest, job, fence)) = row else {
        return Ok(false);
    };
    let stored: RestSourceRef = serde_json::from_str(&source)?;
    let parents: Vec<RestSourceRef> = serde_json::from_str(&parents)?;
    if stored != *reference
        || parents.is_empty()
        || parents.len() > 128
        || hash(serde_json::to_vec(&parents)?) != digest
    {
        return Ok(false);
    }
    let valid:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM consolidation_jobs j JOIN rest_produced_refs p ON p.job_id=j.id JOIN rest_job_attempts a ON a.id=p.attempt_id AND a.job_id=j.id AND a.fence=p.job_fence WHERE p.cycle_id=?1 AND p.source_id=?2 AND p.revision=?3 AND j.cycle_id=?1 AND j.owner_id=?4 AND j.status='success' AND j.fence=?5 AND j.id=?6 AND j.result_digest IS NOT NULL AND ((p.stage=1 AND j.kind='diary_day') OR (p.stage=2 AND j.kind='closed_periods')))",params![cycle,reference.id,reference.revision,reference.owner_id,fence,job],|r|r.get(0))?;
    if !valid {
        return Ok(false);
    }
    if job_sources(db, &job)? != parents {
        return Ok(false);
    }
    for parent in parents {
        if parent.owner_id != reference.owner_id
            || parent.id == reference.id
            || !validate_lineage(db, cycle, &parent, uses_model, depth - 1)?
        {
            return Ok(false);
        }
        if !frozen_source_in(db, cycle, parent.kind, &parent.id).is_ok_and(|f| f == parent) {
            let parent_stage:Option<i64>=db.query_row("SELECT stage FROM rest_produced_refs WHERE cycle_id=?1 AND source_kind=?2 AND source_id=?3 AND revision=?4",params![cycle,parent.kind.text(),parent.id,parent.revision],|r|r.get(0)).optional()?;
            if parent_stage.is_none_or(|p| p >= stage) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// Host-only: call after the canonical writer and finish_job_in succeeded in
/// this same transaction. No HTTP or model parameter can construct this proof.
pub(crate) fn register_produced_entry_in(
    db: &Connection,
    claim: &JobClaim,
    entry_id: &str,
) -> anyhow::Result<RestSourceRef> {
    tx(db)?;
    let state = state_in(db)?;
    ensure!(
        state.phase == RestPhase::Consolidating
            && state.cycle_id.as_deref() == Some(claim.cycle_id())
            && state.generation == claim.generation,
        "rest_producer_cycle_stale"
    );
    let (owner,kind,result_digest):(String,String,String)=db.query_row("SELECT owner_id,kind,result_digest FROM consolidation_jobs WHERE id=?1 AND cycle_id=?2 AND fence=?3 AND status='success' AND result_digest IS NOT NULL AND EXISTS(SELECT 1 FROM rest_job_attempts a WHERE a.id=?4 AND a.job_id=consolidation_jobs.id AND a.fence=?3 AND a.finished_at IS NULL)",params![claim.id,claim.cycle,claim.fence,claim.attempt_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let stage = match kind.as_str() {
        "diary_day" => 1,
        "closed_periods" => 2,
        _ => anyhow::bail!("rest_producer_stage_invalid"),
    };
    let parents = job_sources(db, &claim.id)?;
    let actual = {
        let sql = if stage == 1 {
            "SELECT event_id,event_revision FROM diary_entry_sources WHERE entry_id=?1 ORDER BY event_id LIMIT 129"
        } else {
            "SELECT parent_id,parent_revision FROM diary_entry_parents WHERE entry_id=?1 ORDER BY parent_id LIMIT 129"
        };
        let mut stmt = db.prepare(sql)?;
        let rows = stmt
            .query_map([entry_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let expected = parents
        .iter()
        .map(|p| (p.id.clone(), p.revision))
        .collect::<Vec<_>>();
    ensure!(
        actual == expected,
        "rest_produced_canonical_lineage_mismatch"
    );
    for parent in &parents {
        ensure!(
            parent.owner_id == owner
                && parent.id != entry_id
                && validate_source_in(db, &claim.cycle, parent, true)?,
            "rest_producer_parent_stale"
        );
        if stage == 1 {
            ensure!(
                parent.kind == SourceKind::DiaryEvent,
                "rest_day_parent_kind_invalid"
            );
        } else {
            ensure!(
                parent.kind == SourceKind::DiaryEntry,
                "rest_period_parent_kind_invalid"
            );
        }
    }
    let source = live_source_with_output(db, &claim.cycle, SourceKind::DiaryEntry, entry_id, true)?
        .context("rest_produced_entry_unavailable")?;
    ensure!(source.owner_id == owner, "rest_producer_owner_mismatch");
    let (content, generator): (String, String) = db.query_row(
        "SELECT content,generator FROM diary_entries WHERE id=?1",
        [entry_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ensure!(
        generator.starts_with("model:rest-quotes:")
            && result_digest == hash(serde_json::to_vec(&(entry_id, source.revision, content))?),
        "rest_produced_result_digest_mismatch"
    );
    let expected_period = if stage == 1 { "day" } else { "" };
    let period: String = db.query_row(
        "SELECT period FROM diary_entries WHERE id=?1",
        [entry_id],
        |r| r.get(0),
    )?;
    ensure!(
        (stage == 1 && period == expected_period)
            || (stage == 2 && matches!(period.as_str(), "week" | "month")),
        "rest_producer_period_mismatch"
    );
    let text = serde_json::to_string(&parents)?;
    db.execute(
        "INSERT INTO rest_produced_refs VALUES(?1,'diary_entry',?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            claim.cycle,
            entry_id,
            source.revision,
            owner,
            serde_json::to_string(&source)?,
            claim.id,
            claim.attempt_id,
            claim.fence,
            stage,
            hash(serde_json::to_vec(&parents)?),
            text
        ],
    )?;
    ensure!(
        validate_source_in(db, &claim.cycle, &source, true)?,
        "rest_produced_lineage_invalid"
    );
    Ok(source)
}

/// Bounded metadata union. A produced revision replaces its initial revision
/// only when its complete lineage remains valid; it never scans latest rows.
pub(crate) fn cycle_sources_in(db: &Connection, cycle: &str) -> anyhow::Result<Vec<RestSourceRef>> {
    tx(db)?;
    let mut stmt=db.prepare("SELECT source_json FROM rest_cycle_refs WHERE cycle_id=?1 UNION ALL SELECT source_json FROM rest_produced_refs WHERE cycle_id=?1 ORDER BY source_json LIMIT 1025")?;
    let texts = stmt
        .query_map([cycle], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(texts.len() <= 1024, "rest_cycle_source_limit");
    let mut refs = std::collections::BTreeMap::new();
    for text in texts {
        let reference: RestSourceRef = serde_json::from_str(&text)?;
        if validate_source_in(db, cycle, &reference, false)? {
            let key = (reference.kind.text(), reference.id.clone());
            if refs
                .get(&key)
                .is_none_or(|prior: &RestSourceRef| prior.revision < reference.revision)
            {
                refs.insert(key, reference);
            }
        }
    }
    Ok(refs.into_values().collect())
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields, default)]
pub struct StepCheckpoint {
    pub cursor_ref: Option<String>,
    pub processed_refs: usize,
    pub result_digest: Option<String>,
}
impl StepCheckpoint {
    fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.cursor_ref.as_deref().is_none_or(identifier)
                && self.processed_refs <= 100000
                && self.result_digest.as_deref().is_none_or(digest_ok),
            "invalid_rest_checkpoint_metadata"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepOutcome {
    Success,
    Partial,
    Failure,
    Unknown,
}
impl StepOutcome {
    fn text(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Partial => "partial",
            Self::Failure => "failure",
            Self::Unknown => "unknown",
        }
    }
}
#[derive(Debug, Clone)]
pub struct JobSpec {
    pub owner_id: String,
    pub kind: String,
    pub period_start: String,
    pub period_end: String,
    pub target_revision: i64,
    pub uses_model: bool,
    pub sources: Vec<RestSourceRef>,
}
#[derive(Debug, Clone, Serialize)]
pub struct RestJob {
    pub id: String,
    pub cycle_id: String,
    pub status: String,
    pub fence: i64,
    pub attempts: i64,
    pub error_code: Option<String>,
}
#[derive(Debug, Clone)]
pub struct JobClaim {
    id: String,
    cycle: String,
    generation: i64,
    fence: i64,
    attempt_id: String,
}
impl JobClaim {
    pub fn job_id(&self) -> &str {
        &self.id
    }
    pub fn cycle_id(&self) -> &str {
        &self.cycle
    }
    pub fn fence(&self) -> i64 {
        self.fence
    }
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }
}
pub fn job_in(db: &Connection, id: &str) -> anyhow::Result<RestJob> {
    Ok(db.query_row(
        "SELECT id,cycle_id,status,fence,attempts,error_code FROM consolidation_jobs WHERE id=?1",
        [id],
        |r| {
            Ok(RestJob {
                id: r.get(0)?,
                cycle_id: r.get(1)?,
                status: r.get(2)?,
                fence: r.get(3)?,
                attempts: r.get(4)?,
                error_code: r.get(5)?,
            })
        },
    )?)
}
pub fn queue_job_in(db: &Connection, cycle: &str, spec: &JobSpec) -> anyhow::Result<RestJob> {
    tx(db)?;
    let state = state_in(db)?;
    ensure!(
        state.phase == RestPhase::Consolidating && state.cycle_id.as_deref() == Some(cycle),
        "rest_cycle_not_consolidating"
    );
    ensure!(
        [
            "diary_day",
            "closed_periods",
            "relationships",
            "affinity_candidates",
            "commit_index"
        ]
        .contains(&spec.kind.as_str())
            && identifier(&spec.period_start)
            && identifier(&spec.period_end)
            && spec.target_revision > 0
            && !spec.sources.is_empty()
            && spec.sources.len() <= 128,
        "invalid_rest_job_scope"
    );
    ensure!(
        source_scope_valid(db, cycle, &spec.owner_id, spec.uses_model)?,
        "rest_job_source_scope_stale"
    );
    for source in &spec.sources {
        ensure!(
            source.owner_id == spec.owner_id
                && validate_source_in(db, cycle, source, spec.uses_model)?,
            "rest_job_source_snapshot_stale"
        );
    }
    let digest = hash(serde_json::to_vec(&(spec.uses_model, &spec.sources))?);
    let prior:Option<(String,String)>=db.query_row("SELECT id,snapshot_digest FROM consolidation_jobs WHERE assistant_id=?1 AND owner_id=?2 AND kind=?3 AND period_start=?4 AND period_end=?5 AND target_revision=?6",params![state.assistant_id,spec.owner_id,spec.kind,spec.period_start,spec.period_end,spec.target_revision],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((id, old)) = prior {
        ensure!(old == digest, "rest_job_unique_key_snapshot_conflict");
        return job_in(db, &id);
    }
    let queued: i64 = db.query_row(
        "SELECT count(*) FROM consolidation_jobs WHERE cycle_id=?1",
        [cycle],
        |r| r.get(0),
    )?;
    ensure!(queued < 128, "rest_job_queue_limit");
    let id = uuid::Uuid::new_v4().to_string();
    db.execute("INSERT INTO consolidation_jobs VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'pending',0,0,?9,?10,'{}',NULL,NULL)",params![id,state.assistant_id,cycle,spec.owner_id,spec.kind,spec.period_start,spec.period_end,spec.target_revision,digest,spec.uses_model])?;
    for source in &spec.sources {
        db.execute(
            "INSERT INTO rest_job_sources VALUES(?1,?2,?3,?4)",
            params![
                id,
                source.kind.text(),
                source.id,
                serde_json::to_string(source)?
            ],
        )?;
    }
    job_in(db, &id)
}
fn job_sources(db: &Connection, id: &str) -> anyhow::Result<Vec<RestSourceRef>> {
    let mut s=db.prepare("SELECT source_json FROM rest_job_sources WHERE job_id=?1 ORDER BY source_kind,source_id LIMIT 129")?;
    let texts = s
        .query_map([id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        !texts.is_empty() && texts.len() <= 128,
        "invalid_rest_job_source_count"
    );
    texts
        .into_iter()
        .map(|s| serde_json::from_str(&s).map_err(Into::into))
        .collect()
}
fn job_sources_valid(db: &Connection, id: &str) -> anyhow::Result<bool> {
    let (cycle, model): (String, bool) = db.query_row(
        "SELECT cycle_id,uses_model FROM consolidation_jobs WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    for source in job_sources(db, id)? {
        if !validate_source_in(db, &cycle, &source, model)? {
            return Ok(false);
        }
    }
    Ok(true)
}
fn cycle_policy(db: &Connection, cycle: &str) -> anyhow::Result<RestPolicy> {
    let text: String = db.query_row(
        "SELECT policy_json FROM rest_cycles WHERE id=?1",
        [cycle],
        |r| r.get(0),
    )?;
    Ok(serde_json::from_str(&text)?)
}
pub fn claim_job_in(
    db: &Connection,
    id: &str,
    reserved_tokens: i64,
    clock: &ClockSample,
) -> anyhow::Result<Option<JobClaim>> {
    tx(db)?;
    let state = advance_in(db, clock)?;
    let job = job_in(db, id)?;
    if state.phase != RestPhase::Consolidating
        || state.cycle_id.as_deref() != Some(&job.cycle_id)
        || job.status != "pending"
    {
        return Ok(None);
    }
    let step_running:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM rest_steps s JOIN consolidation_jobs j ON j.cycle_id=s.cycle_id AND j.kind=s.kind WHERE j.id=?1 AND s.status='running')",[id],|r|r.get(0))?;
    if !step_running {
        return Ok(None);
    }
    if physical_work_in(db)? > 0 {
        return Ok(None);
    }
    ensure!(reserved_tokens > 0, "rest_job_budget_required");
    let policy = cycle_policy(db, &job.cycle_id)?;
    let (calls, tokens): (i64, i64) = db.query_row(
        "SELECT calls,reserved_tokens FROM rest_cycles WHERE id=?1",
        [&job.cycle_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if job.attempts >= policy.max_job_attempts as i64
        || calls >= policy.max_cycle_calls as i64
        || tokens.saturating_add(reserved_tokens) > policy.max_cycle_reserved_tokens
    {
        return Ok(None);
    }
    if !job_sources_valid(db, id)? {
        db.execute("UPDATE consolidation_jobs SET status='failure',error_code='source_snapshot_stale' WHERE id=?1",[id])?;
        db.execute("UPDATE rest_cycles SET partial=1,error_code=coalesce(error_code,'source_snapshot_stale') WHERE id=?1",[&job.cycle_id])?;
        return Ok(None);
    }
    let attempt_id = uuid::Uuid::new_v4().to_string();
    let fence = job.fence + 1;
    db.execute("UPDATE consolidation_jobs SET status='running',fence=?1,attempts=attempts+1,error_code=NULL WHERE id=?2",params![fence,id])?;
    db.execute(
        "UPDATE rest_cycles SET calls=calls+1,reserved_tokens=reserved_tokens+?1 WHERE id=?2",
        params![reserved_tokens, job.cycle_id],
    )?;
    db.execute(
        "INSERT INTO rest_job_attempts VALUES(?1,?2,?3,?4,?5,?6,NULL,NULL)",
        params![
            attempt_id,
            id,
            job.cycle_id,
            fence,
            reserved_tokens,
            state.logical_at
        ],
    )?;
    Ok(Some(JobClaim {
        id: id.into(),
        cycle: job.cycle_id,
        generation: state.generation,
        fence,
        attempt_id,
    }))
}
pub fn validate_claim_in(db: &Connection, claim: &JobClaim) -> anyhow::Result<bool> {
    tx(db)?;
    let state = state_in(db)?;
    Ok(state.generation==claim.generation&&state.phase==RestPhase::Consolidating&&state.cycle_id.as_deref()==Some(&claim.cycle)&&state.consolidate_at.is_some_and(|at|state.logical_at<at)&&db.query_row("SELECT EXISTS(SELECT 1 FROM consolidation_jobs j JOIN rest_job_attempts a ON a.job_id=j.id AND a.fence=j.fence WHERE j.id=?1 AND j.fence=?2 AND j.status='running' AND a.id=?3 AND a.finished_at IS NULL)",params![claim.id,claim.fence,claim.attempt_id],|r|r.get::<_,bool>(0))?&&job_sources_valid(db,&claim.id)?)
}
pub fn sources_for_claim_in(
    db: &Connection,
    claim: &JobClaim,
) -> anyhow::Result<Vec<RestSourceRef>> {
    ensure!(validate_claim_in(db, claim)?, "rest_claim_stale");
    job_sources(db, &claim.id)
}
pub fn finish_job_in(
    db: &Connection,
    claim: &JobClaim,
    outcome: StepOutcome,
    checkpoint: &StepCheckpoint,
    error_code: Option<&str>,
    clock: &ClockSample,
) -> anyhow::Result<bool> {
    tx(db)?;
    checkpoint.validate()?;
    ensure!(
        error_code.is_none_or(error_code_ok),
        "invalid_rest_job_error_code"
    );
    ensure!(
        outcome != StepOutcome::Success || checkpoint.result_digest.is_some(),
        "rest_success_result_digest_required"
    );
    let _ = advance_in(db, clock)?;
    if !validate_claim_in(db, claim)? {
        let changed=db.execute("UPDATE consolidation_jobs SET status='failure',result_digest=NULL,error_code='source_or_cycle_fence_stale',fence=fence+1 WHERE id=?1 AND fence=?2 AND status='running'",params![claim.id,claim.fence])?;
        if changed > 0 {
            db.execute("UPDATE rest_cycles SET partial=1,error_code=coalesce(error_code,'source_or_cycle_fence_stale') WHERE id=?1",[&claim.cycle])?;
        }
        return Ok(false);
    }
    db.execute("UPDATE consolidation_jobs SET status=?1,checkpoint_json=?2,result_digest=?3,error_code=?4 WHERE id=?5 AND fence=?6",params![outcome.text(),serde_json::to_string(checkpoint)?,checkpoint.result_digest,error_code,claim.id,claim.fence])?;
    if outcome != StepOutcome::Success {
        db.execute(
            "UPDATE rest_cycles SET partial=1 WHERE id=?1",
            [&claim.cycle],
        )?;
    }
    Ok(true)
}
/// Only the owner of the real transport calls this after it has ended/dropped.
/// A stale result may be discarded while this physical request remains counted.
pub fn request_ended_in(
    db: &Connection,
    claim: &JobClaim,
    outcome: StepOutcome,
    at: i64,
) -> anyhow::Result<()> {
    tx(db)?;
    ensure!(db.execute("UPDATE rest_job_attempts SET finished_at=?1,outcome=?2 WHERE id=?3 AND job_id=?4 AND fence=?5 AND finished_at IS NULL",params![at,outcome.text(),claim.attempt_id,claim.id,claim.fence])?==1,"rest_request_already_ended_or_not_owned");
    Ok(())
}
pub fn retry_job_in(db: &Connection, id: &str) -> anyhow::Result<bool> {
    tx(db)?;
    let job = job_in(db, id)?;
    let state = state_in(db)?;
    if state.phase != RestPhase::Consolidating
        || state.cycle_id.as_deref() != Some(&job.cycle_id)
        || !matches!(job.status.as_str(), "failure" | "partial")
        || job.attempts >= cycle_policy(db, &job.cycle_id)?.max_job_attempts as i64
    {
        return Ok(false);
    }
    let owned: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM rest_job_attempts WHERE job_id=?1 AND finished_at IS NULL)",
        [id],
        |r| r.get(0),
    )?;
    if owned || !job_sources_valid(db, id)? {
        return Ok(false);
    }
    db.execute(
        "UPDATE consolidation_jobs SET status='pending',result_digest=NULL WHERE id=?1",
        [id],
    )?;
    Ok(true)
}
pub fn start_step_in(db: &Connection, cycle: &str, kind: &str, at: i64) -> anyhow::Result<()> {
    tx(db)?;
    let state = state_in(db)?;
    ensure!(
        state.phase == RestPhase::Consolidating && state.cycle_id.as_deref() == Some(cycle),
        "rest_cycle_not_consolidating"
    );
    let ordinal: i64 = db.query_row(
        "SELECT ordinal FROM rest_steps WHERE cycle_id=?1 AND kind=?2",
        params![cycle, kind],
        |r| r.get(0),
    )?;
    ensure!(!db.query_row("SELECT EXISTS(SELECT 1 FROM rest_steps WHERE cycle_id=?1 AND ordinal<?2 AND status IN('pending','running'))",params![cycle,ordinal],|r|r.get::<_,bool>(0))?,"rest_previous_step_unfinished");
    ensure!(db.execute("UPDATE rest_steps SET status='running',updated_at=?1 WHERE cycle_id=?2 AND kind=?3 AND status='pending'",params![at,cycle,kind])?==1,"rest_step_not_pending");
    Ok(())
}
pub fn finish_step_in(
    db: &Connection,
    cycle: &str,
    kind: &str,
    outcome: StepOutcome,
    checkpoint: &StepCheckpoint,
    error_code: Option<&str>,
    at: i64,
) -> anyhow::Result<()> {
    tx(db)?;
    checkpoint.validate()?;
    ensure!(
        error_code.is_none_or(error_code_ok),
        "invalid_rest_step_error_code"
    );
    let state = state_in(db)?;
    ensure!(
        state.phase == RestPhase::Consolidating && state.cycle_id.as_deref() == Some(cycle),
        "rest_cycle_not_consolidating"
    );
    let unfinished:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM consolidation_jobs WHERE cycle_id=?1 AND kind=?2 AND status IN('pending','running'))",params![cycle,kind],|r|r.get(0))?;
    ensure!(!unfinished, "rest_step_jobs_unfinished");
    if outcome == StepOutcome::Success {
        ensure!(!db.query_row("SELECT EXISTS(SELECT 1 FROM consolidation_jobs WHERE cycle_id=?1 AND kind=?2 AND status!='success')",params![cycle,kind],|r|r.get::<_,bool>(0))?,"rest_step_has_unsuccessful_jobs");
    }
    ensure!(db.execute("UPDATE rest_steps SET status=?1,checkpoint_json=?2,result_digest=?3,error_code=?4,updated_at=?5 WHERE cycle_id=?6 AND kind=?7 AND status='running'",params![outcome.text(),serde_json::to_string(checkpoint)?,checkpoint.result_digest,error_code,at,cycle,kind])?==1,"rest_step_not_running");
    if outcome != StepOutcome::Success {
        db.execute("UPDATE rest_cycles SET partial=1 WHERE id=?1", [cycle])?;
    }
    Ok(())
}
fn error_code_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

#[derive(Debug, Clone, Serialize)]
pub struct StepView {
    pub kind: String,
    pub status: String,
    pub checkpoint: StepCheckpoint,
    pub result_digest: Option<String>,
    pub error_code: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct CycleProgress {
    pub cycle_id: String,
    pub generation: i64,
    pub wake_at: i64,
    pub partial: bool,
    pub error_code: Option<String>,
    pub steps: Vec<StepView>,
    pub jobs: Vec<RestJob>,
}
pub fn progress_in(db: &Connection, cycle: &str) -> anyhow::Result<CycleProgress> {
    tx(db)?;
    let(generation,wake_at,partial,error_code):(i64,i64,bool,Option<String>)=db.query_row("SELECT generation,wake_at,partial,error_code FROM rest_cycles WHERE id=?1 AND assistant_id=?2",params![cycle,assistant(db)?],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    let rows = {
        let mut s=db.prepare("SELECT kind,status,checkpoint_json,result_digest,error_code FROM rest_steps WHERE cycle_id=?1 ORDER BY ordinal")?;
        let rows = s
            .query_map([cycle], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let steps = rows
        .into_iter()
        .map(|(kind, status, checkpoint, result_digest, error_code)| {
            Ok(StepView {
                kind,
                status,
                checkpoint: serde_json::from_str(&checkpoint)?,
                result_digest,
                error_code,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let ids = {
        let mut s = db.prepare(
            "SELECT id FROM consolidation_jobs WHERE cycle_id=?1 ORDER BY rowid LIMIT 128",
        )?;
        let ids = s
            .query_map([cycle], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids
    };
    let jobs = ids
        .into_iter()
        .map(|id| job_in(db, &id))
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(CycleProgress {
        cycle_id: cycle.into(),
        generation,
        wake_at,
        partial,
        error_code,
        steps,
        jobs,
    })
}
pub fn frozen_sources_page_in(
    db: &Connection,
    cycle: &str,
    owner: &str,
    offset: usize,
    limit: usize,
) -> anyhow::Result<Vec<RestSourceRef>> {
    tx(db)?;
    ensure!(
        offset <= 512 && (1..=64).contains(&limit),
        "rest_source_page_bounds"
    );
    ensure!(
        source_scope_valid(db, cycle, owner, false)?,
        "rest_source_scope_stale"
    );
    let mut s=db.prepare("SELECT source_json FROM rest_cycle_refs WHERE cycle_id=?1 AND json_extract(source_json,'$.owner_id')=?2 ORDER BY source_kind,source_id LIMIT ?3 OFFSET ?4")?;
    let rows = s
        .query_map(params![cycle, owner, limit as i64, offset as i64], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|s| serde_json::from_str(&s).map_err(Into::into))
        .collect()
}
/// Startup-only integration point, after the executable's exclusive data-dir
/// lock proves the former local service exited and its transports are gone.
/// Never invoke this as a timer/retry/cancellation operation. Other systems'
/// native/model leases and all Outbox attempt facts are deliberately untouched.
pub fn recover_after_confirmed_process_exit_in(
    db: &Connection,
    clock: &ClockSample,
) -> anyhow::Result<RestState> {
    tx(db)?;
    db.execute("UPDATE consolidation_jobs SET status='unknown',fence=fence+1,result_digest=NULL,error_code='previous_process_exited' WHERE status='running' AND id IN(SELECT job_id FROM rest_job_attempts WHERE finished_at IS NULL)",[])?;
    db.execute("UPDATE rest_job_attempts SET finished_at=?1,outcome='unknown_previous_process_exited' WHERE finished_at IS NULL",[clock.wall_at])?;
    db.execute("UPDATE rest_cycles SET partial=1 WHERE id IN(SELECT DISTINCT cycle_id FROM consolidation_jobs WHERE status='unknown' AND error_code='previous_process_exited')",[])?;
    advance_in(db, clock)
}

#[cfg(test)]
#[path = "rest_store_tests.rs"]
mod tests;
