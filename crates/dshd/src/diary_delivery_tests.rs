use super::*;

fn database() -> Connection {
    let mut db = Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("../schema.sql")).unwrap();
    db.execute_batch("ALTER TABLE runs ADD COLUMN run_kind TEXT NOT NULL DEFAULT 'model';
      ALTER TABLE runs ADD COLUMN character_snapshot_json TEXT;
      CREATE TABLE conversation_character_bindings(conversation_id TEXT PRIMARY KEY,draft_card_json TEXT);
      CREATE TABLE run_prompt_snapshots(run_id TEXT PRIMARY KEY,messages_json TEXT);
      CREATE TABLE outbox_reply_parts(outbox_id TEXT NOT NULL,seq INTEGER NOT NULL,payload_json TEXT NOT NULL,status TEXT NOT NULL,platform_message_id TEXT,error TEXT,finished_ms INTEGER,PRIMARY KEY(outbox_id,seq));
      INSERT INTO conversations VALUES('c','qq:fixture:c2c:person','Synthetic','qq:fixture:c2c:person',0);").unwrap();
    memory::initialize(&mut db).unwrap();
    crate::conversation_summary::initialize(&db).unwrap();
    crate::relationships::initialize(&db).unwrap();
    initialize(&mut db).unwrap();
    crate::delivery_context::ensure_schema(&db).unwrap();
    initialize_delivery_capture(&db).unwrap();
    db.execute("INSERT INTO diary_source_policies VALUES('qq:fixture:c2c:person',1,1,0,'Asia/Shanghai',0,0)",[]).unwrap();
    db
}
fn run(db: &Connection, id: &str) {
    db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,response,created_at,updated_at) VALUES(?1,'c','qq:fixture:c2c:person',?1,'原始用户问题','{}','completed','UNSENT_PRIVATE_CANDIDATE',?2,?2)",params![id,now()]).unwrap();
    db.execute("INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,deadline,created_at) VALUES(?1,?1,'fixture','synthetic','{}','UNSENT_PRIVATE_CANDIDATE','unknown',?2,?2)",params![id,now()]).unwrap();
}
fn part(db: &Connection, run: &str, seq: i64, text: &str, status: &str, receipt: Option<&str>) {
    db.execute(
        "INSERT INTO outbox_reply_parts VALUES(?1,?2,?3,?4,?5,NULL,NULL)",
        params![
            run,
            seq,
            json!({"type":"text","text":text}).to_string(),
            status,
            receipt
        ],
    )
    .unwrap();
}
fn captured(db: &Connection, run: &str) -> Event {
    let id: String = db
        .query_row(
            "SELECT id FROM diary_events WHERE origin_id=?1 AND origin_type='conversation_run'",
            [run],
            |r| r.get(0),
        )
        .unwrap();
    event(db, &id, None).unwrap()
}

