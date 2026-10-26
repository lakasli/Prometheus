//! Integration checks at the actual worker boundary, using only disposable
//! data and an in-process loopback provider. No channel sender is started.
use super::*;
use crate::{context_budget, conversation_commitments as commitments, conversation_state};
use axum::{extract::State, routing::post, Json, Router};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};
use tempfile::TempDir;
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};

struct Model {
    calls: AtomicUsize,
    hold: AtomicBool,
    bodies: Mutex<Vec<Value>>,
    release: Notify,
}
async fn model(State(model): State<Arc<Model>>, Json(body): Json<Value>) -> Json<Value> {
    model.calls.fetch_add(1, Ordering::SeqCst);
    model.bodies.lock().unwrap().push(body);
    if model.hold.load(Ordering::SeqCst) {
        model.release.notified().await;
    }
    Json(json!({"choices":[{"message":{"content":"合成回答。"},"finish_reason":"stop"}]}))
}

struct Fixture {
    _directory: TempDir,
    state: AppState,
    model: Arc<Model>,
    server: JoinHandle<()>,
    admin: crate::auth::Admin,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
async fn fixture() -> Fixture {
    let directory = TempDir::new().unwrap();
    let state = AppState::open(crate::state::Options {
        data_dir: directory.path().into(),
        origin: "http://127.0.0.1:18481".into(),
        allow_private_model: true,
        cookie_secure: false,
        web_dir: directory.path().join("web"),
    })
    .unwrap();
    let model = Arc::new(Model {
        calls: AtomicUsize::new(0),
        hold: AtomicBool::new(false),
        bodies: Mutex::new(Vec::new()),
        release: Notify::new(),
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/chat/completions", post(self::model))
        .with_state(model.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let secret = uuid::Uuid::new_v4().to_string();
    crate::state::private_write(
        &directory.path().join("secrets").join(&secret),
        b"synthetic-worker-context-key",
    )
    .unwrap();
    state.db(|db| {
        let (_, mut config) = character::current_config_in(db)?;
        config.model.base_url = format!("http://{address}");
        config.model.credential_ref = Some(secret.clone());
        config.assistant.max_concurrent_runs = 1;
        db.execute("UPDATE config_revisions SET config_json=?1 WHERE revision=1", [serde_json::to_string(&config)?])?;
        db.execute("INSERT INTO secrets(id,kind,created_at)VALUES(?1,'model_api_key',0)", [secret])?;
        db.execute("INSERT INTO admins(id,username,password_hash,created_at)VALUES('owner','worker-context-owner','unused',0)", [])?;
        for (conversation,owner) in [("a","owner"),("b","second-owner")] {
            db.execute("INSERT INTO conversations(id,owner_id,title,created_at)VALUES(?1,?2,'synthetic worker context',?3)", params![conversation,owner,now()])?;
        }
        Ok(())
    }).unwrap();
    Fixture {
        _directory: directory,
        state,
        model,
        server,
        admin: crate::auth::Admin {
            id: "owner".into(),
            username: "worker-context-owner".into(),
            role: "admin".into(),
            csrf_token: "synthetic-context-csrf".into(),
        },
    }
}

fn start(state: &AppState, conversation: &str, input: &str) -> Claimed {
    let owner = if conversation == "a" {
        "owner"
    } else {
        "second-owner"
    };
    let run = enqueue(
        state,
        conversation,
        owner,
        &uuid::Uuid::new_v4().to_string(),
        input,
        None,
    )
    .unwrap();
    let claimed = claim(state)
        .unwrap()
        .expect("queued run should be claimable");
    assert_eq!(claimed.input.id, run);
    claimed
}

fn settled(state: &AppState, claimed: Claimed) {
    settled_with_response(state, claimed, "合成历史已答。");
}

fn settled_with_response(state: &AppState, claimed: Claimed, response: &str) {
    state.db(|db| {
        db.execute("UPDATE runs SET status='completed',response=?2 WHERE id=?1", params![claimed.input.id,response])?;
        db.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at)VALUES(?1,?2,?3,'assistant',?5,?4)", params![uuid::Uuid::new_v4().to_string(),claimed.input.conversation,claimed.input.id,now(),response])?;
        Ok(())
    }).unwrap();
    drop(claimed);
}

fn memory_fixture(state: &AppState, conversation: &str, content: &str) -> memory::MemoryRecord {
    state
        .db(|db| {
            memory::create(
                db,
                "owner",
                &memory::CreateMemory {
                    content: content.into(),
                    visibility: memory::Visibility::Private,
                    source_conversation_id: Some(conversation.into()),
                    source_message_id: None,
                    publish: false,
                },
            )
            .map_err(|error| anyhow::anyhow!("synthetic memory fixture: {}", error.message))
        })
        .unwrap()
}

fn recall(messages: &[Value]) -> Value {
    let content = messages
        .iter()
        .filter_map(|m| m["content"].as_str())
        .find(|s| s.starts_with("DSH_RECALL_DECISION_V1\n"))
        .expect("worker must include the host recall decision");
    serde_json::from_str(content.rsplit('\n').next().unwrap()).unwrap()
}

fn memory_decision(state: &AppState, run: &str, id: &str) -> context_budget::BlockDecision {
    snapshot(state, run)
        .blocks
        .into_iter()
        .find(|block| {
            block.kind == context_budget::BlockKind::Memory
                && block.source_refs.iter().any(|r| r.id == id)
        })
        .expect("visible memory should have an explicit candidate decision")
}

fn budget(state: &AppState, bytes: u64) -> context_budget::BudgetPolicy {
    state
        .db(|db| {
            let tx = db.transaction()?;
            let (_, config) = character::current_config_in(&tx)?;
            let mut policy = context_budget::policy_in(&tx, &config.model.model)?;
            policy.max_input_bytes = bytes;
            policy.estimated_input_limit = bytes;
            let result = context_budget::save_policy_in(
                &tx,
                &policy,
                policy.revision,
                crate::reply_delivery::now_ms(),
            )?;
            tx.commit()?;
            Ok(result)
        })
        .unwrap()
}

fn snapshot(state: &AppState, run: &str) -> context_budget::BudgetSnapshot {
    state
        .db(|db| {
            let tx = db.transaction()?;
            let result = context_budget::snapshot_in(&tx, run)?;
            tx.commit()?;
            Ok(result)
        })
        .unwrap()
        .unwrap()
}

fn scalar(state: &AppState, sql: &str, run: &str) -> i64 {
    state
        .db(|db| Ok(db.query_row(sql, [run], |r| r.get(0))?))
        .unwrap()
}

async fn received(model: &Model) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while model.calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("worker should dispatch to the loopback provider");
}

fn candidate(state: &AppState, claimed: &Claimed, intent: String) -> String {
    state
        .db(|db| {
            let tx = db.transaction()?;
            let scope = conversation_state::scope_for_run_in(&tx, &claimed.input.id)?;
            let value = commitments::propose_in(
                &tx,
                &scope,
                &commitments::Proposal {
                    intent,
                    kind: "reading".into(),
                    time_text: None,
                    expires_at: None,
                    sources: Vec::new(),
                },
                commitments::ProposalOrigin::ModelSuggestion,
                "synthetic-context-proposal",
                now(),
            )?;
            tx.commit()?;
            Ok(value["id"].as_str().unwrap().to_string())
        })
        .unwrap()
}

fn confirm(fixture: &Fixture, source_run: &str, commitment: &str) {
    fixture
        .state
        .db(|db| {
            let tx = db.transaction()?;
            let scope = conversation_state::management_scope_for_source_in(
                &tx,
                &fixture.admin,
                source_run,
            )?;
            commitments::confirm_in(
                &tx,
                &scope,
                &fixture.admin,
                commitment,
                &commitments::Transition {
                    expected_revision: 1,
                    idempotency_key: "synthetic-confirm-during-request",
                    now: now(),
                },
            )?;
            tx.commit()?;
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn complete_current_last_and_continuation_recovers_original_beyond_recent_twelve() {
    let fixture = fixture().await;
    let original = format!(
        "讨论合成灯光方案，原始细节{}末尾唯一证据TAIL_OLD_FULL_734。",
        "完整资料段".repeat(110)
    );
    let topic = start(&fixture.state, "a", &original);
    let source_run = topic.input.id.clone();
    prepare_messages(&fixture.state, &topic.input).unwrap();
    settled(&fixture.state, topic);
    for _ in 0..14 {
        let filler = start(&fixture.state, "a", "嗯");
        prepare_messages(&fixture.state, &filler.input).unwrap();
        settled(&fixture.state, filler);
    }
    let current_text = "继续";
    let current = start(&fixture.state, "a", current_text);
    let messages = prepare_messages(&fixture.state, &current.input).unwrap();
    assert_eq!(
        messages.last(),
        Some(&json!({"role":"user","content":current_text}))
    );
    assert!(
        messages
            .iter()
            .any(|m| m["role"] == "user" && m["content"] == original),
        "required continuation must include the whole original, including its tail"
    );
    let compiled = snapshot(&fixture.state, &current.input.id);
    assert!(compiled
        .blocks
        .iter()
        .any(|b| b.id == format!("history:{source_run}") && b.required && b.selected));
    assert!(compiled
        .blocks
        .iter()
        .any(|b| b.id == format!("delivery:{source_run}") && b.required && b.selected));
    assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn ordinary_suggestion_receives_host_capabilities_without_inventing_an_activity() {
    let fixture = fixture().await;
    let current = start(&fixture.state, "a", "我们明天可以试试那个方法。");
    let messages = prepare_messages(&fixture.state, &current.input).unwrap();
    let system = messages
        .iter()
        .filter(|m| m["role"] == "system")
        .filter_map(|m| m["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(system.contains("DSH_COMMITMENT_HOST_CAPABILITIES_V1"));
    assert!(system.contains("\"schedule_core_available\":true"));
    assert!(system.contains("\"automatic_activity_creation_in_this_context\":false"));
    assert!(system.contains("\"proactive_external_reminders_available\":false"));
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM conversation_commitments WHERE source_run_id=?1",
            &current.input.id
        ),
        0
    );
    assert_eq!(
        fixture
            .state
            .db(|db| Ok(
                db.query_row("SELECT count(*) FROM schedule_activities", [], |r| r
                    .get::<_, i64>(0))?
            ))
            .unwrap(),
        0
    );
    assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn separate_source_focus_and_prompt_do_not_mix() {
    let fixture = fixture().await;
    for (conversation, canary) in [("a", "SOURCE_A_ONLY_561"), ("b", "SOURCE_B_ONLY_927")] {
        let run = start(
            &fixture.state,
            conversation,
            &format!("讨论合成方案{canary}"),
        );
        prepare_messages(&fixture.state, &run.input).unwrap();
        settled(&fixture.state, run);
    }
    let a = start(&fixture.state, "a", "继续");
    let a_messages = prepare_messages(&fixture.state, &a.input).unwrap();
    let a_text = serde_json::to_string(&a_messages).unwrap();
    assert!(a_text.contains("SOURCE_A_ONLY_561"));
    assert!(!a_text.contains("SOURCE_B_ONLY_927"));
    settled(&fixture.state, a);
    let b = start(&fixture.state, "b", "继续");
    let b_text =
        serde_json::to_string(&prepare_messages(&fixture.state, &b.input).unwrap()).unwrap();
    assert!(b_text.contains("SOURCE_B_ONLY_927"));
    assert!(!b_text.contains("SOURCE_A_ONLY_561"));
}

#[tokio::test]
async fn unknown_group_speaker_keeps_current_question_without_inheritable_commitments() {
    let fixture = fixture().await;
    let owner = "qq:synthetic:group:synthetic-team";
    fixture.state.db(|db| {
        db.execute("INSERT INTO conversations(id,owner_id,title,source_key,created_at)VALUES('unknown-group',?1,'synthetic group',?1,?2)",params![owner,now()])?;
        Ok(())
    }).unwrap();
    let input = "请提醒我明天阅读合成资料";
    let run = enqueue(
        &fixture.state,
        "unknown-group",
        owner,
        "unknown-group-input",
        input,
        None,
    )
    .unwrap();
    let claimed = claim(&fixture.state).unwrap().unwrap();
    assert_eq!(claimed.input.id, run);
    // No fabricated platform speaker evidence is attached to this synthetic
    // group fixture. It must not be promoted to an owner/person identity.
    let messages = prepare_messages(&fixture.state, &claimed.input).unwrap();
    assert_eq!(
        messages.last(),
        Some(&json!({"role":"user","content":input}))
    );
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM conversation_commitments WHERE source_run_id=?1",
            &run
        ),
        0
    );
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM conversation_commitment_usage WHERE run_id=?1",
            &run
        ),
        0
    );
    assert_eq!(scalar(&fixture.state,"SELECT count(*) FROM conversation_commitment_contexts WHERE run_id=?1 AND references_json='[]'",&run),1);
    assert!(snapshot(&fixture.state, &run).accepted);
}

#[tokio::test]
async fn budget_rejection_is_durable_and_process_sends_no_provider_or_outbox_request() {
    let fixture = fixture().await;
    budget(&fixture.state, 1);
    let claimed = start(
        &fixture.state,
        "a",
        "完整合成现问不得被静默截断。末尾CANARY_CURRENT_447",
    );
    let run = claimed.input.id.clone();
    process(&fixture.state, claimed, &CancellationToken::new()).await;
    let proof = snapshot(&fixture.state, &run);
    assert!(!proof.accepted);
    assert!(proof
        .blocks
        .iter()
        .any(|b| b.kind == context_budget::BlockKind::CurrentInput && b.required));
    assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM outbox WHERE run_id=?1",
            &run
        ),
        0
    );
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM run_prompt_snapshots WHERE run_id=?1",
            &run
        ),
        0
    );
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM native_worker_leases WHERE run_id=?1",
            &run
        ),
        0
    );
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM runs WHERE id=?1 AND status='failed' AND error LIKE '%预算%'",
            &run
        ),
        1
    );
}

