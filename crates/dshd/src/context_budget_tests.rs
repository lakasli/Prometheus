use super::*;

fn database() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE runs(id TEXT PRIMARY KEY,config_json TEXT NOT NULL);
      CREATE TABLE run_prompt_snapshots(run_id TEXT PRIMARY KEY,messages_json TEXT NOT NULL);",
    )
    .unwrap();
    ensure_schema(&db).unwrap();
    insert_run(&db, "r", "synthetic-model", "https://provider.invalid/v1");
    db
}

fn insert_run(db: &Connection, id: &str, model: &str, endpoint: &str) {
    db.execute(
        "INSERT INTO runs VALUES(?1,?2)",
        params![
            id,
            json!({"model":{"model":model,"base_url":endpoint}}).to_string()
        ],
    )
    .unwrap();
}

fn block(
    id: &str,
    kind: BlockKind,
    required: bool,
    priority: u16,
    role: &str,
    text: impl Into<String>,
) -> ContextBlock {
    ContextBlock::new(
        id,
        kind,
        required,
        priority,
        vec![json!({"role":role,"content":text.into()})],
    )
}

fn required() -> Vec<ContextBlock> {
    vec![
        block(
            "host",
            BlockKind::HostPolicy,
            false,
            0,
            "system",
            "Mandatory host policy.",
        ),
        block(
            "current",
            BlockKind::CurrentInput,
            false,
            0,
            "user",
            "Complete current question.",
        ),
    ]
}

fn policy() -> BudgetPolicy {
    BudgetPolicy::defaults("synthetic-model")
}

#[test]
fn unknown_window_is_honest_and_preserves_the_96kb_request_capacity() {
    let mut db = database();
    let mut blocks = required();
    blocks[1].messages[0]["content"] = json!("x".repeat(95_500));
    let tx = db.transaction().unwrap();
    let result = compile_in(&tx, "r", &policy(), 4_096, &blocks, 1).unwrap();
    assert!(result.accepted);
    assert_eq!(result.snapshot.policy.provider_context_window, None);
    assert_eq!(result.snapshot.policy.window_basis, "unknown");
    assert!(!result.snapshot.token_counts_exact);
    assert_eq!(result.snapshot.estimator_version, ESTIMATOR_VERSION);
    assert!(result.snapshot.serialized_bytes <= 96_000);
    assert!(result.snapshot.estimated_total_tokens > result.snapshot.estimated_input_tokens);
    assert_eq!(result.snapshot.actual_response_tokens, 4_096);
    assert!(validate_in(&tx, "r", "synthetic-model", &result.messages, 4_096).unwrap());
}

#[test]
fn mandatory_policy_question_correction_delivery_and_unique_focus_survive_trimming() {
    let mut db = database();
    let mut p = policy();
    p.max_input_bytes = 1_000;
    let mut blocks = required();
    blocks.insert(
        1,
        block(
            "correction",
            BlockKind::Correction,
            false,
            0,
            "user",
            "The corrected date is tomorrow.",
        ),
    );
    blocks.insert(
        2,
        block(
            "delivery",
            BlockKind::Delivery,
            true,
            0,
            "user",
            "Previous result is unknown.",
        ),
    );
    let mut focus = block(
        "focus",
        BlockKind::Focus,
        true,
        0,
        "user",
        "One verified topic reference.",
    );
    focus.source_refs.push(SourceRef {
        kind: "conversation_focus".into(),
        id: "one-focus".into(),
        revision: 2,
        epoch: Some(3),
        digest: Some("a".repeat(64)),
    });
    blocks.insert(3, focus);
    blocks.insert(
        4,
        block(
            "optional-diary",
            BlockKind::Diary,
            false,
            999,
            "user",
            "汉字".repeat(1_000),
        ),
    );
    let tx = db.transaction().unwrap();
    let result = compile_in(&tx, "r", &p, 1_024, &blocks, 1).unwrap();
    assert!(result.accepted);
    assert_eq!(
        result.selected_block_ids,
        vec!["host", "correction", "delivery", "focus", "current"]
    );
    assert_eq!(
        result.messages.last().unwrap(),
        &blocks.last().unwrap().messages[0]
    );
    assert_eq!(result.snapshot.blocks[4].reason, "input_bytes_exceeded");
    assert_eq!(result.snapshot.actual_response_tokens, 1_024);
    assert_eq!(result.snapshot.policy.response_reserve_tokens, 4_096);
}

