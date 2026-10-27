use super::*;
use crate::auth::Admin;

fn fixture() -> (tempfile::TempDir, AppState) {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open(crate::state::Options {
        data_dir: directory.path().into(),
        origin: "http://localhost".into(),
        allow_private_model: false,
        cookie_secure: false,
        web_dir: directory.path().join("web"),
    })
    .unwrap();
    state.db(|db|{
        db.execute("INSERT INTO admins VALUES('admin','rest_runtime_admin','unused',0)",[])?;
        db.execute("INSERT INTO conversations(id,owner_id,title,created_at)VALUES('chat','admin','synthetic only',?1)",[now()])?;
        db.execute("UPDATE trigger_runtime SET clock_trusted=1,space_ok=1 WHERE id=1",[])?;
        let tx=db.transaction()?;
        let a=admin_in(&tx)?;
        let p=rest::RestPolicy{natural_enabled:true,default_sleep_seconds:Some(600),queue_limit:1,..Default::default()};
        rest::save_policy_in(&tx,&a,1,&p,now())?;
        tx.commit()?;Ok(())
    }).unwrap();
    (directory, state)
}
fn admin_in(db: &Connection) -> anyhow::Result<rest::VerifiedRequester> {
    rest::requester_for_admin_in(
        db,
        &Admin {
            id: "admin".into(),
            username: "rest_runtime_admin".into(),
            role: "admin".into(),
            csrf_token: String::new(),
        },
    )
}
fn input(state: &AppState, text: &str) -> String {
    crate::worker::enqueue(
        state,
        "chat",
        "admin",
        &uuid::Uuid::new_v4().to_string(),
        text,
        None,
    )
    .unwrap()
}
fn grant(state: &AppState) {
    let seed = input(state, "你好");
    state
        .db(|db| {
            let tx = db.transaction()?;
            let a = admin_in(&tx)?;
            let source = rest::requester_for_run_in(&tx, &seed)?;
            rest::set_source_grant_in(
                &tx,
                &a,
                &source,
                0,
                &rest::GrantPermissions {
                    can_sleep: true,
                    can_wake: true,
                    valid_until: now() + 3600,
                },
                now(),
            )?;
            tx.execute(
                "UPDATE runs SET status='completed',response='synthetic greeting' WHERE id=?1",
                [seed],
            )?;
            tx.commit()?;
            Ok(())
        })
        .unwrap();
}
fn freeze_cache_context(state: &AppState, run: &str) {
    state
        .db(|db| {
            let tx = db.transaction()?;
            tx.execute("UPDATE runs SET status='running' WHERE id=?1", [run])?;
            crate::conversation_identity::context_in(&tx, run, "chat", "admin")?;
            tx.commit()?;
            Ok(())
        })
        .unwrap();
}
fn scalar(state: &AppState, sql: &str) -> i64 {
    state
        .db(|db| Ok(db.query_row(sql, [], |r| r.get(0))?))
        .unwrap()
}