#[tokio::test]
async fn dropped_memory_and_commitment_never_register_as_presented_usage() {
    let fixture = fixture().await;
    let source = start(&fixture.state, "a", "合成阅读资料源");
    let commitment = candidate(&fixture.state, &source, "约定事项".repeat(75));
    prepare_messages(&fixture.state, &source.input).unwrap();
    let source_proof = snapshot(&fixture.state, &source.input.id);
    let mandatory_bytes = source_proof
        .blocks
        .iter()
        .filter(|b| b.required)
        .map(|b| b.serialized_bytes)
        .sum::<u64>();
    settled(&fixture.state, source);
    fixture
        .state
        .db(|db| {
            memory::create(
                db,
                "owner",
                &memory::CreateMemory {
                    content: format!("DROP_MEMORY_CANARY_721{}", "合成记忆资料".repeat(180)),
                    visibility: memory::Visibility::Private,
                    source_conversation_id: Some("a".into()),
                    source_message_id: None,
                    publish: false,
                },
            )
            .map_err(|error| anyhow::anyhow!("memory fixture: {}", error.message))?;
            Ok(())
        })
        .unwrap();
    budget(&fixture.state, mandatory_bytes + 128);
    let current = start(&fixture.state, "a", "本轮独立问题");
    let messages = prepare_messages(&fixture.state, &current.input).unwrap();
    let proof = snapshot(&fixture.state, &current.input.id);
    assert!(proof.accepted);
    assert!(proof
        .blocks
        .iter()
        .any(|b| b.kind == context_budget::BlockKind::Memory && !b.selected));
    assert!(proof
        .blocks
        .iter()
        .any(|b| b.kind == context_budget::BlockKind::Commitment && !b.selected));
    assert!(!serde_json::to_string(&messages)
        .unwrap()
        .contains("DROP_MEMORY_CANARY_721"));
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM memory_run_uses WHERE run_id=?1",
            &current.input.id
        ),
        0
    );
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM conversation_commitment_usage WHERE run_id=?1",
            &current.input.id
        ),
        0
    );
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM conversation_commitments WHERE id=?1 AND invalidated_at IS NULL",
            &commitment
        ),
        1
    );
}

