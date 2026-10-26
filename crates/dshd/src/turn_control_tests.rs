use super::*;
use rusqlite::{params, Connection};
use std::sync::{Arc, Barrier};
use std::time::Duration;

const OWNER: &str = "qq:synthetic:c2c:a";

fn schema(db: &Connection) {
    db.execute_batch(include_str!("../schema.sql")).unwrap();
    db.execute_batch("ALTER TABLE runs ADD COLUMN run_kind TEXT NOT NULL DEFAULT 'model';
      CREATE TABLE outbox_reply_parts(outbox_id TEXT,seq INTEGER,payload_json TEXT,status TEXT,error TEXT,platform_message_id TEXT,finished_ms INTEGER,PRIMARY KEY(outbox_id,seq));
      CREATE TABLE native_worker_leases(run_id TEXT PRIMARY KEY,claimed_at INTEGER);
      CREATE TABLE run_prompt_snapshots(run_id TEXT PRIMARY KEY,messages_json TEXT);
      CREATE TABLE memory_context_watermarks(conversation_id TEXT PRIMARY KEY,through_run_rowid INTEGER);
      CREATE TABLE weixin_outbox(id TEXT PRIMARY KEY,run_id TEXT,status TEXT,error_code TEXT,content TEXT,updated_at INTEGER);
      INSERT INTO conversations VALUES('c','qq:synthetic:c2c:a','synthetic',NULL,0);").unwrap();
    ensure_schema(db).unwrap();
}

fn database() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    schema(&db);
    db
}

fn insert(db: &Connection, id: &str, conversation: &str, owner: &str, input: &str) {
    db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,created_at,updated_at) VALUES(?1,?2,?3,?1,?4,'{}','running',0,0)",params![id,conversation,owner,input]).unwrap();
    db.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES(?1,?2,?1,'user',?3,0)",params![id,conversation,input]).unwrap();
}

fn add(db: &mut Connection, id: &str, input: &str) -> InputDecision {
    let tx = db.transaction().unwrap();
    insert(&tx, id, "c", OWNER, input);
    let result = register_input_in(&tx, id, id, input, OWNER, true, 1000).unwrap();
    tx.commit().unwrap();
    result
}

fn outbox(db: &Connection, run: &str, status: &str, parts: &[&str]) {
    db.execute(
        "UPDATE runs SET status='completed',response='one. two.' WHERE id=?1",
        [run],
    )
    .unwrap();
    db.execute("INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,deadline,created_at) VALUES(?1,?1,'synthetic','key','{}','one. two.',?2,999999,0)",params![run,status]).unwrap();
    for (i, status) in parts.iter().enumerate() {
        db.execute("INSERT INTO outbox_reply_parts(outbox_id,seq,payload_json,status,platform_message_id,finished_ms) VALUES(?1,?2,'{\"type\":\"text\",\"text\":\"synthetic\"}',?3,?4,?5)",params![run,i as i64+1,status,(*status=="sent").then_some("sent-evidence"),(*status=="sent").then_some(999)]).unwrap();
    }
}

fn allowed(db: &mut Connection, run: &str) -> bool {
    let tx = db.transaction().unwrap();
    publication_allowed_in(&tx, run).unwrap()
}

#[test]
fn closed_forms_do_not_cancel_thanks_another_question_quotes_or_negation() {
    for text in [
        "等等，改成明天",
        "刚才那个数字是 23",
        "取消刚才的问题",
        "上一条不用回答了",
    ] {
        assert!(is_explicit_correction(text), "{text}");
    }
    for text in [
        "谢谢",
        "另外问个问题",
        "改成明天",
        "等等",
        "等等，改成",
        "不要取消刚才的问题",
        "如果我说等等，改成明天呢",
        "他说“等等，改成明天”",
        "等等，改成明天，另外问个问题",
        "/memory forget 1",
        "别改，等等，改成明天",
        "刚才那个数字是不是23？",
        "刚才那个数字是23吗",
        "等等，改成明天？",
    ] {
        assert!(!is_explicit_correction(text), "{text}");
    }
    let mut db = database();
    add(&mut db, "r1", "原问题");
    let second = add(&mut db, "r2", "谢谢");
    assert_eq!(second.predecessor_run_id.as_deref(), Some("r1"));
    assert!(second.correction_of_run_id.is_none());
    assert!(allowed(&mut db, "r1"));
}

