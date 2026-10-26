use crate::{
    api,
    config::RuntimeConfig,
    qq,
    state::{create_bootstrap_token, now, private_write, AppState, Options},
    worker,
};
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Json,
};
use ed25519_dalek::{Signer, SigningKey};
use rusqlite::params;
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const ORIGIN: &str = "http://127.0.0.1:18419";

#[path = "api_publication_tests.rs"]
mod publication_tests;

#[tokio::test]
async fn credential_deletion_preserves_frozen_channel_replies_and_disabled_weixin_account() {
    let temporary = TempDir::new().unwrap();
    let state = open(temporary.path());
    let (cookie, csrf) = bootstrap(&state).await;
    let qq_key = uuid::Uuid::new_v4().to_string();
    let weixin_key = uuid::Uuid::new_v4().to_string();
    for reference in [&qq_key, &weixin_key] {
        private_write(
            &state.0.options.data_dir.join("secrets").join(reference),
            b"synthetic-channel-credential",
        )
        .unwrap();
    }
    state.db(|db| {
        db.execute("INSERT INTO secrets(id,kind,created_at) VALUES(?1,'qq_app_secret',?3),(?2,'weixin_bot_token',?3)",params![qq_key,weixin_key,now()])?;
        db.execute("INSERT INTO conversations(id,owner_id,title,created_at) VALUES('credential-test','source','synthetic',1)",[])?;
        db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,response,created_at,updated_at) VALUES('credential-run','credential-test','source','one','synthetic',?1,'completed','synthetic reply',1,1)",[serde_json::to_string(&RuntimeConfig::default())?])?;
        db.execute("INSERT INTO outbox(id,run_id,account_id,credential_ref,target_json,content,status,deadline,created_at) VALUES('credential-out','credential-run','synthetic',?1,'{}','synthetic reply','queued',?2,1)",params![qq_key,now()+300])?;
        Ok(())
    }).unwrap();
    let qq_path = format!("/api/admin/v1/secrets/{qq_key}");
    let blocked = request(
        &state,
        "DELETE",
        &qq_path,
        Value::Null,
        Some(&cookie),
        Some(&csrf),
        Some(ORIGIN),
    )
    .await;
    assert_eq!(blocked.0, StatusCode::CONFLICT);
    assert_eq!(blocked.1["error"]["code"], "credential_in_use");
    assert!(state
        .0
        .options
        .data_dir
        .join("secrets")
        .join(&qq_key)
        .exists());
    state
        .db(|db| {
            db.execute(
                "UPDATE outbox SET status='sent' WHERE id='credential-out'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        request(
            &state,
            "DELETE",
            &qq_path,
            Value::Null,
            Some(&cookie),
            Some(&csrf),
            Some(ORIGIN)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(!state
        .0
        .options
        .data_dir
        .join("secrets")
        .join(&qq_key)
        .exists());
    state
        .db(|db| {
            crate::weixin_store::replace_account(
                db,
                0,
                "admin",
                &crate::weixin_store::AccountInput {
                    account_id: "synthetic_bot".into(),
                    base_url: "https://ilinkai.weixin.qq.com/".into(),
                    credential_ref: weixin_key.clone(),
                    linked_user_id: "synthetic_owner".into(),
                    enabled: false,
                },
            )?;
            Ok(())
        })
        .unwrap();
    let wx_path = format!("/api/admin/v1/secrets/{weixin_key}");
    assert_eq!(
        request(
            &state,
            "DELETE",
            &wx_path,
            Value::Null,
            Some(&cookie),
            Some(&csrf),
            Some(ORIGIN)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert!(state
        .0
        .options
        .data_dir
        .join("secrets")
        .join(&weixin_key)
        .exists());
}

#[tokio::test]
async fn cancelled_native_request_retains_its_credential_and_slot_until_lease_release() {
    let temporary = TempDir::new().unwrap();
    let state = open(temporary.path());
    let (cookie, csrf) = bootstrap(&state).await;
    let key = uuid::Uuid::new_v4().to_string();
    private_write(
        &state.0.options.data_dir.join("secrets").join(&key),
        b"synthetic-cancelled-model-key",
    )
    .unwrap();
    state.db(|db| {
        let owner:String=db.query_row("SELECT id FROM admins LIMIT 1",[],|row|row.get(0))?;
        let mut config=RuntimeConfig::default();
        config.model.credential_ref=Some(key.clone());
        db.execute("INSERT INTO secrets(id,kind,created_at) VALUES(?1,'model_api_key',?2)",params![key,now()])?;
        db.execute("INSERT INTO conversations(id,owner_id,title,created_at) VALUES('leased-secret',?1,'synthetic lease',?2)",params![owner,now()])?;
        db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,created_at,updated_at) VALUES('leased-secret-run','leased-secret',?1,'one','synthetic',?2,'cancelled',?3,?3)",params![owner,serde_json::to_string(&config)?,now()])?;
        db.execute("INSERT INTO native_worker_leases(run_id,claimed_at) VALUES('leased-secret-run',?1)",[now()])?;
        Ok(())
    }).unwrap();
    let status = request(
        &state,
        "GET",
        "/api/admin/v1/status",
        Value::Null,
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert_eq!(status.1["queue"]["running"], 1);
    let path = format!("/api/admin/v1/secrets/{key}");
    let blocked = request(
        &state,
        "DELETE",
        &path,
        Value::Null,
        Some(&cookie),
        Some(&csrf),
        Some(ORIGIN),
    )
    .await;
    assert_eq!(blocked.0, StatusCode::CONFLICT);
    assert_eq!(blocked.1["error"]["code"], "credential_in_use");
    assert!(state
        .0
        .options
        .data_dir
        .join("secrets")
        .join(&key)
        .is_file());
    state
        .db(|db| {
            db.execute(
                "DELETE FROM native_worker_leases WHERE run_id='leased-secret-run'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        request(
            &state,
            "DELETE",
            &path,
            Value::Null,
            Some(&cookie),
            Some(&csrf),
            Some(ORIGIN)
        )
        .await
        .0,
        StatusCode::OK
    );
}

fn open(path: &Path) -> AppState {
    AppState::open(Options {
        data_dir: path.to_owned(),
        origin: ORIGIN.into(),
        allow_private_model: true,
        cookie_secure: false,
        web_dir: path.join("web"),
    })
    .unwrap()
}
async fn request(
    state: &AppState,
    method: &str,
    path: &str,
    body: Value,
    cookie: Option<&str>,
    csrf: Option<&str>,
    origin: Option<&str>,
) -> (StatusCode, Value, Option<String>) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("Host", "127.0.0.1:18419")
        .header("Content-Type", "application/json");
    if let Some(c) = cookie {
        builder = builder.header("Cookie", c);
    }
    if let Some(c) = csrf {
        builder = builder.header("X-CSRF-Token", c);
    }
    if let Some(o) = origin {
        builder = builder.header("Origin", o);
    }
    let response = api::router(state.clone())
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get("set-cookie")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.split(';').next().unwrap().to_string());
    let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| json!({"text":String::from_utf8_lossy(&bytes)}));
    (status, value, cookie)
}
async fn bootstrap(state: &AppState) -> (String, String) {
    let token_path = create_bootstrap_token(&state.0.options.data_dir).unwrap();
    let token: Value = serde_json::from_slice(&std::fs::read(token_path).unwrap()).unwrap();
    let (status, value, cookie) = request(
        state,
        "POST",
        "/api/admin/v1/auth/bootstrap",
        json!({"username":"admin","password":"test-password-42","bootstrap_token":token["token"]}),
        None,
        None,
        Some(ORIGIN),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    (
        cookie.unwrap(),
        value["csrf_token"].as_str().unwrap().to_string(),
    )
}

#[tokio::test]
async fn authentication_csrf_secrets_and_revision_conflict() {
    let temp = TempDir::new().unwrap();
    let state = open(temp.path());
    assert_eq!(
        request(
            &state,
            "GET",
            "/api/admin/v1/config",
            Value::Null,
            None,
            None,
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (cookie, csrf) = bootstrap(&state).await;
    assert!(!state
        .0
        .options
        .data_dir
        .join("bootstrap-token.json")
        .exists());
    assert_eq!(
        request(
            &state,
            "POST",
            "/api/admin/v1/config/drafts",
            json!({}),
            Some(&cookie),
            None,
            Some(ORIGIN)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &state,
            "POST",
            "/api/admin/v1/config/drafts",
            json!({}),
            Some(&cookie),
            Some(&csrf),
            Some("https://attacker.invalid")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let secret = "fixture-key-do-not-log";
    let (status, value, _) = request(
        &state,
        "POST",
        "/api/admin/v1/secrets",
        json!({"kind":"model_api_key","value":secret}),
        Some(&cookie),
        Some(&csrf),
        Some(ORIGIN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!value.to_string().contains(secret));
    let reference = value["id"].as_str().unwrap();
    let (_, value, _) = request(
        &state,
        "GET",
        "/api/admin/v1/secrets",
        Value::Null,
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert!(!value.to_string().contains(secret));
    assert_eq!(value["items"].as_array().unwrap().len(), 1);
    let mut config = RuntimeConfig::default();
    config.model.credential_ref = Some(reference.into());
    let (status, draft, _) = request(
        &state,
        "POST",
        "/api/admin/v1/config/drafts",
        json!({"base_revision":1,"config":config}),
        Some(&cookie),
        Some(&csrf),
        Some(ORIGIN),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let draft_id = draft["id"].as_str().unwrap();
    let path = format!("/api/admin/v1/config/drafts/{draft_id}/validate");
    assert_eq!(
        request(
            &state,
            "POST",
            &path,
            json!({}),
            Some(&cookie),
            Some(&csrf),
            Some(ORIGIN)
        )
        .await
        .1["valid"],
        true
    );
    let apply_path = format!("/api/admin/v1/config/drafts/{draft_id}/apply");
    let make = || {
        Request::builder()
            .method("POST")
            .uri(&apply_path)
            .header("Host", "127.0.0.1:18419")
            .header("Origin", ORIGIN)
            .header("Cookie", &cookie)
            .header("X-CSRF-Token", &csrf)
            .header("If-Match", "\"1\"")
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        api::router(state.clone())
            .oneshot(make())
            .await
            .unwrap()
            .status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(
        api::router(state.clone())
            .oneshot(make())
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    let (_, value, _) = request(
        &state,
        "GET",
        "/api/admin/v1/config",
        Value::Null,
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert_eq!(value["revision"], 2);
    assert_eq!(value["applied_revision"], 2);
    assert!(!value.to_string().contains(secret));
    assert_eq!(
        request(
            &state,
            "POST",
            "/api/admin/v1/auth/logout",
            json!({}),
            Some(&cookie),
            Some(&csrf),
            Some(ORIGIN)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &state,
            "GET",
            "/api/admin/v1/config",
            Value::Null,
            Some(&cookie),
            None,
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn conversation_owner_isolation() {
    let temp = TempDir::new().unwrap();
    let state = open(temp.path());
    let (cookie, csrf) = bootstrap(&state).await;
    state.db(|db|{db.execute("INSERT INTO conversations(id,owner_id,title,created_at) VALUES('other-conversation','other-user','Private',?1)",[now()])?;Ok(())}).unwrap();
    assert_eq!(
        request(
            &state,
            "GET",
            "/api/admin/v1/conversations/other-conversation/messages",
            Value::Null,
            Some(&cookie),
            None,
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(
            &state,
            "POST",
            "/api/admin/v1/conversations/other-conversation/messages",
            json!({"content":"read","client_message_id":"1"}),
            Some(&cookie),
            Some(&csrf),
            Some(ORIGIN)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

async fn mock_model() -> (String, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>) {
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let log = requests.clone();
    let router = axum::Router::new().route(
        "/chat/completions",
        axum::routing::post(move |Json(body): Json<Value>| {
            let log = log.clone();
            async move {
                let n = {
                    let mut values = log.lock().unwrap();
                    values.push(body);
                    values.len()
                };
                tokio::time::sleep(Duration::from_millis(40)).await;
                Json(json!({"choices":[{"message":{"content":format!("fixture-reply-{n}")}}]}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (format!("http://{address}"), requests, task)
}
fn configure_model(state: &AppState, base: &str) {
    let reference = uuid::Uuid::new_v4().to_string();
    private_write(
        &state.0.options.data_dir.join("secrets").join(&reference),
        b"fixture-api-key",
    )
    .unwrap();
    let mut config = RuntimeConfig::default();
    config.model.base_url = base.into();
    config.model.credential_ref = Some(reference.clone());
    state
        .db(|db| {
            db.execute(
                "INSERT INTO secrets(id,kind,created_at) VALUES(?1,'model_api_key',?2)",
                params![reference, now()],
            )?;
            db.execute(
                "UPDATE config_revisions SET config_json=?1 WHERE revision=1",
                [serde_json::to_string(&config)?],
            )?;
            Ok(())
        })
        .unwrap();
}
async fn wait_completed(state: &AppState, id: &str) {
    for _ in 0..200 {
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
            return;
        }
        assert!(
            ["queued", "running"].contains(&status.as_str()),
            "unexpected {status}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("run did not complete");
}

#[tokio::test]
async fn queued_history_orders_runs_and_deduplicates_after_restart() {
    let temp = TempDir::new().unwrap();
    let state = open(temp.path());
    let (base, requests, mock) = mock_model().await;
    configure_model(&state, &base);
    state.db(|db|{db.execute("INSERT INTO conversations(id,owner_id,title,created_at) VALUES('conversation','owner','Test',?1)",[now()])?;Ok(())}).unwrap();
    let first = worker::enqueue(
        &state,
        "conversation",
        "owner",
        "first",
        "question-one",
        None,
    )
    .unwrap();
    let duplicate = worker::enqueue(
        &state,
        "conversation",
        "owner",
        "first",
        "question-one",
        None,
    )
    .unwrap();
    assert_eq!(first, duplicate);
    let second = worker::enqueue(
        &state,
        "conversation",
        "owner",
        "second",
        "question-two",
        None,
    )
    .unwrap();
    drop(state);
    let state = open(temp.path());
    let cancel = CancellationToken::new();
    let cloned = state.clone();
    let token = cancel.clone();
    let task = tokio::spawn(async move { worker::run_worker(cloned, token).await });
    wait_completed(&state, &second).await;
    cancel.cancel();
    task.await.unwrap();
    let values = requests.lock().unwrap();
    assert_eq!(values.len(), 2);
    assert!(
        !values[0].to_string().contains("question-two"),
        "future queued input leaked into earlier context"
    );
    assert!(
        values[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "assistant" && m["content"] == "fixture-reply-1"),
        "previous assistant reply missing from queued second turn"
    );
    let total = state
        .db(|db| Ok(db.query_row("SELECT count(*) FROM messages", [], |r| r.get::<_, i64>(0))?))
        .unwrap();
    assert_eq!(total, 4);
    mock.abort();
}

#[test]
fn interrupted_requests_are_not_blindly_replayed() {
    let temp = TempDir::new().unwrap();
    let state = open(temp.path());
    state
        .db(|db| {
            db.execute(
                "INSERT INTO conversations(id,owner_id,title,created_at) VALUES('c','u','Test',?1)",
                [now()],
            )?;
            Ok(())
        })
        .unwrap();
    let run = worker::enqueue(&state, "c", "u", "1", "hello", None).unwrap();
    state
        .db(|db| {
            db.execute("UPDATE runs SET status='running' WHERE id=?1", [&run])?;
            Ok(())
        })
        .unwrap();
    drop(state);
    let restored = open(temp.path());
    let status = restored
        .db(|db| {
            Ok(
                db.query_row("SELECT status FROM runs WHERE id=?1", [run], |r| {
                    r.get::<_, String>(0)
                })?,
            )
        })
        .unwrap();
    assert_eq!(status, "interrupted");
}

#[test]
fn qq_inbox_run_and_message_commit_atomically() {
    let temp = TempDir::new().unwrap();
    let state = open(temp.path());
    let mut config = RuntimeConfig::default();
    config.qq.enabled = true;
    config.qq.app_id = "12345".into();
    config.qq.credential_ref = Some(uuid::Uuid::new_v4().to_string());
    let event = json!({"op":0,"id":"event-1","t":"C2C_MESSAGE_CREATE","d":{"id":"message-1","author":{"user_openid":"user1"},"content":"hello"}});
    state.db(|db|{db.execute_batch("CREATE TRIGGER fail_run BEFORE INSERT ON runs BEGIN SELECT RAISE(ABORT,'fixture'); END;")?;Ok(())}).unwrap();
    assert!(qq::ingest(&state, &config, &event).is_err());
    let count = state
        .db(|db| Ok(db.query_row("SELECT count(*) FROM inbox", [], |r| r.get::<_, i64>(0))?))
        .unwrap();
    assert_eq!(count, 0);
    state
        .db(|db| {
            db.execute_batch("DROP TRIGGER fail_run;")?;
            Ok(())
        })
        .unwrap();
    qq::ingest(&state, &config, &event).unwrap();
    qq::ingest(&state, &config, &event).unwrap();
    for table in ["inbox", "runs", "messages"] {
        let count = state
            .db(|db| {
                Ok(
                    db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(count, 1, "{table}");
    }
}

#[test]
fn qq_signature_rejects_tampering() {
    let secret = "fixture-secret-for-signature";
    let timestamp = "1725442341";
    let body = b"{\"op\":0}";
    let bytes = secret.as_bytes();
    let mut seed = [0u8; 32];
    for (i, value) in seed.iter_mut().enumerate() {
        *value = bytes[i % bytes.len()];
    }
    let key = SigningKey::from_bytes(&seed);
    let mut message = timestamp.as_bytes().to_vec();
    message.extend_from_slice(body);
    let signature = hex::encode(key.sign(&message).to_bytes());
    assert!(qq::verify_signature(secret, timestamp, &signature, body));
    assert!(!qq::verify_signature(
        secret,
        timestamp,
        &signature,
        b"tampered"
    ));
}

#[test]
fn qq_gateway_text_and_binary_share_bounded_json_parser() {
    use tokio_tungstenite::tungstenite::Message;
    let payload = r#"{"op":0,"t":"READY","d":{"session_id":"fixture-only"}}"#;
    assert_eq!(
        qq::parse_gateway_message(&Message::Text(payload.into())).unwrap(),
        qq::parse_gateway_message(&Message::Binary(payload.as_bytes().to_vec().into())).unwrap()
    );
    assert!(qq::parse_gateway_message(&Message::Binary(vec![0xff, 0xfe].into())).is_err());
    assert!(
        qq::parse_gateway_message(&Message::Binary(vec![b' '; 128 * 1024 + 1].into())).is_err()
    );
    assert!(qq::parse_gateway_message(&Message::Text("x".repeat(128 * 1024 + 1).into())).is_err());
    assert_eq!(
        qq::parse_gateway_message(&Message::Ping(vec![1].into())).unwrap(),
        None
    );
}

#[test]
fn qq_gateway_close_codes_control_recovery_without_reason_text() {
    let auth = qq::close_policy(4004);
    assert!(auth.clear_token);
    assert!(!auth.clear_session);
    assert!(!auth.terminal);
    for code in [4006, 4007, 4009, 4900, 4913] {
        let action = qq::close_policy(code);
        assert!(action.clear_token && action.clear_session);
        assert!(!action.terminal);
    }
    assert_eq!(qq::close_policy(4008).retry_delay_seconds, Some(60));
    for code in [4914, 4915] {
        assert!(qq::close_policy(code).terminal);
    }
    let regular = qq::close_policy(1000);
    assert!(!regular.clear_token && !regular.clear_session && !regular.terminal);
}

#[tokio::test]
async fn qq_url_validation_allows_unsigned_challenge_but_not_unsigned_events() {
    let temp = TempDir::new().unwrap();
    let state = open(temp.path());
    let reference = uuid::Uuid::new_v4().to_string();
    private_write(
        &temp.path().join("secrets").join(&reference),
        b"fixture-qq-secret",
    )
    .unwrap();
    let mut config = RuntimeConfig::default();
    config.qq.enabled = true;
    config.qq.app_id = "12345".into();
    config.qq.transport = "webhook".into();
    config.qq.credential_ref = Some(reference.clone());
    state
        .db(|db| {
            db.execute(
                "INSERT INTO secrets(id,kind,created_at) VALUES(?1,'qq_app_secret',?2)",
                params![reference, now()],
            )?;
            db.execute(
                "UPDATE config_revisions SET config_json=?1 WHERE revision=1",
                [serde_json::to_string(&config)?],
            )?;
            Ok(())
        })
        .unwrap();
    let (status, value, _) = request(
        &state,
        "POST",
        "/webhooks/qq",
        json!({"op":13,"d":{"plain_token":"fixture-token-123","event_ts":now().to_string()}}),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["plain_token"], "fixture-token-123");
    assert_eq!(value["signature"].as_str().unwrap().len(), 128);
    let (status, _, _) = request(
        &state,
        "POST",
        "/webhooks/qq",
        json!({"op":0,"t":"C2C_MESSAGE_CREATE","d":{}}),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = request(
        &state,
        "POST",
        "/webhooks/qq",
        json!({"op":13,"d":{"plain_token":"{\"op\":0}","event_ts":now().to_string()}}),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[test]
fn model_urls_reject_secrets_and_private_networks_by_default() {
    use crate::config::validate_model_url;
    assert!(validate_model_url("https://api.deepseek.com", false).is_ok());
    for bad in [
        "http://api.deepseek.com",
        "https://127.0.0.1",
        "https://169.254.169.254",
        "https://localhost",
        "https://name:secret@example.com",
        "https://example.com?api_key=secret",
    ] {
        assert!(validate_model_url(bad, false).is_err(), "{bad}");
    }
    assert!(validate_model_url("http://127.0.0.1:9999", true).is_ok());
}

fn published_qq_card(state: &AppState, name: &str, shared: bool) -> crate::character::CardRef {
    state
        .db(|db| {
            let tx = db.transaction()?;
            let (_, config) = crate::character::current_config_in(&tx)?;
            let draft = crate::character::create(
                &tx,
                "fixture-admin",
                crate::character::template(name, "按当前角色自然回复。"),
            )?;
            let result = crate::character::publish(
                &tx,
                &draft.card_id,
                &draft.draft_id,
                "fixture-admin",
                &draft.draft_etag,
                &uuid::Uuid::new_v4().to_string(),
                &config,
            )?;
            let revision = result["revision"].as_i64().unwrap();
            crate::character::set_qq_access(&tx, &draft.card_id, &revision.to_string(), shared)?;
            tx.commit()?;
            Ok(crate::character::CardRef {
                card_id: draft.card_id,
                revision,
            })
        })
        .unwrap()
}
fn qq_fixture_config() -> RuntimeConfig {
    let mut config = RuntimeConfig::default();
    config.qq.app_id = "12345".into();
    config.qq.credential_ref = Some("fixture-qq-reference".into());
    config
}
fn qq_private_fixture(event: &str, message: &str, user: &str, text: &str) -> Value {
    json!({"op":0,"id":event,"t":"C2C_MESSAGE_CREATE","d":{"id":message,"content":text,"author":{"user_openid":user}}})
}

#[test]
fn qq_persona_switch_is_disabled_without_replacing_history_and_duplicate_commands_are_deduplicated()
{
    let temp = TempDir::new().unwrap();
    let state = open(temp.path());
    let config = qq_fixture_config();
    let role = published_qq_card(&state, "角色甲", true);
    qq::ingest(
        &state,
        &config,
        &qq_private_fixture("e0", "m0", "alice", "旧会话消息"),
    )
    .unwrap();
    let old: String = state
        .db(|db| {
            Ok(db.query_row(
                "SELECT id FROM conversations WHERE source_key='qq:12345:c2c:alice'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    let command = format!("/角色 切换 {}", role.card_id);
    qq::ingest(
        &state,
        &config,
        &qq_private_fixture("e1", "m1", "alice", &command),
    )
    .unwrap();
    let new: String = state
        .db(|db| {
            Ok(db.query_row(
                "SELECT id FROM conversations WHERE source_key='qq:12345:c2c:alice'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(old, new);
    qq::ingest(
        &state,
        &config,
        &qq_private_fixture("e1-duplicate-dispatch", "m1", "alice", &command),
    )
    .unwrap();
    let after: String = state
        .db(|db| {
            Ok(db.query_row(
                "SELECT id FROM conversations WHERE source_key='qq:12345:c2c:alice'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(new, after);
    qq::ingest(
        &state,
        &config,
        &qq_private_fixture("e2", "m2", "alice", "介绍一下自己"),
    )
    .unwrap();
    qq::ingest(
        &state,
        &config,
        &qq_private_fixture("e3", "m3", "bob", "别人的会话"),
    )
    .unwrap();
    state.db(|db| {
        let snapshot:String=db.query_row("SELECT character_snapshot_json FROM runs WHERE owner_id='qq:12345:c2c:alice' AND client_message_id='m2'",[],|r|r.get(0))?;
        let snapshot:Value=serde_json::from_str(&snapshot)?;
        assert_eq!(snapshot["card_id"],crate::character::PERSONALITY_ID); assert_eq!(snapshot["revision"],0);
        assert!(!snapshot["system_prompt"].as_str().unwrap().contains("角色甲"));
        assert_eq!(db.query_row("SELECT count(*) FROM outbox WHERE content LIKE '%不再支持角色切换%'",[],|r|r.get::<_,i64>(0))?,1);
        assert_eq!(db.query_row("SELECT run_kind FROM runs WHERE client_message_id='m1'",[],|r|r.get::<_,String>(0))?,"character_command");
        assert_eq!(db.query_row("SELECT count(*) FROM conversation_character_bindings b JOIN conversations c ON c.id=b.conversation_id WHERE c.owner_id='qq:12345:c2c:bob'",[],|r|r.get::<_,i64>(0))?,0);
        assert!(crate::character::default_reference(db)?.is_none());
        assert_eq!(db.query_row("SELECT count(*) FROM runs WHERE conversation_id=?1 AND client_message_id='m0'",[old],|r|r.get::<_,i64>(0))?,1);
        Ok(())
    }).unwrap();
    drop(state);
    let state = open(temp.path());
    state
        .db(|db| {
            assert_eq!(
                crate::character::connection_character(db, &new)?,
                Value::Null
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn qq_unshared_cards_and_group_persona_commands_do_not_change_bindings() {
    let temp = TempDir::new().unwrap();
    let state = open(temp.path());
    let config = qq_fixture_config();
    let role = published_qq_card(&state, "私有角色", false);
    qq::ingest(
        &state,
        &config,
        &qq_private_fixture("p1", "p1", "alice", &format!("/角色 切换 {}", role.card_id)),
    )
    .unwrap();
    let event = json!({"op":0,"id":"g1","t":"GROUP_AT_MESSAGE_CREATE","d":{"id":"g1","content":"/角色 切换 私有角色","group_openid":"group1","author":{"member_openid":"alice"}}});
    qq::ingest(&state, &config, &event).unwrap();
    state
        .db(|db| {
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM conversation_character_bindings",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM runs WHERE run_kind='model'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM outbox WHERE content LIKE '每个实例仅保留一种性格%'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            assert!(crate::character::list_published(db)?.is_empty());
            Ok(())
        })
        .unwrap();
}

#[test]
fn legacy_model_preferences_do_not_override_instance_routing_or_reenable_switching() {
    let temp = TempDir::new().unwrap();
    let state = open(temp.path());
    let qq_config = qq_fixture_config();
    let fallback = published_qq_card(&state, "可恢复角色", true);
    state
        .db(|db| {
            let tx = db.transaction()?;
            let (_, config) = crate::character::current_config_in(&tx)?;
            let mut card = crate::character::template("偏好角色", "简短回应。");
            card["data"]["extensions"]["dsh"]["modules"]["chat"] =
                json!({"mode":"prefer","provider_hint":"deepseek","model_hint":config.model.model});
            let draft = crate::character::create(&tx, "fixture-admin", card)?;
            let mapped = crate::character::map_draft_model(
                &tx,
                &draft.card_id,
                &draft.draft_id,
                "fixture-admin",
                &draft.draft_etag,
                &config,
            )?;
            let published = crate::character::publish(
                &tx,
                &draft.card_id,
                &draft.draft_id,
                "fixture-admin",
                &mapped.draft_etag,
                "prefer-fixture-key",
                &config,
            )?;
            let reference = crate::character::CardRef {
                card_id: draft.card_id,
                revision: published["revision"].as_i64().unwrap(),
            };
            crate::character::set_default(&tx, &reference, "none", &config)?;
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    qq::ingest(
        &state,
        &qq_config,
        &qq_private_fixture("r0", "r0", "alice", "建立偏好角色会话"),
    )
    .unwrap();
    state
        .db(|db| {
            let (_, mut config) = crate::character::current_config_in(db)?;
            config.model.model = "different-model".into();
            db.execute(
                "UPDATE config_revisions SET config_json=?1 WHERE revision=1",
                [serde_json::to_string(&config)?],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(qq::ingest(
        &state,
        &qq_config,
        &qq_private_fixture("rx", "rx", "alice", "历史角色模型偏好不再覆盖实例模型")
    )
    .is_ok());
    qq::ingest(
        &state,
        &qq_config,
        &qq_private_fixture("r1", "r1", "alice", "/角色"),
    )
    .unwrap();
    qq::ingest(
        &state,
        &qq_config,
        &qq_private_fixture("r2", "r2", "alice", "/角色 当前"),
    )
    .unwrap();
    qq::ingest(
        &state,
        &qq_config,
        &qq_private_fixture(
            "r3",
            "r3",
            "alice",
            &format!("/角色 切换 {}", fallback.card_id),
        ),
    )
    .unwrap();
    qq::ingest(
        &state,
        &qq_config,
        &qq_private_fixture("r4", "r4", "alice", "恢复后可以继续对话"),
    )
    .unwrap();
    state.db(|db| {
        assert_eq!(db.query_row("SELECT count(*) FROM runs WHERE run_kind='character_command' AND status='completed'",[],|r|r.get::<_,i64>(0))?,3);
        assert_eq!(db.query_row("SELECT count(*) FROM inbox WHERE event_key LIKE '%:rx'",[],|r|r.get::<_,i64>(0))?,1);
        let id:String=db.query_row("SELECT id FROM conversations WHERE source_key='qq:12345:c2c:alice'",[],|r|r.get(0))?;
        let snapshot = crate::character::freeze_for_run(db,&id,1,&crate::character::current_config_in(db)?.1,"用户")?.unwrap();
        assert_eq!(snapshot.card_id, crate::character::PERSONALITY_ID);
        assert_eq!(snapshot.name,"偏好角色");
        Ok(())
    }).unwrap();
}