#[tokio::test]
async fn cached_prompt_and_budget_are_immutable_after_policy_revision_changes() {
    let fixture = fixture().await;
    let claimed = start(&fixture.state, "a", "合成缓存问题CACHE_INPUT_663");
    let first = prepare_messages(&fixture.state, &claimed.input).unwrap();
    let proof = snapshot(&fixture.state, &claimed.input.id);
    let new_policy = budget(&fixture.state, 1);
    assert!(new_policy.revision > proof.policy.revision);
    let replay = prepare_messages(&fixture.state, &claimed.input).unwrap();
    assert_eq!(first, replay);
    assert_eq!(snapshot(&fixture.state, &claimed.input.id), proof);
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM run_prompt_snapshots WHERE run_id=?1",
            &claimed.input.id
        ),
        1
    );
}

#[tokio::test]
async fn changed_commitment_blocks_cached_dispatch_before_provider_receives_anything() {
    let fixture = fixture().await;
    let source = start(&fixture.state, "a", "合成阅读资料源");
    let source_run = source.input.id.clone();
    let commitment = candidate(&fixture.state, &source, "合成阅读约定".into());
    prepare_messages(&fixture.state, &source.input).unwrap();
    settled(&fixture.state, source);
    let current = start(&fixture.state, "a", "约定进度怎样");
    prepare_messages(&fixture.state, &current.input).unwrap();
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM conversation_commitment_usage WHERE run_id=?1",
            &current.input.id
        ),
        1
    );
    confirm(&fixture, &source_run, &commitment);
    let run = current.input.id.clone();
    assert!(prepare_messages(&fixture.state, &current.input).is_err());
    process(&fixture.state, current, &CancellationToken::new()).await;
    assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM outbox WHERE run_id=?1",
            &run
        ),
        0
    );
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM runs WHERE id=?1 AND status='failed'",
            &run
        ),
        1
    );
}