#[test]
fn explicit_correction_keeps_the_request_snapshot_and_http_lease() {
    let mut db = database();
    add(&mut db, "r1", "请计算 2+3");
    db.execute_batch("INSERT INTO native_worker_leases VALUES('r1',100);INSERT INTO run_prompt_snapshots VALUES('r1','immutable-original');").unwrap();
    let correction = add(&mut db, "r2", "刚才那个数字是 23");
    assert_eq!(correction.input_seq, 2);
    assert!(correction.predecessor_suppressed);
    assert_eq!(correction.correction_of_run_id.as_deref(), Some("r1"));
    assert!(!allowed(&mut db, "r1"));
    let retained: (String, String, String, i64) = db.query_row("SELECT r.input,r.status,p.messages_json,(SELECT count(*) FROM native_worker_leases) FROM runs r JOIN run_prompt_snapshots p ON p.run_id=r.id WHERE r.id='r1'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
    assert_eq!(
        retained,
        (
            "请计算 2+3".into(),
            "running".into(),
            "immutable-original".into(),
            1
        )
    );
    let tx = db.transaction().unwrap();
    let context = correction_context_in(&tx, "r2").unwrap().unwrap();
    assert_eq!(context.original_input, "请计算 2+3");
    assert_eq!(context.correction_input, "刚才那个数字是 23");
    assert_eq!(context.predecessor_decision, "suppressed");
    assert!(!claim_allowed_in(&tx, "r1", 1).unwrap());
}

#[test]
fn partial_delivery_only_cancels_unclaimed_parts_and_preserves_sent_evidence() {
    let mut db = database();
    add(&mut db, "r1", "原问题");
    outbox(&db, "r1", "queued", &["sent", "queued", "queued"]);
    assert!(add(&mut db, "r2", "等等，改成明天").predecessor_suppressed);
    let rows: Vec<(String, Option<String>, Option<i64>)> = db
        .prepare(
            "SELECT status,platform_message_id,finished_ms FROM outbox_reply_parts ORDER BY seq",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        rows[0],
        ("sent".into(), Some("sent-evidence".into()), Some(999))
    );
    assert_eq!(rows[1].0, "cancelled");
    assert_eq!(rows[2].0, "cancelled");
    let response: String = db
        .query_row("SELECT response FROM runs WHERE id='r1'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(response, "one. two.");
}

#[test]
fn in_flight_unknown_and_effects_never_become_unsent_or_replayed() {
    for state in ["sending", "unknown"] {
        let mut db = database();
        add(&mut db, "r1", "原问题");
        outbox(&db, "r1", state, &[state, "queued"]);
        let correction = add(&mut db, "r2", "等等，改成明天");
        assert!(!correction.predecessor_suppressed);
        assert_eq!(correction.reason, "delivery_in_flight_or_unknown");
        let kept: String = db
            .query_row(
                "SELECT status FROM outbox_reply_parts WHERE seq=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, state);
        assert!(allowed(&mut db, "r1"));
    }
    let mut db = database();
    add(&mut db, "r1", "原问题");
    {
        let tx = db.transaction().unwrap();
        assert!(mark_effects_started_in(&tx, "r1", 1).unwrap());
        tx.commit().unwrap();
    }
    assert_eq!(
        add(&mut db, "r2", "等等，改成明天").reason,
        "effects_started"
    );
    assert!(allowed(&mut db, "r1"));
}

#[test]
fn a_correction_committed_first_prevents_later_effects() {
    let mut db = database();
    add(&mut db, "r1", "原问题");
    add(&mut db, "r2", "等等，改成明天");
    let tx = db.transaction().unwrap();
    assert!(!mark_effects_started_in(&tx, "r1", 2000).unwrap());
    assert_eq!(
        tx.query_row(
            "SELECT effects_started FROM run_publication_decisions WHERE run_id='r1'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn unregistered_boundary_media_and_group_speakers_do_not_get_superseded() {
    let mut db = database();
    add(&mut db, "r1", "原问题");
    insert(&db, "boundary", "c", OWNER, "untracked command");
    assert!(!add(&mut db, "r2", "等等，改成明天").predecessor_suppressed);
    assert!(allowed(&mut db, "r1"));
    let mut db = database();
    add(&mut db, "r1", "原问题");
    outbox(&db, "r1", "queued", &["queued"]);
    db.execute(
        "UPDATE outbox_reply_parts SET payload_json='{\"type\":\"voice\"}'",
        [],
    )
    .unwrap();
    assert_eq!(
        add(&mut db, "r2", "等等，改成明天").reason,
        "predecessor_media_reply"
    );
    let tx = db.transaction().unwrap();
    tx.execute(
        "INSERT INTO conversations VALUES('group','qq:synthetic:group:g','g',NULL,0)",
        [],
    )
    .unwrap();
    for (id, text) in [("g1", "原问题"), ("g2", "等等，改成明天")] {
        insert(&tx, id, "group", "qq:synthetic:group:g", text);
        let decision =
            register_input_in(&tx, id, id, text, "qq:synthetic:group:g", true, 1).unwrap();
        assert!(!decision.predecessor_suppressed);
    }
}

#[test]
fn replay_is_idempotent_and_batch_fragments_keep_monotonic_sequences() {
    let mut db = database();
    add(&mut db, "r1", "first fragment");
    let tx = db.transaction().unwrap();
    let first = register_input_in(&tx, "r1", "r1", "first fragment", OWNER, true, 1000).unwrap();
    assert_eq!(first.input_seq, 1);
    let second = register_input_in(&tx, "r1", "fragment-2", "more", OWNER, true, 1001).unwrap();
    assert_eq!(second.input_seq, 2);
    assert!(register_input_in(&tx, "r1", "fragment-2", "different", OWNER, true, 1002).is_err());
    assert_eq!(
        tx.query_row("SELECT count(*) FROM conversation_inputs", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    tx.commit().unwrap();
    assert_eq!(add(&mut db, "r2", "new question").input_seq, 3);
}

#[test]
fn forgetting_blocks_correction_context_even_after_link_creation() {
    let mut db = database();
    add(&mut db, "r1", "private forgotten marker");
    add(&mut db, "r2", "等等，改成明天");
    let tx = db.transaction().unwrap();
    tx.execute(
        "INSERT INTO memory_context_watermarks SELECT 'c',rowid FROM runs WHERE id='r1'",
        [],
    )
    .unwrap();
    assert!(correction_context_in(&tx, "r2").unwrap().is_none());
}

#[test]
fn a_completed_local_or_fully_sent_answer_is_not_retracted() {
    for channel in ["local", "qq", "weixin"] {
        let mut db = database();
        add(&mut db, "r1", "原问题");
        match channel {
            "qq" => outbox(&db, "r1", "sent", &["sent"]),
            "weixin" => {
                db.execute(
                    "INSERT INTO weixin_outbox VALUES('w','r1','accepted',NULL,NULL,0)",
                    [],
                )
                .unwrap();
            }
            _ => {
                db.execute(
                    "UPDATE runs SET status='completed',response='already visible' WHERE id='r1'",
                    [],
                )
                .unwrap();
            }
        }
        let decision = add(&mut db, "r2", "等等，改成明天");
        assert!(!decision.predecessor_suppressed, "{channel}");
        assert!(decision.correction_of_run_id.is_some());
    }
}

#[test]
fn correction_survives_restart_and_a_stale_sender_revision_is_fenced() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("turns.sqlite");
    let mut db = Connection::open(&path).unwrap();
    schema(&db);
    add(&mut db, "r1", "原问题");
    let revision = {
        let tx = db.transaction().unwrap();
        active_revision_in(&tx, "r1").unwrap().unwrap()
    };
    add(&mut db, "r2", "等等，改成明天");
    drop(db);
    let mut restarted = Connection::open(&path).unwrap();
    ensure_schema(&restarted).unwrap();
    let tx = restarted.transaction().unwrap();
    assert!(!claim_allowed_in(&tx, "r1", revision).unwrap());
    assert!(correction_context_in(&tx, "r2").unwrap().is_some());
    assert_eq!(
        tx.query_row(
            "SELECT count(*) FROM run_publication_events WHERE run_id='r1'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
}

#[test]
fn two_database_connections_serialize_correction_and_sender_claim() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("race.sqlite");
    let mut db = Connection::open(&path).unwrap();
    schema(&db);
    add(&mut db, "r1", "原问题");
    outbox(&db, "r1", "queued", &["queued", "queued"]);
    let barrier = Arc::new(Barrier::new(2));
    let sender_path = path.clone();
    let sender_barrier = barrier.clone();
    let sender = std::thread::spawn(move || {
        let mut db = Connection::open(sender_path).unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        let revision = active_revision_in(&tx, "r1").unwrap().unwrap();
        assert!(claim_allowed_in(&tx, "r1", revision).unwrap());
        tx.execute("UPDATE outbox SET status='sending' WHERE run_id='r1'", [])
            .unwrap();
        tx.execute(
            "UPDATE outbox_reply_parts SET status='sending' WHERE outbox_id='r1' AND seq=1",
            [],
        )
        .unwrap();
        sender_barrier.wait();
        tx.commit().unwrap();
    });
    barrier.wait();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    insert(&tx, "r2", "c", OWNER, "等等，改成明天");
    let decision = register_input_in(&tx, "r2", "r2", "等等，改成明天", OWNER, true, 2000).unwrap();
    assert!(!decision.predecessor_suppressed);
    assert_eq!(decision.reason, "delivery_in_flight_or_unknown");
    assert_eq!(
        tx.query_row(
            "SELECT status FROM outbox_reply_parts WHERE seq=1",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "sending"
    );
    tx.commit().unwrap();
    sender.join().unwrap();
}

#[test]
fn public_mutation_and_claim_apis_require_the_callers_transaction() {
    let mut db = database();
    add(&mut db, "r1", "原问题");
    assert!(active_revision_in(&db, "r1").is_err());
    assert!(register_input_in(&db, "r1", "later", "extra", OWNER, true, 1).is_err());
    assert!(mark_effects_started_in(&db, "r1", 1).is_err());
}