#[test]
fn direct_control_uses_no_model_slot_and_wake_keeps_existing_physical_lease() {
    let (_directory, state) = fixture();
    grant(&state);
    let active = input(&state, "正在处理的普通请求");
    state
        .db(|db| {
            db.execute("UPDATE runs SET status='running' WHERE id=?1", [&active])?;
            db.execute(
                "INSERT INTO native_worker_leases VALUES(?1,?2)",
                params![active, now()],
            )?;
            Ok(())
        })
        .unwrap();
    let sleep = input(&state, "快去睡觉啦");
    tick(&state).unwrap();
    state
        .db(|db| {
            let wake = rest::state_in(db)?.wake_at.unwrap();
            let content: String =
                db.query_row("SELECT response FROM runs WHERE id=?1", [&sleep], |r| {
                    r.get(0)
                })?;
            let expected = chrono::DateTime::from_timestamp(wake, 0)
                .unwrap()
                .with_timezone(&chrono_tz::Asia::Shanghai)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string();
            assert!(content.contains(&expected) && content.contains("Asia/Shanghai"));
            Ok(())
        })
        .unwrap();
    assert_eq!(
        state.db(|db| Ok(rest::state_in(db)?.phase)).unwrap(),
        rest::RestPhase::SleepPending
    );
    assert_eq!(
        scalar(&state, "SELECT count(*) FROM run_prompt_snapshots"),
        0
    );
    assert_eq!(scalar(&state, "SELECT count(*) FROM rest_operations"), 1);
    assert_eq!(
        scalar(
            &state,
            "SELECT count(*) FROM rest_ack_permits WHERE used_at IS NOT NULL"
        ),
        1
    );
    assert_eq!(
        state
            .db(|db| Ok(db
                .query_row("SELECT status FROM runs WHERE id=?1", [sleep], |r| r
                    .get::<_, String>(0))?))
            .unwrap(),
        "completed"
    );
    input(&state, "醒醒");
    state
        .db(|db| {
            db.execute(
                "UPDATE trigger_runtime SET clock_trusted=0,space_ok=0 WHERE id=1",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    tick(&state).unwrap();
    assert_eq!(
        state.db(|db| Ok(rest::state_in(db)?.phase)).unwrap(),
        rest::RestPhase::Awake
    );
    assert_eq!(
        scalar(&state, "SELECT count(*) FROM native_worker_leases"),
        1
    );
    assert_eq!(
        scalar(&state, "SELECT count(*) FROM runs WHERE status='running'"),
        1
    );
}

#[test]
fn no_grant_or_late_controls_do_not_change_state_or_request_a_model() {
    let (_directory, state) = fixture();
    input(&state, "快去睡觉啦");
    tick(&state).unwrap();
    assert_eq!(scalar(&state, "SELECT count(*) FROM rest_operations"), 0);
    assert_eq!(
        scalar(
            &state,
            "SELECT count(*) FROM rest_control_candidates WHERE status='rejected'"
        ),
        1
    );
    grant(&state);
    let late = input(&state, "快去睡觉啦");
    state
        .db(|db| {
            db.execute(
                "UPDATE rest_control_candidates SET received_at=?1 WHERE run_id=?2",
                params![now() - 121, late],
            )?;
            Ok(())
        })
        .unwrap();
    tick(&state).unwrap();
    assert_eq!(scalar(&state, "SELECT count(*) FROM rest_operations"), 0);
    assert_eq!(
        scalar(&state, "SELECT count(*) FROM run_prompt_snapshots"),
        0
    );
    assert_eq!(
        state.db(|db| Ok(rest::state_in(db)?.phase)).unwrap(),
        rest::RestPhase::Awake
    );
    input(&state, "他说‘去睡觉吧’，我只是转述。");
    input(&state, "晚安");
    assert_eq!(
        scalar(&state, "SELECT count(*) FROM rest_control_candidates"),
        2
    );
    assert!(!crate::qq::text_batch::is_eligible(
        "c2c", "醒醒", false, "model"
    ));
}

#[test]
fn sleeping_overflow_rejects_execution_but_retains_original_messages() {
    let (_directory, state) = fixture();
    grant(&state);
    input(&state, "快去睡觉啦");
    tick(&state).unwrap();
    let first = input(&state, "睡眠期间第一条");
    let second = input(&state, "睡眠期间第二条");
    state
        .db(|db| {
            assert_eq!(
                db.query_row("SELECT status FROM runs WHERE id=?1", [first], |r| r
                    .get::<_, String>(0))?,
                "queued"
            );
            assert_eq!(
                db.query_row("SELECT status FROM runs WHERE id=?1", [second], |r| r
                    .get::<_, String>(0))?,
                "failed"
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(
        scalar(
            &state,
            "SELECT count(*) FROM messages WHERE role='user' AND content LIKE '睡眠期间%'"
        ),
        2
    );
    input(&state, "醒醒");
    tick(&state).unwrap();
    assert_eq!(
        state.db(|db| Ok(rest::state_in(db)?.phase)).unwrap(),
        rest::RestPhase::Awake
    );
}

#[test]
fn deferred_result_is_queued_without_a_visible_reply_and_reuses_completed_plan() {
    let (_directory, state) = fixture();
    grant(&state);
    let run = input(&state, "普通问题");
    state
        .db(|db| {
            db.execute("UPDATE runs SET status='running' WHERE id=?1", [&run])?;
            db.execute(
                "INSERT INTO native_worker_leases VALUES(?1,?2)",
                params![run, now()],
            )?;
            Ok(())
        })
        .unwrap();
    let raw = r#"[{"type":"text","text":"完整回答。","delay_ms":0}]"#;
    freeze_cache_context(&state, &run);
    assert!(cache_generation(&state, &run, raw).unwrap());
    assert!(cache_generation(&state, &run, "different generation").is_err());
    input(&state, "快去睡觉啦");
    tick(&state).unwrap();
    let plan = crate::chat_reply::parse(raw, &Default::default()).unwrap();
    assert!(!checkpoint_plan(&state, &run, &plan).unwrap());
    state
        .db(|db| {
            assert_eq!(cached_generation_in(db, &run)?.as_deref(), Some(raw));
            assert_eq!(cached_plan_in(db, &run)?.unwrap().readable(), "完整回答。");
            assert_eq!(
                db.query_row("SELECT status FROM runs WHERE id=?1", [&run], |r| r
                    .get::<_, String>(0))?,
                "queued"
            );
            assert!(!db.query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE run_id=?1 AND role='assistant')",
                [&run],
                |r| r.get::<_, bool>(0)
            )?);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        scalar(&state, "SELECT count(*) FROM native_worker_leases"),
        1
    );
}

#[test]
fn restart_resumes_completed_checkpoint_but_never_replays_unknown_media() {
    let (_directory, state) = fixture();
    let complete = input(&state, "已完成模型");
    let unknown = input(&state, "媒体未确认");
    for run in [&complete, &unknown] {
        freeze_cache_context(&state, run);
        assert!(cache_generation(&state, run, "synthetic only").unwrap());
    }
    begin_media_stage(&state, &unknown).unwrap();
    state
        .db(|db| {
            db.execute(
                "UPDATE runs SET status='interrupted' WHERE id IN(?1,?2)",
                params![complete, unknown],
            )?;
            Ok(())
        })
        .unwrap();
    recover_after_confirmed_exit(&state).unwrap();
    state
        .db(|db| {
            assert_eq!(
                db.query_row("SELECT status FROM runs WHERE id=?1", [complete], |r| {
                    r.get::<_, String>(0)
                })?,
                "queued"
            );
            assert_eq!(
                db.query_row("SELECT status FROM runs WHERE id=?1", [unknown], |r| r
                    .get::<_, String>(
                    0
                ))?,
                "failed"
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn absent_schema_is_legacy_but_partial_runtime_is_an_error() {
    let db = Connection::open_in_memory().unwrap();
    assert!(automatic_allowed_in(&db).unwrap());
    db.execute_batch("CREATE TABLE rest_meta(id INTEGER);")
        .unwrap();
    assert!(automatic_allowed_in(&db).is_err());
}

#[test]
fn startup_never_repairs_partial_rest_into_awake_or_clears_its_state() {
    let (_directory, state) = fixture();
    grant(&state);
    input(&state, "快去睡觉啦");
    tick(&state).unwrap();
    state
        .db(|db| {
            let original: String =
                db.query_row("SELECT phase FROM assistant_rest_states", [], |r| r.get(0))?;
            db.execute_batch("DROP TABLE rest_ack_permits")?;
            assert!(initialize_all(db).is_err());
            assert_eq!(
                db.query_row("SELECT phase FROM assistant_rest_states", [], |r| r
                    .get::<_, String>(0))?,
                original
            );
            assert!(!db.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='rest_ack_permits')",
                [],
                |r| r.get::<_, bool>(0)
            )?);
            assert_eq!(
                db.query_row("SELECT count(*) FROM rest_cycles", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .unwrap();
}