#[tokio::test]
async fn changed_commitment_while_http_is_owned_blocks_publication_and_keeps_lease() {
    let fixture = fixture().await;
    let source = start(&fixture.state, "a", "合成阅读资料源");
    let source_run = source.input.id.clone();
    let commitment = candidate(&fixture.state, &source, "合成阅读约定".into());
    prepare_messages(&fixture.state, &source.input).unwrap();
    settled(&fixture.state, source);
    fixture.model.hold.store(true, Ordering::SeqCst);
    let claimed = start(&fixture.state, "a", "约定进度怎样");
    let run = claimed.input.id.clone();
    let state = fixture.state.clone();
    let task =
        tokio::spawn(async move { process(&state, claimed, &CancellationToken::new()).await });
    received(&fixture.model).await;
    confirm(&fixture, &source_run, &commitment);
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM native_worker_leases WHERE run_id=?1",
            &run
        ),
        1
    );
    enqueue(
        &fixture.state,
        "b",
        "second-owner",
        "pending-independent",
        "独立合成问题",
        None,
    )
    .unwrap();
    assert!(claim(&fixture.state).unwrap().is_none());
    fixture.model.release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM outbox WHERE run_id=?1",
            &run
        ),
        0
    );
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM messages WHERE run_id=?1 AND role='assistant'",
            &run
        ),
        0
    );
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM runs WHERE id=?1 AND status='failed'",
            &run
        ),
        1
    );
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM native_worker_leases WHERE run_id=?1",
            &run
        ),
        0
    );
}

