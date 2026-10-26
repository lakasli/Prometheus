//! Deterministic context selection with a versioned, explicitly estimated budget.
//! Blocks are indivisible. Persistent evidence contains references and hashes,
//! never another copy of source text or an already frozen prompt.
use anyhow::{ensure, Context};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub use crate::conversation_state::SourceRef;

pub const MAX_INPUT_BYTES: u64 = 96_000;
pub const MAX_RESPONSE_TOKENS: u64 = 4_096;
pub const ESTIMATOR_VERSION: &str = "utf8_serialized_bytes_v1";
const MAX_BLOCKS: usize = 128;
const MAX_CANDIDATE_BYTES: usize = 1_048_576;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BudgetPolicy {
    pub schema_version: u32,
    pub revision: i64,
    pub model_id: String,
    /// None means the provider's actual context window is unknown.
    pub provider_context_window: Option<u64>,
    /// Endpoint binding prevents a declared window following a same-name
    /// model to a different service. Only the hash is persisted here.
    pub provider_fingerprint: Option<String>,
    pub window_basis: String,
    /// A local planning limit, never advertised as the provider's real window.
    pub estimated_input_limit: u64,
    pub max_input_bytes: u64,
    pub response_reserve_tokens: u64,
    pub protocol_reserve_tokens: u64,
    pub safety_reserve_tokens: u64,
    pub estimator_version: String,
}

impl BudgetPolicy {
    pub fn defaults(model_id: &str) -> Self {
        Self {
            schema_version: 1,
            revision: 0,
            model_id: model_id.to_owned(),
            provider_context_window: None,
            provider_fingerprint: None,
            window_basis: "unknown".into(),
            estimated_input_limit: MAX_INPUT_BYTES,
            max_input_bytes: MAX_INPUT_BYTES,
            response_reserve_tokens: MAX_RESPONSE_TOKENS,
            protocol_reserve_tokens: 1_024,
            safety_reserve_tokens: 4_096,
            estimator_version: ESTIMATOR_VERSION.into(),
        }
    }

    fn input_limit(&self) -> u64 {
        let reserved = self.response_reserve_tokens
            + self.protocol_reserve_tokens
            + self.safety_reserve_tokens;
        self.provider_context_window
            .map(|window| window.saturating_sub(reserved))
            .unwrap_or(self.estimated_input_limit)
            .min(self.estimated_input_limit)
    }
}

