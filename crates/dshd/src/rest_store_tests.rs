use super::*;
use serde_json::json;
const AT: i64 = 1_793_030_000;
fn clock(seconds: i64) -> ClockSample {
    ClockSample::new(AT + seconds, seconds * 1000, "synthetic_boot", true).unwrap()
}
fn schema(db: &mut Connection) {
    db.execute_batch(include_str!("../schema.sql")).unwrap();
    db.execute_batch("ALTER TABLE runs ADD COLUMN run_kind TEXT NOT NULL DEFAULT 'model';ALTER TABLE runs ADD COLUMN character_snapshot_json TEXT;CREATE TABLE conversation_character_bindings(conversation_id TEXT PRIMARY KEY,draft_card_json TEXT);
    INSERT INTO admins VALUES('admin','synthetic_admin','unused',0);
    INSERT INTO conversations VALUES('a','owner_a','Synthetic A',NULL,0),('b','owner_b','Synthetic B',NULL,0),('group','qq:synthetic:group:g','Synthetic group','qq:synthetic:group:g',0);").unwrap();
    crate::memory::initialize(db).unwrap();
    crate::conversation_identity::initialize(db).unwrap();
    crate::relationships::initialize(db).unwrap();
    crate::diary::initialize(db).unwrap();
    crate::worker::initialize(db).unwrap();
    initialize(db).unwrap();
}
fn database() -> Connection {
    let mut db = Connection::open_in_memory().unwrap();
    schema(&mut db);
    db
}
fn admin(db: &Connection) -> VerifiedRequester {
    requester_for_admin_in(
        db,
        &Admin {
            id: "admin".into(),
            username: "synthetic_admin".into(),
            role: "admin".into(),
            csrf_token: String::new(),
        },
    )
    .unwrap()
}
fn configure(db: &Connection, natural: bool) {
    let a = admin(db);
    let p = RestPolicy {
        natural_enabled: natural,
        default_sleep_seconds: Some(100),
        drain_seconds: 10,
        consolidation_seconds: 30,
        ..Default::default()
    };
    save_policy_in(db, &a, 1, &p, AT).unwrap();
}
fn source(
    db: &Connection,
    id: &str,
    conversation: &str,
    input: &str,
    speaker: Option<&str>,
) -> anyhow::Result<VerifiedRequester> {
    let owner: String = db.query_row(
        "SELECT owner_id FROM conversations WHERE id=?1",
        [conversation],
        |r| r.get(0),
    )?;
    db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,created_at,updated_at) VALUES(?1,?2,?3,?1,?4,'{}','queued',?5,?5)",params![id,conversation,owner,input,AT])?;
    db.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES(?1,?2,?1,'user',?3,?4)",params![id,conversation,input,AT])?;
    crate::conversation_identity::freeze_run_in(db, id)?;
    if conversation == "group" {
        crate::conversation_identity::record_qq_message_in(
            db,
            id,
            "GROUP_AT_MESSAGE_CREATE",
            &json!({"id":id,"group_openid":"g","author":speaker.map(|s|json!({"member_openid":s})).unwrap_or(json!({}))}),
        )?;
    }
    requester_for_run_in(db, id)
}
fn grant(db: &Connection, source: &VerifiedRequester) {
    set_source_grant_in(
        db,
        &admin(db),
        source,
        0,
        &GrantPermissions {
            can_sleep: true,
            can_wake: true,
            valid_until: AT + 1000,
        },
        AT,
    )
    .unwrap();
}
fn begin(db: &Connection) -> RestOperation {
    sleep_in(
        db,
        &admin(db),
        0,
        "sleep_once",
        &Default::default(),
        &clock(0),
    )
    .unwrap()
}
fn event(db: &Connection, id: &str, owner: &str, content: &str) {
    db.execute(
        "INSERT OR IGNORE INTO diary_source_policies VALUES(?1,1,1,1,'Asia/Shanghai',?2,?2)",
        params![owner, AT - 100],
    )
    .unwrap();
    db.execute("INSERT INTO diary_events(id,assistant_id,revision,visibility,source_owner_id,origin_type,origin_id,happened_at,observed_at,content,assertion_kind) VALUES(?1,?2,1,'private',?3,'user_explicit',?1,?4,?4,?5,'user_claim')",params![id,assistant(db).unwrap(),owner,AT,content]).unwrap();
}
fn job(db: &Connection, cycle: &str, event_id: &str, revision: i64) -> RestJob {
    let reference = frozen_source_in(db, cycle, SourceKind::DiaryEvent, event_id).unwrap();
    queue_job_in(
        db,
        cycle,
        &JobSpec {
            owner_id: reference.owner_id.clone(),
            kind: "diary_day".into(),
            period_start: "2026-10-26".into(),
            period_end: "2026-10-26".into(),
            target_revision: revision,
            uses_model: true,
            sources: vec![reference],
        },
    )
    .unwrap()
}
#[test]
fn defaults_are_off_missing_time_is_denied_and_only_real_admin_can_configure() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    let (_, p) = policy_in(&tx).unwrap();
    assert!(!p.natural_enabled && !p.schedule_enabled);
    assert!(RestPolicy {
        natural_enabled: true,
        drain_seconds: 0,
        ..Default::default()
    }
    .validate()
    .is_err());
    assert!(sleep_in(
        &tx,
        &admin(&tx),
        0,
        "missing_time",
        &Default::default(),
        &clock(0)
    )
    .is_err());
    let user = source(&tx, "source", "a", "快去睡觉啦！", None).unwrap();
    assert!(save_policy_in(&tx, &user, 1, &p, AT).is_err());
    assert!(requester_for_admin_in(
        &tx,
        &Admin {
            id: "missing".into(),
            username: "synthetic_admin".into(),
            role: "admin".into(),
            csrf_token: String::new()
        }
    )
    .is_err());
    assert_eq!(state_in(&tx).unwrap().phase, RestPhase::Awake);
}
#[test]
fn source_controls_need_exact_versioned_grant_and_closed_direct_input() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    configure(&tx, true);
    let a = source(&tx, "a_sleep", "a", "快去睡觉啦！", None).unwrap();
    assert!(sleep_in(&tx, &a, 0, "ungranted", &Default::default(), &clock(0)).is_err());
    grant(&tx, &a);
    let b = source(&tx, "b_sleep", "b", "快去睡觉啦！", None).unwrap();
    assert!(sleep_in(&tx, &b, 0, "wrong_scope", &Default::default(), &clock(0)).is_err());
    let quoted = source(&tx, "quote", "a", "如果我说“快去睡觉啦”，你会怎样？", None).unwrap();
    assert!(sleep_in(&tx, &quoted, 0, "quoted", &Default::default(), &clock(0)).is_err());
    let goodnight = source(&tx, "goodnight", "a", "晚安，先不聊了。", None).unwrap();
    assert!(sleep_in(
        &tx,
        &goodnight,
        0,
        "goodnight",
        &Default::default(),
        &clock(0)
    )
    .is_err());
    let op = sleep_in(&tx, &a, 0, "accepted", &Default::default(), &clock(0)).unwrap();
    assert_eq!(op.wake_at, Some(AT + 100));
    let wake = source(&tx, "a_wake", "a", "醒醒！", None).unwrap();
    set_source_grant_in(
        &tx,
        &admin(&tx),
        &wake,
        1,
        &GrantPermissions {
            can_sleep: false,
            can_wake: false,
            valid_until: AT + 1000,
        },
        AT,
    )
    .unwrap();
    assert!(wake_in(&tx, &wake, op.generation, "revoked", &clock(1)).is_err());
    assert!(advance_in(&tx, &clock(100)).unwrap().phase == RestPhase::Waking);
}
#[test]
fn group_owner_is_not_a_speaker_and_other_members_cannot_borrow_control() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    configure(&tx, true);
    assert!(source(&tx, "unknown", "group", "快去睡觉啦", None).is_err());
    let a = source(&tx, "member_a", "group", "快去睡觉啦", Some("member_a")).unwrap();
    grant(&tx, &a);
    let b = source(&tx, "member_b", "group", "快去睡觉啦", Some("member_b")).unwrap();
    assert_ne!(a.speaker_ref(), b.speaker_ref());
    assert_ne!(a.speaker_ref(), a.owner_id());
    assert!(sleep_in(&tx, &b, 0, "other_member", &Default::default(), &clock(0)).is_err());
    assert!(sleep_in(
        &tx,
        &a,
        0,
        "member_accepted",
        &Default::default(),
        &clock(0)
    )
    .is_ok());
}
#[test]
fn restart_and_idempotency_preserve_cycle_generation_and_original_deadlines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("synthetic.db");
    let original = {
        let mut db = Connection::open(&path).unwrap();
        schema(&mut db);
        let tx = db.transaction().unwrap();
        configure(&tx, false);
        let op = begin(&tx);
        tx.commit().unwrap();
        op
    };
    let mut db = Connection::open(&path).unwrap();
    initialize(&db).unwrap();
    let tx = db.transaction().unwrap();
    let repeated = sleep_in(
        &tx,
        &admin(&tx),
        0,
        "sleep_once",
        &Default::default(),
        &clock(20),
    )
    .unwrap();
    assert_eq!(repeated.id, original.id);
    assert_eq!(repeated.cycle_id, original.cycle_id);
    assert_eq!(repeated.wake_at, Some(AT + 100));
    assert!(sleep_in(
        &tx,
        &admin(&tx),
        0,
        "different",
        &Default::default(),
        &clock(20)
    )
    .is_err());
    assert_eq!(
        advance_in(&tx, &clock(100)).unwrap().phase,
        RestPhase::Waking
    );
    assert_eq!(
        advance_in(&tx, &clock(100)).unwrap().phase,
        RestPhase::Awake
    );
    assert_eq!(state_in(&tx).unwrap().generation, 1);
}
#[test]
fn rollback_fractional_ticks_forward_jump_and_clock_loss_never_extend_sleep() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    configure(&tx, false);
    let op = begin(&tx);
    for ms in (200..=1000).step_by(200) {
        let c = ClockSample::new(AT - 20, ms, "synthetic_boot", false).unwrap();
        advance_in(&tx, &c).unwrap();
    }
    assert_eq!(state_in(&tx).unwrap().logical_at, AT + 1);
    let backwards = ClockSample::new(AT - 100, 100000, "synthetic_boot", false).unwrap();
    assert_eq!(
        advance_in(&tx, &backwards).unwrap().phase,
        RestPhase::Waking
    );
    assert_eq!(state_in(&tx).unwrap().wake_at, op.wake_at);
    advance_in(&tx, &backwards).unwrap();
    let c = ClockSample::new(AT + 101, 101000, "synthetic_boot", true).unwrap();
    sleep_in(&tx, &admin(&tx), 1, "second", &Default::default(), &c).unwrap();
    let reboot = ClockSample::new(AT - 500, 0, "different_boot", false).unwrap();
    assert_eq!(advance_in(&tx, &reboot).unwrap().phase, RestPhase::Waking);
    assert_eq!(
        state_in(&tx).unwrap().reason.as_deref(),
        Some("clock_continuity_lost")
    );
    advance_in(&tx, &reboot).unwrap();
    sleep_in(
        &tx,
        &admin(&tx),
        2,
        "third",
        &Default::default(),
        &ClockSample::new(AT + 202, 1000, "different_boot", true).unwrap(),
    )
    .unwrap();
    assert_eq!(
        advance_in(
            &tx,
            &ClockSample::new(AT + 2000, 1001, "different_boot", false).unwrap()
        )
        .unwrap()
        .phase,
        RestPhase::Waking
    );
}
#[test]
fn pending_drains_or_reaches_deadline_without_deleting_any_physical_lease_or_receipt() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    configure(&tx, false);
    source(&tx, "inflight", "a", "合成旧问题", None).unwrap();
    tx.execute("UPDATE runs SET status='cancelled' WHERE id='inflight'", [])
        .unwrap();
    tx.execute(
        "INSERT INTO native_worker_leases VALUES('inflight',?1)",
        [AT],
    )
    .unwrap();
    tx.execute("INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,platform_message_id,deadline,created_at) VALUES('unknown','inflight','synthetic','synthetic','{}','already_attempted','unknown','unchanged_receipt',?1,?1)",[AT]).unwrap();
    let op = begin(&tx);
    assert_eq!(
        advance_in(&tx, &clock(1)).unwrap().phase,
        RestPhase::SleepPending
    );
    assert_eq!(
        advance_in(&tx, &clock(10)).unwrap().phase,
        RestPhase::Consolidating
    );
    assert_eq!(
        tx.query_row("SELECT count(*) FROM native_worker_leases", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        tx.query_row(
            "SELECT status||':'||platform_message_id FROM outbox WHERE id='unknown'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "unknown:unchanged_receipt"
    );
    assert!(matches!(
        admission_in(&tx, RestWork::Receiver, AT + 11).unwrap(),
        RestAdmission::Allow { .. }
    ));
    assert!(matches!(
        admission_in(&tx, RestWork::Automatic, AT + 11).unwrap(),
        RestAdmission::Hold { .. }
    ));
    assert!(matches!(
        admission_in(
            &tx,
            RestWork::Publication {
                run_id: "inflight",
                original_deadline: Some(AT + 5),
                payload_digest: None
            },
            AT + 11
        )
        .unwrap(),
        RestAdmission::Reject { .. }
    ));
    assert_eq!(
        advance_in(&tx, &clock(100)).unwrap().phase,
        RestPhase::Waking
    );
    assert_eq!(
        advance_in(&tx, &clock(100)).unwrap().phase,
        RestPhase::Awake
    );
    assert_eq!(state_in(&tx).unwrap().cycle_id, op.cycle_id);
    assert_eq!(
        tx.query_row("SELECT count(*) FROM native_worker_leases", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[test]
fn acknowledgement_permit_is_exact_payload_run_grant_and_phase_not_a_send_grant() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    configure(&tx, true);
    let user = source(&tx, "control", "a", "快去睡觉啦！", None).unwrap();
    grant(&tx, &user);
    let op = sleep_in(
        &tx,
        &user,
        0,
        "control_sleep",
        &Default::default(),
        &clock(0),
    )
    .unwrap();
    let payload = hash("fixed host acknowledgement");
    bind_ack_payload_in(
        &tx,
        &user,
        op.ack_permit_id.as_deref().unwrap(),
        &payload,
        AT,
    )
    .unwrap();
    assert_eq!(
        advance_in(&tx, &clock(1)).unwrap().phase,
        RestPhase::SleepPending
    );
    assert!(matches!(
        admission_in(
            &tx,
            RestWork::Publication {
                run_id: "control",
                original_deadline: Some(AT + 60),
                payload_digest: Some(&payload)
            },
            AT + 1
        )
        .unwrap(),
        RestAdmission::Allow { .. }
    ));
    assert!(matches!(
        admission_in(
            &tx,
            RestWork::Publication {
                run_id: "control",
                original_deadline: Some(AT + 60),
                payload_digest: Some(&hash("model candidate"))
            },
            AT + 1
        )
        .unwrap(),
        RestAdmission::Reject { .. }
    ));
    assert!(mark_ack_attempt_started_in(&tx, "control", &payload, AT + 1).unwrap());
    assert!(!mark_ack_attempt_started_in(&tx, "control", &payload, AT + 1).unwrap());
    assert_eq!(
        advance_in(&tx, &clock(2)).unwrap().phase,
        RestPhase::Consolidating
    );
    assert!(matches!(
        admission_in(
            &tx,
            RestWork::Publication {
                run_id: "control",
                original_deadline: Some(AT + 60),
                payload_digest: Some(&payload)
            },
            AT + 2
        )
        .unwrap(),
        RestAdmission::Reject { .. }
    ));
    advance_in(&tx, &clock(100)).unwrap();
    advance_in(&tx, &clock(100)).unwrap();
    assert!(
        matches!(admission_in(&tx,RestWork::Publication{run_id:"control",original_deadline:Some(AT+200),payload_digest:Some(&payload)},AT+101).unwrap(),RestAdmission::Reject{code,..} if code=="rest_control_ack_not_permitted")
    );
}
#[test]
fn frozen_versions_cutoffs_and_revocation_fence_results_without_releasing_request() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    configure(&tx, false);
    event(&tx, "before", "owner_a", "SYNTHETIC_EVENT_ORIGINAL");
    let op = begin(&tx);
    let cycle = op.cycle_id.unwrap();
    advance_in(&tx, &clock(1)).unwrap();
    start_step_in(&tx, &cycle, "diary_day", AT + 1).unwrap();
    let job = job(&tx, &cycle, "before", 1);
    event(&tx, "after", "owner_a", "SYNTHETIC_EVENT_LATE");
    assert!(frozen_source_in(&tx, &cycle, SourceKind::DiaryEvent, "after").is_err());
    let claim = claim_job_in(&tx, &job.id, 500, &clock(2)).unwrap().unwrap();
    assert_eq!(sources_for_claim_in(&tx, &claim).unwrap().len(), 1);
    tx.execute(
        "UPDATE diary_events SET revision=2,content='corrected source' WHERE id='before'",
        [],
    )
    .unwrap();
    assert!(!finish_job_in(
        &tx,
        &claim,
        StepOutcome::Success,
        &StepCheckpoint {
            result_digest: Some(hash("discarded result")),
            ..Default::default()
        },
        None,
        &clock(3)
    )
    .unwrap());
    assert_eq!(running_count(&tx).unwrap(), 1);
    assert!(job_in(&tx, &job.id)
        .unwrap()
        .error_code
        .unwrap()
        .contains("stale"));
    assert!(
        frozen_source_in(&tx, &cycle, SourceKind::DiaryEvent, "before")
            .unwrap()
            .revision
            == 1
    );
    request_ended_in(&tx, &claim, StepOutcome::Failure, AT + 4).unwrap();
    assert_eq!(running_count(&tx).unwrap(), 0);
    assert!(!retry_job_in(&tx, &job.id).unwrap());
}
#[test]
fn one_automatic_slot_bounded_retry_and_unknown_result_require_distinct_checkpoints() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    configure(&tx, false);
    event(&tx, "first", "owner_a", "first evidence");
    event(&tx, "second", "owner_a", "second evidence");
    let op = begin(&tx);
    let cycle = op.cycle_id.unwrap();
    advance_in(&tx, &clock(1)).unwrap();
    start_step_in(&tx, &cycle, "diary_day", AT + 1).unwrap();
    let first = job(&tx, &cycle, "first", 1);
    let duplicate = job(&tx, &cycle, "first", 1);
    assert_eq!(first.id, duplicate.id);
    let second = job(&tx, &cycle, "second", 2);
    let a = claim_job_in(&tx, &first.id, 500, &clock(2))
        .unwrap()
        .unwrap();
    assert!(claim_job_in(&tx, &second.id, 500, &clock(2))
        .unwrap()
        .is_none());
    assert!(finish_job_in(
        &tx,
        &a,
        StepOutcome::Failure,
        &Default::default(),
        Some("provider_failed"),
        &clock(3)
    )
    .unwrap());
    assert!(!retry_job_in(&tx, &first.id).unwrap());
    request_ended_in(&tx, &a, StepOutcome::Failure, AT + 3).unwrap();
    assert!(retry_job_in(&tx, &first.id).unwrap());
    let b = claim_job_in(&tx, &first.id, 500, &clock(4))
        .unwrap()
        .unwrap();
    assert!(b.fence() > a.fence());
    assert!(finish_job_in(
        &tx,
        &b,
        StepOutcome::Unknown,
        &Default::default(),
        Some("provider_unknown"),
        &clock(5)
    )
    .unwrap());
    request_ended_in(&tx, &b, StepOutcome::Unknown, AT + 5).unwrap();
    assert!(!retry_job_in(&tx, &first.id).unwrap());
    assert!(finish_step_in(
        &tx,
        &cycle,
        "diary_day",
        StepOutcome::Success,
        &Default::default(),
        None,
        AT + 5
    )
    .is_err());
    assert_eq!(
        advance_in(&tx, &clock(40)).unwrap().phase,
        RestPhase::Sleeping
    );
    assert_eq!(
        advance_in(&tx, &clock(100)).unwrap().phase,
        RestPhase::Waking
    );
    advance_in(&tx, &clock(100)).unwrap();
    assert_eq!(job_in(&tx, &first.id).unwrap().status, "unknown");
    assert_eq!(
        tx.query_row(
            "SELECT calls,reserved_tokens FROM rest_cycles WHERE id=?1",
            [cycle],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        )
        .unwrap(),
        (2, 1000)
    );
}
#[test]
fn every_state_and_partial_step_result_is_persistent_and_does_not_claim_full_success() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    configure(&tx, false);
    let op = begin(&tx);
    let cycle = op.cycle_id.unwrap();
    assert_eq!(
        advance_in(&tx, &clock(1)).unwrap().phase,
        RestPhase::Consolidating
    );
    for kind in [
        "diary_day",
        "closed_periods",
        "relationships",
        "affinity_candidates",
        "commit_index",
        "checkpoint",
    ] {
        start_step_in(&tx, &cycle, kind, AT + 1).unwrap();
        let outcome = if kind == "relationships" {
            StepOutcome::Partial
        } else {
            StepOutcome::Success
        };
        finish_step_in(
            &tx,
            &cycle,
            kind,
            outcome,
            &StepCheckpoint {
                processed_refs: 2,
                result_digest: Some(hash(kind)),
                cursor_ref: None,
            },
            None,
            AT + 1,
        )
        .unwrap();
    }
    assert_eq!(
        advance_in(&tx, &clock(2)).unwrap().phase,
        RestPhase::Sleeping
    );
    assert_eq!(
        tx.query_row(
            "SELECT partial FROM rest_cycles WHERE id=?1",
            [&cycle],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        advance_in(&tx, &clock(100)).unwrap().phase,
        RestPhase::Waking
    );
    assert_eq!(
        advance_in(&tx, &clock(100)).unwrap().phase,
        RestPhase::Awake
    );
    assert_eq!(
        tx.query_row(
            "SELECT status FROM rest_steps WHERE cycle_id=?1 AND kind='relationships'",
            [cycle],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "partial"
    );
}

#[test]
fn a_saved_control_message_cannot_be_replayed_under_a_new_key_for_another_cycle() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    configure(&tx, true);
    let control = source(&tx, "real_control", "a", "快去睡觉啦", None).unwrap();
    grant(&tx, &control);
    let first = sleep_in(
        &tx,
        &control,
        0,
        "first_key",
        &Default::default(),
        &clock(0),
    )
    .unwrap();
    let duplicate = sleep_in(
        &tx,
        &control,
        0,
        "another_key",
        &Default::default(),
        &clock(1),
    )
    .unwrap();
    assert_eq!(first.id, duplicate.id);
    advance_in(&tx, &clock(100)).unwrap();
    advance_in(&tx, &clock(100)).unwrap();
    assert!(sleep_in(
        &tx,
        &control,
        1,
        "replay_old_input",
        &Default::default(),
        &clock(101)
    )
    .is_err());
    assert_eq!(
        tx.query_row("SELECT count(*) FROM rest_cycles", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn bounded_snapshot_is_partial_and_never_prevents_deadline_waking() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    configure(&tx, false);
    for index in 0..65 {
        tx.execute(
            "INSERT INTO diary_source_policies VALUES(?1,1,1,0,'Asia/Shanghai',?2,?2)",
            params![format!("synthetic_owner_{index:03}"), AT - 1],
        )
        .unwrap();
    }
    let op = begin(&tx);
    let cycle = op.cycle_id.unwrap();
    let progress = progress_in(&tx, &cycle).unwrap();
    assert!(progress.partial);
    assert_eq!(progress.steps[0].status, "partial");
    assert_eq!(
        tx.query_row(
            "SELECT count(*) FROM rest_cycle_sources WHERE cycle_id=?1",
            [&cycle],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        64
    );
    assert_eq!(
        advance_in(&tx, &clock(100)).unwrap().phase,
        RestPhase::Waking
    );
    advance_in(&tx, &clock(100)).unwrap();
    assert_eq!(state_in(&tx).unwrap().phase, RestPhase::Awake);
}

#[test]
fn revoked_model_permission_or_epoch_discards_a_result_and_metadata_never_copies_source_body() {
    for change in ["policy", "epoch"] {
        let mut db = database();
        let tx = db.transaction().unwrap();
        configure(&tx, false);
        event(
            &tx,
            "original",
            "owner_a",
            "SOURCE_BODY_MUST_NOT_BE_COPIED_873",
        );
        let op = begin(&tx);
        let cycle = op.cycle_id.unwrap();
        advance_in(&tx, &clock(1)).unwrap();
        start_step_in(&tx, &cycle, "diary_day", AT + 1).unwrap();
        let job = job(&tx, &cycle, "original", 1);
        let claim = claim_job_in(&tx, &job.id, 200, &clock(2)).unwrap().unwrap();
        assert!(!serde_json::to_string(
            &frozen_sources_page_in(&tx, &cycle, "owner_a", 0, 64).unwrap()
        )
        .unwrap()
        .contains("SOURCE_BODY_MUST_NOT_BE_COPIED_873"));
        if change == "policy" {
            tx.execute("UPDATE diary_source_policies SET revision=2,enabled=0 WHERE source_owner_id='owner_a'",[]).unwrap();
        } else {
            tx.execute("UPDATE diary_meta SET deletion_epoch=deletion_epoch+1", [])
                .unwrap();
        }
        assert!(!finish_job_in(
            &tx,
            &claim,
            StepOutcome::Success,
            &StepCheckpoint {
                result_digest: Some(hash("old result")),
                ..Default::default()
            },
            None,
            &clock(3)
        )
        .unwrap());
        assert_eq!(running_count(&tx).unwrap(), 1);
        assert!(tx
            .query_row(
                "SELECT result_digest IS NULL FROM consolidation_jobs WHERE id=?1",
                [&job.id],
                |r| r.get::<_, bool>(0)
            )
            .unwrap());
        request_ended_in(&tx, &claim, StepOutcome::Failure, AT + 3).unwrap();
    }
}

#[test]
fn restart_retains_pending_request_until_explicit_confirmed_exit_recovery_and_never_retries_unknown(
) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("restart.db");
    let (cycle, id, wake) = {
        let mut db = Connection::open(&path).unwrap();
        schema(&mut db);
        let tx = db.transaction().unwrap();
        configure(&tx, false);
        event(&tx, "source", "owner_a", "synthetic restart source");
        let op = begin(&tx);
        let cycle = op.cycle_id.unwrap();
        advance_in(&tx, &clock(1)).unwrap();
        start_step_in(&tx, &cycle, "diary_day", AT + 1).unwrap();
        let job = job(&tx, &cycle, "source", 1);
        claim_job_in(&tx, &job.id, 200, &clock(2)).unwrap().unwrap();
        tx.commit().unwrap();
        (cycle, job.id, op.wake_at)
    };
    let mut db = Connection::open(&path).unwrap();
    initialize(&db).unwrap();
    let tx = db.transaction().unwrap();
    assert_eq!(running_count(&tx).unwrap(), 1);
    let state = recover_after_confirmed_process_exit_in(&tx, &clock(5)).unwrap();
    assert_eq!(state.wake_at, wake);
    assert_eq!(state.phase, RestPhase::Consolidating);
    assert_eq!(running_count(&tx).unwrap(), 0);
    assert_eq!(job_in(&tx, &id).unwrap().status, "unknown");
    assert!(!retry_job_in(&tx, &id).unwrap());
    assert!(progress_in(&tx, &cycle).unwrap().partial);
    assert_eq!(
        advance_in(&tx, &clock(100)).unwrap().phase,
        RestPhase::Waking
    );
}

#[test]
fn queued_run_cap_and_original_deadline_do_not_stop_receivers_or_refresh_reply_windows() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    let mut p = RestPolicy {
        default_sleep_seconds: Some(100),
        queue_limit: 1,
        ..Default::default()
    };
    save_policy_in(&tx, &admin(&tx), 1, &p, AT).unwrap();
    source(&tx, "one", "a", "first incoming", None).unwrap();
    source(&tx, "two", "b", "second incoming", None).unwrap();
    begin(&tx);
    assert!(
        matches!(admission_in(&tx,RestWork::Ordinary{run_id:"two",original_deadline:Some(AT+40)},AT+1).unwrap(),RestAdmission::Reject{code,..} if code=="rest_pending_queue_full")
    );
    assert!(matches!(
        admission_in(&tx, RestWork::Receiver, AT + 1).unwrap(),
        RestAdmission::Allow { .. }
    ));
    advance_in(&tx, &clock(100)).unwrap();
    advance_in(&tx, &clock(100)).unwrap();
    assert!(
        matches!(admission_in(&tx,RestWork::Ordinary{run_id:"one",original_deadline:Some(AT+40)},AT+101).unwrap(),RestAdmission::Reject{code,..} if code=="original_reply_deadline_expired")
    );
    p.queue_limit = 2;
    assert!(save_policy_in(&tx, &admin(&tx), 1, &p, AT).is_err());
}

#[test]
fn schedule_permit_is_versioned_exact_minute_and_cannot_be_saved_for_later() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    let nominal = AT - AT % 60;
    let local = chrono::DateTime::from_timestamp(nominal, 0)
        .unwrap()
        .with_timezone(&chrono_tz::Asia::Shanghai);
    let p = RestPolicy {
        schedule_enabled: true,
        sleep_local: Some(format!("{:02}:{:02}", local.hour(), local.minute())),
        default_sleep_seconds: Some(100),
        ..Default::default()
    };
    save_policy_in(&tx, &admin(&tx), 1, &p, AT).unwrap();
    let permit = requester_for_schedule_in(&tx, 2, nominal, &clock(0)).unwrap();
    assert!(requester_for_schedule_in(&tx, 2, i64::MIN, &clock(0)).is_err());
    assert!(sleep_in(
        &tx,
        &permit,
        0,
        "late_rule",
        &Default::default(),
        &clock(120)
    )
    .is_err());
    assert_eq!(state_in(&tx).unwrap().phase, RestPhase::Awake);
}

#[test]
fn a_fully_completed_cycle_remains_successful_after_its_normal_wake_deadline() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    configure(&tx, false);
    let op = begin(&tx);
    let cycle = op.cycle_id.unwrap();
    advance_in(&tx, &clock(1)).unwrap();
    for kind in [
        "diary_day",
        "closed_periods",
        "relationships",
        "affinity_candidates",
        "commit_index",
        "checkpoint",
    ] {
        start_step_in(&tx, &cycle, kind, AT + 1).unwrap();
        finish_step_in(
            &tx,
            &cycle,
            kind,
            StepOutcome::Success,
            &StepCheckpoint {
                result_digest: Some(hash(kind)),
                ..Default::default()
            },
            None,
            AT + 1,
        )
        .unwrap();
    }
    assert_eq!(
        advance_in(&tx, &clock(2)).unwrap().phase,
        RestPhase::Sleeping
    );
    advance_in(&tx, &clock(100)).unwrap();
    advance_in(&tx, &clock(100)).unwrap();
    let progress = progress_in(&tx, &cycle).unwrap();
    assert!(!progress.partial);
    assert!(progress.error_code.is_none());
    assert!(progress.steps.iter().all(|s| s.status == "success"));
}

