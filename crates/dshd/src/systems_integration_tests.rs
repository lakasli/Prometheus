//! Cross-system admission with real, deliberately held HTTP requests. These
//! tests exercise populated queues rather than empty-queue count shortcuts.
use crate::{
    diary, diary_model, relationships, relationships_model, schedule, schedule_model,
    state::{now, AppState, Options},
    worker,
};
use axum::{extract::State, routing::post, Json, Router};
use chrono::{TimeZone, Utc};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tempfile::TempDir;
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};
use tokio_util::sync::CancellationToken;

struct HeldProvider {
    requests: AtomicUsize,
    payloads: Mutex<Vec<Value>>,
    seen: Notify,
    release: Notify,
}
async fn provider(State(s): State<Arc<HeldProvider>>, Json(payload): Json<Value>) -> Json<Value> {
    s.payloads.lock().unwrap().push(payload);
    s.requests.fetch_add(1, Ordering::SeqCst);
    s.seen.notify_one();
    s.release.notified().await;
    Json(json!({"choices":[{"message":{"content":"cancelled fixture output"}}]}))
}
async fn fixture() -> (TempDir, AppState, Arc<HeldProvider>, JoinHandle<()>) {
    let dir = TempDir::new().unwrap();
    let state = AppState::open(Options {
        data_dir: dir.path().into(),
        origin: "http://127.0.0.1:18437".into(),
        allow_private_model: true,
        cookie_secure: false,
        web_dir: dir.path().join("web"),
    })
    .unwrap();
    let p = Arc::new(HeldProvider {
        requests: AtomicUsize::new(0),
        payloads: Mutex::new(Vec::new()),
        seen: Notify::new(),
        release: Notify::new(),
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/chat/completions", post(provider))
        .with_state(p.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let reference = uuid::Uuid::new_v4().to_string();
    crate::state::private_write(
        &dir.path().join("secrets").join(&reference),
        b"synthetic-shared-pool-key",
    )
    .unwrap();
    state.db(|db|{let(_,mut c)=crate::character::current_config_in(db)?;c.model.base_url=format!("http://{address}");c.model.credential_ref=Some(reference.clone());c.assistant.max_concurrent_runs=2;db.execute("UPDATE config_revisions SET config_json=?1 WHERE revision=1",[serde_json::to_string(&c)?])?;db.execute("INSERT INTO secrets(id,kind,created_at) VALUES(?1,'model_api_key',0)",[reference])?;db.execute("INSERT INTO admins VALUES('owner','shared-pool-owner','unused',0)",[])?;db.execute("INSERT INTO conversations(id,owner_id,title,created_at) VALUES('chat','owner','synthetic integration source',0)",[])?;db.execute("UPDATE trigger_runtime SET clock_trusted=1,space_ok=1 WHERE id=1",[])?;Ok(())}).unwrap();
    crate::trigger_scheduler::acknowledge(&state, now(), true).unwrap();
    state
        .db(|db| {
            Ok(diary::save_policy(
                db,
                "owner",
                0,
                &diary::SourcePolicy {
                    source_owner_id: "owner".into(),
                    enabled: true,
                    allow_model: true,
                    timezone: diary::TIMEZONE.into(),
                },
            ))
        })
        .unwrap()
        .unwrap();
    (dir, state, p, server)
}
fn note(state: &AppState, text: &str) -> diary::Event {
    state
        .db(|db| {
            Ok(diary::note(
                db,
                "owner",
                &diary::Note {
                    content: text.into(),
                    source_conversation_id: Some("chat".into()),
                    visibility: "private".into(),
                    publish: false,
                    happened_at: None,
                },
            ))
        })
        .unwrap()
        .unwrap()
}
fn diary_job(state: &AppState) -> diary::Entry {
    let day = Utc
        .timestamp_opt(now(), 0)
        .unwrap()
        .with_timezone(&chrono_tz::Asia::Shanghai)
        .date_naive()
        .to_string();
    state
        .db(|db| {
            let tx = db.transaction()?;
            let e = diary::consolidate_in(
                &tx,
                "private",
                Some("owner"),
                "day",
                &day,
                diary::TIMEZONE,
                now(),
            );
            if e.is_ok() {
                tx.commit()?;
            }
            Ok(e)
        })
        .unwrap()
        .unwrap()
}
fn reading_job(state: &AppState, key: &str) -> String {
    state.db(|db|{let resource=schedule::create_resource(db,"owner",&json!({"title":"authorized synthetic text","type":"text","content":"这是一段获授权的测试阅读文本，只有真实模型笔记提交成功才推进进度。"}),now())?;
        let activity=schedule::create(db,"owner",&json!({"title":"bounded reading","type":"reading","purpose":"synthetic shared admission","start_at":now()-1,"end_at":now()+600,"time_zone":"Asia/Shanghai","resource_id":resource["id"],"target_ref":null,"repeat":null,"movable":true,"grant_id":null}),key,false,now())?;
        schedule::tick_in(db,now(),false)?;Ok(activity["id"].as_str().unwrap().to_owned())}).unwrap()
}
fn person(state: &AppState) -> relationships::Person {
    state
        .db(|db| {
            Ok(relationships::create(
                db,
                relationships::Access::Admin("owner"),
                &relationships::CreatePerson {
                    display_name: "合成人物".into(),
                    scope: relationships::ScopeInput {
                        visibility: "private".into(),
                        source_conversation_id: Some("chat".into()),
                        publish: false,
                    },
                    reason: "共享准入验收合成人物".into(),
                },
            ))
        })
        .unwrap()
        .unwrap()
}
fn relationship_job(
    state: &AppState,
    person: &relationships::Person,
    event: &diary::Event,
    key: &str,
) -> relationships_model::Job {
    state
        .db(|db| {
            Ok(relationships_model::enqueue(
                db,
                "owner",
                person.revision,
                &relationships_model::GenerateInput {
                    person_id: person.id.clone(),
                    source_type: "diary_event".into(),
                    source_id: event.id.clone(),
                    source_revision: event.revision,
                    target_person_ids: vec![],
                    confirmed: true,
                    idempotency_key: key.into(),
                },
            ))
        })
        .unwrap()
        .unwrap()
}
async fn wait_request(p: &HeldProvider, count: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while p.requests.load(Ordering::SeqCst) < count {
            p.seen.notified().await;
        }
    })
    .await
    .expect("provider did not receive real request");
}
fn counts(s: &AppState) -> (i64, i64, i64, i64) {
    s.db(|db| {
        Ok((
            diary_model::running_count(db)?,
            schedule_model::running_count(db)?,
            relationships_model::running_count(db)?,
            worker::running_count(db)?,
        ))
    })
    .unwrap()
}
async fn done(handle: JoinHandle<()>, system: &str) {
    tokio::time::timeout(Duration::from_secs(3), handle)
        .await
        .unwrap_or_else(|_| panic!("{system} cancelled request did not stop"))
        .unwrap();
}

#[tokio::test]
async fn populated_three_system_queues_share_one_automatic_http_slot_and_release_after_cancellation(
) {
    let (_dir, state, p, server) = fixture().await;
    let event = note(&state, "合成事实：我喜欢阅读诗歌。");
    diary_job(&state);
    let activity = reading_job(&state, "first-reading");
    let person = person(&state);
    relationship_job(&state, &person, &event, "first-person-job");
    // All three queues really contain ready work. An interactive queued message
    // is sufficient to defer every automatic claimant before any HTTP request.
    let chat = worker::enqueue(
        &state,
        "chat",
        "owner",
        "interactive-first",
        "请先处理本条合成消息",
        None,
    )
    .unwrap();
    assert!(diary_model::claim_next(&state).unwrap().is_none());
    assert!(schedule_model::claim_next(&state).unwrap().is_none());
    assert!(relationships_model::claim_next(&state).unwrap().is_none());
    assert_eq!(p.requests.load(Ordering::SeqCst), 0);
    state
        .db(|db| {
            db.execute("UPDATE runs SET status='cancelled' WHERE id=?1", [chat])?;
            Ok(())
        })
        .unwrap();
    let claimed = diary_model::claim_next(&state)
        .unwrap()
        .expect("ready diary must claim");
    let token = CancellationToken::new();
    let s = state.clone();
    let t = token.clone();
    let diary_task = tokio::spawn(async move { diary_model::run_claimed(&s, claimed, &t).await });
    wait_request(&p, 1).await;
    assert_eq!(counts(&state), (1, 0, 0, 1));
    assert!(schedule_model::claim_next(&state).unwrap().is_none());
    assert!(relationships_model::claim_next(&state).unwrap().is_none());
    // Source correction cancels logical work synchronously, while the held
    // physical request remains counted until its future observes invalidation.
    let event = state
        .db(|db| {
            Ok(diary::correct(
                db,
                "owner",
                &event.id,
                event.revision,
                "合成更正：我喜欢阅读两首诗。",
            ))
        })
        .unwrap()
        .unwrap();
    assert_eq!(counts(&state).0, 1);
    assert!(schedule_model::claim_next(&state).unwrap().is_none());
    done(diary_task, "diary").await;
    assert_eq!(counts(&state), (0, 0, 0, 0));
    let claimed = schedule_model::claim_next(&state)
        .unwrap()
        .expect("ready reading must claim after diary stops");
    let token = CancellationToken::new();
    let s = state.clone();
    let t = token.clone();
    let schedule_task =
        tokio::spawn(async move { schedule_model::run_claimed(&s, claimed, &t).await });
    wait_request(&p, 2).await;
    assert_eq!(counts(&state), (0, 1, 0, 1));
    diary_job(&state);
    // The old queued profile request has a stale source/deletion snapshot.
    state
        .db(|db| {
            db.execute(
                "UPDATE relationship_model_jobs SET status='cancelled' WHERE status='queued'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    relationship_job(&state, &person, &event, "revised-person-job");
    assert!(diary_model::claim_next(&state).unwrap().is_none());
    assert!(relationships_model::claim_next(&state).unwrap().is_none());
    state
        .db(|db| {
            schedule::change(
                db,
                "owner",
                &activity,
                1,
                "series",
                None,
                None,
                Some("cancelled"),
                false,
                now(),
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(counts(&state).1, 1);
    done(schedule_task, "schedule").await;
    assert_eq!(counts(&state), (0, 0, 0, 0));
    let claimed = relationships_model::claim_next(&state)
        .unwrap()
        .expect("ready profile job must claim");
    let token = CancellationToken::new();
    let s = state.clone();
    let t = token.clone();
    let relation_task =
        tokio::spawn(async move { relationships_model::run_claimed(&s, claimed, &t).await });
    wait_request(&p, 3).await;
    let _second = reading_job(&state, "second-reading");
    assert_eq!(counts(&state), (0, 0, 1, 1));
    assert!(diary_model::claim_next(&state).unwrap().is_none());
    assert!(schedule_model::claim_next(&state).unwrap().is_none());
    token.cancel();
    assert_eq!(counts(&state).2, 1);
    done(relation_task, "relationships").await;
    assert_eq!(counts(&state), (0, 0, 0, 0));
    assert_eq!(p.requests.load(Ordering::SeqCst), 3);
    let next = diary_model::claim_next(&state)
        .unwrap()
        .expect("next diary remains runnable after profile cancellation");
    drop(next);
    assert_eq!(counts(&state), (0, 0, 0, 0));
    p.release.notify_waiters();
    server.abort();
}

#[tokio::test]
async fn dropping_each_claim_before_its_future_is_polled_releases_the_reserved_slot() {
    let (_dir, state, p, server) = fixture().await;
    let event = note(&state, "合成日记来源");
    diary_job(&state);
    let activity = reading_job(&state, "drop-reading");
    let person = person(&state);
    relationship_job(&state, &person, &event, "drop-person");
    let claimed = diary_model::claim_next(&state).unwrap().unwrap();
    assert_eq!(counts(&state).0, 1);
    drop(claimed);
    assert_eq!(counts(&state).0, 0);
    let claimed = schedule_model::claim_next(&state).unwrap().unwrap();
    assert_eq!(counts(&state).1, 1);
    drop(claimed);
    assert_eq!(counts(&state).1, 0);
    // Explicitly cancel the reading so the profile claimant has ready work and
    // does not rely on an empty schedule queue for this assertion.
    state
        .db(|db| {
            schedule::change(
                db,
                "owner",
                &activity,
                1,
                "series",
                None,
                None,
                Some("cancelled"),
                false,
                now(),
            )?;
            Ok(())
        })
        .unwrap();
    let claimed = relationships_model::claim_next(&state).unwrap().unwrap();
    assert_eq!(counts(&state).2, 1);
    drop(claimed);
    assert_eq!(counts(&state), (0, 0, 0, 0));
    assert_eq!(p.requests.load(Ordering::SeqCst), 0);
    server.abort();
}

async fn wait_completed(state: &AppState, id: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = state
                .db(|db| {
                    Ok(
                        db.query_row("SELECT status FROM runs WHERE id=?1", [id], |r| {
                            r.get::<_, String>(0)
                        })?,
                    )
                })
                .unwrap();
            if status == "completed" {
                break;
            }
            assert!(
                !matches!(status.as_str(), "failed" | "cancelled" | "interrupted"),
                "ordinary test run failed"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("ordinary run did not finish");
}

#[tokio::test]
async fn revoked_legacy_memory_and_earlier_context_never_reach_actual_provider_after_restart() {
    let (_dir, state, p, server) = fixture().await;
    let canary = "合成来源撤权金丝雀_紫色鲸鱼_7faed83";
    let old = state
        .db(|db| {
            Ok(crate::memory::create(
                db,
                "owner",
                &crate::memory::CreateMemory {
                    content: canary.into(),
                    visibility: crate::memory::Visibility::Private,
                    source_conversation_id: Some("chat".into()),
                    source_message_id: None,
                    publish: false,
                },
            ))
        })
        .unwrap()
        .unwrap();
    let first = worker::enqueue(
        &state,
        "chat",
        "owner",
        "before-revoke",
        "请参考之前明确记录的资料简短回应。",
        None,
    )
    .unwrap();
    let stop = CancellationToken::new();
    let s = state.clone();
    let t = stop.clone();
    let worker_task = tokio::spawn(async move { worker::run_worker(s, t).await });
    wait_request(&p, 1).await;
    assert!(
        p.payloads.lock().unwrap()[0].to_string().contains(canary),
        "precondition: authorized memory must have reached real provider payload"
    );
    p.release.notify_one();
    wait_completed(&state, &first).await;
    stop.cancel();
    done(worker_task, "ordinary-before-restart").await;
    state
        .db(|db| {
            Ok(diary::save_policy(
                db,
                "owner",
                1,
                &diary::SourcePolicy {
                    source_owner_id: "owner".into(),
                    enabled: false,
                    allow_model: false,
                    timezone: diary::TIMEZONE.into(),
                },
            ))
        })
        .unwrap()
        .unwrap();
    assert!(state
        .db(|db| Ok(diary::event(
            db,
            &format!("legacy:{}", old.id),
            Some("owner")
        )))
        .unwrap()
        .is_err());
    let old_snapshots: i64 = state
        .db(|db| {
            Ok(db.query_row(
                "SELECT count(*) FROM run_prompt_snapshots WHERE run_id=?1",
                [&first],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(old_snapshots, 0);
    let options = state.0.options.clone();
    drop(state);
    let reopened = AppState::open(options).unwrap();
    assert!(
        reopened
            .db(|db| Ok(diary::event(
                db,
                &format!("legacy:{}", old.id),
                Some("owner")
            )))
            .unwrap()
            .is_err(),
        "restart must retain source revocation watermark"
    );
    // The old table remains an auditable compatibility projection. It must not
    // bypass the canonical source revocation during ordinary context assembly.
    assert!(reopened
        .db(|db| Ok(crate::memory::get(db, &old.id)))
        .unwrap()
        .is_ok());
    let second = worker::enqueue(
        &reopened,
        "chat",
        "owner",
        "after-revoke",
        "请简短回应当前这条普通消息。",
        None,
    )
    .unwrap();
    let stop = CancellationToken::new();
    let s = reopened.clone();
    let t = stop.clone();
    let worker_task = tokio::spawn(async move { worker::run_worker(s, t).await });
    wait_request(&p, 2).await;
    assert!(
        !p.payloads.lock().unwrap()[1].to_string().contains(canary),
        "revoked canonical source leaked through legacy or prior context"
    );
    p.release.notify_one();
    wait_completed(&reopened, &second).await;
    stop.cancel();
    done(worker_task, "ordinary-after-restart").await;
    assert_eq!(counts(&reopened), (0, 0, 0, 0));
    server.abort();
}

#[tokio::test]
async fn recent_diary_fallback_is_bounded_private_and_forgotten_after_restart() {
    let (_dir, state, p, server) = fixture().await;
    state
        .db(|db| {
            Ok(diary::save_policy(
                db,
                "owner",
                1,
                &diary::SourcePolicy {
                    source_owner_id: "owner".into(),
                    enabled: true,
                    allow_model: false,
                    timezone: diary::TIMEZONE.into(),
                },
            ))
        })
        .unwrap()
        .unwrap();
    let own = note(
        &state,
        "RECENT_DIARY_OWNER_CANARY_6fc81：今天完成了一次真实来源合成验收记录。",
    );
    diary_job(&state);
    state.db(|db|{db.execute("INSERT INTO conversations(id,owner_id,title,created_at) VALUES('foreign-chat','foreign-owner','private fixture',0)",[])?;Ok(())}).unwrap();
    state
        .db(|db| {
            Ok(diary::note(
                db,
                "owner",
                &diary::Note {
                    content: "RECENT_DIARY_FOREIGN_CANARY_7baf1".into(),
                    source_conversation_id: Some("foreign-chat".into()),
                    visibility: "private".into(),
                    publish: false,
                    happened_at: None,
                },
            ))
        })
        .unwrap()
        .unwrap();
    let day = Utc
        .timestamp_opt(now(), 0)
        .unwrap()
        .with_timezone(&chrono_tz::Asia::Shanghai)
        .date_naive()
        .to_string();
    state
        .db(|db| {
            let tx = db.transaction()?;
            let result = diary::consolidate_in(
                &tx,
                "private",
                Some("foreign-owner"),
                "day",
                &day,
                diary::TIMEZONE,
                now(),
            );
            if result.is_ok() {
                tx.commit()?;
            }
            Ok(result)
        })
        .unwrap()
        .unwrap();
    let first = worker::enqueue(
        &state,
        "chat",
        "owner",
        "recent-diary-before",
        "请回顾最近的近况。",
        None,
    )
    .unwrap();
    let stop = CancellationToken::new();
    let s = state.clone();
    let token = stop.clone();
    let task = tokio::spawn(async move { worker::run_worker(s, token).await });
    wait_request(&p, 1).await;
    {
        let payloads = p.payloads.lock().unwrap();
        let text = payloads[0].to_string();
        assert!(text.contains("RECENT_DIARY_OWNER_CANARY_6fc81"));
        assert!(text.contains("recent_visible_day"));
        assert!(!text.contains("RECENT_DIARY_FOREIGN_CANARY_7baf1"));
        let supplied = payloads[0]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|m| {
                m["content"]
                    .as_str()
                    .filter(|v| v.contains("recent_visible_day"))
            })
            .unwrap();
        let json = supplied.rsplit_once('\n').unwrap().1;
        assert!(json.len() <= 8000);
        let entries: Value = serde_json::from_str(json).unwrap();
        assert!(
            entries[0].get("content").is_none(),
            "duplicate content must not consume the request budget"
        );
    }
    p.release.notify_one();
    wait_completed(&state, &first).await;
    stop.cancel();
    done(task, "recent-diary-before").await;
    state
        .db(|db| Ok(diary::forget(db, "owner", &own.id, own.revision)))
        .unwrap()
        .unwrap();
    let options = state.0.options.clone();
    drop(state);
    let reopened = AppState::open(options).unwrap();
    let second = worker::enqueue(
        &reopened,
        "chat",
        "owner",
        "recent-diary-after",
        "还有哪些近况可以回顾？",
        None,
    )
    .unwrap();
    let stop = CancellationToken::new();
    let s = reopened.clone();
    let token = stop.clone();
    let task = tokio::spawn(async move { worker::run_worker(s, token).await });
    wait_request(&p, 2).await;
    let text = p.payloads.lock().unwrap()[1].to_string();
    assert!(!text.contains("RECENT_DIARY_OWNER_CANARY_6fc81"));
    assert!(!text.contains("RECENT_DIARY_FOREIGN_CANARY_7baf1"));
    p.release.notify_one();
    wait_completed(&reopened, &second).await;
    stop.cancel();
    done(task, "recent-diary-after").await;
    server.abort();
}
