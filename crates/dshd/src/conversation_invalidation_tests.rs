use super::*;
use crate::{conversation_focus as focus, conversation_identity as identity, turn_control};
use serde_json::json;

const AT: i64 = 1_793_030_000;
fn schema(db: &Connection) {
    db.execute_batch(include_str!("../schema.sql")).unwrap();
    db.execute_batch("ALTER TABLE runs ADD COLUMN run_kind TEXT NOT NULL DEFAULT 'model';ALTER TABLE runs ADD COLUMN character_snapshot_json TEXT;
      CREATE TABLE conversation_character_bindings(conversation_id TEXT PRIMARY KEY,draft_card_json TEXT);
      CREATE TABLE run_prompt_snapshots(run_id TEXT PRIMARY KEY,messages_json TEXT);
      CREATE TABLE memory_context_watermarks(conversation_id TEXT PRIMARY KEY,assistant_id TEXT NOT NULL,through_run_rowid INTEGER NOT NULL,updated_at INTEGER NOT NULL);
      CREATE TABLE native_worker_leases(run_id TEXT PRIMARY KEY,claimed_at INTEGER NOT NULL);
      CREATE TABLE diary_model_leases(run_id TEXT PRIMARY KEY,request_digest TEXT NOT NULL);
      CREATE TABLE outbox_reply_parts(outbox_id TEXT NOT NULL,seq INTEGER NOT NULL,payload_json TEXT NOT NULL,status TEXT NOT NULL,platform_message_id TEXT,error TEXT,finished_ms INTEGER,PRIMARY KEY(outbox_id,seq));
      CREATE TABLE weixin_outbox(id TEXT PRIMARY KEY,run_id TEXT NOT NULL UNIQUE,content TEXT,status TEXT NOT NULL,error_code TEXT,server_message_id TEXT,updated_at INTEGER NOT NULL);
      INSERT INTO settings VALUES('assistant.identity.v1','stable');").unwrap();
    identity::initialize(db).unwrap();
    turn_control::ensure_schema(db).unwrap();
    state::initialize(db).unwrap();
    commitments::ensure_schema(db).unwrap();
    crate::delivery_context::ensure_schema(db).unwrap();
}
fn database() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    schema(&db);
    db
}
fn insert_run(db: &Connection, id: &str, conversation: &str, owner: &str, text: &str) {
    db.execute(
        "INSERT OR IGNORE INTO conversations VALUES(?1,?2,'Synthetic',?2,?3)",
        params![conversation, owner, AT],
    )
    .unwrap();
    db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,created_at,updated_at)VALUES(?1,?2,?3,?1,?4,'{}','running',?5,?5)",params![id,conversation,owner,text,AT]).unwrap();
    db.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at)VALUES(?1,?2,?1,'user',?3,?4)",params![id,conversation,text,AT]).unwrap();
    identity::freeze_run_in(db, id).unwrap();
    turn_control::register_input_in(db, id, id, text, owner, true, AT * 1000).unwrap();
}
struct Cohort {
    owner: String,
    conversation: String,
    source: String,
    consumer: String,
    commitment: String,
}
fn cohort(db: &mut Connection, name: &str, owner: &str) -> Cohort {
    let source = format!("{name}-source");
    let consumer = format!("{name}-consumer");
    let conversation = format!("{name}-conversation");
    let tx = db.transaction().unwrap();
    insert_run(
        &tx,
        &source,
        &conversation,
        owner,
        &format!("请提醒我明天整理CANARY_{name}_PRIVATE"),
    );
    focus::observe_input_in(&tx, &source, AT, &focus::FocusPolicy::default()).unwrap();
    let scope = state::scope_for_run_in(&tx, &source).unwrap();
    let proposal = commitments::propose_natural_in(&tx, &scope, AT)
        .unwrap()
        .unwrap();
    tx.execute("UPDATE runs SET status='completed' WHERE id=?1", [&source])
        .unwrap();
    insert_run(&tx, &consumer, &conversation, owner, "继续");
    focus::observe_input_in(&tx, &consumer, AT, &focus::FocusPolicy::default()).unwrap();
    let selected = focus::context_for_run_in(&tx, &consumer, AT).unwrap();
    state::record_usage_in(&tx, &consumer, &selected.entity_refs, AT).unwrap();
    let selected = commitments::context_for_run_in(&tx, &consumer, 4, 12000, AT).unwrap();
    assert_eq!(selected.items.len(), 1);
    commitments::record_usage_in(&tx, &consumer, &selected.source_refs, AT).unwrap();
    tx.execute(
        "INSERT INTO run_prompt_snapshots VALUES(?1,?2)",
        params![consumer, format!("CANARY_{name}_PRIVATE")],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO native_worker_leases VALUES(?1,?2)",
        params![consumer, AT],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO diary_model_leases VALUES(?1,'physical-request-still-owned')",
        [&consumer],
    )
    .unwrap();
    tx.commit().unwrap();
    Cohort {
        owner: owner.into(),
        conversation,
        source,
        consumer,
        commitment: proposal["id"].as_str().unwrap().into(),
    }
}
fn status(db: &Connection, id: &str) -> String {
    db.query_row("SELECT status FROM runs WHERE id=?1", [id], |r| r.get(0))
        .unwrap()
}
fn additional_consumer(db: &mut Connection, cohort: &Cohort, id: &str) {
    let tx = db.transaction().unwrap();
    insert_run(&tx, id, &cohort.conversation, &cohort.owner, "继续");
    focus::observe_input_in(&tx, id, AT, &focus::FocusPolicy::default()).unwrap();
    let selected = focus::context_for_run_in(&tx, id, AT).unwrap();
    state::record_usage_in(&tx, id, &selected.entity_refs, AT).unwrap();
    let selected = commitments::context_for_run_in(&tx, id, 4, 12000, AT).unwrap();
    commitments::record_usage_in(&tx, id, &selected.source_refs, AT).unwrap();
    tx.commit().unwrap();
}
fn qq(db: &Connection, run: &str, parent: &str) {
    db.execute("INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,platform_message_id,deadline,created_at)VALUES(?1,?1,'synthetic','synthetic','{}','actual durable candidate',?2,?3,?4,?4)",params![run,parent,(parent=="sent").then_some("actual-parent-receipt"),AT]).unwrap();
    for (seq, state) in [(1, "sent"), (2, "sending"), (3, "queued"), (4, "unknown")] {
        db.execute(
            "INSERT INTO outbox_reply_parts VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                run,
                seq,
                json!({"type":"text","text":format!("actual-part-{seq}")}).to_string(),
                state,
                (state == "sent").then_some("confirmed-platform-id"),
                (state == "unknown").then_some("original_unknown_diagnostic"),
                if state == "sent" {
                    Some(AT * 1000)
                } else {
                    None
                }
            ],
        )
        .unwrap();
    }
}

