//! Compatibility commands must obey the canonical diary's persistent revocation.
use super::*;
use crate::{
    diary, memory_commands,
    state::{AppState, Options},
    worker,
};
use tempfile::TempDir;

const OWNER: &str = "qq:synthetic:c2c:owner";
const CANARY: &str = "REVOKED_COMPATIBILITY_CANARY";
fn options(root: &std::path::Path) -> Options {
    Options {
        data_dir: root.to_owned(),
        origin: "http://127.0.0.1:18571".into(),
        allow_private_model: false,
        cookie_secure: false,
        web_dir: root.join("web"),
    }
}
fn command(state: &AppState, text: &str) -> String {
    let id = worker::enqueue(
        state,
        "source-chat",
        OWNER,
        &uuid::Uuid::new_v4().to_string(),
        text,
        None,
    )
    .unwrap();
    state
        .db(|db| {
            db.execute("UPDATE runs SET status='running' WHERE id=?1", [&id])?;
            Ok(())
        })
        .unwrap();
    assert!(memory_commands::execute(state, &id).unwrap());
    state
        .db(|db| Ok(db.query_row("SELECT response FROM runs WHERE id=?1", [&id], |r| r.get(0))?))
        .unwrap()
}
fn policy(state: &AppState, expected: i64, enabled: bool) {
    state
        .db(|db| {
            Ok(diary::save_policy(
                db,
                "synthetic-admin",
                expected,
                &diary::SourcePolicy {
                    source_owner_id: OWNER.into(),
                    enabled,
                    allow_model: false,
                    timezone: diary::TIMEZONE.into(),
                },
            ))
        })
        .unwrap()
        .unwrap();
}

#[test]
fn remember_revoke_commands_restart_and_reenable_never_resurrect_old_source() {
    let temp = TempDir::new().unwrap();
    let state = AppState::open(options(temp.path())).unwrap();
    state.db(|db|{db.execute("INSERT INTO conversations(id,owner_id,title,created_at) VALUES('source-chat',?1,'synthetic',?2)",params![OWNER,now()])?;Ok(())}).unwrap();
    policy(&state, 0, true);
    let remembered = command(&state, &format!("/memory remember {CANARY}"));
    assert!(remembered.contains("已记住"));
    let id: String = state
        .db(|db| {
            Ok(db.query_row(
                "SELECT id FROM memory_records WHERE source_owner_id=?1",
                [OWNER],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert!(command(&state, &format!("/memory show {id}")).contains(CANARY));
    assert!(command(&state, "/memory list").contains(CANARY));
    policy(&state, 1, false);
    assert!(!command(&state, &format!("/memory show {id}")).contains(CANARY));
    assert!(!command(&state, "/memory list").contains(CANARY));
    state
        .db(|db| {
            assert_eq!(
                visible_list(db, OWNER, &ListOptions::default())
                    .unwrap()
                    .total,
                0
            );
            assert!(get_visible(db, OWNER, &id).is_err());
            assert!(get(db, &id).unwrap().content.contains(CANARY));
            assert_eq!(admin_list(db, &ListOptions::default()).unwrap().total, 1);
            Ok(())
        })
        .unwrap();
    drop(state);
    let state = AppState::open(options(temp.path())).unwrap();
    assert!(!command(&state, &format!("/memory show {id}")).contains(CANARY));
    assert!(!command(&state, "/memory list").contains(CANARY));
    policy(&state, 2, true);
    assert!(!command(&state, &format!("/memory show {id}")).contains(CANARY));
    assert!(!command(&state, "/memory list").contains(CANARY));
    command(&state, "/memory remember AFTER_REAUTHORIZATION_CANARY");
    let list = command(&state, "/memory list");
    assert!(list.contains("AFTER_REAUTHORIZATION_CANARY"));
    assert!(!list.contains(CANARY));
    state
        .db(|db| {
            let page = visible_list(
                db,
                OWNER,
                &ListOptions {
                    limit: 1,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(page.total, 1);
            assert_eq!(page.items.len(), 1);
            assert_eq!(admin_list(db, &ListOptions::default()).unwrap().total, 2);
            assert!(export(db).unwrap().to_string().contains(CANARY));
            Ok(())
        })
        .unwrap();
}

#[test]
fn canonical_projection_mismatch_is_excluded_before_pagination_and_total() {
    let temp = TempDir::new().unwrap();
    let state = AppState::open(options(temp.path())).unwrap();
    state.db(|db|{db.execute("INSERT INTO conversations(id,owner_id,title,created_at) VALUES('source-chat',?1,'synthetic',?2)",params![OWNER,now()])?;Ok(())}).unwrap();
    command(&state, "/memory remember VALID_COMPATIBILITY_CANARY");
    command(&state, "/memory remember MISMATCHED_COMPATIBILITY_CANARY");
    state
        .db(|db| {
            let id: String = db.query_row(
                "SELECT id FROM memory_records WHERE content='MISMATCHED_COMPATIBILITY_CANARY'",
                [],
                |r| r.get(0),
            )?;
            db.execute(
                "UPDATE diary_events SET revision=revision+1 WHERE id=?1",
                [format!("legacy:{id}")],
            )?;
            assert!(get_visible(db, OWNER, &id).is_err());
            let page = visible_list(
                db,
                OWNER,
                &ListOptions {
                    limit: 1,
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(page.total, 1);
            assert_eq!(page.items.len(), 1);
            assert_eq!(page.items[0].content, "VALID_COMPATIBILITY_CANARY");
            assert_eq!(admin_list(db, &ListOptions::default()).unwrap().total, 2);
            Ok(())
        })
        .unwrap();
}

#[test]
fn legacy_schema_without_diary_stays_compatible_but_present_incomplete_schema_fails_closed() {
    let mut db = Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("../schema.sql")).unwrap();
    initialize(&mut db).unwrap();
    let item = create(
        &mut db,
        "synthetic-admin",
        &CreateMemory {
            content: "legacy-only".into(),
            visibility: Visibility::Shared,
            source_conversation_id: None,
            source_message_id: None,
            publish: true,
        },
    )
    .unwrap();
    assert!(get_visible(&db, OWNER, &item.id).is_ok());
    assert_eq!(
        visible_list(&db, OWNER, &ListOptions::default())
            .unwrap()
            .total,
        1
    );
    db.execute_batch("CREATE TABLE diary_events(id TEXT PRIMARY KEY)")
        .unwrap();
    assert!(get_visible(&db, OWNER, &item.id).is_err());
    assert!(visible_list(&db, OWNER, &ListOptions::default()).is_err());
    assert!(get(&db, &item.id).is_ok());
}
