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
