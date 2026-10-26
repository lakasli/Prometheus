use super::*;
use crate::{
    context_budget::{BlockKind, ContextBlock},
    state::{token_hash, Options},
};
use axum::body::Body;
use tower::ServiceExt;

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
    state
        .db(|db| {
            budget::ensure_schema(db)?;
            conversation_state::initialize(db)?;
            for (id, name, token) in [("a", "budget-admin-a", '1'), ("b", "budget-admin-b", '2')] {
                db.execute(
                    "INSERT INTO admins VALUES(?1,?2,'unused',0)",
                    params![id, name],
                )?;
                db.execute(
                    "INSERT INTO sessions VALUES(?1,?2,'budget-csrf',?3)",
                    params![token_hash(&token.to_string().repeat(64)), id, now() + 3600],
                )?;
            }
            Ok(())
        })
        .unwrap();
    (directory, state)
}

struct Reply {
    status: u16,
    body: Value,
    headers: HeaderMap,
    raw: String,
}

async fn call(
    state: &AppState,
    method: &str,
    path: &str,
    body: Option<Value>,
    token: Option<char>,
    headers: &[(&str, &str)],
) -> Reply {
    let mut request = axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if !headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("host"))
    {
        request = request.header("host", "localhost");
    }
    if let Some(token) = token {
        request = request.header(
            "cookie",
            format!("dsh_session={}", token.to_string().repeat(64)),
        );
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = router()
        .with_state(state.clone())
        .oneshot(
            request
                .body(
                    body.map(|value| Body::from(value.to_string()))
                        .unwrap_or_else(Body::empty),
                )
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 512 * 1024)
        .await
        .unwrap();
    let raw = String::from_utf8(bytes.to_vec()).unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(json!({"non_json_rejection":true}));
    Reply {
        status,
        body,
        headers,
        raw,
    }
}

fn proposed(state: &AppState) -> Value {
    state
        .db(|db| {
            let tx = db.transaction()?;
            let (_, config) = character::current_config_in(&tx)?;
            Ok(serde_json::to_value(budget::policy_in(
                &tx,
                &config.model.model,
            )?)?)
        })
        .unwrap()
}

fn write_headers(version: &str) -> [(&str, &str); 3] {
    [
        ("origin", "http://localhost"),
        ("x-csrf-token", "budget-csrf"),
        ("if-match", version),
    ]
}

fn source(state: &AppState, id: &str, owner: &str, record_selection: bool, bind: bool) {
    state.db(|db|{
        let tx=db.transaction()?;
        let (_,config)=character::current_config_in(&tx)?;
        let channel=owner.starts_with("qq:")||owner.starts_with("weixin:");
        tx.execute("INSERT INTO conversations(id,owner_id,title,source_key,created_at) VALUES(?1,?2,'synthetic budget source',?3,?4)",params![id,owner,channel.then_some(owner),now()])?;
        tx.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,created_at,updated_at,run_kind) VALUES(?1,?1,?2,?1,'PRIVATE_CURRENT_SENTINEL',?3,'running',?4,?4,'model')",params![id,owner,serde_json::to_string(&config)?,now()])?;
        tx.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES(?1,?2,?2,'user','PRIVATE_CURRENT_SENTINEL',?3)",params![format!("{id}-message"),id,now()])?;
        if bind {crate::conversation_identity::freeze_run_in(&tx,id)?;}
        if record_selection {
            let blocks=vec![
                ContextBlock::new("host",BlockKind::HostPolicy,true,100,vec![json!({"role":"system","content":"PRIVATE_HOST_POLICY_SENTINEL"})]),
                ContextBlock::new("current",BlockKind::CurrentInput,true,100,vec![json!({"role":"user","content":"PRIVATE_CURRENT_SENTINEL"})]),
            ];
            let policy=budget::policy_in(&tx,&config.model.model)?;
            assert!(budget::compile_in(&tx,id,&policy,1_024,&blocks,1)?.accepted);
        }
        tx.commit()?;
        Ok(())
    }).unwrap();
}

