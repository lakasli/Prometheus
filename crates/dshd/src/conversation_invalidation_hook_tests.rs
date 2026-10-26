//! Real domain mutation hooks, with temporary app storage and no network.
use crate::{
    conversation_commitments as commitments, conversation_identity, conversation_state, diary,
    memory, relationships,
    state::{now, AppState, Options},
    turn_control,
};
use rusqlite::{params, Connection};
use serde_json::json;

fn fixture() -> (tempfile::TempDir, AppState) {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open(Options {
        data_dir: directory.path().into(),
        origin: "http://localhost".into(),
        allow_private_model: false,
        cookie_secure: false,
        web_dir: directory.path().join("web"),
    })
    .unwrap();
    state.db(|db|{db.execute_batch("INSERT INTO conversations(id,owner_id,title,created_at) VALUES('a','owner-a','Synthetic A',0),('b','owner-b','Synthetic B',0);")?;Ok(())}).unwrap();
    (directory, state)
}
fn consumer(
    db: &mut Connection,
    id: &str,
    conversation: &str,
    owner: &str,
    source: Option<conversation_state::SourceRef>,
) -> String {
    let tx = db.transaction().unwrap();
    let at = now();
    let input = format!("请整理当前授权资料 {id}");
    tx.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,created_at,updated_at,run_kind) VALUES(?1,?2,?3,?1,?4,'{}','running',?5,?5,'model')",params![id,conversation,owner,input,at]).unwrap();
    tx.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES(?1,?2,?1,'user',?3,?4)",params![id,conversation,input,at]).unwrap();
    conversation_identity::freeze_run_in(&tx, id).unwrap();
    turn_control::register_input_in(&tx, id, id, &input, owner, true, at * 1000).unwrap();
    let scope = conversation_state::scope_for_run_in(&tx, id).unwrap();
    let value = commitments::propose_in(
        &tx,
        &scope,
        &commitments::Proposal {
            intent: format!("CANARY_DERIVED_{id}"),
            kind: "reading".into(),
            time_text: None,
            expires_at: None,
            sources: source.into_iter().collect(),
        },
        commitments::ProposalOrigin::ModelSuggestion,
        "synthetic-proposal",
        at,
    )
    .unwrap();
    let context = commitments::context_for_run_in(&tx, id, 4, 12000, at).unwrap();
    commitments::record_usage_in(&tx, id, &context.source_refs, at).unwrap();
    tx.execute("INSERT INTO run_prompt_snapshots(run_id,messages_json,digest,compiler_version,created_at) VALUES(?1,'synthetic frozen private context','synthetic-digest','hook-test',?2)",params![id,at]).unwrap();
    tx.execute(
        "INSERT INTO native_worker_leases VALUES(?1,?2)",
        params![id, at],
    )
    .unwrap();
    tx.commit().unwrap();
    value["id"].as_str().unwrap().into()
}
fn status(db: &Connection, id: &str) -> String {
    db.query_row("SELECT status FROM runs WHERE id=?1", [id], |r| r.get(0))
        .unwrap()
}
fn check_a_only(db: &Connection, a: &str, b: &str, a_commitment: &str, b_commitment: &str) {
    assert_eq!(status(db, a), "cancelled");
    assert_eq!(status(db, b), "running");
    assert_eq!(
        db.query_row(
            "SELECT intent FROM conversation_commitments WHERE id=?1",
            [a_commitment],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        ""
    );
    assert!(!db
        .query_row(
            "SELECT intent FROM conversation_commitments WHERE id=?1",
            [b_commitment],
            |r| r.get::<_, String>(0)
        )
        .unwrap()
        .is_empty());
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM run_prompt_snapshots WHERE run_id=?1",
            [a],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM run_prompt_snapshots WHERE run_id=?1",
            [b],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM native_worker_leases WHERE run_id IN(?1,?2)",
            params![a, b],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
}
#[test]
fn memory_forgetting_calls_the_exact_new_state_hook_and_retains_physical_leases() {
    let (_directory, state) = fixture();
    state
        .db(|db| {
            let record = memory::create(
                db,
                "synthetic-admin",
                &memory::CreateMemory {
                    content: "PRIVATE_ORIGINAL_MEMORY".into(),
                    visibility: memory::Visibility::Private,
                    source_conversation_id: Some("a".into()),
                    source_message_id: None,
                    publish: false,
                },
            )
            .unwrap();
            let source = conversation_state::SourceRef {
                kind: "memory".into(),
                id: record.id.clone(),
                revision: record.revision,
                epoch: None,
                digest: None,
            };
            let a = consumer(db, "a-consumer", "a", "owner-a", Some(source));
            let b = consumer(db, "b-consumer", "b", "owner-b", None);
            memory::forget(db, "synthetic-admin", &record.id, record.revision).unwrap();
            check_a_only(db, "a-consumer", "b-consumer", &a, &b);
            assert!(db
                .query_row(
                    "SELECT content FROM messages WHERE run_id='a-consumer' AND role='user'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap()
                .contains("a-consumer"));
            Ok(())
        })
        .unwrap();
}
#[test]
fn changing_a_derived_diary_revision_clears_actual_consumers_without_advancing_history_floor() {
    let (_directory, state) = fixture();
    state
        .db(|db| {
            diary::save_policy(
                db,
                "synthetic-admin",
                0,
                &diary::SourcePolicy {
                    source_owner_id: "owner-a".into(),
                    enabled: true,
                    allow_model: true,
                    timezone: diary::TIMEZONE.into(),
                },
            )
            .unwrap();
            diary::note(
                db,
                "synthetic-admin",
                &diary::Note {
                    content: "今天完成了有来源的合成阅读".into(),
                    source_conversation_id: Some("a".into()),
                    visibility: "private".into(),
                    publish: false,
                    happened_at: Some(now()),
                },
            )
            .unwrap();
            let day = chrono::DateTime::<chrono::Utc>::from_timestamp(now(), 0)
                .unwrap()
                .with_timezone(&chrono_tz::Asia::Shanghai)
                .date_naive()
                .to_string();
            let entry = {
                let tx = db.transaction().unwrap();
                let entry = diary::consolidate_in(
                    &tx,
                    "private",
                    Some("owner-a"),
                    "day",
                    &day,
                    diary::TIMEZONE,
                    now(),
                )
                .unwrap();
                tx.commit().unwrap();
                entry
            };
            let source = conversation_state::SourceRef {
                kind: "diary_entry".into(),
                id: entry.id.clone(),
                revision: entry.revision,
                epoch: None,
                digest: None,
            };
            let a = consumer(db, "diary-a", "a", "owner-a", Some(source));
            let b = consumer(db, "diary-b", "b", "owner-b", None);
            let before: i64 = db
                .query_row("SELECT count(*) FROM memory_context_watermarks", [], |r| {
                    r.get(0)
                })
                .unwrap();
            let tx = db.transaction().unwrap();
            let job = diary::prepare_generation_in(&tx).unwrap().unwrap();
            assert_eq!(job.entry_id, entry.id);
            diary::commit_generation_in(
                &tx,
                &job,
                &diary::GenerationOutput {
                    facts: "我整理了当天实际记录的阅读经历。".into(),
                    feelings: "这段交流很平静。".into(),
                },
                "synthetic-provider",
            )
            .unwrap();
            tx.commit().unwrap();
            check_a_only(db, "diary-a", "diary-b", &a, &b);
            let after: i64 = db
                .query_row("SELECT count(*) FROM memory_context_watermarks", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(before, after);
            Ok(())
        })
        .unwrap();
}
#[test]
fn forgetting_an_approved_person_assertion_invalidates_its_actual_new_state_consumers() {
    let (_directory, state) = fixture();
    state
        .db(|db| {
            let person = relationships::create(
                db,
                relationships::Access::Admin("synthetic-admin"),
                &relationships::CreatePerson {
                    display_name: "合成人物".into(),
                    scope: relationships::ScopeInput {
                        visibility: "private".into(),
                        source_conversation_id: Some("a".into()),
                        publish: false,
                    },
                    reason: "合成授权来源".into(),
                },
            )
            .unwrap();
            let assertion = relationships::propose_assertion(
                db,
                relationships::Access::Admin("synthetic-admin"),
                &person.id,
                person.revision,
                &relationships::CreateAssertion {
                    field: "hobby".into(),
                    value: json!("合成偏好"),
                    evidence: relationships::Evidence {
                        source_type: "manual".into(),
                        source_id: None,
                        source_revision: None,
                        statement_kind: "self_report".into(),
                        reason: "本人合成陈述".into(),
                    },
                    valid_from: None,
                    valid_until: None,
                },
            )
            .unwrap();
            let assertion = relationships::review_assertion(
                db,
                relationships::Access::Admin("synthetic-admin"),
                &assertion.id,
                assertion.revision,
                &relationships::ReviewAssertion {
                    decision: "approve".into(),
                    supersedes: Vec::new(),
                    reason: "合成审核".into(),
                },
            )
            .unwrap();
            let source = conversation_state::SourceRef {
                kind: "person_assertion".into(),
                id: assertion.id.clone(),
                revision: assertion.revision,
                epoch: None,
                digest: None,
            };
            let a = consumer(db, "person-a", "a", "owner-a", Some(source));
            let b = consumer(db, "person-b", "b", "owner-b", None);
            relationships::forget_assertion(
                db,
                relationships::Access::Admin("synthetic-admin"),
                &assertion.id,
                assertion.revision,
            )
            .unwrap();
            check_a_only(db, "person-a", "person-b", &a, &b);
            Ok(())
        })
        .unwrap();
}