#[tokio::test]
async fn revoked_original_source_fences_inflight_response_without_releasing_transport_lease() {
    let fixture = fixture().await;
    let source = start(&fixture.state, "a", "讨论合成主题SOURCE_REVOKED_142");
    let source_run = source.input.id.clone();
    prepare_messages(&fixture.state, &source.input).unwrap();
    settled(&fixture.state, source);
    fixture.model.hold.store(true, Ordering::SeqCst);
    let claimed = start(&fixture.state, "a", "继续");
    let run = claimed.input.id.clone();
    let state = fixture.state.clone();
    let task =
        tokio::spawn(async move { process(&state, claimed, &CancellationToken::new()).await });
    received(&fixture.model).await;
    fixture
        .state
        .db(|db| {
            let tx = db.transaction()?;
            let result = crate::conversation_invalidation::invalidate_source_in(
                &tx,
                "run_input",
                &source_run,
                Some(1),
                now(),
            )?;
            assert!(result.affected_run_ids.contains(&run));
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM native_worker_leases WHERE run_id=?1",
            &run
        ),
        1
    );
    assert!(claim(&fixture.state).unwrap().is_none());
    fixture.model.release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM outbox WHERE run_id=?1",
            &run
        ),
        0
    );
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM messages WHERE run_id=?1 AND role='assistant'",
            &run
        ),
        0
    );
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM runs WHERE id=?1 AND status='cancelled'",
            &run
        ),
        1
    );
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM native_worker_leases WHERE run_id=?1",
            &run
        ),
        0
    );
}