#[test]
fn captures_actual_text_and_unknown_evidence_never_generated_candidate() {
    let db = database();
    run(&db, "r");
    part(&db, "r", 1, "实际平台接受的句子", "sent", Some("receipt"));
    part(&db, "r", 2, "UNSENT_PART_SECRET", "unknown", None);
    assert!(capture_completed_in(&db, "r").unwrap());
    let entry = captured(&db, "r");
    assert!(entry.content.contains("原始用户问题"));
    assert!(entry.content.contains("实际平台接受的句子"));
    assert!(!entry.content.contains("UNSENT_PRIVATE_CANDIDATE"));
    assert!(!entry.content.contains("UNSENT_PART_SECRET"));
    assert!(entry.content.contains("unknown"));
    assert!(entry.content.contains("不能当作未发出"));
    let snapshots: i64 = db
        .query_row(
            "SELECT count(*) FROM delivery_context_snapshots WHERE kind='diary_capture'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(snapshots, 1);
    assert!(!capture_completed_in(&db, "r").unwrap());
}

#[test]
fn pending_new_delivery_waits_and_late_receipts_revise_then_converge() {
    let mut db = database();
    run(&db, "r");
    part(&db, "r", 1, "第一句", "sent", Some("first"));
    part(&db, "r", 2, "第二句", "sending", None);
    assert!(!capture_completed_in(&db, "r").unwrap());
    db.execute(
        "UPDATE outbox_reply_parts SET status='unknown' WHERE seq=2",
        [],
    )
    .unwrap();
    assert!(capture_completed_in(&db, "r").unwrap());
    let original = captured(&db, "r");
    db.execute(
        "UPDATE outbox_reply_parts SET status='sent',platform_message_id='late' WHERE seq=2",
        [],
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    assert_eq!(refresh_delivery_captures_in(&tx).unwrap(), 1);
    tx.commit().unwrap();
    let revised = captured(&db, "r");
    assert_eq!(revised.revision, original.revision + 1);
    assert!(revised.content.contains("第二句"));
    let tx = db.transaction().unwrap();
    assert_eq!(refresh_delivery_captures_in(&tx).unwrap(), 0);
    tx.commit().unwrap();
    assert_eq!(captured(&db, "r").revision, revised.revision);
    let floor: i64 = db
        .query_row("SELECT count(*) FROM memory_context_watermarks", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        floor, 0,
        "an unused event cannot cancel unrelated current turns"
    );
}

#[test]
fn late_receipt_invalidates_existing_diary_derivatives_and_old_versions() {
    let mut db = database();
    run(&db, "r");
    part(&db, "r", 1, "第一句", "sent", Some("first"));
    part(&db, "r", 2, "第二句", "unknown", None);
    capture_completed_in(&db, "r").unwrap();
    let source = captured(&db, "r");
    let day = local_date(now(), TIMEZONE.parse().unwrap())
        .unwrap()
        .to_string();
    let derived = {
        let tx = db.transaction().unwrap();
        let e = consolidate_in(
            &tx,
            "private",
            Some("qq:fixture:c2c:person"),
            "day",
            &day,
            TIMEZONE,
            now(),
        )
        .unwrap();
        tx.commit().unwrap();
        e
    };
    db.execute(
        "UPDATE outbox_reply_parts SET status='sent',platform_message_id='late' WHERE seq=2",
        [],
    )
    .unwrap();
    assert!(capture_completed_in(&db, "r").unwrap());
    assert!(entry(&db, &derived.id, None).is_err());
    assert!(captured(&db, "r").revision > source.revision);
    let count:i64=db.query_row("SELECT count(*) FROM diary_event_versions WHERE event_id=?1 AND revision=?2 AND content IS NOT NULL",params![source.id,source.revision],|r|r.get(0)).unwrap();
    assert_eq!(count, 0);
    assert!(!capture_completed_in(&db, "r").unwrap());
}

#[test]
fn forgotten_revoked_and_human_corrected_captures_do_not_resurrect() {
    let mut db = database();
    for id in ["forgotten", "corrected", "revoked"] {
        run(&db, id);
        part(&db, id, 1, "先前已发", "sent", Some(id));
        capture_completed_in(&db, id).unwrap();
    }
    let forgotten = captured(&db, "forgotten");
    forget(&mut db, "admin", &forgotten.id, forgotten.revision).unwrap();
    let corrected = captured(&db, "corrected");
    correct(
        &mut db,
        "admin",
        &corrected.id,
        corrected.revision,
        "用户明确更正的记录",
    )
    .unwrap();
    db.execute(
        "UPDATE outbox_reply_parts SET platform_message_id='late-changed'",
        [],
    )
    .unwrap();
    assert!(!capture_completed_in(&db, "forgotten").unwrap());
    assert!(event(&db, &forgotten.id, None).is_err());
    assert!(!capture_completed_in(&db, "corrected").unwrap());
    assert_eq!(captured(&db, "corrected").content, "用户明确更正的记录");
    save_policy(
        &mut db,
        "admin",
        1,
        &SourcePolicy {
            source_owner_id: "qq:fixture:c2c:person".into(),
            enabled: false,
            allow_model: false,
            timezone: TIMEZONE.into(),
        },
    )
    .unwrap();
    save_policy(
        &mut db,
        "admin",
        2,
        &SourcePolicy {
            source_owner_id: "qq:fixture:c2c:person".into(),
            enabled: true,
            allow_model: false,
            timezone: TIMEZONE.into(),
        },
    )
    .unwrap();
    assert!(!capture_completed_in(&db, "revoked").unwrap());
    let id: String = db
        .query_row(
            "SELECT id FROM diary_events WHERE origin_id='revoked'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(event(&db, &id, None).is_err());
}

#[test]
fn legacy_migration_removes_unconfirmed_raw_and_is_restart_idempotent() {
    let db = database();
    run(&db, "legacy");
    part(&db, "legacy", 1, "未证实的旧正文", "sending", None);
    db.execute("INSERT INTO diary_events(id,assistant_id,revision,visibility,source_owner_id,source_conversation_id,origin_type,origin_id,happened_at,observed_at,content,assertion_kind) VALUES('old',?1,1,'private','qq:fixture:c2c:person','c','conversation_run','legacy',?2,?2,'我的回复：UNSENT_PRIVATE_CANDIDATE','direct_experience')",params![memory::identity(&db).unwrap(),now()]).unwrap();
    db.execute("INSERT INTO diary_event_versions VALUES('old',1,'UNSENT_PRIVATE_CANDIDATE','old','captured',0)",[]).unwrap();
    run(&db, "legacy-manual");
    db.execute("INSERT INTO diary_events(id,assistant_id,revision,visibility,source_owner_id,source_conversation_id,origin_type,origin_id,happened_at,observed_at,content,assertion_kind) VALUES('manual',?1,2,'private','qq:fixture:c2c:person','c','conversation_run','legacy-manual',?2,?2,'人明确更正的独立记录','direct_experience')",params![memory::identity(&db).unwrap(),now()]).unwrap();
    db.execute("INSERT INTO diary_event_versions VALUES('manual',2,'人明确更正的独立记录','admin','corrected',0)",[]).unwrap();
    db.execute("DELETE FROM diary_capture_migrations", [])
        .unwrap();
    initialize_delivery_capture(&db).unwrap();
    let migrated = event(&db, "old", None).unwrap();
    assert!(!migrated.content.contains("UNSENT_PRIVATE_CANDIDATE"));
    assert!(migrated.content.contains("原始用户问题"));
    assert!(!migrated.content.contains("未证实的旧正文"));
    assert!(migrated.revision > 1);
    initialize_delivery_capture(&db).unwrap();
    assert_eq!(event(&db, "old", None).unwrap().revision, migrated.revision);
    let unsafe_versions:i64=db.query_row("SELECT count(*) FROM diary_event_versions WHERE content LIKE '%UNSENT_PRIVATE_CANDIDATE%'",[],|r|r.get(0)).unwrap();
    assert_eq!(unsafe_versions, 0);
    assert_eq!(
        event(&db, "manual", None).unwrap().content,
        "人明确更正的独立记录"
    );
    assert!(!capture_completed_in(&db, "legacy-manual").unwrap());
}

#[test]
fn receipt_backlog_is_persisted_and_each_refresh_has_a_fixed_work_limit() {
    let mut db = database();
    for index in 0..33 {
        let id = format!("bounded-{index}");
        run(&db, &id);
        part(&db, &id, 1, "待核对的句子", "unknown", None);
        capture_completed_in(&db, &id).unwrap();
    }
    db.execute(
        "UPDATE outbox_reply_parts SET status='sent',platform_message_id='late-receipt'",
        [],
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    assert_eq!(refresh_delivery_captures_in(&tx).unwrap(), 32);
    tx.commit().unwrap();
    let remaining: i64 = db
        .query_row("SELECT count(*) FROM diary_capture_dirty", [], |r| r.get(0))
        .unwrap();
    assert_eq!(remaining, 1);
    let tx = db.transaction().unwrap();
    assert_eq!(refresh_delivery_captures_in(&tx).unwrap(), 1);
    tx.commit().unwrap();
    let tx = db.transaction().unwrap();
    assert_eq!(refresh_delivery_captures_in(&tx).unwrap(), 0);
    tx.commit().unwrap();
}