#[test]
fn mandatory_overflow_persists_rejection_without_any_partial_request() {
    let mut db = database();
    let mut p = policy();
    p.max_input_bytes = 128;
    let mut blocks = required();
    blocks[1].messages[0]["content"] =
        json!("A complete user question must never be silently cut.".repeat(30));
    let tx = db.transaction().unwrap();
    let result = compile_in(&tx, "r", &p, 1_024, &blocks, 1).unwrap();
    assert!(!result.accepted);
    assert!(result.messages.is_empty() && result.selected_block_ids.is_empty());
    assert_eq!(
        result.rejection_reason.as_deref(),
        Some("mandatory_input_bytes_exceeded")
    );
    assert!(result.snapshot.blocks.iter().all(|block| !block.selected));
    assert!(!validate_in(&tx, "r", "synthetic-model", &[], 1_024).unwrap());
    tx.commit().unwrap();
    assert_eq!(
        db.query_row("SELECT accepted FROM context_budget_snapshots", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM run_prompt_snapshots", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn declared_model_window_reserves_response_protocol_and_safety_before_optional_data() {
    let mut db = database();
    let mut p = policy();
    p.response_reserve_tokens = 1_024;
    p.protocol_reserve_tokens = 256;
    p.safety_reserve_tokens = 512;
    p.provider_context_window = Some(4_096);
    p.window_basis = "administrator_declared".into();
    p.provider_fingerprint = Some(provider_fingerprint("https://provider.invalid/v1"));
    let mut blocks = required();
    blocks.insert(
        1,
        block(
            "history",
            BlockKind::History,
            false,
            1,
            "user",
            "x".repeat(3_000),
        ),
    );
    let tx = db.transaction().unwrap();
    let result = compile_in(&tx, "r", &p, 1_024, &blocks, 1).unwrap();
    assert!(result.accepted);
    assert_eq!(result.snapshot.effective_input_limit, 2_304);
    assert_eq!(
        result.snapshot.blocks[1].reason,
        "estimated_input_budget_exceeded"
    );
    assert!(result.snapshot.estimated_total_tokens <= 4_096);
}

#[test]
fn a_same_name_model_at_another_provider_cannot_inherit_a_declared_window() {
    let mut db = database();
    let mut p = policy();
    p.provider_context_window = Some(32_000);
    p.window_basis = "administrator_declared".into();
    p.provider_fingerprint = Some(provider_fingerprint("https://another.invalid/v1"));
    let tx = db.transaction().unwrap();
    let result = compile_in(&tx, "r", &p, 1_024, &required(), 1).unwrap();
    assert!(!result.accepted && result.messages.is_empty());
    assert_eq!(
        result.rejection_reason.as_deref(),
        Some("provider_binding_changed")
    );
}

#[test]
fn actual_response_tokens_must_fit_the_independent_reserved_amount() {
    let mut db = database();
    let mut p = policy();
    p.response_reserve_tokens = 1_024;
    let tx = db.transaction().unwrap();
    let result = compile_in(&tx, "r", &p, 4_096, &required(), 1).unwrap();
    assert_eq!(
        result.rejection_reason.as_deref(),
        Some("response_reservation_exceeded")
    );
    assert!(!result.accepted);
    assert!(compile_in(&tx, "r", &p, 4_097, &required(), 1).is_err());
}

#[test]
fn example_pairs_and_json_are_selected_or_dropped_as_complete_atomic_groups() {
    let mut db = database();
    let mut p = policy();
    p.max_input_bytes = 700;
    let mut blocks = required();
    let mut user = block(
        "example-user",
        BlockKind::Style,
        false,
        20,
        "user",
        "示例问题".repeat(40),
    );
    user.atomic_group = Some("example-one".into());
    let mut answer = block(
        "example-answer",
        BlockKind::Style,
        false,
        20,
        "assistant",
        "完整示例回答".repeat(40),
    );
    answer.atomic_group = Some("example-one".into());
    let document = json!({"中文":"完整UTF-8正文","nested":{"items":[1,2,3]}}).to_string();
    blocks.insert(1, user);
    blocks.insert(2, answer);
    blocks.insert(
        3,
        block(
            "json-source",
            BlockKind::Memory,
            false,
            10,
            "user",
            document.clone(),
        ),
    );
    let tx = db.transaction().unwrap();
    let result = compile_in(&tx, "r", &p, 1_024, &blocks, 1).unwrap();
    assert!(result.accepted);
    assert!(!result.selected_block_ids.contains(&"example-user".into()));
    assert!(!result.selected_block_ids.contains(&"example-answer".into()));
    let preserved = result
        .messages
        .iter()
        .find(|m| m["content"] == document)
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(preserved["content"].as_str().unwrap()).unwrap()["中文"],
        "完整UTF-8正文"
    );
}

#[test]
fn one_required_member_protects_its_entire_group_from_partial_trimming() {
    let mut db = database();
    let mut p = policy();
    p.max_input_bytes = 500;
    let mut blocks = required();
    let mut evidence = block(
        "necessary-delivery",
        BlockKind::Delivery,
        true,
        1,
        "user",
        "Required evidence.",
    );
    evidence.atomic_group = Some("turn".into());
    let mut history = block(
        "history",
        BlockKind::History,
        false,
        1,
        "assistant",
        "x".repeat(1_000),
    );
    history.atomic_group = Some("turn".into());
    blocks.insert(1, evidence);
    blocks.insert(2, history);
    let tx = db.transaction().unwrap();
    let result = compile_in(&tx, "r", &p, 1_024, &blocks, 1).unwrap();
    assert!(!result.accepted);
    assert!(result.snapshot.blocks[2].required);
}

#[test]
fn priority_controls_selection_without_reordering_the_supplied_chronology() {
    let mut db = database();
    let mut p = policy();
    p.max_input_bytes = 650;
    let mut blocks = required();
    blocks.insert(
        1,
        block("low", BlockKind::Diary, false, 1, "user", "l".repeat(300)),
    );
    blocks.insert(
        2,
        block(
            "high",
            BlockKind::History,
            false,
            500,
            "user",
            "h".repeat(300),
        ),
    );
    let tx = db.transaction().unwrap();
    let result = compile_in(&tx, "r", &p, 1_024, &blocks, 1).unwrap();
    assert_eq!(result.selected_block_ids, vec!["host", "high", "current"]);
    assert_eq!(result.messages[1], blocks[2].messages[0]);
    assert_eq!(result.messages[2], blocks[3].messages[0]);
}

#[test]
fn duplicate_underlying_day_week_and_month_evidence_is_not_injected_repeatedly() {
    let mut db = database();
    let mut blocks = required();
    let mut day = block(
        "day",
        BlockKind::Diary,
        false,
        100,
        "user",
        "One source recollection.",
    );
    day.dedupe_keys = vec!["raw-event:e1:rev1".into()];
    let mut week = block(
        "week",
        BlockKind::Diary,
        false,
        10,
        "user",
        "The same evidence in a week.",
    );
    week.dedupe_keys = day.dedupe_keys.clone();
    blocks.insert(1, week);
    blocks.insert(2, day);
    let tx = db.transaction().unwrap();
    let result = compile_in(&tx, "r", &policy(), 1_024, &blocks, 1).unwrap();
    assert_eq!(result.selected_block_ids, vec!["host", "day", "current"]);
    assert_eq!(result.snapshot.blocks[1].reason, "duplicate_source");
}

#[test]
fn budget_snapshot_contains_only_hashes_versions_and_references_not_prompt_text() {
    let mut db = database();
    let mut blocks = required();
    blocks[1].messages[0]["content"] = json!("PRIVATE_CURRENT_INPUT_SENTINEL");
    let mut memory = block(
        "memory",
        BlockKind::Memory,
        false,
        10,
        "user",
        "PRIVATE_RECALL_SENTINEL",
    );
    memory.source_refs = vec![SourceRef {
        kind: "diary_entry".into(),
        id: "opaque-entry".into(),
        revision: 3,
        epoch: Some(4),
        digest: Some("b".repeat(64)),
    }];
    blocks.insert(1, memory);
    let tx = db.transaction().unwrap();
    let result = compile_in(&tx, "r", &policy(), 1_024, &blocks, 1).unwrap();
    let metadata: String = tx
        .query_row(
            "SELECT snapshot_json FROM context_budget_snapshots",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!metadata.contains("PRIVATE_CURRENT_INPUT_SENTINEL"));
    assert!(!metadata.contains("PRIVATE_RECALL_SENTINEL"));
    assert!(!metadata.contains("Mandatory host policy."));
    assert_eq!(result.snapshot.blocks[1].source_refs[0].revision, 3);
    assert!(result.selected_block_ids.contains(&"memory".into()));
}

#[test]
fn policy_updates_are_per_model_revisioned_bounded_and_do_not_rewrite_frozen_requests() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    let initial = policy_in(&tx, "synthetic-model").unwrap();
    let compiled = compile_in(&tx, "r", &initial, 1_024, &required(), 10).unwrap();
    tx.execute(
        "INSERT INTO run_prompt_snapshots VALUES('r',?1)",
        [serde_json::to_string(&compiled.messages).unwrap()],
    )
    .unwrap();
    let mut proposed = initial.clone();
    proposed.max_input_bytes = 1_000;
    let updated = save_policy_in(&tx, &proposed, 0, 11).unwrap();
    assert_eq!(updated.revision, 1);
    assert_eq!(policy_in(&tx, "other-model").unwrap().revision, 0);
    assert!(save_policy_in(&tx, &proposed, 0, 12).is_err());
    assert!(validate_in(&tx, "r", "synthetic-model", &compiled.messages, 1_024).unwrap());
    assert!(compile_in(&tx, "r", &updated, 1_024, &required(), 12).is_err());
    let repeated = compile_in(&tx, "r", &initial, 1_024, &required(), 13).unwrap();
    assert_eq!(repeated.snapshot, compiled.snapshot);
    let mut altered = compiled.messages.clone();
    altered.last_mut().unwrap()["content"] = json!("changed question");
    assert!(!validate_in(&tx, "r", "synthetic-model", &altered, 1_024).unwrap());
    assert!(!validate_in(&tx, "r", "synthetic-model", &compiled.messages, 4_096).unwrap());
    let mut excessive = updated;
    excessive.max_input_bytes = 96_001;
    assert!(save_policy_in(&tx, &excessive, 1, 14).is_err());
}