#[tokio::test]
async fn unavailable_fts_keeps_verified_history_and_never_claims_no_memory() {
    let fixture = fixture().await;
    let original = "合成原话：书房方案唯一编号HISTORY_WHILE_FTS_DOWN_381";
    let source = start(&fixture.state, "a", original);
    prepare_messages(&fixture.state, &source.input).unwrap();
    settled(&fixture.state, source);
    let memory = memory_fixture(
        &fixture.state,
        "a",
        "不得在索引不可用时偷用的合成记忆MEMORY_FTS_614",
    );
    fixture
        .state
        .db(|db| {
            db.execute_batch("DROP TABLE memory_fts;")?;
            Ok(())
        })
        .unwrap();
    let current = start(&fixture.state, "a", "合成原话的方案编号是什么");
    let run = current.input.id.clone();
    let messages = prepare_messages(&fixture.state, &current.input).unwrap();
    let decision = recall(&messages);
    assert_eq!(decision["status"], "unavailable");
    assert_eq!(decision["conditions"]["unavailable"], true);
    assert!(messages
        .iter()
        .any(|m| m["role"] == "user" && m["content"] == original));
    assert!(!snapshot(&fixture.state, &run)
        .blocks
        .iter()
        .any(|b| b.kind == context_budget::BlockKind::Memory));
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM memory_run_uses WHERE run_id=?1",
            &run
        ),
        0
    );
    process(&fixture.state, current, &CancellationToken::new()).await;
    assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM runs WHERE id=?1 AND status='completed'",
            &run
        ),
        1
    );
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM memory_records WHERE id=?1 AND revision=1",
            &memory.id
        ),
        1
    );
    let bodies = fixture.model.bodies.lock().unwrap();
    let actual = serde_json::to_string(&bodies[0]["messages"]).unwrap();
    assert!(actual.contains("HISTORY_WHILE_FTS_DOWN_381"));
    assert!(!actual.contains("MEMORY_FTS_614"));
}

#[tokio::test]
async fn unavailable_authority_ledger_refuses_dispatch_with_a_safe_specific_reason() {
    let fixture = fixture().await;
    fixture
        .state
        .db(|db| {
            db.execute_batch("ALTER TABLE memory_records RENAME TO unavailable_memory_records;")?;
            Ok(())
        })
        .unwrap();
    let current = start(&fixture.state, "a", "合成记忆问题LEDGER_UNAVAILABLE_536");
    let run = current.input.id.clone();
    let error = prepare_messages(&fixture.state, &current.input).unwrap_err();
    assert_eq!(error.code, "context_source_unavailable");
    assert!(error.message.contains("不可核验"));
    assert!(!error.message.contains("unavailable_memory_records"));
    process(&fixture.state, current, &CancellationToken::new()).await;
    assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM outbox WHERE run_id=?1",
            &run
        ),
        0
    );
    let persisted: (String, Option<String>) = fixture
        .state
        .db(|db| {
            Ok(
                db.query_row("SELECT status,error FROM runs WHERE id=?1", [&run], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?,
            )
        })
        .unwrap();
    assert_eq!(persisted.0, "failed");
    assert!(
        persisted.1.as_ref().is_some_and(|e| e.contains("不可核验")),
        "specific unavailable diagnosis must survive final result save: {persisted:?}"
    );
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM native_worker_leases WHERE run_id=?1",
            &run
        ),
        0
    );
}