#[tokio::test]
async fn default_policy_requires_a_real_admin_and_reports_unknown_window_with_revision_zero() {
    let (_dir, state) = fixture();
    assert_eq!(
        call(
            &state,
            "GET",
            "/api/admin/v1/context-policy",
            None,
            None,
            &[]
        )
        .await
        .status,
        401
    );
    let response = call(
        &state,
        "GET",
        "/api/admin/v1/context-policy",
        None,
        Some('1'),
        &[],
    )
    .await;
    assert_eq!(response.status, 200);
    assert_eq!(response.headers[header::ETAG], "\"0\"");
    assert_eq!(response.headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(response.body["policy"]["revision"], 0);
    assert!(response.body["policy"]["provider_context_window"].is_null());
    assert!(response.body["policy"]["provider_fingerprint"].is_null());
    assert_eq!(response.body["window_status"], "unknown");
    assert_eq!(response.body["token_counts_exact"], false);
    assert_eq!(response.body["host_limits"]["max_input_bytes"], 96_000);
    assert_eq!(response.body["host_limits"]["max_response_tokens"], 4_096);
    assert!(!response.raw.contains("https://api.deepseek.com"));
}

#[tokio::test]
async fn standalone_router_enforces_host_exact_origin_and_csrf_before_any_policy_mutation() {
    let (_dir, state) = fixture();
    let body = proposed(&state);
    for headers in [
        vec![("if-match", "\"0\""), ("x-csrf-token", "budget-csrf")],
        vec![
            ("origin", "http://foreign"),
            ("if-match", "\"0\""),
            ("x-csrf-token", "budget-csrf"),
        ],
        vec![("origin", "http://localhost"), ("if-match", "\"0\"")],
        vec![
            ("host", "foreign"),
            ("origin", "http://localhost"),
            ("if-match", "\"0\""),
            ("x-csrf-token", "budget-csrf"),
        ],
    ] {
        assert_eq!(
            call(
                &state,
                "PUT",
                "/api/admin/v1/context-policy",
                Some(body.clone()),
                Some('1'),
                &headers
            )
            .await
            .status,
            403
        );
    }
    assert_eq!(
        call(
            &state,
            "PUT",
            "/api/admin/v1/context-policy",
            Some(body),
            None,
            &write_headers("\"0\"")
        )
        .await
        .status,
        401
    );
    assert_eq!(
        state
            .db(|db| Ok(db.query_row(
                "SELECT count(*) FROM context_budget_policy_versions",
                [],
                |r| r.get::<_, i64>(0)
            )?))
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn writes_require_one_canonical_if_match_and_conflicts_do_not_add_an_audit_event() {
    let (_dir, state) = fixture();
    let body = proposed(&state);
    let headers = [
        ("origin", "http://localhost"),
        ("x-csrf-token", "budget-csrf"),
    ];
    assert_eq!(
        call(
            &state,
            "PUT",
            "/api/admin/v1/context-policy",
            Some(body.clone()),
            Some('1'),
            &headers
        )
        .await
        .status,
        428
    );
    for version in [
        "0",
        "*",
        "W/\"0\"",
        "\"0\",\"1\"",
        "\"00\"",
        "\"+0\"",
        "\"-1\"",
        " \"0\" ",
    ] {
        assert_eq!(
            call(
                &state,
                "PUT",
                "/api/admin/v1/context-policy",
                Some(body.clone()),
                Some('1'),
                &write_headers(version)
            )
            .await
            .status,
            400,
            "{version}"
        );
    }
    let duplicate = [
        ("origin", "http://localhost"),
        ("x-csrf-token", "budget-csrf"),
        ("if-match", "\"0\""),
        ("if-match", "\"0\""),
    ];
    assert_eq!(
        call(
            &state,
            "PUT",
            "/api/admin/v1/context-policy",
            Some(body.clone()),
            Some('1'),
            &duplicate
        )
        .await
        .status,
        400
    );
    let saved = call(
        &state,
        "PUT",
        "/api/admin/v1/context-policy",
        Some(body.clone()),
        Some('1'),
        &write_headers("\"0\""),
    )
    .await;
    assert_eq!(saved.status, 200);
    assert_eq!(saved.headers[header::ETAG], "\"1\"");
    assert_eq!(saved.body["policy"]["revision"], 1);
    assert_eq!(
        call(
            &state,
            "PUT",
            "/api/admin/v1/context-policy",
            Some(body),
            Some('1'),
            &write_headers("\"0\"")
        )
        .await
        .status,
        409
    );
    assert_eq!(
        state
            .db(|db| Ok(db.query_row(
                "SELECT count(*) FROM audit_events WHERE action='context_policy.update'",
                [],
                |r| r.get::<_, i64>(0)
            )?))
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn declared_windows_bind_to_the_servers_current_endpoint_and_unknown_is_never_verified() {
    let (_dir, state) = fixture();
    let view = call(
        &state,
        "GET",
        "/api/admin/v1/context-policy",
        None,
        Some('1'),
        &[],
    )
    .await
    .body;
    let mut body = view["policy"].clone();
    body["provider_context_window"] = json!(32_000);
    body["window_basis"] = json!("administrator_declared");
    body["provider_fingerprint"] = json!("a".repeat(64));
    assert_eq!(
        call(
            &state,
            "PUT",
            "/api/admin/v1/context-policy",
            Some(body.clone()),
            Some('1'),
            &write_headers("\"0\"")
        )
        .await
        .status,
        422
    );
    body["provider_fingerprint"] = view["current_provider_fingerprint"].clone();
    let result = call(
        &state,
        "PUT",
        "/api/admin/v1/context-policy",
        Some(body),
        Some('1'),
        &write_headers("\"0\""),
    )
    .await;
    assert_eq!(result.status, 200);
    assert_eq!(result.body["window_status"], "administrator_declared");
    assert_eq!(
        result.body["policy"]["provider_fingerprint"],
        view["current_provider_fingerprint"]
    );
    let mut unknown = result.body["policy"].clone();
    unknown["provider_context_window"] = Value::Null;
    unknown["provider_fingerprint"] = Value::Null;
    unknown["window_basis"] = json!("verified");
    assert_eq!(
        call(
            &state,
            "PUT",
            "/api/admin/v1/context-policy",
            Some(unknown.clone()),
            Some('1'),
            &write_headers("\"1\"")
        )
        .await
        .status,
        422
    );
    unknown["window_basis"] = json!("unknown");
    let reset = call(
        &state,
        "PUT",
        "/api/admin/v1/context-policy",
        Some(unknown),
        Some('1'),
        &write_headers("\"1\""),
    )
    .await;
    assert_eq!(reset.status, 200);
    assert_eq!(reset.body["window_status"], "unknown");
    assert!(reset.body["policy"]["provider_context_window"].is_null());
}

#[tokio::test]
async fn same_name_endpoint_changes_are_visible_and_require_an_explicit_new_binding() {
    let (_dir, state) = fixture();
    let initial = call(
        &state,
        "GET",
        "/api/admin/v1/context-policy",
        None,
        Some('1'),
        &[],
    )
    .await
    .body;
    let mut declared = initial["policy"].clone();
    declared["provider_context_window"] = json!(32_000);
    declared["window_basis"] = json!("administrator_declared");
    declared["provider_fingerprint"] = initial["current_provider_fingerprint"].clone();
    let saved = call(
        &state,
        "PUT",
        "/api/admin/v1/context-policy",
        Some(declared),
        Some('1'),
        &write_headers("\"0\""),
    )
    .await;
    assert_eq!(saved.status, 200);
    state
        .db(|db| {
            let tx = db.transaction()?;
            let (revision, mut config) = character::current_config_in(&tx)?;
            config.model.base_url = "https://another-provider.invalid/v1".into();
            tx.execute(
                "INSERT INTO config_revisions(revision,config_json,created_at) VALUES(?1,?2,?3)",
                params![revision + 1, serde_json::to_string(&config)?, now()],
            )?;
            tx.execute(
                "UPDATE settings SET value=?1 WHERE key='active_revision'",
                [(revision + 1).to_string()],
            )?;
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    let changed = call(
        &state,
        "GET",
        "/api/admin/v1/context-policy",
        None,
        Some('1'),
        &[],
    )
    .await;
    assert_eq!(changed.status, 200);
    assert_eq!(changed.body["window_status"], "provider_binding_changed");
    assert_eq!(changed.body["applied"], false);
    let old = saved.body["policy"].clone();
    assert_eq!(
        call(
            &state,
            "PUT",
            "/api/admin/v1/context-policy",
            Some(old.clone()),
            Some('1'),
            &write_headers("\"1\"")
        )
        .await
        .status,
        422
    );
    let mut rebound = old;
    rebound["provider_fingerprint"] = changed.body["current_provider_fingerprint"].clone();
    let rebound = call(
        &state,
        "PUT",
        "/api/admin/v1/context-policy",
        Some(rebound),
        Some('1'),
        &write_headers("\"1\""),
    )
    .await;
    assert_eq!(rebound.status, 200);
    assert_eq!(rebound.body["policy"]["revision"], 2);
    assert_eq!(rebound.body["applied"], true);
}

#[tokio::test]
async fn only_the_current_model_and_hard_host_limits_can_be_saved_without_runtime_or_personality_changes(
) {
    let (_dir, state) = fixture();
    let before = state.current_config().unwrap();
    let personality = state
        .db(|db| Ok(character::personality_state(db, &before.1)?))
        .unwrap();
    let original = proposed(&state);
    for (field, value) in [
        ("model_id", json!("other-model")),
        ("max_input_bytes", json!(96_001)),
        ("estimated_input_limit", json!(96_001)),
        ("response_reserve_tokens", json!(4_097)),
        ("safety_reserve_tokens", json!(0)),
    ] {
        let mut body = original.clone();
        body[field] = value;
        assert_eq!(
            call(
                &state,
                "PUT",
                "/api/admin/v1/context-policy",
                Some(body),
                Some('1'),
                &write_headers("\"0\"")
            )
            .await
            .status,
            422,
            "{field}"
        );
    }
    let mut body = original;
    body["max_input_bytes"] = json!(80_000);
    assert_eq!(
        call(
            &state,
            "PUT",
            "/api/admin/v1/context-policy",
            Some(body),
            Some('1'),
            &write_headers("\"0\"")
        )
        .await
        .status,
        200
    );
    let after = state.current_config().unwrap();
    assert_eq!(before.0, after.0);
    assert_eq!(
        serde_json::to_value(before.1).unwrap(),
        serde_json::to_value(after.1.clone()).unwrap()
    );
    assert_eq!(
        state
            .db(|db| Ok(character::personality_state(db, &after.1)?))
            .unwrap(),
        personality
    );
}

#[tokio::test]
async fn policy_save_rolls_back_when_the_same_transaction_cannot_write_its_audit() {
    let (_dir, state) = fixture();
    state.db(|db|{db.execute_batch("CREATE TRIGGER synthetic_audit_failure BEFORE INSERT ON audit_events WHEN NEW.action='context_policy.update' BEGIN SELECT RAISE(ABORT,'synthetic audit refusal'); END;")?;Ok(())}).unwrap();
    assert_eq!(
        call(
            &state,
            "PUT",
            "/api/admin/v1/context-policy",
            Some(proposed(&state)),
            Some('1'),
            &write_headers("\"0\"")
        )
        .await
        .status,
        500
    );
    assert_eq!(proposed(&state)["revision"], 0);
    assert_eq!(
        state
            .db(|db| Ok(db.query_row(
                "SELECT count(*) FROM context_budget_policy_versions",
                [],
                |r| r.get::<_, i64>(0)
            )?))
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn selection_is_metadata_only_and_matches_own_web_and_managed_channel_audit_boundaries() {
    let (_dir, state) = fixture();
    source(&state, "own", "a", true, true);
    source(&state, "foreign", "b", true, true);
    source(&state, "qq", "qq:synthetic:c2c:peer", true, true);
    source(&state, "weixin", "weixin:synthetic:c2c:peer", true, true);
    for run in ["own", "foreign", "qq", "weixin"] {
        let route = format!("/api/admin/v1/runs/{run}/context-selection");
        assert_eq!(
            call(&state, "GET", &route, None, None, &[]).await.status,
            401
        );
        let response = call(&state, "GET", &route, None, Some('1'), &[]).await;
        assert_eq!(response.status, if run == "foreign" { 404 } else { 200 });
        assert!(!response.raw.contains("PRIVATE_CURRENT_SENTINEL"));
        assert!(!response.raw.contains("PRIVATE_HOST_POLICY_SENTINEL"));
        if run != "foreign" {
            assert_eq!(response.body["available"], true);
            assert_eq!(response.body["selection"]["token_counts_exact"], false);
            assert_eq!(response.headers[header::CACHE_CONTROL], "no-store");
        }
    }
    assert_eq!(
        call(
            &state,
            "GET",
            "/api/admin/v1/runs/foreign/context-selection",
            None,
            Some('2'),
            &[]
        )
        .await
        .status,
        200
    );
    assert_eq!(
        call(
            &state,
            "GET",
            "/api/admin/v1/runs/missing/context-selection",
            None,
            Some('1'),
            &[]
        )
        .await
        .status,
        404
    );
}

#[tokio::test]
async fn an_admin_id_looking_like_a_channel_does_not_bypass_another_admins_privacy() {
    let (_dir, state) = fixture();
    let hidden = "qq:synthetic:c2c:hidden-admin";
    state
        .db(|db| {
            db.execute(
                "INSERT INTO admins VALUES(?1,'hidden-admin','unused',0)",
                [hidden],
            )?;
            Ok(())
        })
        .unwrap();
    source(&state, "hidden", hidden, true, true);
    assert_eq!(
        call(
            &state,
            "GET",
            "/api/admin/v1/runs/hidden/context-selection",
            None,
            Some('1'),
            &[]
        )
        .await
        .status,
        404
    );
}

#[tokio::test]
async fn missing_legacy_selection_does_not_backfill_prompt_binding_or_provider_proof() {
    let (_dir, state) = fixture();
    source(&state, "legacy", "a", false, false);
    state.db(|db|{db.execute("INSERT INTO run_prompt_snapshots(run_id,messages_json,digest,compiler_version,created_at) VALUES('legacy','PRIVATE_OLD_PROMPT','old','historical',0)",[])?;Ok(())}).unwrap();
    let result = call(
        &state,
        "GET",
        "/api/admin/v1/runs/legacy/context-selection",
        None,
        Some('1'),
        &[],
    )
    .await;
    assert_eq!(result.status, 200);
    assert_eq!(result.body["available"], false);
    assert_eq!(result.body["reason"], "context_budget_not_recorded");
    assert!(!result.raw.contains("PRIVATE_OLD_PROMPT"));
    assert_eq!(state.db(|db|Ok(db.query_row("SELECT (SELECT count(*) FROM context_budget_snapshots)+(SELECT count(*) FROM run_conversation_bindings)+(SELECT count(*) FROM run_conversation_contexts)",[],|r|r.get::<_,i64>(0))?)).unwrap(),0);
}

#[tokio::test]
async fn stored_selection_without_an_existing_source_binding_cannot_mint_a_new_one_on_read() {
    let (_dir, state) = fixture();
    source(&state, "unbound", "a", true, false);
    assert_eq!(
        call(
            &state,
            "GET",
            "/api/admin/v1/runs/unbound/context-selection",
            None,
            Some('1'),
            &[]
        )
        .await
        .status,
        404
    );
    assert_eq!(
        state
            .db(|db| Ok(db.query_row(
                "SELECT count(*) FROM run_conversation_bindings",
                [],
                |r| r.get::<_, i64>(0)
            )?))
            .unwrap(),
        0
    );
}