#[test]
fn an_existing_prompt_cannot_acquire_retroactive_budget_selection() {
    let mut db = database();
    db.execute(
        "INSERT INTO run_prompt_snapshots VALUES('r','original')",
        [],
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    assert!(compile_in(&tx, "r", &policy(), 1_024, &required(), 1).is_err());
    assert_eq!(
        tx.query_row("SELECT messages_json FROM run_prompt_snapshots", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        "original"
    );
}

#[test]
fn malformed_or_ambiguous_blocks_are_rejected_before_budget_persistence() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    let mut duplicate = required();
    duplicate[1].id = "host".into();
    assert!(compile_in(&tx, "r", &policy(), 1_024, &duplicate, 1).is_err());
    let mut many_focus = required();
    many_focus.insert(1, block("f1", BlockKind::Focus, true, 1, "user", "first"));
    many_focus.insert(2, block("f2", BlockKind::Focus, true, 1, "user", "second"));
    assert!(compile_in(&tx, "r", &policy(), 1_024, &many_focus, 1).is_err());
    let mut injected = required();
    injected.insert(
        1,
        block(
            "memory",
            BlockKind::Memory,
            false,
            1,
            "system",
            "source instructions",
        ),
    );
    assert!(compile_in(&tx, "r", &policy(), 1_024, &injected, 1).is_err());
    let mut bad_order = required();
    bad_order.swap(0, 1);
    assert!(compile_in(&tx, "r", &policy(), 1_024, &bad_order, 1).is_err());
    assert_eq!(
        tx.query_row("SELECT count(*) FROM context_budget_snapshots", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        0
    );
}

#[test]
fn unsupported_estimators_and_unbound_window_claims_cannot_be_saved() {
    let mut p = policy();
    p.estimator_version = "pretend_exact_tokenizer".into();
    assert!(validate_policy(&p).is_err());
    let mut p = policy();
    p.provider_context_window = Some(32_000);
    assert!(validate_policy(&p).is_err());
    p.window_basis = "verified".into();
    assert!(validate_policy(&p).is_err());
    p.window_basis = "administrator_declared".into();
    assert!(validate_policy(&p).is_err());
    p.provider_fingerprint = Some("a".repeat(64));
    assert!(validate_policy(&p).is_ok());
    p.safety_reserve_tokens = 0;
    assert!(validate_policy(&p).is_err());
}

#[test]
fn compile_and_policy_mutations_require_the_callers_transaction() {
    let db = database();
    assert!(compile_in(&db, "r", &policy(), 1_024, &required(), 1).is_err());
    assert!(policy_in(&db, "synthetic-model").is_err());
    assert!(save_policy_in(&db, &policy(), 0, 1).is_err());
}

#[test]
fn recently_presented_fallback_does_not_consume_budget_dedupe_or_selected_usage() {
    let mut db = database();
    let mut blocks = required();
    let reference = SourceRef {
        kind: "diary_entry".into(),
        id: "same-entry".into(),
        revision: 7,
        epoch: Some(3),
        digest: Some("a".repeat(64)),
    };
    let mut fallback = block(
        "automatic-fallback",
        BlockKind::Diary,
        false,
        900,
        "user",
        "RECENT_AUTOMATIC_FALLBACK_BODY",
    );
    fallback.exclusion_reason = Some(ExclusionReason::RecentlyPresented);
    fallback.dedupe_keys.push("event:same-source:7".into());
    fallback.source_refs.push(reference.clone());
    let mut explicit = block(
        "explicit-current-recall",
        BlockKind::Memory,
        false,
        100,
        "user",
        "EXPLICIT_CURRENT_RECALL_BODY",
    );
    explicit.dedupe_keys = fallback.dedupe_keys.clone();
    explicit.source_refs.push(reference);
    blocks.insert(1, fallback);
    blocks.insert(2, explicit);
    let tx = db.transaction().unwrap();
    let result = compile_in(&tx, "r", &policy(), 1024, &blocks, 1).unwrap();
    assert!(result.accepted);
    assert_eq!(
        result.selected_block_ids,
        vec!["host", "explicit-current-recall", "current"]
    );
    assert_eq!(result.snapshot.blocks[1].reason, "recently_presented");
    assert!(!result.snapshot.blocks[1].selected);
    assert_eq!(result.snapshot.blocks[2].reason, "selected_priority");
    let request = serde_json::to_string(&result.messages).unwrap();
    assert!(!request.contains("RECENT_AUTOMATIC_FALLBACK_BODY"));
    assert!(request.contains("EXPLICIT_CURRENT_RECALL_BODY"));
    let selected_refs: Vec<_> = result
        .snapshot
        .blocks
        .iter()
        .filter(|b| b.selected)
        .flat_map(|b| &b.source_refs)
        .collect();
    assert_eq!(selected_refs.len(), 1);
    assert_eq!(selected_refs[0].revision, 7);
}

#[test]
fn one_excluded_member_skips_the_whole_optional_atomic_group() {
    let mut db = database();
    let mut blocks = required();
    let mut first = block(
        "day-fallback",
        BlockKind::Diary,
        false,
        500,
        "user",
        "DAY_FALLBACK_MUST_NOT_ENTER",
    );
    first.atomic_group = Some("one-evidence-bundle".into());
    first.exclusion_reason = Some(ExclusionReason::RecentlyPresented);
    let mut second = block(
        "related-evidence",
        BlockKind::Delivery,
        false,
        600,
        "user",
        "BUNDLE_EVIDENCE_MUST_NOT_ENTER",
    );
    second.atomic_group = first.atomic_group.clone();
    blocks.insert(1, first);
    blocks.insert(2, second);
    let tx = db.transaction().unwrap();
    let result = compile_in(&tx, "r", &policy(), 1024, &blocks, 1).unwrap();
    assert_eq!(result.selected_block_ids, vec!["host", "current"]);
    assert!(result.snapshot.blocks[1..3]
        .iter()
        .all(|b| !b.selected && b.reason == "recently_presented"));
    assert_eq!(result.messages.len(), 2);
}

#[test]
fn mandatory_and_required_atomic_groups_cannot_carry_any_host_exclusion() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    for index in [0, 1] {
        let mut blocks = required();
        blocks[index].exclusion_reason = Some(ExclusionReason::RecentlyPresented);
        let error = compile_in(&tx, "r", &policy(), 1024, &blocks, 1).unwrap_err();
        assert!(error
            .to_string()
            .contains("mandatory context groups cannot carry exclusion markers"));
    }
    let mut blocks = required();
    let mut necessary = block(
        "necessary-delivery",
        BlockKind::Delivery,
        true,
        1,
        "user",
        "Necessary state.",
    );
    necessary.atomic_group = Some("required-bundle".into());
    let mut optional = block(
        "optional-related-data",
        BlockKind::Diary,
        false,
        1,
        "user",
        "Optional body.",
    );
    optional.atomic_group = necessary.atomic_group.clone();
    optional.exclusion_reason = Some(ExclusionReason::RecentlyPresented);
    blocks.insert(1, necessary);
    blocks.insert(2, optional);
    assert!(compile_in(&tx, "r", &policy(), 1024, &blocks, 1)
        .unwrap_err()
        .to_string()
        .contains("mandatory context groups cannot carry exclusion markers"));
    assert_eq!(
        tx.query_row("SELECT count(*) FROM context_budget_snapshots", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        0
    );
}

#[test]
fn old_blocks_default_to_no_exclusion_and_json_cannot_assign_the_host_marker() {
    let original = block(
        "fallback",
        BlockKind::Diary,
        false,
        1,
        "user",
        "Original complete text.",
    );
    let mut document = serde_json::to_value(&original).unwrap();
    assert!(document.get("exclusion_reason").is_none());
    let legacy: ContextBlock = serde_json::from_value(document.clone()).unwrap();
    assert!(legacy.exclusion_reason.is_none());
    document["exclusion_reason"] = json!("recently_presented");
    // deny_unknown_fields plus skip_deserializing deliberately rejects this
    // host-only field, including a spelling that happens to name a valid enum.
    assert!(serde_json::from_value::<ContextBlock>(document.clone()).is_err());
    document["exclusion_reason"] = json!("grant_arbitrary_model_permission");
    assert!(serde_json::from_value::<ContextBlock>(document).is_err());
    assert!(
        serde_json::from_value::<ExclusionReason>(json!("grant_arbitrary_model_permission"))
            .is_err()
    );
    let mut host = original;
    host.exclusion_reason = Some(ExclusionReason::RecentlyPresented);
    assert_eq!(
        serde_json::to_value(host).unwrap()["exclusion_reason"],
        "recently_presented"
    );
}

#[test]
fn a_later_recently_presented_observation_cannot_rewrite_an_already_frozen_budget() {
    let mut db = database();
    let mut blocks = required();
    blocks.insert(
        1,
        block(
            "fallback",
            BlockKind::Diary,
            false,
            1,
            "user",
            "Already frozen source text.",
        ),
    );
    let tx = db.transaction().unwrap();
    let original = compile_in(&tx, "r", &policy(), 1024, &blocks, 1).unwrap();
    assert!(original.selected_block_ids.contains(&"fallback".into()));
    blocks[1].exclusion_reason = Some(ExclusionReason::RecentlyPresented);
    assert!(compile_in(&tx, "r", &policy(), 1024, &blocks, 2)
        .unwrap_err()
        .to_string()
        .contains("frozen context budget cannot be rewritten"));
    assert!(validate_in(&tx, "r", "synthetic-model", &original.messages, 1024).unwrap());
    assert_eq!(snapshot_in(&tx, "r").unwrap().unwrap(), original.snapshot);
}