#[tokio::test]
async fn prompt_inclusion_alone_does_not_cool_down_a_memory() {
    let fixture = fixture().await;
    let text = "合成偏好是青色百合灯罩，仅作测试。";
    let memory = memory_fixture(&fixture.state, "a", text);
    let first = start(&fixture.state, "a", "今天讨论一般事项");
    prepare_messages(&fixture.state, &first.input).unwrap();
    assert!(memory_decision(&fixture.state, &first.input.id, &memory.id).selected);
    settled_with_response(&fixture.state, first, "合成回答没有复述记忆。");
    let next = start(&fixture.state, "a", "嗯");
    prepare_messages(&fixture.state, &next.input).unwrap();
    let decision = memory_decision(&fixture.state, &next.input.id, &memory.id);
    assert!(decision.selected);
    assert_ne!(decision.reason, "recently_presented");
}

#[tokio::test]
async fn delivered_same_revision_cools_down_but_explicit_recall_and_lexical_match_restore_it() {
    let fixture = fixture().await;
    let text = "合成偏好是青色百合灯罩，仅作测试。";
    let memory = memory_fixture(&fixture.state, "a", text);
    let first = start(&fixture.state, "a", "今天讨论一般事项");
    prepare_messages(&fixture.state, &first.input).unwrap();
    settled_with_response(&fixture.state, first, text);
    let excluded = start(&fixture.state, "a", "嗯");
    prepare_messages(&fixture.state, &excluded.input).unwrap();
    let decision = memory_decision(&fixture.state, &excluded.input.id, &memory.id);
    assert!(!decision.selected);
    assert_eq!(decision.reason, "recently_presented");
    assert_eq!(
        scalar(
            &fixture.state,
            "SELECT count(*) FROM memory_run_uses WHERE run_id=?1",
            &excluded.input.id
        ),
        0
    );
    settled(&fixture.state, excluded);
    let explicit = start(&fixture.state, "a", "你还记得我的偏好吗");
    prepare_messages(&fixture.state, &explicit.input).unwrap();
    assert!(memory_decision(&fixture.state, &explicit.input.id, &memory.id).selected);
    settled(&fixture.state, explicit);
    let lexical = start(&fixture.state, "a", "青色百合灯罩");
    let messages = prepare_messages(&fixture.state, &lexical.input).unwrap();
    assert!(memory_decision(&fixture.state, &lexical.input.id, &memory.id).selected);
    assert_eq!(recall(&messages)["retrieval_basis"], "lexical_match");
}

#[tokio::test]
async fn a_substantive_new_question_is_not_cooled_down_just_for_lacking_lexical_overlap() {
    let fixture = fixture().await;
    let text = "合成偏好是青色百合灯罩，仅作测试。";
    let memory = memory_fixture(&fixture.state, "a", text);
    let first = start(&fixture.state, "a", "今天讨论一般事项");
    prepare_messages(&fixture.state, &first.input).unwrap();
    settled_with_response(&fixture.state, first, text);
    let current = start(&fixture.state, "a", "请给出下一步具体建议");
    prepare_messages(&fixture.state, &current.input).unwrap();
    assert!(memory_decision(&fixture.state, &current.input.id, &memory.id).selected);
}

#[tokio::test]
async fn updated_memory_revision_is_not_cooled_down_by_an_older_delivered_revision() {
    let fixture = fixture().await;
    let text = "合成偏好是青色百合灯罩，仅作测试。";
    let memory = memory_fixture(&fixture.state, "a", text);
    let first = start(&fixture.state, "a", "今天讨论一般事项");
    prepare_messages(&fixture.state, &first.input).unwrap();
    settled_with_response(&fixture.state, first, text);
    fixture
        .state
        .db(|db| {
            memory::update(
                db,
                "owner",
                &memory.id,
                1,
                &memory::UpdateMemory {
                    content: text.into(),
                    visibility: None,
                    publish: false,
                },
            )
            .map_err(|e| anyhow::anyhow!("synthetic memory revision: {}", e.message))
        })
        .unwrap();
    let current = start(&fixture.state, "a", "嗯");
    prepare_messages(&fixture.state, &current.input).unwrap();
    let decision = memory_decision(&fixture.state, &current.input.id, &memory.id);
    assert!(decision.selected);
    assert!(decision.source_refs.iter().all(|r| r.revision == 2));
    assert_ne!(decision.reason, "recently_presented");
}