#[test]
fn rolling_wall_time_back_does_not_resurrect_an_expired_control_acknowledgement() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    configure(&tx, true);
    let user = source(&tx, "control", "a", "快去睡觉啦", None).unwrap();
    grant(&tx, &user);
    let op = sleep_in(&tx, &user, 0, "ack_sleep", &Default::default(), &clock(0)).unwrap();
    let digest = hash("fixed acknowledgement");
    bind_ack_payload_in(
        &tx,
        &user,
        op.ack_permit_id.as_deref().unwrap(),
        &digest,
        AT,
    )
    .unwrap();
    let backwards = ClockSample::new(AT - 100, 11000, "synthetic_boot", false).unwrap();
    advance_in(&tx, &backwards).unwrap();
    assert!(bind_ack_payload_in(
        &tx,
        &user,
        op.ack_permit_id.as_deref().unwrap(),
        &digest,
        AT - 100
    )
    .is_err());
    assert!(!mark_ack_attempt_started_in(&tx, "control", &digest, AT - 100).unwrap());
}

#[test]
fn a_frozen_person_assertion_expiring_during_the_cycle_cannot_be_used_as_current_fact() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    configure(&tx, false);
    event(&tx, "basis", "owner_a", "synthetic approved field basis");
    tx.execute("INSERT INTO relationship_people(id,assistant_id,display_name,revision,visibility,source_owner_id,created_at,updated_at) VALUES('person',?1,'Synthetic person',1,'private','owner_a',?2,?2)",params![assistant(&tx).unwrap(),AT-1]).unwrap();
    tx.execute("INSERT INTO relationship_assertions(id,person_id,field,value_json,revision,status,statement_kind,source_type,source_id,source_revision,visibility,source_owner_id,valid_until,created_at,updated_at) VALUES('assertion','person','job','\"synthetic teacher\"',1,'approved','user_claim','diary_event','basis',1,'private','owner_a',?1,?2,?2)",params![AT+3,AT-1]).unwrap();
    let op = begin(&tx);
    let cycle = op.cycle_id.unwrap();
    let reference =
        frozen_source_in(&tx, &cycle, SourceKind::PersonAssertion, "assertion").unwrap();
    assert!(validate_source_in(&tx, &cycle, &reference, true).unwrap());
    advance_in(&tx, &clock(4)).unwrap();
    assert!(!validate_source_in(&tx, &cycle, &reference, true).unwrap());
    assert_eq!(
        frozen_source_in(&tx, &cycle, SourceKind::PersonAssertion, "assertion").unwrap(),
        reference
    );
}

#[test]
fn a_future_rest_schema_is_rejected_without_resetting_the_pending_cycle() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    configure(&tx, false);
    let original = begin(&tx);
    tx.execute("UPDATE rest_meta SET schema_version=2 WHERE id=1", [])
        .unwrap();
    assert!(initialize(&tx).is_err());
    let state = state_in(&tx).unwrap();
    assert_eq!(state.phase, RestPhase::SleepPending);
    assert_eq!(state.cycle_id, original.cycle_id);
    assert_eq!(state.wake_at, original.wake_at);
}
