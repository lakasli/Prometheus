use super::*;
use crate::{auth::Admin, state::Options};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

fn fixture() -> (tempfile::TempDir, AppState) {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(Options {
        data_dir: dir.path().into(),
        origin: "http://127.0.0.1:18271".into(),
        allow_private_model: true,
        cookie_secure: false,
        web_dir: dir.path().join("web"),
    })
    .unwrap();
    state.db(|db|{db.execute("INSERT INTO admins VALUES('rest-admin','rest-synthetic-admin','unused',0)",[])?;db.execute("INSERT INTO conversations VALUES('rest-a','owner-a','Synthetic A',NULL,0),('rest-b','owner-b','Synthetic B',NULL,0),('rest-a2','owner-a','Synthetic A2',NULL,0)",[])?;db.execute("UPDATE trigger_runtime SET clock_trusted=1,space_ok=1 WHERE id=1",[])?;rest_store::initialize(db)?;Ok(())}).unwrap();
    (dir, state)
}
fn admin(db: &Connection) -> rest_store::VerifiedRequester {
    rest_store::requester_for_admin_in(
        db,
        &Admin {
            id: "rest-admin".into(),
            username: "rest-synthetic-admin".into(),
            role: "admin".into(),
            csrf_token: String::new(),
        },
    )
    .unwrap()
}
fn insert_event(
    state: &AppState,
    id: &str,
    owner: &str,
    conversation: &str,
    content: &str,
    happened: i64,
) {
    state.db(|db|{db.execute("INSERT OR IGNORE INTO diary_source_policies VALUES(?1,1,1,1,'Asia/Shanghai',?2,?2)",params![owner,now()-100])?;db.execute("INSERT INTO diary_events(id,assistant_id,revision,visibility,source_owner_id,source_conversation_id,origin_type,origin_id,happened_at,observed_at,content,assertion_kind) VALUES(?1,?2,1,'private',?3,?4,'explicit_note',?1,?5,?6,?7,'self_report')",params![id,crate::memory::identity(db).map_err(|_|anyhow::anyhow!("identity"))?,owner,conversation,happened,now(),content])?;Ok(())}).unwrap();
}
fn start(state: &AppState) -> String {
    state
        .db(|db| {
            let tx = db.transaction()?;
            let requester = admin(&tx);
            let policy = rest_store::RestPolicy {
                default_sleep_seconds: Some(200),
                drain_seconds: 0,
                consolidation_seconds: 150,
                max_cycle_calls: 16,
                max_cycle_reserved_tokens: 96000,
                ..Default::default()
            };
            rest_store::save_policy_in(&tx, &requester, 1, &policy, now())?;
            let clock = crate::rest_clock::sample_in(&tx)?;
            let result = rest_store::sleep_in(
                &tx,
                &requester,
                0,
                "synthetic-rest-cycle",
                &Default::default(),
                &clock,
            )?;
            let _ = rest_store::advance_in(&tx, &clock)?;
            tx.commit()?;
            Ok(result.cycle_id.unwrap())
        })
        .unwrap()
}
async fn provider(
    delay: Duration,
    bad: bool,
) -> (
    String,
    Arc<AtomicUsize>,
    Arc<Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let counts = count.clone();
    let payloads = seen.clone();
    let router=axum::Router::new().route("/chat/completions",axum::routing::post(move|axum::Json(value):axum::Json<Value>|{let count=counts.clone();let seen=payloads.clone();async move{
        count.fetch_add(1,Ordering::SeqCst);seen.lock().unwrap().push(value.clone());tokio::time::sleep(delay).await;
        let input:Value=serde_json::from_str(value["messages"][1]["content"].as_str().unwrap()).unwrap();
        let selections=input["sources"].as_array().unwrap().iter().map(|source|{let text=source["text"].as_str().unwrap();let quote=if bad{"模型捏造已完成并且已发出"}else{text.lines().rev().find(|line|!line.trim().is_empty()&&line.trim().chars().count()<=280).unwrap().trim()};json!({"source_id":source["source_id"],"quote":quote})}).collect::<Vec<_>>();
        axum::Json(json!({"choices":[{"message":{"content":json!({"selections":selections}).to_string()},"finish_reason":"stop"}]}))
    }}));
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (url, count, seen, handle)
}
fn set_model(state: &AppState, url: &str) {
    let credential = uuid::Uuid::new_v4().to_string();
    std::fs::write(
        state.0.options.data_dir.join("secrets").join(&credential),
        "synthetic-rest-loopback-key",
    )
    .unwrap();
    state
        .db(|db| {
            db.execute(
                "INSERT INTO secrets VALUES(?1,'model_api_key',?2)",
                params![credential, now()],
            )?;
            let (_, mut config) = crate::character::current_config_in(db)?;
            config.model.base_url = url.into();
            config.model.model = "synthetic-evidence-selector".into();
            config.model.credential_ref = Some(credential);
            db.execute(
                "UPDATE config_revisions SET config_json=?1 WHERE revision=1",
                [serde_json::to_string(&config)?],
            )?;
            Ok(())
        })
        .unwrap();
}
fn first(state: &AppState) -> ClaimedJob {
    claim_next(state)
        .unwrap()
        .expect("real authorized bounded job")
}
async fn drain(state: &AppState) {
    for _ in 0..30 {
        let Some(job) = claim_next(state).unwrap() else {
            return;
        };
        run_claimed(state, job, &CancellationToken::new()).await;
    }
    panic!("executor must be bounded");
}