#[tokio::test]
async fn unknown_qq_delivery_never_counts_as_a_recently_presented_memory() {
    let fixture = fixture().await;
    let owner = "qq:synthetic:c2c:synthetic-user";
    fixture.state.db(|db| {db.execute("INSERT INTO conversations(id,owner_id,title,source_key,created_at)VALUES('unknown-delivery',?1,'synthetic delivery',?1,?2)",params![owner,now()])?;Ok(())}).unwrap();
    let text = "合成偏好是青色百合灯罩，仅作测试。";
    let memory = memory_fixture(&fixture.state, "unknown-delivery", text);
    let target = ReplyTarget {
        kind: "c2c".into(),
        target: "synthetic-user".into(),
        message_id: "synthetic-platform-input".into(),
        account_id: "synthetic".into(),
        credential_ref: "not-configured".into(),
        deadline: now() + 600,
    };
    let first = enqueue(
        &fixture.state,
        "unknown-delivery",
        owner,
        "unknown-source",
        "今天讨论一般事项",
        Some(target),
    )
    .unwrap();
    let claimed = claim(&fixture.state).unwrap().unwrap();
    assert_eq!(claimed.input.id, first);
    prepare_messages(&fixture.state, &claimed.input).unwrap();
    assert!(memory_decision(&fixture.state, &first, &memory.id).selected);
    settled_with_response(&fixture.state, claimed, text);
    fixture.state.db(|db| {
        db.execute("INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,deadline,created_at)VALUES('synthetic-unknown-outbox',?1,'synthetic','not-configured','{}',?2,'unknown',?3,?4)",params![first,text,now()+600,now()])?;
        db.execute("INSERT INTO outbox_reply_parts(outbox_id,seq,payload_json,delay_ms,status)VALUES('synthetic-unknown-outbox',1,?1,1000,'unknown')",[json!({"type":"text","text":text}).to_string()])?;
        Ok(())
    }).unwrap();
    let current = enqueue(
        &fixture.state,
        "unknown-delivery",
        owner,
        "next-unknown-delivery",
        "嗯",
        None,
    )
    .unwrap();
    let claimed = claim(&fixture.state).unwrap().unwrap();
    assert_eq!(claimed.input.id, current);
    prepare_messages(&fixture.state, &claimed.input).unwrap();
    let decision = memory_decision(&fixture.state, &current, &memory.id);
    assert!(decision.selected);
    assert_ne!(decision.reason, "recently_presented");
    assert_eq!(fixture.model.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn detailed_request_keeps_required_reply_contract_after_history_before_complete_input() {
    let fixture = fixture().await;
    let source = start(
        &fixture.state,
        "a",
        "我们讨论完全平方公式，之后请继续这个题目",
    );
    prepare_messages(&fixture.state, &source.input).unwrap();
    settled(&fixture.state, source);
    let input = "这题详细点，请证明 x²+2x+1=(x+1)²，保留完整核心步骤。";
    let claimed = start(&fixture.state, "a", input);
    let run = claimed.input.id.clone();
    process(&fixture.state, claimed, &CancellationToken::new()).await;
    let bodies = fixture.model.bodies.lock().unwrap();
    assert_eq!(bodies.len(), 1);
    let messages = bodies[0]["messages"].as_array().unwrap();
    assert_eq!(messages.last().unwrap()["role"], "user");
    assert!(messages.last().unwrap()["content"]
        .as_str()
        .unwrap()
        .contains(input));
    let contract = &messages[messages.len() - 2];
    assert_eq!(contract["role"], "system");
    let text = contract["content"].as_str().unwrap();
    assert!(text.starts_with("DSH_REPLY_COMPLETION_CONTRACT_V1"));
    assert!(text.contains("完整句总数也不得超过4"));
    drop(bodies);
    let policy = snapshot(&fixture.state, &run);
    let block = policy
        .blocks
        .iter()
        .find(|b| b.id == "reply_completion_contract")
        .unwrap();
    assert!(block.required && block.selected && block.serialized_bytes > 0);
}