pub fn provider_fingerprint(endpoint: &str) -> String {
    hash(endpoint.trim().trim_end_matches('/').as_bytes())
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn safe_identifier(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn validate_policy(policy: &BudgetPolicy) -> anyhow::Result<()> {
    ensure!(
        policy.schema_version == 1 && policy.revision >= 0,
        "unsupported budget policy version"
    );
    ensure!(
        safe_identifier(&policy.model_id, 200),
        "invalid budget model identity"
    );
    ensure!(
        policy.estimator_version == ESTIMATOR_VERSION,
        "unsupported token estimator"
    );
    ensure!(
        (1..=MAX_INPUT_BYTES).contains(&policy.max_input_bytes),
        "input byte limit exceeds host bound"
    );
    ensure!(
        (1..=MAX_INPUT_BYTES).contains(&policy.estimated_input_limit),
        "estimated input limit exceeds host bound"
    );
    ensure!(
        (1..=MAX_RESPONSE_TOKENS).contains(&policy.response_reserve_tokens),
        "response reservation exceeds host bound"
    );
    ensure!(
        (256..=8_192).contains(&policy.protocol_reserve_tokens),
        "invalid protocol reservation"
    );
    ensure!(
        (512..=16_384).contains(&policy.safety_reserve_tokens),
        "invalid safety reservation"
    );
    ensure!(
        policy
            .provider_fingerprint
            .as_deref()
            .is_none_or(valid_digest),
        "invalid provider fingerprint"
    );
    match policy.provider_context_window {
        None => ensure!(
            policy.window_basis == "unknown",
            "unknown model window requires unknown basis"
        ),
        Some(window) => {
            ensure!(
                policy.window_basis == "administrator_declared",
                "provider window is an explicit administrator declaration"
            );
            ensure!(
                policy.provider_fingerprint.is_some(),
                "declared window requires an endpoint binding"
            );
            let reserved = policy.response_reserve_tokens
                + policy.protocol_reserve_tokens
                + policy.safety_reserve_tokens;
            ensure!(
                window > reserved && window <= 1_048_576,
                "declared window cannot fit reservations or exceeds host bound"
            );
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    HostPolicy,
    CurrentInput,
    Correction,
    Delivery,
    Focus,
    Commitment,
    History,
    Summary,
    Memory,
    Diary,
    Person,
    Style,
    ToolResult,
}

/// A bounded host selection fact. This never changes source authorization,
/// visibility, stored memory or permission to execute an action.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionReason {
    RecentlyPresented,
}

impl ExclusionReason {
    fn label(self) -> &'static str {
        match self {
            Self::RecentlyPresented => "recently_presented",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextBlock {
    pub id: String,
    pub kind: BlockKind,
    pub required: bool,
    /// Larger values are selected before smaller values; ties retain input order.
    pub priority: u16,
    pub messages: Vec<Value>,
    /// The host builder supplies the versions/epochs of all source material it
    /// has already authorized. Budget selection is not an ACL grant.
    pub source_refs: Vec<SourceRef>,
    /// Host-supplied evidence identities, e.g. the same raw source underlying
    /// a day/week/month recollection. Intersecting optional groups are dropped.
    pub dedupe_keys: Vec<String>,
    /// Members of an atomic group are selected or dropped together.
    pub atomic_group: Option<String>,
    /// Set only by the authorized Rust builder after checking actual settled
    /// usage. JSON input cannot mark a block as recently presented. Omission
    /// keeps older blocks and their default candidate digests compatible.
    #[serde(default, skip_serializing_if = "Option::is_none", skip_deserializing)]
    pub exclusion_reason: Option<ExclusionReason>,
}

impl ContextBlock {
    pub fn new(
        id: impl Into<String>,
        kind: BlockKind,
        required: bool,
        priority: u16,
        messages: Vec<Value>,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            required,
            priority,
            messages,
            source_refs: Vec::new(),
            dedupe_keys: Vec::new(),
            atomic_group: None,
            exclusion_reason: None,
        }
    }
    fn mandatory(&self) -> bool {
        self.required
            || matches!(
                self.kind,
                BlockKind::HostPolicy
                    | BlockKind::CurrentInput
                    | BlockKind::Correction
                    | BlockKind::ToolResult
            )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BlockDecision {
    pub id: String,
    pub kind: BlockKind,
    pub required: bool,
    pub selected: bool,
    pub reason: String,
    pub atomic_group: Option<String>,
    pub content_digest: String,
    pub serialized_bytes: u64,
    pub estimated_tokens: u64,
    pub source_refs: Vec<SourceRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BudgetSnapshot {
    pub schema_version: u32,
    pub run_id: String,
    pub model_id: String,
    pub policy: BudgetPolicy,
    pub policy_digest: String,
    pub estimator_version: String,
    pub token_counts_exact: bool,
    pub actual_response_tokens: u64,
    pub effective_input_limit: u64,
    pub candidate_digest: String,
    pub messages_digest: Option<String>,
    pub serialized_bytes: u64,
    pub estimated_input_tokens: u64,
    pub estimated_total_tokens: u64,
    pub accepted: bool,
    pub rejection_reason: Option<String>,
    pub blocks: Vec<BlockDecision>,
    pub created_ms: i64,
}

#[derive(Debug)]
pub struct CompiledContext {
    pub accepted: bool,
    pub messages: Vec<Value>,
    pub snapshot: BudgetSnapshot,
    pub selected_block_ids: Vec<String>,
    pub rejection_reason: Option<String>,
}

pub fn ensure_schema(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS context_budget_policy_versions(
          model_id TEXT NOT NULL,revision INTEGER NOT NULL,policy_json TEXT NOT NULL,
          created_ms INTEGER NOT NULL,PRIMARY KEY(model_id,revision));
         CREATE TABLE IF NOT EXISTS context_budget_policy_current(
          model_id TEXT PRIMARY KEY,revision INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS context_budget_snapshots(
          run_id TEXT PRIMARY KEY REFERENCES runs(id),snapshot_json TEXT NOT NULL,
          accepted INTEGER NOT NULL CHECK(accepted IN (0,1)),
          messages_digest TEXT,created_ms INTEGER NOT NULL);",
    )?;
    Ok(())
}

fn require_transaction(db: &Connection) -> anyhow::Result<()> {
    ensure!(
        !db.is_autocommit(),
        "context budgeting requires a transaction"
    );
    Ok(())
}

pub fn policy_in(db: &Connection, model_id: &str) -> anyhow::Result<BudgetPolicy> {
    require_transaction(db)?;
    ensure!(
        safe_identifier(model_id, 200),
        "invalid budget model identity"
    );
    let document: Option<String> = db.query_row(
        "SELECT v.policy_json FROM context_budget_policy_current c JOIN context_budget_policy_versions v ON v.model_id=c.model_id AND v.revision=c.revision WHERE c.model_id=?1",
        [model_id], |r| r.get(0),
    ).optional()?;
    let policy = document
        .map(|s| serde_json::from_str(&s))
        .transpose()?
        .unwrap_or_else(|| BudgetPolicy::defaults(model_id));
    validate_policy(&policy)?;
    ensure!(policy.model_id == model_id, "stored budget model mismatch");
    Ok(policy)
}

/// Authorization belongs to the management caller. This function provides
/// optimistic revision checks and immutable policy history, not a permission.
pub fn save_policy_in(
    db: &Connection,
    proposed: &BudgetPolicy,
    expected_revision: i64,
    at_ms: i64,
) -> anyhow::Result<BudgetPolicy> {
    require_transaction(db)?;
    validate_policy(proposed)?;
    let current = policy_in(db, &proposed.model_id)?;
    ensure!(
        current.revision == expected_revision && proposed.revision == expected_revision,
        "context_budget_policy_conflict"
    );
    let mut next = proposed.clone();
    next.revision = current
        .revision
        .checked_add(1)
        .context("policy revision overflow")?;
    db.execute("INSERT INTO context_budget_policy_versions(model_id,revision,policy_json,created_ms) VALUES(?1,?2,?3,?4)", params![next.model_id,next.revision,serde_json::to_string(&next)?,at_ms])?;
    db.execute("INSERT INTO context_budget_policy_current(model_id,revision) VALUES(?1,?2) ON CONFLICT(model_id) DO UPDATE SET revision=excluded.revision",params![next.model_id,next.revision])?;
    Ok(next)
}

fn validate_blocks(blocks: &[ContextBlock]) -> anyhow::Result<()> {
    ensure!(
        !blocks.is_empty() && blocks.len() <= MAX_BLOCKS,
        "invalid context block count"
    );
    ensure!(
        blocks
            .iter()
            .filter(|b| b.kind == BlockKind::CurrentInput)
            .count()
            == 1,
        "context requires exactly one complete current input"
    );
    ensure!(
        blocks
            .last()
            .is_some_and(|b| b.kind == BlockKind::CurrentInput),
        "the complete current input must be the last block"
    );
    ensure!(
        blocks.iter().any(|b| b.kind == BlockKind::HostPolicy),
        "context requires host policy"
    );
    ensure!(
        blocks.iter().filter(|b| b.kind == BlockKind::Focus).count() <= 1,
        "ambiguous focus blocks"
    );
    ensure!(
        blocks.iter().map(|b| b.source_refs.len()).sum::<usize>() <= 512,
        "too many context source references"
    );
    let mut ids = BTreeSet::new();
    let mut focus_refs = BTreeSet::new();
    let mut candidate_bytes = 0usize;
    for block in blocks {
        ensure!(
            safe_identifier(&block.id, 256) && ids.insert(&block.id),
            "duplicate or invalid context block identity"
        );
        ensure!(
            block
                .atomic_group
                .as_deref()
                .is_none_or(|s| safe_identifier(s, 128)),
            "invalid atomic group identity"
        );
        ensure!(
            !block.messages.is_empty() && block.messages.len() <= 32,
            "invalid atomic message count"
        );
        ensure!(
            block.dedupe_keys.len() <= 64
                && block.dedupe_keys.iter().all(|s| safe_identifier(s, 512)),
            "invalid context dedupe identity"
        );
        for source in &block.source_refs {
            ensure!(
                safe_identifier(&source.kind, 64)
                    && safe_identifier(&source.id, 512)
                    && source.revision >= 0
                    && source.epoch.is_none_or(|e| e >= 0)
                    && source.digest.as_deref().is_none_or(valid_digest),
                "invalid context source reference"
            );
            if source.kind == "conversation_focus" {
                focus_refs.insert((&source.id, source.revision));
            }
        }
        for message in &block.messages {
            let role = message["role"].as_str().unwrap_or("");
            ensure!(
                ["system", "user", "assistant", "tool"].contains(&role)
                    && message["content"].is_string(),
                "context message must contain complete text and a supported role"
            );
            if block.kind == BlockKind::HostPolicy {
                ensure!(role == "system", "host policy must use the system role");
            }
            if !matches!(block.kind, BlockKind::HostPolicy | BlockKind::Style) {
                ensure!(
                    role != "system",
                    "source data cannot become system instructions"
                );
            }
            if !matches!(block.kind, BlockKind::History | BlockKind::Style) {
                ensure!(
                    role != "assistant",
                    "source data cannot impersonate an assistant turn"
                );
            }
            if role == "tool" {
                ensure!(
                    safe_identifier(message["tool_call_id"].as_str().unwrap_or(""), 256),
                    "tool result requires its call identity"
                );
            }
        }
        if block.kind == BlockKind::CurrentInput {
            ensure!(
                block.messages.len() == 1 && block.messages[0]["role"] == "user",
                "current input must remain a single complete user message"
            );
        }
        candidate_bytes = candidate_bytes
            .checked_add(serde_json::to_vec(&block.messages)?.len())
            .context("context size overflow")?;
        ensure!(
            candidate_bytes <= MAX_CANDIDATE_BYTES,
            "context candidates exceed host bound"
        );
    }
    ensure!(focus_refs.len() <= 1, "multiple active focus references");
    ensure!(
        groups(blocks)
            .iter()
            .all(|group| !group.mandatory || group.exclusion.is_none()),
        "mandatory context groups cannot carry exclusion markers"
    );
    Ok(())
}

#[derive(Debug)]
struct SelectionGroup {
    indices: Vec<usize>,
    mandatory: bool,
    priority: u16,
    keys: BTreeSet<String>,
    exclusion: Option<ExclusionReason>,
}

fn groups(blocks: &[ContextBlock]) -> Vec<SelectionGroup> {
    let mut groups = Vec::<SelectionGroup>::new();
    let mut named = BTreeMap::<String, usize>::new();
    for (index, block) in blocks.iter().enumerate() {
        let existing = block
            .atomic_group
            .as_ref()
            .and_then(|id| named.get(id))
            .copied();
        if let Some(group) = existing {
            groups[group].indices.push(index);
            groups[group].mandatory |= block.mandatory();
            groups[group].priority = groups[group].priority.max(block.priority);
            groups[group].keys.extend(block.dedupe_keys.iter().cloned());
            groups[group].exclusion = groups[group].exclusion.or(block.exclusion_reason);
        } else {
            let group = groups.len();
            if let Some(id) = &block.atomic_group {
                named.insert(id.clone(), group);
            }
            groups.push(SelectionGroup {
                indices: vec![index],
                mandatory: block.mandatory(),
                priority: block.priority,
                keys: block.dedupe_keys.iter().cloned().collect(),
                exclusion: block.exclusion_reason,
            });
        }
    }
    groups
}

fn selected_messages(blocks: &[ContextBlock], indices: &BTreeSet<usize>) -> Vec<Value> {
    blocks
        .iter()
        .enumerate()
        .filter(|(i, _)| indices.contains(i))
        .flat_map(|(_, b)| b.messages.iter().cloned())
        .collect()
}

fn wire_bytes(model: &str, messages: &[Value], response_tokens: u64) -> anyhow::Result<Vec<u8>> {
    Ok(serde_json::to_vec(
        &json!({"model":model,"messages":messages,"stream":false,"max_tokens":response_tokens}),
    )?)
}

fn size(
    blocks: &[ContextBlock],
    selected: &BTreeSet<usize>,
    model: &str,
    response_tokens: u64,
) -> anyhow::Result<u64> {
    Ok(wire_bytes(model, &selected_messages(blocks, selected), response_tokens)?.len() as u64)
}

/// The estimator deliberately counts serialized UTF-8 bytes (including wire
/// framing) as estimated tokens. It is conservative planning, not a tokenizer,
/// a verified upper bound, a provider capability probe or a billing estimate.
fn estimated_tokens(serialized_bytes: u64) -> u64 {
    serialized_bytes
}

fn rejection(policy: &BudgetPolicy, bytes: u64) -> Option<&'static str> {
    if bytes > policy.max_input_bytes {
        Some("input_bytes_exceeded")
    } else if estimated_tokens(bytes) > policy.input_limit() {
        Some("estimated_input_budget_exceeded")
    } else {
        None
    }
}

fn table_exists(db: &Connection, name: &str) -> anyhow::Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [name],
        |r| r.get(0),
    )?)
}

pub fn snapshot_in(db: &Connection, run: &str) -> anyhow::Result<Option<BudgetSnapshot>> {
    require_transaction(db)?;
    let document: Option<String> = db
        .query_row(
            "SELECT snapshot_json FROM context_budget_snapshots WHERE run_id=?1",
            [run],
            |r| r.get(0),
        )
        .optional()?;
    document.map(|s| Ok(serde_json::from_str(&s)?)).transpose()
}

/// Commit accepted=false metadata with the Run's controlled rejection, then
/// return without constructing a provider request. An Err indicates malformed
/// host inputs or an attempted rewrite, not an ordinary budget rejection.
pub fn compile_in(
    db: &Connection,
    run: &str,
    policy: &BudgetPolicy,
    response_tokens: u64,
    blocks: &[ContextBlock],
    at_ms: i64,
) -> anyhow::Result<CompiledContext> {
    require_transaction(db)?;
    validate_policy(policy)?;
    validate_blocks(blocks)?;
    ensure!(
        response_tokens > 0 && response_tokens <= MAX_RESPONSE_TOKENS,
        "requested output exceeds host bound"
    );
    let config: String = db.query_row("SELECT config_json FROM runs WHERE id=?1", [run], |r| {
        r.get(0)
    })?;
    let config: Value = serde_json::from_str(&config)?;
    let model = config["model"]["model"]
        .as_str()
        .context("run lacks frozen model identity")?;
    let endpoint = config["model"]["base_url"]
        .as_str()
        .context("run lacks frozen provider identity")?;
    ensure!(
        model == policy.model_id,
        "budget policy does not match frozen model identity"
    );
    let candidate_digest = hash(&serde_json::to_vec(blocks)?);
    let policy_digest = hash(&serde_json::to_vec(policy)?);
    let existing = snapshot_in(db, run)?;
    if let Some(existing) = &existing {
        ensure!(
            existing.candidate_digest == candidate_digest
                && existing.policy_digest == policy_digest
                && existing.actual_response_tokens == response_tokens,
            "frozen context budget cannot be rewritten"
        );
    } else if table_exists(db, "run_prompt_snapshots")? {
        let frozen: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM run_prompt_snapshots WHERE run_id=?1)",
            [run],
            |r| r.get(0),
        )?;
        ensure!(
            !frozen,
            "cannot retrofit a different budget onto an already frozen prompt"
        );
    }
    let mut decisions = Vec::with_capacity(blocks.len());
    for block in blocks {
        let bytes = serde_json::to_vec(&block.messages)?;
        decisions.push(BlockDecision {
            id: block.id.clone(),
            kind: block.kind,
            required: block.mandatory(),
            selected: false,
            reason: "not_selected".into(),
            atomic_group: block.atomic_group.clone(),
            content_digest: hash(&bytes),
            serialized_bytes: bytes.len() as u64,
            estimated_tokens: estimated_tokens(bytes.len() as u64),
            source_refs: block.source_refs.clone(),
        });
    }
    let groups = groups(blocks);
    let mut selected = BTreeSet::new();
    let mut used_keys = BTreeSet::new();
    for group in groups.iter().filter(|g| g.mandatory) {
        selected.extend(group.indices.iter().copied());
        used_keys.extend(group.keys.iter().cloned());
        for &index in &group.indices {
            decisions[index].selected = true;
            decisions[index].reason = "required".into();
            decisions[index].required = true;
        }
    }
    let mandatory_bytes = size(blocks, &selected, model, response_tokens)?;
    let binding_matches = policy
        .provider_fingerprint
        .as_ref()
        .is_none_or(|fingerprint| *fingerprint == provider_fingerprint(endpoint));
    let mut rejected = if !binding_matches {
        Some("provider_binding_changed".to_owned())
    } else if response_tokens > policy.response_reserve_tokens {
        Some("response_reservation_exceeded".to_owned())
    } else {
        rejection(policy, mandatory_bytes).map(|s| format!("mandatory_{s}"))
    };
    if rejected.is_none() {
        let mut optional = groups.iter().filter(|g| !g.mandatory).collect::<Vec<_>>();
        optional.sort_by_key(|g| (std::cmp::Reverse(g.priority), g.indices[0]));
        for group in optional {
            let reason = if let Some(reason) = group.exclusion {
                Some(reason.label())
            } else if !group.keys.is_disjoint(&used_keys) {
                Some("duplicate_source")
            } else {
                let mut trial = selected.clone();
                trial.extend(group.indices.iter().copied());
                rejection(policy, size(blocks, &trial, model, response_tokens)?)
            };
            if let Some(reason) = reason {
                for &index in &group.indices {
                    decisions[index].reason = reason.into();
                }
            } else {
                selected.extend(group.indices.iter().copied());
                used_keys.extend(group.keys.iter().cloned());
                for &index in &group.indices {
                    decisions[index].selected = true;
                    decisions[index].reason = "selected_priority".into();
                }
            }
        }
    }
    let messages = if rejected.is_none() {
        selected_messages(blocks, &selected)
    } else {
        Vec::new()
    };
    let bytes = if rejected.is_none() {
        wire_bytes(model, &messages, response_tokens)?.len() as u64
    } else {
        mandatory_bytes
    };
    if rejected.is_some() {
        for decision in &mut decisions {
            decision.selected = false;
            decision.reason = if decision.required {
                "required_context_rejected"
            } else {
                "request_rejected"
            }
            .into();
        }
        selected.clear();
    }
    let snapshot = BudgetSnapshot {
        schema_version: 1,
        run_id: run.into(),
        model_id: model.into(),
        policy: policy.clone(),
        policy_digest,
        estimator_version: ESTIMATOR_VERSION.into(),
        token_counts_exact: false,
        actual_response_tokens: response_tokens,
        effective_input_limit: policy.input_limit(),
        candidate_digest,
        messages_digest: rejected
            .is_none()
            .then(|| hash(&serde_json::to_vec(&messages).expect("already serialized messages"))),
        serialized_bytes: bytes,
        estimated_input_tokens: estimated_tokens(bytes),
        estimated_total_tokens: estimated_tokens(bytes)
            + policy.response_reserve_tokens
            + policy.protocol_reserve_tokens
            + policy.safety_reserve_tokens,
        accepted: rejected.is_none(),
        rejection_reason: rejected.take(),
        blocks: decisions,
        created_ms: existing.as_ref().map(|s| s.created_ms).unwrap_or(at_ms),
    };
    if let Some(existing) = existing {
        ensure!(existing == snapshot, "frozen budget evidence differs");
    } else {
        db.execute("INSERT INTO context_budget_snapshots(run_id,snapshot_json,accepted,messages_digest,created_ms) VALUES(?1,?2,?3,?4,?5)",params![run,serde_json::to_string(&snapshot)?,snapshot.accepted,snapshot.messages_digest,at_ms])?;
    }
    Ok(CompiledContext {
        accepted: snapshot.accepted,
        messages,
        selected_block_ids: selected.into_iter().map(|i| blocks[i].id.clone()).collect(),
        rejection_reason: snapshot.rejection_reason.clone(),
        snapshot,
    })
}

/// Validate the frozen result without consulting the current policy revision;
/// hot updates affect new Runs and never silently rewrite this request.
pub fn validate_in(
    db: &Connection,
    run: &str,
    model_id: &str,
    messages: &[Value],
    response_tokens: u64,
) -> anyhow::Result<bool> {
    require_transaction(db)?;
    let Some(snapshot) = snapshot_in(db, run)? else {
        return Ok(false);
    };
    if !snapshot.accepted
        || snapshot.model_id != model_id
        || snapshot.actual_response_tokens != response_tokens
    {
        return Ok(false);
    }
    validate_policy(&snapshot.policy)?;
    if snapshot.schema_version != 1
        || snapshot.estimator_version != ESTIMATOR_VERSION
        || snapshot.token_counts_exact
        || snapshot.policy_digest != hash(&serde_json::to_vec(&snapshot.policy)?)
        || snapshot.actual_response_tokens > snapshot.policy.response_reserve_tokens
        || snapshot.effective_input_limit != snapshot.policy.input_limit()
    {
        return Ok(false);
    }
    let messages_bytes = serde_json::to_vec(messages)?;
    let bytes = wire_bytes(model_id, messages, response_tokens)?;
    Ok(
        snapshot.messages_digest.as_deref() == Some(hash(&messages_bytes).as_str())
            && snapshot.serialized_bytes == bytes.len() as u64
            && snapshot.estimated_input_tokens == estimated_tokens(bytes.len() as u64)
            && rejection(&snapshot.policy, bytes.len() as u64).is_none(),
    )
}

#[cfg(test)]
#[path = "context_budget_tests.rs"]
mod tests;