#[test]
fn exact_source_invalidation_clears_a_both_modules_but_not_b_or_unreferenced_runs() {
    let mut db = database();
    let a = cohort(&mut db, "A", "qq:synthetic:c2c:a");
    let b = cohort(&mut db, "B", "qq:synthetic:c2c:b");
    let tx = db.transaction().unwrap();
    insert_run(
        &tx,
        "unreferenced",
        &a.conversation,
        &a.owner,
        "未读取新派生状态的独立请求",
    );
    // Same timestamp must never be used to discover the present cohort.
    tx.execute(
        "UPDATE conversation_commitment_usage SET invalidated_at=?1 WHERE run_id=?2",
        params![AT, b.consumer],
    )
    .unwrap();
    let result = invalidate_source_in(&tx, "run_input", &a.source, Some(1), AT).unwrap();
    assert_eq!(result.affected_run_ids, vec![a.consumer.clone()]);
    assert_eq!(result.cancelled_runs, 1);
    assert!(result.focus_entities > 0);
    assert_eq!(result.commitments, 1);
    assert_eq!(status(&tx, &a.consumer), "cancelled");
    assert_eq!(status(&tx, &b.consumer), "running");
    assert_eq!(status(&tx, "unreferenced"), "running");
    assert_eq!(
        tx.query_row(
            "SELECT count(*) FROM run_prompt_snapshots WHERE run_id=?1",
            [&a.consumer],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        tx.query_row(
            "SELECT count(*) FROM run_prompt_snapshots WHERE run_id=?1",
            [&b.consumer],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert!(tx
        .query_row(
            "SELECT invalidated_at IS NULL FROM conversation_commitment_contexts WHERE run_id=?1",
            [&b.consumer],
            |r| r.get::<_, bool>(0)
        )
        .unwrap());
    let a_body: String = tx
        .query_row(
            "SELECT intent FROM conversation_commitments WHERE id=?1",
            [&a.commitment],
            |r| r.get(0),
        )
        .unwrap();
    assert!(a_body.is_empty());
    let b_body: String = tx
        .query_row(
            "SELECT intent FROM conversation_commitments WHERE id=?1",
            [&b.commitment],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!b_body.is_empty());
    let left:i64=tx.query_row("SELECT count(*) FROM conversation_state_entities WHERE content_json LIKE '%CANARY_A_PRIVATE%'",[],|r|r.get(0)).unwrap();
    assert_eq!(left, 0);
    assert_eq!(
        tx.query_row("SELECT count(*) FROM native_worker_leases", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        tx.query_row("SELECT count(*) FROM diary_model_leases", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        tx.query_row("SELECT count(*) FROM memory_context_watermarks", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
}

#[test]
fn a_source_change_preserves_unrelated_frozen_entities_in_the_same_scope() {
    let mut db = database();
    let a = cohort(&mut db, "A", "qq:synthetic:c2c:a");
    let tx = db.transaction().unwrap();
    insert_run(
        &tx,
        "other-source",
        &a.conversation,
        &a.owner,
        "讨论一个独立的合成照明主题",
    );
    focus::observe_input_in(&tx, "other-source", AT, &Default::default()).unwrap();
    insert_run(&tx, "other-consumer", &a.conversation, &a.owner, "继续");
    focus::observe_input_in(&tx, "other-consumer", AT, &Default::default()).unwrap();
    let other = focus::context_for_run_in(&tx, "other-consumer", AT).unwrap();
    assert_eq!(
        other.current_focus.as_ref().unwrap().source_run_id,
        "other-source"
    );
    state::record_usage_in(&tx, "other-consumer", &other.entity_refs, AT).unwrap();
    tx.execute(
        "INSERT INTO run_prompt_snapshots VALUES('other-consumer','unrelated-frozen-prompt')",
        [],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO native_worker_leases VALUES('other-consumer',?1)",
        [AT],
    )
    .unwrap();
    let before: i64 = tx
        .query_row(
            "SELECT epoch FROM conversation_state_heads WHERE scope_key=?1",
            [&other.scope_ref],
            |r| r.get(0),
        )
        .unwrap();
    let result = invalidate_source_in(&tx, "run_input", &a.source, Some(1), AT).unwrap();
    assert_eq!(result.affected_run_ids, vec![a.consumer]);
    assert_eq!(status(&tx, "other-consumer"), "running");
    assert!(state::validate_context_at_in(&tx, "other-consumer", AT).unwrap());
    let after: i64 = tx
        .query_row(
            "SELECT epoch FROM conversation_state_heads WHERE scope_key=?1",
            [&other.scope_ref],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        before, after,
        "a source edit must not invalidate its entire scope epoch"
    );
    assert_eq!(
        tx.query_row(
            "SELECT messages_json FROM run_prompt_snapshots WHERE run_id='other-consumer'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "unrelated-frozen-prompt"
    );
    assert_eq!(
        tx.query_row("SELECT count(*) FROM native_worker_leases", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
}
#[test]
fn revision_bound_and_conversation_scope_are_precise_and_transactional() {
    let mut db = database();
    let a = cohort(&mut db, "A", "qq:synthetic:c2c:a");
    let b = cohort(&mut db, "B", "qq:synthetic:c2c:b");
    {
        let tx = db.transaction().unwrap();
        let result = invalidate_source_in(&tx, "run_input", &a.source, Some(0), AT).unwrap();
        assert!(result.affected_run_ids.is_empty());
        assert_eq!(status(&tx, &a.consumer), "running");
        tx.commit().unwrap();
    }
    {
        let tx = db.transaction().unwrap();
        let result = invalidate_conversation_in(&tx, &a.owner, &a.conversation, AT).unwrap();
        assert_eq!(result.affected_run_ids, vec![a.consumer.clone()]);
        assert_eq!(status(&tx, &b.consumer), "running");
        tx.rollback().unwrap();
    }
    assert_eq!(status(&db, &a.consumer), "running");
    assert!(db
        .query_row(
            "SELECT intent!='' FROM conversation_commitments WHERE id=?1",
            [&a.commitment],
            |r| r.get::<_, bool>(0)
        )
        .unwrap());
    let tx = db.transaction().unwrap();
    let result = invalidate_conversation_in(&tx, &b.owner, &a.conversation, AT).unwrap();
    assert!(result.affected_run_ids.is_empty());
    assert_eq!(status(&tx, &a.consumer), "running");
}
#[test]
fn partial_delivery_preserves_claimed_sent_unknown_and_all_physical_leases() {
    let mut db = database();
    let a = cohort(&mut db, "A", "qq:synthetic:c2c:a");
    db.execute(
        "UPDATE runs SET status='completed',response='historical generated response' WHERE id=?1",
        [&a.consumer],
    )
    .unwrap();
    qq(&db, &a.consumer, "unknown");
    type PartSnapshot = (i64, String, Option<String>, Option<String>, Option<i64>);
    let original: Vec<PartSnapshot> = {
        let mut s=db.prepare("SELECT seq,status,platform_message_id,error,finished_ms FROM outbox_reply_parts WHERE seq!=3 ORDER BY seq").unwrap();
        let rows = s
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        rows
    };
    let tx = db.transaction().unwrap();
    let result = invalidate_source_in(&tx, "run_input", &a.source, None, AT).unwrap();
    assert_eq!(result.cancelled_runs, 0);
    assert_eq!(result.cancelled_qq_parts, 1);
    assert_eq!(result.cancelled_qq_outboxes, 0);
    assert_eq!(status(&tx, &a.consumer), "completed");
    assert_eq!(
        tx.query_row(
            "SELECT status FROM outbox WHERE id=?1",
            [&a.consumer],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "unknown"
    );
    let mut s=tx.prepare("SELECT seq,status,platform_message_id,error,finished_ms FROM outbox_reply_parts WHERE seq!=3 ORDER BY seq").unwrap();
    let after = s
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(original, after);
    assert_eq!(
        tx.query_row(
            "SELECT status FROM outbox_reply_parts WHERE seq=3",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "cancelled"
    );
    assert_eq!(
        tx.query_row("SELECT count(*) FROM native_worker_leases", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        tx.query_row("SELECT request_digest FROM diary_model_leases", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        "physical-request-still-owned"
    );
    assert_eq!(
        tx.query_row(
            "SELECT decision FROM run_publication_decisions WHERE run_id=?1",
            [&a.consumer],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "suppressed"
    );
}
#[test]
fn queued_parent_and_weixin_pending_only_are_cancelled_without_retries() {
    let mut db = database();
    let a = cohort(&mut db, "A", "qq:synthetic:c2c:a");
    qq(&db, &a.consumer, "queued");
    let w = cohort(&mut db, "W", "weixin:synthetic:c2c:owner");
    for id in ["inflight-run", "unknown-run", "accepted-run"] {
        additional_consumer(&mut db, &w, id);
    }
    db.execute(
        "INSERT INTO weixin_outbox VALUES('pending',?1,'unsent-content','queued',NULL,NULL,0)",
        [&w.consumer],
    )
    .unwrap();
    db.execute("INSERT INTO weixin_outbox VALUES('inflight','inflight-run','possible-inflight-content','sending',NULL,NULL,0)",[]).unwrap();
    db.execute("INSERT INTO weixin_outbox VALUES('unknown','unknown-run','unknown-content','unknown','original_unknown',NULL,0)",[]).unwrap();
    db.execute("INSERT INTO weixin_outbox VALUES('accepted','accepted-run','accepted-content','accepted',NULL,'original-accepted-id',0)",[]).unwrap();
    let tx = db.transaction().unwrap();
    let q = invalidate_owner_in(&tx, &a.owner, AT).unwrap();
    assert_eq!(q.cancelled_qq_outboxes, 1);
    assert_eq!(status(&tx, &w.consumer), "running");
    let result = invalidate_owner_in(&tx, &w.owner, AT).unwrap();
    assert_eq!(result.cancelled_weixin_outboxes, 1);
    for (id, expected) in [
        ("pending", "cancelled"),
        ("inflight", "sending"),
        ("unknown", "unknown"),
        ("accepted", "accepted"),
    ] {
        assert_eq!(
            tx.query_row("SELECT status FROM weixin_outbox WHERE id=?1", [id], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            expected
        );
    }
    assert!(tx
        .query_row(
            "SELECT content IS NULL FROM weixin_outbox WHERE id='pending'",
            [],
            |r| r.get::<_, bool>(0)
        )
        .unwrap());
    assert_eq!(
        tx.query_row(
            "SELECT server_message_id FROM weixin_outbox WHERE id='accepted'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "original-accepted-id"
    );
    assert_eq!(
        tx.query_row("SELECT count(*) FROM weixin_outbox", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        4
    );
}
#[test]
fn absent_feature_is_legacy_but_partial_tables_or_columns_fail_closed() {
    let legacy = Connection::open_in_memory().unwrap();
    legacy.execute_batch(include_str!("../schema.sql")).unwrap();
    assert!(!initialized_schema(&legacy).unwrap());
    assert!(
        invalidate_source_in(&legacy, "memory", "old", None, AT)
            .unwrap()
            .legacy_schema
    );
    legacy
        .execute("CREATE TABLE conversation_state_heads(scope_key TEXT)", [])
        .unwrap();
    assert!(initialized_schema(&legacy).is_err());
    assert!(invalidate_owner_in(&legacy, "owner", AT).is_err());
    let mut db = database();
    let a = cohort(&mut db, "A", "qq:synthetic:c2c:a");
    db.execute(
        "ALTER TABLE conversation_commitment_usage DROP COLUMN digest",
        [],
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    assert!(invalidate_source_in(&tx, "run_input", &a.source, None, AT).is_err());
    assert_eq!(status(&tx, &a.consumer), "running");
    assert!(tx
        .query_row(
            "SELECT content_json IS NOT NULL FROM conversation_state_entities WHERE id=?1",
            [format!("focus:{}", a.source)],
            |r| r.get::<_, bool>(0)
        )
        .unwrap());
}
#[test]
fn invalidation_tombstones_and_cleared_bodies_survive_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("invalidation.sqlite");
    let a;
    {
        let mut db = Connection::open(&path).unwrap();
        schema(&db);
        a = cohort(&mut db, "A", "qq:synthetic:c2c:a");
        let tx = db.transaction().unwrap();
        invalidate_source_in(&tx, "run_input", &a.source, None, AT).unwrap();
        tx.commit().unwrap();
    }
    let mut db = Connection::open(&path).unwrap();
    state::initialize(&db).unwrap();
    commitments::ensure_schema(&db).unwrap();
    let tx = db.transaction().unwrap();
    assert!(focus::context_for_run_in(&tx, &a.consumer, AT + 1)
        .unwrap()
        .current_focus
        .is_none());
    assert!(
        commitments::context_for_run_in(&tx, &a.consumer, 4, 12000, AT + 1)
            .unwrap()
            .items
            .is_empty()
    );
    let repeated = invalidate_source_in(&tx, "run_input", &a.source, None, AT + 1).unwrap();
    assert_eq!(repeated.cancelled_runs, 0);
    assert_eq!(
        tx.query_row(
            "SELECT count(*) FROM conversation_commitments WHERE intent LIKE '%CANARY_A_PRIVATE%'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(tx.query_row("SELECT count(*) FROM conversation_state_entities WHERE content_json LIKE '%CANARY_A_PRIVATE%'",[],|r|r.get::<_,i64>(0)).unwrap(),0);
}