#[test]
fn quote_gate_rejects_invented_or_partial_negation_and_free_summary() {
    let sources = vec![(
        "source-a".into(),
        "尚未完成合成事项，明天继续。\n另一整行。".into(),
    )];
    for raw in [
        json!({"selections":[{"source_id":"source-a","quote":"完成合成事项"}]}),
        json!({"selections":[{"source_id":"other","quote":"另一整行。"}]}),
        json!({"selections":[{"source_id":"source-a","quote":"另一整行。"}],"confidence":1}),
    ] {
        assert!(
            verified_facts(&raw.to_string(), worker::ModelFinishReason::Stop, &sources).is_err()
        );
    }
    let raw =
        json!({"selections":[{"source_id":"source-a","quote":"尚未完成合成事项，明天继续。"}]})
            .to_string();
    let facts = verified_facts(&raw, worker::ModelFinishReason::Stop, &sources).unwrap();
    assert!(facts.contains("计划或约定陈述") && facts.contains("不证明已完成或已送达"));
    assert!(verified_facts(&raw, worker::ModelFinishReason::Length, &sources).is_err());
}
#[tokio::test]
async fn actual_loopback_canonical_write_is_scoped_idempotent_and_produced() {
    let (_dir, state) = fixture();
    let (url, count, seen, server) = provider(Duration::ZERO, false).await;
    set_model(&state, &url);
    insert_event(
        &state,
        "event-a",
        "owner-a",
        "rest-a",
        "明天准备检查合成清单。",
        now() - 86400,
    );
    insert_event(
        &state,
        "event-b",
        "owner-b",
        "rest-b",
        "私有 B 合成记录。",
        now() - 86400,
    );
    let cycle = start(&state);
    drain(&state).await;
    assert_eq!(count.load(Ordering::SeqCst), 2);
    {
        let payloads = seen.lock().unwrap();
        for request in payloads.iter() {
            let raw = request.to_string();
            assert!(!(raw.contains("合成清单") && raw.contains("私有 B")));
        }
    }
    state
        .db(|db| {
            let tx = db.transaction()?;
            let entries: i64 = tx.query_row(
                "SELECT count(*) FROM diary_entries WHERE generator LIKE 'model:rest-quotes:%'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(entries, 2);
            let produced: i64 = tx.query_row(
                "SELECT count(*) FROM rest_produced_refs WHERE cycle_id=?1",
                [&cycle],
                |r| r.get(0),
            )?;
            assert_eq!(produced, 2);
            let refs = rest_store::cycle_sources_in(&tx, &cycle)?;
            assert_eq!(
                refs.iter()
                    .filter(|r| r.kind == SourceKind::DiaryEntry)
                    .count(),
                2
            );
            assert_eq!(rest_store::running_count(&tx)?, 0);
            let writes: i64 =
                tx.query_row("SELECT count(*) FROM relationship_assertions", [], |r| {
                    r.get(0)
                })?;
            assert_eq!(writes, 0);
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    drain(&state).await;
    assert_eq!(count.load(Ordering::SeqCst), 2);
    server.abort();
}
#[tokio::test]
async fn no_authorization_or_sources_makes_zero_requests_and_zero_facts() {
    let (_dir, state) = fixture();
    let (url, count, _, server) = provider(Duration::ZERO, false).await;
    set_model(&state, &url);
    start(&state);
    drain(&state).await;
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(
        state
            .db(
                |db| Ok(db.query_row("SELECT count(*) FROM diary_entries", [], |r| r
                    .get::<_, i64>(0))?)
            )
            .unwrap(),
        0
    );
    server.abort();
}
#[tokio::test]
async fn bounded_batch_overflow_and_same_owner_cross_conversation_are_partial() {
    let (_dir, state) = fixture();
    let (url, count, _, server) = provider(Duration::ZERO, false).await;
    set_model(&state, &url);
    for n in 0..17 {
        insert_event(
            &state,
            &format!("many-{n}"),
            "owner-a",
            "rest-a",
            "有界合成记录。",
            now() - 86400,
        );
    }
    insert_event(
        &state,
        "cross-1",
        "owner-b",
        "rest-b",
        "合成 B。",
        now() - 172800,
    );
    state
        .db(|db| {
            db.execute(
                "INSERT INTO conversations VALUES('rest-b2','owner-b','Synthetic B2',NULL,0)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    insert_event(
        &state,
        "cross-2",
        "owner-b",
        "rest-b2",
        "另一会话合成记录。",
        now() - 172800,
    );
    let cycle = start(&state);
    drain(&state).await;
    assert_eq!(count.load(Ordering::SeqCst), 0);
    let progress = state
        .db(|db| {
            let tx = db.transaction()?;
            rest_store::progress_in(&tx, &cycle)
        })
        .unwrap();
    assert!(progress.partial);
    assert!(progress
        .steps
        .iter()
        .any(|s| s.kind == "diary_day" && s.status == "partial"));
    server.abort();
}
#[tokio::test]
async fn unverified_model_output_never_becomes_diary_fact() {
    let (_dir, state) = fixture();
    let (url, count, _, server) = provider(Duration::ZERO, true).await;
    set_model(&state, &url);
    insert_event(
        &state,
        "source",
        "owner-a",
        "rest-a",
        "尚未执行的合成计划。",
        now() - 86400,
    );
    start(&state);
    let job = first(&state);
    run_claimed(&state, job, &CancellationToken::new()).await;
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(
        state
            .db(
                |db| Ok(db.query_row("SELECT count(*) FROM diary_entries", [], |r| r
                    .get::<_, i64>(0))?)
            )
            .unwrap(),
        0
    );
    assert_eq!(state.db(|db| rest_store::running_count(db)).unwrap(), 0);
    server.abort();
}
#[tokio::test]
async fn late_message_or_human_revision_blocks_canonical_commit() {
    let (_dir, state) = fixture();
    let (url, count, _, server) = provider(Duration::from_millis(180), false).await;
    set_model(&state, &url);
    insert_event(
        &state,
        "source",
        "owner-a",
        "rest-a",
        "合成旧消息。",
        now() - 86400,
    );
    start(&state);
    let job = first(&state);
    let cloned = state.clone();
    let task =
        tokio::spawn(async move { run_claimed(&cloned, job, &CancellationToken::new()).await });
    while count.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    insert_event(
        &state,
        "late",
        "owner-a",
        "rest-a",
        "截止后新到的合成消息。",
        now() - 86400,
    );
    task.await.unwrap();
    assert_eq!(
        state
            .db(
                |db| Ok(db.query_row("SELECT count(*) FROM diary_entries", [], |r| r
                    .get::<_, i64>(0))?)
            )
            .unwrap(),
        0
    );
    assert_eq!(state.db(|db| rest_store::running_count(db)).unwrap(), 0);
    server.abort();
}
#[tokio::test]
async fn source_revoke_and_profile_change_stop_before_request() {
    for revoke in [true, false] {
        let (_dir, state) = fixture();
        let (url, count, _, server) = provider(Duration::ZERO, false).await;
        set_model(&state, &url);
        insert_event(
            &state,
            "source",
            "owner-a",
            "rest-a",
            "合成来源。",
            now() - 86400,
        );
        start(&state);
        let job = first(&state);
        state.db(|db|{if revoke{db.execute("UPDATE diary_source_policies SET enabled=0,revision=revision+1 WHERE source_owner_id='owner-a'",[])?;}else{let (_,mut config)=crate::character::current_config_in(db)?;config.model.model="changed-after-freeze".into();db.execute("UPDATE config_revisions SET config_json=?1 WHERE revision=1",[serde_json::to_string(&config)?])?;}Ok(())}).unwrap();
        run_claimed(&state, job, &CancellationToken::new()).await;
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert_eq!(state.db(|db| rest_store::running_count(db)).unwrap(), 0);
        server.abort();
    }
}
#[tokio::test]
async fn wake_fences_result_and_keeps_other_physical_lease() {
    let (_dir, state) = fixture();
    let (url, count, _, server) = provider(Duration::from_secs(2), false).await;
    set_model(&state, &url);
    insert_event(
        &state,
        "source",
        "owner-a",
        "rest-a",
        "合成待整理来源。",
        now() - 86400,
    );
    start(&state);
    let job = first(&state);
    let cloned = state.clone();
    let task =
        tokio::spawn(async move { run_claimed(&cloned, job, &CancellationToken::new()).await });
    while count.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    state.db(|db|{let tx=db.transaction()?;tx.execute("INSERT INTO diary_model_leases(id,entry_id,source_owner_id,config_json,started_at) VALUES('other-physical','other-entry','owner-b','{}',?1)",[now()])?;let generation=rest_store::state_in(&tx)?.generation;let clock=crate::rest_clock::sample_in(&tx)?;let _=rest_store::wake_in(&tx,&admin(&tx),generation,"synthetic-wake",&clock)?;assert_eq!(rest_store::running_count(&tx)?,1);tx.commit()?;Ok(())}).unwrap();
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.db(|db| rest_store::running_count(db)).unwrap(), 0);
    assert_eq!(
        state
            .db(|db| crate::diary_model::running_count(db))
            .unwrap(),
        1
    );
    assert_eq!(
        state
            .db(
                |db| Ok(db.query_row("SELECT count(*) FROM diary_entries", [], |r| r
                    .get::<_, i64>(0))?)
            )
            .unwrap(),
        0
    );
    server.abort();
}
#[test]
fn sensitive_source_is_not_duplicated_in_new_derived_tables() {
    let (_dir, state) = fixture();
    insert_event(
        &state,
        "secret-source",
        "owner-a",
        "rest-a",
        "password = synthetic-never-copy-value",
        now() - 86400,
    );
    start(&state);
    assert!(claim_next(&state).unwrap().is_none());
    let canary = "synthetic-never-copy-value";
    state
        .db(|db| {
            for table in [
                "rest_consolidation_targets",
                "rest_consolidation_profiles",
                "rest_consolidation_blocks",
                "rest_produced_refs",
            ] {
                let mut stmt = db.prepare(&format!("SELECT * FROM {table}"))?;
                let columns = stmt.column_count();
                let mut rows = stmt.query([])?;
                while let Some(row) = rows.next()? {
                    for col in 0..columns {
                        if let Ok(text) = row.get::<_, String>(col) {
                            assert!(!text.contains(canary));
                        }
                    }
                }
            }
            Ok(())
        })
        .unwrap();
}
#[test]
fn dropped_unpolled_future_ends_only_owned_attempt_and_records_unknown() {
    let (_dir, state) = fixture();
    set_model(&state, "http://127.0.0.1:1");
    insert_event(
        &state,
        "source",
        "owner-a",
        "rest-a",
        "合成未轮询请求。",
        now() - 86400,
    );
    start(&state);
    let job = first(&state);
    let id = job.job_id().to_owned();
    assert_eq!(state.db(|db| rest_store::running_count(db)).unwrap(), 1);
    drop(job);
    assert_eq!(state.db(|db| rest_store::running_count(db)).unwrap(), 0);
    assert_eq!(
        state
            .db(|db| Ok(rest_store::job_in(db, &id)?.status))
            .unwrap(),
        "unknown"
    );
}

#[tokio::test]
async fn same_cycle_day_week_month_uses_produced_lineage_and_keeps_incomplete_coverage() {
    let (_dir, state) = fixture();
    let (url, count, _, server) = provider(Duration::ZERO, false).await;
    set_model(&state, &url);
    let current = chrono::DateTime::from_timestamp(now(), 0)
        .unwrap()
        .with_timezone(&chrono_tz::Asia::Shanghai)
        .date_naive();
    let prior = current.with_day(1).unwrap() - Days::days(1);
    let day = prior.with_day(10).unwrap();
    let happened = chrono_tz::Asia::Shanghai
        .from_local_datetime(&day.and_hms_opt(12, 0, 0).unwrap())
        .single()
        .unwrap()
        .timestamp();
    insert_event(
        &state,
        "past-source",
        "owner-a",
        "rest-a",
        "取消此前的合成安排，尚未完成。",
        happened,
    );
    let cycle = start(&state);
    drain(&state).await;
    assert_eq!(count.load(Ordering::SeqCst), 3);
    state.db(|db|{let tx=db.transaction()?;let refs=rest_store::cycle_sources_in(&tx,&cycle)?;let produced=refs.iter().filter(|r|r.kind==SourceKind::DiaryEntry).collect::<Vec<_>>();assert_eq!(produced.len(),3);for reference in produced{assert!(rest_store::validate_source_in(&tx,&cycle,reference,true)?);let entry=diary::entry(&tx,&reference.id,Some("owner-a")).map_err(|_|anyhow::anyhow!("entry"))?;assert_eq!(entry.source_count,1);assert!(!entry.observation_complete);assert_eq!(entry.status,"incomplete");assert!(entry.facts.contains("取消陈述")&&entry.facts.contains("不证明已完成或已送达"));}let stages:i64=tx.query_row("SELECT count(*) FROM rest_produced_refs WHERE cycle_id=?1 AND stage=2",[&cycle],|r|r.get(0))?;assert_eq!(stages,2);let foreign:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM diary_entry_sources s JOIN diary_events e ON e.id=s.event_id WHERE e.source_owner_id!='owner-a')",[],|r|r.get(0))?;assert!(!foreign);tx.commit()?;Ok(())}).unwrap();
    // A source deletion/version change invalidates the entire produced DAG.
    state.db(|db|{let tx=db.transaction()?;tx.execute("UPDATE diary_events SET revision=revision+1,content='已纠正' WHERE id='past-source'",[])?;assert!(rest_store::cycle_sources_in(&tx,&cycle)?.is_empty());tx.commit()?;Ok(())}).unwrap();
    server.abort();
}

#[tokio::test]
async fn human_entry_revision_during_http_is_not_overwritten() {
    let (_dir, state) = fixture();
    let (url, count, _, server) = provider(Duration::from_millis(180), false).await;
    set_model(&state, &url);
    let happened = now() - 86400;
    insert_event(
        &state,
        "source",
        "owner-a",
        "rest-a",
        "合成旧材料。",
        happened,
    );
    state
        .db(|db| {
            let tx = db.transaction()?;
            let day = chrono::DateTime::from_timestamp(happened, 0)
                .unwrap()
                .with_timezone(&chrono_tz::Asia::Shanghai)
                .date_naive()
                .to_string();
            diary::consolidate_in(
                &tx,
                "private",
                Some("owner-a"),
                "day",
                &day,
                diary::TIMEZONE,
                now(),
            )
            .map_err(|_| anyhow::anyhow!("seed"))?;
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    start(&state);
    let job = first(&state);
    let target = job.prepared.target.id.clone();
    let cloned = state.clone();
    let task =
        tokio::spawn(async move { run_claimed(&cloned, job, &CancellationToken::new()).await });
    while count.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    state.db(|db|{let tx=db.transaction()?;tx.execute("UPDATE diary_entries SET revision=revision+1,facts='人工修订保留',content='人工修订保留',generator='human' WHERE id=?1",[&target])?;tx.execute("INSERT INTO diary_entry_versions SELECT id,revision,facts,feelings,generator,'rest-admin',?2 FROM diary_entries WHERE id=?1",params![target,now()])?;tx.commit()?;Ok(())}).unwrap();
    task.await.unwrap();
    assert_eq!(
        state
            .db(|db| Ok(db.query_row(
                "SELECT content FROM diary_entries WHERE id=?1",
                [&target],
                |r| r.get::<_, String>(0)
            )?))
            .unwrap(),
        "人工修订保留"
    );
    assert_eq!(state.db(|db| rest_store::running_count(db)).unwrap(), 0);
    server.abort();
}

#[test]
fn restart_reconstructs_pending_envelope_and_never_retries_unknown_attempt() {
    let (_dir, state) = fixture();
    set_model(&state, "http://127.0.0.1:1");
    insert_event(
        &state,
        "source",
        "owner-a",
        "rest-a",
        "合成重启来源。",
        now() - 86400,
    );
    let cycle = start(&state);
    // Simulate a process exit after the durable claim, before any transport.
    let prepared = state
        .db(|db| {
            let tx = db.transaction()?;
            let clock = crate::rest_clock::sample_in(&tx)?;
            let p = prepare_next_in(&tx, &clock)?.unwrap();
            tx.commit()?;
            Ok(p)
        })
        .unwrap();
    let id = prepared.job_id().to_owned();
    assert_eq!(state.db(|db| rest_store::running_count(db)).unwrap(), 1);
    state
        .db(|db| {
            let tx = db.transaction()?;
            rest_store::initialize(&tx)?;
            let clock = crate::rest_clock::sample_in(&tx)?;
            let _ = rest_store::recover_after_confirmed_process_exit_in(&tx, &clock)?;
            assert_eq!(rest_store::job_in(&tx, &id)?.status, "unknown");
            assert!(!rest_store::retry_job_in(&tx, &id)?);
            let count: i64 =
                tx.query_row("SELECT calls FROM rest_cycles WHERE id=?1", [&cycle], |r| {
                    r.get(0)
                })?;
            assert_eq!(count, 1);
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    assert!(claim_next(&state).unwrap().is_none());
    assert_eq!(state.db(|db| rest_store::running_count(db)).unwrap(), 0);
    drop(prepared);
}

#[tokio::test]
async fn actual_unknown_delivery_projection_does_not_feed_unaccepted_candidate() {
    let (_dir, state) = fixture();
    let (url, count, seen, server) = provider(Duration::ZERO, false).await;
    set_model(&state, &url);
    state.db(|db|{db.execute("INSERT INTO conversations VALUES('rest-qq','qq:synthetic:c2c:person','Synthetic private','qq:synthetic:c2c:person',0)",[])?;db.execute("INSERT INTO diary_source_policies VALUES('qq:synthetic:c2c:person',1,1,1,'Asia/Shanghai',0,0)",[])?;let at=now()-86400;db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,response,created_at,updated_at) VALUES('delivery-source','rest-qq','qq:synthetic:c2c:person','delivery-source','合成用户原文','{}','completed','UNACCEPTED_PRIVATE_CANDIDATE',?1,?1)",[at])?;db.execute("INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,deadline,created_at) VALUES('delivery-outbox','delivery-source','synthetic','no-real-credential','{}','UNACCEPTED_PRIVATE_CANDIDATE','unknown',?1,?1)",[at+3600])?;db.execute("INSERT INTO outbox_reply_parts(outbox_id,seq,payload_json,delay_ms,status) VALUES('delivery-outbox',1,?1,0,'unknown')",[json!({"type":"text","text":"UNACCEPTED_PRIVATE_CANDIDATE"}).to_string()])?;let tx=db.transaction()?;assert!(diary::capture_completed_in(&tx,"delivery-source").map_err(|_|anyhow::anyhow!("capture"))?);tx.commit()?;Ok(())}).unwrap();
    start(&state);
    drain(&state).await;
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let requests = seen.lock().unwrap();
    let payload = requests[0].to_string();
    assert!(payload.contains("unknown"));
    assert!(!payload.contains("UNACCEPTED_PRIVATE_CANDIDATE"));
    drop(requests);
    state
        .db(|db| {
            let facts: String =
                db.query_row("SELECT facts FROM diary_entries", [], |r| r.get(0))?;
            assert!(
                facts.contains("未知状态陈述")
                    && facts.contains("不证明已完成或已送达")
                    && !facts.contains("UNACCEPTED_PRIVATE_CANDIDATE")
            );
            Ok(())
        })
        .unwrap();
    server.abort();
}

#[tokio::test]
async fn real_forget_clears_canonical_body_and_does_not_resurrect_on_next_prepare() {
    let (_dir, state) = fixture();
    let (url, _, _, server) = provider(Duration::ZERO, false).await;
    set_model(&state, &url);
    insert_event(
        &state,
        "source",
        "owner-a",
        "rest-a",
        "合成随后遗忘的记录。",
        now() - 86400,
    );
    let cycle = start(&state);
    drain(&state).await;
    state
        .db(|db| {
            diary::forget(db, "rest-admin", "source", 1).map_err(|_| anyhow::anyhow!("forget"))?;
            let tx = db.transaction()?;
            assert!(rest_store::cycle_sources_in(&tx, &cycle)?.is_empty());
            let raw: i64 = tx.query_row(
                "SELECT count(*) FROM diary_entries WHERE content!='' AND status!='invalidated'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(raw, 0);
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    drain(&state).await;
    assert_eq!(
        state
            .db(|db| Ok(db.query_row(
                "SELECT count(*) FROM rest_job_attempts WHERE finished_at IS NULL",
                [],
                |r| r.get::<_, i64>(0)
            )?))
            .unwrap(),
        0
    );
    server.abort();
}

#[test]
fn host_clock_space_gate_blocks_new_model_but_not_wake_deadline() {
    let (_dir, state) = fixture();
    set_model(&state, "http://127.0.0.1:1");
    insert_event(
        &state,
        "source",
        "owner-a",
        "rest-a",
        "合成资源门禁记录。",
        now() - 86400,
    );
    start(&state);
    state
        .db(|db| {
            db.execute(
                "UPDATE trigger_runtime SET clock_trusted=0,space_ok=0 WHERE id=1",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(claim_next(&state).unwrap().is_none());
    assert_eq!(state.db(|db| rest_store::running_count(db)).unwrap(), 0);
    state
        .db(|db| {
            let tx = db.transaction()?;
            let state = rest_store::state_in(&tx)?;
            let clock = ClockSample::new(
                state.wake_at.unwrap() + 1,
                0,
                "synthetic-changed-boot",
                false,
            )?;
            let awake = rest_store::advance_in(&tx, &clock)?;
            assert!(matches!(awake.phase, RestPhase::Waking | RestPhase::Awake));
            tx.commit()?;
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn produced_proof_requires_original_job_parent_digest_and_attempt_fence() {
    let (_dir, state) = fixture();
    let (url, _, _, server) = provider(Duration::ZERO, false).await;
    set_model(&state, &url);
    insert_event(
        &state,
        "source",
        "owner-a",
        "rest-a",
        "合成来源链证明。",
        now() - 86400,
    );
    let cycle = start(&state);
    drain(&state).await;
    state.db(|db|{let tx=db.transaction()?;let reference=rest_store::cycle_sources_in(&tx,&cycle)?.into_iter().find(|r|r.kind==SourceKind::DiaryEntry).unwrap();assert!(rest_store::validate_source_in(&tx,&cycle,&reference,true)?);let original:String=tx.query_row("SELECT parent_digest FROM rest_produced_refs WHERE cycle_id=?1",[&cycle],|r|r.get(0))?;tx.execute("UPDATE rest_produced_refs SET parent_digest=?2 WHERE cycle_id=?1",params![cycle,hash("wrong-lineage")])?;assert!(!rest_store::validate_source_in(&tx,&cycle,&reference,true)?);tx.execute("UPDATE rest_produced_refs SET parent_digest=?2,job_fence=job_fence+1 WHERE cycle_id=?1",params![cycle,original])?;assert!(!rest_store::validate_source_in(&tx,&cycle,&reference,true)?);tx.rollback()?;Ok(())}).unwrap();
    server.abort();
}

fn api_session(state: &AppState) {
    state
        .db(|db| {
            db.execute(
                "INSERT OR IGNORE INTO sessions VALUES(?1,'rest-admin','rest-hook-csrf',?2)",
                params![crate::state::token_hash(&"c".repeat(64)), now() + 3600],
            )?;
            Ok(())
        })
        .unwrap();
}

async fn api_delete(
    state: &AppState,
    path: &str,
    authenticated: bool,
    csrf: bool,
) -> (axum::http::StatusCode, Value) {
    use tower::ServiceExt;
    let mut request = axum::http::Request::builder()
        .method("DELETE")
        .uri(path)
        .header("Host", "127.0.0.1:18271")
        .header("Origin", &state.0.options.origin);
    if authenticated {
        request = request.header("Cookie", format!("dsh_session={}", "c".repeat(64)));
    }
    if csrf {
        request = request.header("X-CSRF-Token", "rest-hook-csrf");
    }
    let response = crate::api::router(state.clone())
        .oneshot(request.body(axum::body::Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 256 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn actual_router_secret_delete_keeps_exact_rest_credential_until_real_store_attempt_ends() {
    use axum::http::StatusCode;
    let (_dir, state) = fixture();
    api_session(&state);
    set_model(&state, "http://127.0.0.1:1");
    insert_event(
        &state,
        "credential-source",
        "owner-a",
        "rest-a",
        "合成凭证租约来源。",
        now() - 86400,
    );
    start(&state);
    let job = first(&state);
    let credential = job.prepared.config.model.credential_ref.clone().unwrap();
    let path = format!("/api/admin/v1/secrets/{credential}");
    // The current profile no longer references it, and the cycle is logically
    // stopped. Only the exact unfinished Rest attempt retains this key.
    state
        .db(|db| {
            let tx = db.transaction()?;
            let (_, mut config) = crate::character::current_config_in(&tx)?;
            config.model.credential_ref = None;
            tx.execute(
                "UPDATE config_revisions SET config_json=?1 WHERE revision=1",
                [serde_json::to_string(&config)?],
            )?;
            let generation = rest_store::state_in(&tx)?.generation;
            let clock = crate::rest_clock::sample_in(&tx)?;
            let _ =
                rest_store::wake_in(&tx, &admin(&tx), generation, "credential-test-wake", &clock)?;
            assert_eq!(rest_store::job_in(&tx, job.job_id())?.status, "unknown");
            assert!(credential_in_use(&tx, &credential)?);
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        api_delete(&state, &path, false, true).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        api_delete(&state, &path, true, false).await.0,
        StatusCode::FORBIDDEN
    );
    state
        .db(|db| {
            db.execute(
                "ALTER TABLE rest_consolidation_profiles RENAME TO synthetic_hidden_profiles",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        api_delete(&state, &path, true, true).await.0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert!(state
        .0
        .options
        .data_dir
        .join("secrets")
        .join(&credential)
        .is_file());
    state
        .db(|db| {
            db.execute(
                "ALTER TABLE synthetic_hidden_profiles RENAME TO rest_consolidation_profiles",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let blocked = api_delete(&state, &path, true, true).await;
    assert_eq!(blocked.0, StatusCode::CONFLICT);
    assert_eq!(blocked.1["error"]["code"], "credential_in_use");
    assert!(state
        .0
        .options
        .data_dir
        .join("secrets")
        .join(&credential)
        .is_file());
    assert_eq!(state.db(|db| rest_store::running_count(db)).unwrap(), 1);
    let unused = uuid::Uuid::new_v4().to_string();
    std::fs::write(
        state.0.options.data_dir.join("secrets").join(&unused),
        "unused-synthetic-key",
    )
    .unwrap();
    state
        .db(|db| {
            db.execute(
                "INSERT INTO secrets VALUES(?1,'model_api_key',?2)",
                params![unused, now()],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        api_delete(
            &state,
            &format!("/api/admin/v1/secrets/{unused}"),
            true,
            true
        )
        .await
        .0,
        StatusCode::OK
    );
    drop(job);
    assert_eq!(state.db(|db| rest_store::running_count(db)).unwrap(), 0);
    assert!(!state.db(|db| credential_in_use(db, &credential)).unwrap());
    assert_eq!(
        api_delete(&state, &path, true, true).await.0,
        StatusCode::OK
    );
    assert!(!state
        .0
        .options
        .data_dir
        .join("secrets")
        .join(&credential)
        .exists());
    state
        .db(|db| {
            let exists: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM secrets WHERE id=?1)",
                [&credential],
                |r| r.get(0),
            )?;
            assert!(!exists);
            Ok(())
        })
        .unwrap();
}

fn cached_api_run(state: &AppState, id: &str, owner: &str, conversation: &str) -> String {
    let run = worker::enqueue(state, conversation, owner, id, "合成缓存展示请求。", None).unwrap();
    state.db(|db|{db.execute("UPDATE runs SET status='running' WHERE id=?1",[&run])?;db.execute("INSERT INTO rest_deferred_presentations(run_id,raw_generation,plan_json,media_inflight,updated_at) VALUES(?1,'private-cancel-cache-canary',NULL,0,?2)",params![run,now()])?;Ok(())}).unwrap();
    run
}

#[tokio::test]
async fn actual_router_cancel_clears_only_owned_cache_and_preserves_leases_and_delivery_facts() {
    use axum::http::StatusCode;
    let (_dir, state) = fixture();
    api_session(&state);
    state.db(|db|{db.execute("INSERT INTO conversations VALUES('api-owned','rest-admin','Synthetic owned',NULL,0)",[])?;Ok(())}).unwrap();
    let run = cached_api_run(&state, "own-cache", "rest-admin", "api-owned");
    let foreign = cached_api_run(&state, "foreign-cache", "owner-a", "rest-a");
    state.db(|db|{db.execute("INSERT INTO native_worker_leases VALUES(?1,?2)",params![run,now()])?;db.execute("INSERT INTO diary_model_leases(id,entry_id,source_owner_id,config_json,started_at) VALUES('unrelated-model-physical','synthetic-entry','owner-b','{}',?1)",[now()])?;db.execute("INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,platform_message_id,deadline,created_at) VALUES('cancel-facts',?1,'synthetic','synthetic','{}','synthetic in-flight','sending','prior-platform-proof',?2,?2)",params![run,now()+3600])?;for (seq,status,receipt) in [(1,"unknown",None),(2,"sent",Some("sent-receipt"))]{db.execute("INSERT INTO outbox_reply_parts(outbox_id,seq,payload_json,delay_ms,status,platform_message_id) VALUES('cancel-facts',?1,?2,0,?3,?4)",params![seq,json!({"type":"text","text":"synthetic factual part"}).to_string(),status,receipt])?;}Ok(())}).unwrap();
    let path = format!("/api/admin/v1/runs/{run}");
    assert_eq!(
        api_delete(&state, &path, false, true).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        api_delete(&state, &path, true, false).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        api_delete(&state, &format!("/api/admin/v1/runs/{foreign}"), true, true)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let cancelled = api_delete(&state, &path, true, true).await;
    assert_eq!(cancelled.0, StatusCode::OK);
    assert_eq!(cancelled.1["status"], "cancelled");
    assert!(!cancelled
        .1
        .to_string()
        .contains("private-cancel-cache-canary"));
    state.db(|db|{let row:(String,bool)=db.query_row("SELECT status,EXISTS(SELECT 1 FROM rest_deferred_presentations WHERE run_id=runs.id) FROM runs WHERE id=?1",[&run],|r|Ok((r.get(0)?,r.get(1)?)))?;assert_eq!(row,("cancelled".into(),false));let preserved:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM rest_deferred_presentations WHERE run_id=?1)",[&foreign],|r|r.get(0))?;assert!(preserved);let native:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM native_worker_leases WHERE run_id=?1)",[&run],|r|r.get(0))?;assert!(native);assert_eq!(crate::diary_model::running_count(db)?,1);let outbox:(String,Option<String>)=db.query_row("SELECT status,platform_message_id FROM outbox WHERE id='cancel-facts'",[],|r|Ok((r.get(0)?,r.get(1)?)))?;assert_eq!(outbox,("sending".into(),Some("prior-platform-proof".into())));let parts={let mut stmt=db.prepare("SELECT status,platform_message_id FROM outbox_reply_parts WHERE outbox_id='cancel-facts' ORDER BY seq")?;let rows=stmt.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;rows};assert_eq!(parts,vec![("unknown".into(),None),("sent".into(),Some("sent-receipt".into()))]);Ok(())}).unwrap();
}

#[tokio::test]
async fn actual_router_cancel_cache_delete_failure_rolls_back_logical_cancel_in_same_transaction() {
    use axum::http::StatusCode;
    let (_dir, state) = fixture();
    api_session(&state);
    state.db(|db|{db.execute("INSERT INTO conversations VALUES('api-owned','rest-admin','Synthetic owned',NULL,0)",[])?;Ok(())}).unwrap();
    let run = cached_api_run(&state, "rollback-cache", "rest-admin", "api-owned");
    state.db(|db|{db.execute_batch("CREATE TRIGGER synthetic_fail_cache_delete BEFORE DELETE ON rest_deferred_presentations BEGIN SELECT RAISE(ABORT,'synthetic cache delete blocked');END;")?;Ok(())}).unwrap();
    let response = api_delete(&state, &format!("/api/admin/v1/runs/{run}"), true, true).await;
    assert_eq!(response.0, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!response
        .1
        .to_string()
        .contains("private-cancel-cache-canary"));
    state
        .db(|db| {
            let status: String =
                db.query_row("SELECT status FROM runs WHERE id=?1", [&run], |r| r.get(0))?;
            assert_eq!(status, "running");
            let cache: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM rest_deferred_presentations WHERE run_id=?1)",
                [&run],
                |r| r.get(0),
            )?;
            assert!(cache);
            Ok(())
        })
        .unwrap();
}

#[test]
fn credential_reader_allows_only_fully_absent_legacy_schema_and_exact_unfinished_attempt() {
    let db = Connection::open_in_memory().unwrap();
    assert!(!credential_in_use(&db, "key-a").unwrap());
    db.execute_batch(
        "CREATE TABLE rest_consolidation_profiles(cycle_id TEXT,credential_ref TEXT);",
    )
    .unwrap();
    assert!(credential_in_use(&db, "key-a").is_err());
    db.execute_batch("DROP TABLE rest_consolidation_profiles;CREATE TABLE rest_job_attempts(cycle_id TEXT,finished_at INTEGER);").unwrap();
    assert!(credential_in_use(&db, "key-a").is_err());
    db.execute_batch("CREATE TABLE rest_consolidation_profiles(cycle_id TEXT,credential_ref TEXT);INSERT INTO rest_consolidation_profiles VALUES('cycle-a','key-a');INSERT INTO rest_job_attempts VALUES('cycle-a',NULL);").unwrap();
    assert!(credential_in_use(&db, "key-a").unwrap());
    assert!(!credential_in_use(&db, "key-b").unwrap());
    db.execute("UPDATE rest_job_attempts SET finished_at=1", [])
        .unwrap();
    assert!(!credential_in_use(&db, "key-a").unwrap());
}
