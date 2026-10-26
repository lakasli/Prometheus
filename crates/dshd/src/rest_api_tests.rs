use super::*;
use axum::body::Body;
use tower::ServiceExt;

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
    state
        .db(|db| {
            for (id, name, token) in [("a", "rest-admin-a", '1'), ("b", "rest-admin-b", '2')] {
                db.execute(
                    "INSERT INTO admins VALUES(?1,?2,'unused',0)",
                    params![id, name],
                )?;
                db.execute(
                    "INSERT INTO sessions VALUES(?1,?2,'rest-csrf',?3)",
                    params![
                        crate::state::token_hash(&token.to_string().repeat(64)),
                        id,
                        now() + 3600
                    ],
                )?;
            }
            db.execute("UPDATE trigger_runtime SET clock_trusted=1 WHERE id=1", [])?;
            Ok(())
        })
        .unwrap();
    (directory, state)
}

struct Reply {
    status: u16,
    value: Value,
    headers: HeaderMap,
    raw: String,
}
async fn call(
    state: &AppState,
    method: &str,
    path: &str,
    body: Value,
    token: Option<char>,
    extra: &[(&str, &str)],
) -> Reply {
    let mut request = axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if !extra
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
    for (name, value) in extra {
        request = request.header(*name, *value);
    }
    let reply = router()
        .with_state(state.clone())
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = reply.status().as_u16();
    let headers = reply.headers().clone();
    let bytes = axum::body::to_bytes(reply.into_body(), 256 * 1024)
        .await
        .unwrap();
    let raw = String::from_utf8(bytes.to_vec()).unwrap();
    let value = serde_json::from_str(&raw).unwrap_or(json!({"non_json_rejection":true}));
    Reply {
        status,
        value,
        headers,
        raw,
    }
}
fn writes<'a>(revision: &'a str, key: &'a str) -> [(&'a str, &'a str); 4] {
    [
        ("origin", "http://localhost"),
        ("x-csrf-token", "rest-csrf"),
        ("if-match", revision),
        ("idempotency-key", key),
    ]
}
fn scalar(state: &AppState, sql: &str) -> i64 {
    state
        .db(|db| Ok(db.query_row(sql, [], |r| r.get(0))?))
        .unwrap()
}
fn source(state: &AppState, chat: &str, owner: &str, speaker: Option<&str>) -> String {
    state.db(|db| {db.execute("INSERT OR IGNORE INTO conversations(id,owner_id,title,source_key,created_at)VALUES(?1,?2,'synthetic rest source',?3,?4)",params![chat,owner,(owner.starts_with("qq:")||owner.starts_with("weixin:")).then_some(owner),now()])?;Ok(())}).unwrap();
    let run = crate::worker::enqueue(
        state,
        chat,
        owner,
        &uuid::Uuid::new_v4().to_string(),
        "快去睡觉啦",
        None,
    )
    .unwrap();
    state.db(|db| {
        let tx=db.transaction()?;
        if let Some(speaker)=speaker {crate::conversation_identity::record_qq_message_in(&tx,&run,"GROUP_AT_MESSAGE_CREATE",&json!({"id":format!("synthetic-{run}"),"group_openid":"synthetic-team","author":{"member_openid":speaker}}))?;}
        tx.execute("UPDATE runs SET status='completed',response='synthetic fixture only' WHERE id=?1",[&run])?;
        tx.commit()?;Ok(())
    }).unwrap();
    run
}

#[tokio::test]
async fn defaults_require_actual_admin_and_do_not_enable_natural_or_timed_control() {
    let (_directory, state) = fixture();
    assert_eq!(
        call(&state, "GET", "/api/admin/v1/rest", Value::Null, None, &[])
            .await
            .status,
        401
    );
    let view = call(
        &state,
        "GET",
        "/api/admin/v1/rest",
        Value::Null,
        Some('1'),
        &[],
    )
    .await;
    assert_eq!(view.status, 200);
    assert_eq!(view.value["state"]["phase"], "awake");
    assert_eq!(view.headers[header::ETAG], "\"0\"");
    assert_eq!(view.value["admission"]["management"]["decision"], "allow");
    let policy = call(
        &state,
        "GET",
        "/api/admin/v1/rest/policy",
        Value::Null,
        Some('1'),
        &[],
    )
    .await;
    assert_eq!(policy.status, 200);
    assert_eq!(policy.value["revision"], 1);
    assert_eq!(policy.value["policy"]["natural_enabled"], false);
    assert_eq!(policy.value["policy"]["schedule_enabled"], false);
    assert_eq!(policy.headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(scalar(&state, "SELECT count(*) FROM rest_cycles"), 0);
}

#[tokio::test]
async fn standalone_routes_check_host_origin_csrf_and_do_not_accept_client_clock_or_authority() {
    let (_directory, state) = fixture();
    let body = json!({"wake_at":now()+600});
    for headers in [
        vec![
            ("x-csrf-token", "rest-csrf"),
            ("if-match", "\"0\""),
            ("idempotency-key", "no-origin"),
        ],
        vec![
            ("origin", "http://evil"),
            ("x-csrf-token", "rest-csrf"),
            ("if-match", "\"0\""),
            ("idempotency-key", "bad-origin"),
        ],
        vec![
            ("origin", "http://localhost"),
            ("if-match", "\"0\""),
            ("idempotency-key", "no-csrf"),
        ],
        vec![
            ("host", "evil"),
            ("origin", "http://localhost"),
            ("x-csrf-token", "rest-csrf"),
            ("if-match", "\"0\""),
            ("idempotency-key", "bad-host"),
        ],
    ] {
        assert_eq!(
            call(
                &state,
                "POST",
                "/api/admin/v1/rest/sleep",
                body.clone(),
                Some('1'),
                &headers
            )
            .await
            .status,
            403
        );
    }
    for forged in [
        json!({"wake_at":now()+600,"clock_trusted":true}),
        json!({"wake_at":now()+600,"source_owner":"qq:forged"}),
        json!({"wake_at":now()+600,"phase":"sleeping"}),
    ] {
        assert_eq!(
            call(
                &state,
                "POST",
                "/api/admin/v1/rest/sleep",
                forged,
                Some('1'),
                &writes("\"0\"", "forged")
            )
            .await
            .status,
            422
        );
    }
    assert_eq!(scalar(&state, "SELECT count(*) FROM rest_operations"), 0);
    assert_eq!(scalar(&state, "SELECT count(*) FROM rest_cycles"), 0);
}

#[tokio::test]
async fn missing_and_stale_versions_are_explicit_and_wake_is_independent_of_ntp_sleep_gate() {
    let (_directory, state) = fixture();
    let body = json!({"wake_at":now()+600});
    assert_eq!(
        call(
            &state,
            "POST",
            "/api/admin/v1/rest/sleep",
            body.clone(),
            Some('1'),
            &[
                ("origin", "http://localhost"),
                ("x-csrf-token", "rest-csrf"),
                ("idempotency-key", "missing-version")
            ]
        )
        .await
        .status,
        428
    );
    assert_eq!(
        call(
            &state,
            "POST",
            "/api/admin/v1/rest/sleep",
            body.clone(),
            Some('1'),
            &writes("\"99\"", "stale")
        )
        .await
        .status,
        409
    );
    state
        .db(|db| {
            db.execute("UPDATE trigger_runtime SET clock_trusted=0 WHERE id=1", [])?;
            Ok(())
        })
        .unwrap();
    let rejected = call(
        &state,
        "POST",
        "/api/admin/v1/rest/sleep",
        body,
        Some('1'),
        &writes("\"0\"", "untrusted-sleep"),
    )
    .await;
    assert_eq!(rejected.status, 409);
    assert_eq!(rejected.value["error"]["code"], "rest_clock_untrusted");
    assert_eq!(scalar(&state, "SELECT count(*) FROM rest_cycles"), 0);
    let wake = call(
        &state,
        "POST",
        "/api/admin/v1/rest/wake",
        json!({}),
        Some('1'),
        &writes("\"0\"", "wake-without-ntp"),
    )
    .await;
    assert_eq!(wake.status, 200);
    assert_eq!(wake.value["state"]["phase"], "awake");
}

#[tokio::test]
async fn exact_sleep_replay_never_extends_wake_and_explicit_wake_preserves_physical_leases() {
    let (_directory, state) = fixture();
    let wake_at = now() + 600;
    let body = json!({"wake_at":wake_at});
    let first = call(
        &state,
        "POST",
        "/api/admin/v1/rest/sleep",
        body.clone(),
        Some('1'),
        &writes("\"0\"", "one-sleep"),
    )
    .await;
    assert_eq!(first.status, 200);
    assert_eq!(first.value["state"]["phase"], "sleep_pending");
    assert_eq!(first.value["state"]["generation"], 1);
    let replay = call(
        &state,
        "POST",
        "/api/admin/v1/rest/sleep",
        body,
        Some('1'),
        &writes("\"0\"", "one-sleep"),
    )
    .await;
    assert_eq!(replay.status, 200);
    assert_eq!(
        replay.value["operation"]["id"],
        first.value["operation"]["id"]
    );
    assert_eq!(replay.value["state"]["wake_at"], wake_at);
    assert_eq!(replay.value["idempotent_replay"], true);
    assert_eq!(scalar(&state, "SELECT count(*) FROM rest_cycles"), 1);
    assert_eq!(
        scalar(
            &state,
            "SELECT count(*) FROM audit_events WHERE action='rest.sleep'"
        ),
        1
    );
    assert_eq!(
        call(
            &state,
            "POST",
            "/api/admin/v1/rest/sleep",
            json!({"wake_at":wake_at+1}),
            Some('1'),
            &writes("\"0\"", "one-sleep")
        )
        .await
        .status,
        409
    );
    let run = source(&state, "lease-source", "a", None);
    state
        .db(|db| {
            db.execute(
                "INSERT INTO native_worker_leases(run_id,claimed_at)VALUES(?1,?2)",
                params![run, now()],
            )?;
            db.execute("UPDATE trigger_runtime SET clock_trusted=0 WHERE id=1", [])?;
            Ok(())
        })
        .unwrap();
    let wake = call(
        &state,
        "POST",
        "/api/admin/v1/rest/wake",
        json!({}),
        Some('1'),
        &writes("\"1\"", "explicit-wake"),
    )
    .await;
    assert_eq!(wake.status, 200);
    assert_eq!(wake.value["state"]["phase"], "awake");
    assert_eq!(
        scalar(&state, "SELECT count(*) FROM native_worker_leases"),
        1
    );
}

#[tokio::test]
async fn policy_has_its_own_revision_idempotency_and_audit_without_editing_runtime_or_personality()
{
    let (_directory, state) = fixture();
    let before: Vec<(String, String)> = state
        .db(|db| {
            let mut q = db.prepare("SELECT key,value FROM settings ORDER BY key")?;
            let rows = q
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .unwrap();
    let before_config: String = state
        .db(|db| {
            Ok(db.query_row(
                "SELECT config_json FROM config_revisions WHERE revision=1",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    let mut policy = serde_json::to_value(RestPolicy::default()).unwrap();
    policy["default_sleep_seconds"] = json!(600);
    let saved = call(
        &state,
        "PUT",
        "/api/admin/v1/rest/policy",
        policy.clone(),
        Some('1'),
        &writes("\"1\"", "policy-one"),
    )
    .await;
    assert_eq!(saved.status, 200);
    assert_eq!(saved.value["revision"], 2);
    assert_eq!(
        call(
            &state,
            "PUT",
            "/api/admin/v1/rest/policy",
            policy.clone(),
            Some('1'),
            &writes("\"1\"", "policy-one")
        )
        .await
        .value["idempotent_replay"],
        true
    );
    assert_eq!(
        call(
            &state,
            "PUT",
            "/api/admin/v1/rest/policy",
            policy,
            Some('1'),
            &writes("\"1\"", "different-key-stale")
        )
        .await
        .status,
        409
    );
    assert_eq!(
        scalar(
            &state,
            "SELECT count(*) FROM audit_events WHERE action='rest.policy.update'"
        ),
        1
    );
    let after: Vec<(String, String)> = state
        .db(|db| {
            let mut q = db.prepare("SELECT key,value FROM settings ORDER BY key")?;
            let rows = q
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .unwrap();
    let after_config: String = state
        .db(|db| {
            Ok(db.query_row(
                "SELECT config_json FROM config_revisions WHERE revision=1",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(before_config, after_config);
}

#[tokio::test]
async fn grants_bind_real_manageable_sources_and_do_not_create_a_model_or_recipient_permission() {
    let (_directory, state) = fixture();
    let own = source(&state, "own", "a", None);
    let foreign = source(&state, "foreign", "b", None);
    let qq = source(&state, "qq", "qq:synthetic:c2c:RAW_PLATFORM_ID", None);
    for run in [own, qq] {
        let path = format!("/api/admin/v1/rest/control-grants/{run}");
        let view = call(&state, "GET", &path, Value::Null, Some('1'), &[]).await;
        assert_eq!(view.status, 200);
        assert_eq!(view.value["revision"], 0);
        assert_eq!(view.value["permissions"]["can_sleep"], false);
        assert!(!view.raw.contains("RAW_PLATFORM_ID"));
        let permissions = json!({"can_sleep":true,"can_wake":false,"valid_until":now()+600});
        let key = format!("grant-{run}");
        let saved = call(
            &state,
            "PUT",
            &path,
            permissions.clone(),
            Some('1'),
            &writes("\"0\"", &key),
        )
        .await;
        assert_eq!(saved.status, 200);
        assert_eq!(saved.value["revision"], 1);
        assert_eq!(saved.value["recipient_or_model_permission"], false);
        assert_eq!(
            call(
                &state,
                "PUT",
                &path,
                permissions,
                Some('1'),
                &writes("\"0\"", &key)
            )
            .await
            .value["idempotent_replay"],
            true
        );
        assert_eq!(saved.value["natural_control_enabled"], false);
    }
    assert_eq!(
        call(
            &state,
            "GET",
            &format!("/api/admin/v1/rest/control-grants/{foreign}"),
            Value::Null,
            Some('1'),
            &[]
        )
        .await
        .status,
        404
    );
    let forgotten = source(&state, "lost-binding", "a", None);
    state
        .db(|db| {
            db.execute(
                "DELETE FROM run_conversation_bindings WHERE run_id=?1",
                [&forgotten],
            )?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        call(
            &state,
            "GET",
            &format!("/api/admin/v1/rest/control-grants/{forgotten}"),
            Value::Null,
            Some('1'),
            &[]
        )
        .await
        .status,
        404
    );
    assert_eq!(scalar(&state, "SELECT count(*) FROM rest_cycles"), 0);
}

#[tokio::test]
async fn group_control_grants_require_unique_verified_speaker_and_stay_separate() {
    let (_directory, state) = fixture();
    let owner = "qq:synthetic:group:synthetic-team";
    let a = source(&state, "group", owner, Some("member-a"));
    let b = source(&state, "group", owner, Some("member-b"));
    let unknown = source(&state, "group", owner, None);
    let path = format!("/api/admin/v1/rest/control-grants/{a}");
    assert_eq!(
        call(
            &state,
            "PUT",
            &path,
            json!({"can_sleep":true,"can_wake":true,"valid_until":now()+600}),
            Some('1'),
            &writes("\"0\"", "group-a")
        )
        .await
        .status,
        200
    );
    let other = call(
        &state,
        "GET",
        &format!("/api/admin/v1/rest/control-grants/{b}"),
        Value::Null,
        Some('1'),
        &[],
    )
    .await;
    assert_eq!(other.status, 200);
    assert_eq!(other.value["revision"], 0);
    assert_eq!(
        call(
            &state,
            "GET",
            &format!("/api/admin/v1/rest/control-grants/{unknown}"),
            Value::Null,
            Some('1'),
            &[]
        )
        .await
        .status,
        404
    );
    assert_eq!(call(&state,"PUT",&path,json!({"can_sleep":true,"can_wake":true,"valid_until":now()+600,"speaker_ref":"member-b"}),Some('1'),&writes("\"1\"","forged-speaker")).await.status,422);
}

#[tokio::test]
async fn cycle_readers_are_bounded_metadata_and_cannot_submit_completion() {
    let (_directory, state) = fixture();
    let mut cycle = String::new();
    for generation in 0..3 {
        let rev = format!("\"{generation}\"");
        let key = format!("cycle-{generation}");
        let saved = call(
            &state,
            "POST",
            "/api/admin/v1/rest/sleep",
            json!({"wake_at":now()+600}),
            Some('1'),
            &writes(&rev, &key),
        )
        .await;
        assert_eq!(saved.status, 200);
        cycle = saved.value["operation"]["cycle_id"]
            .as_str()
            .unwrap()
            .into();
        let rev = format!("\"{}\"", generation + 1);
        let key = format!("wake-{generation}");
        assert_eq!(
            call(
                &state,
                "POST",
                "/api/admin/v1/rest/wake",
                json!({}),
                Some('1'),
                &writes(&rev, &key)
            )
            .await
            .status,
            200
        );
    }
    let current = call(
        &state,
        "GET",
        "/api/admin/v1/rest/cycles/current",
        Value::Null,
        Some('1'),
        &[],
    )
    .await;
    assert_eq!(current.status, 200);
    assert_eq!(current.value["current"]["cycle_id"], cycle);
    let page = call(
        &state,
        "GET",
        "/api/admin/v1/rest/cycles?limit=2",
        Value::Null,
        Some('1'),
        &[],
    )
    .await;
    assert_eq!(page.status, 200);
    assert_eq!(page.value["items"].as_array().unwrap().len(), 2);
    let next = format!(
        "/api/admin/v1/rest/cycles?limit=2&after={}",
        page.value["next_cursor"].as_i64().unwrap()
    );
    assert_eq!(
        call(&state, "GET", &next, Value::Null, Some('1'), &[])
            .await
            .value["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        call(
            &state,
            "POST",
            &format!("/api/admin/v1/rest/cycles/{cycle}"),
            json!({"status":"success","lease_finished":true}),
            Some('1'),
            &writes("\"3\"", "cannot-complete")
        )
        .await
        .status,
        405
    );
    assert_eq!(
        scalar(
            &state,
            "SELECT count(*) FROM rest_steps WHERE status='success'"
        ),
        0
    );
}

#[tokio::test]
async fn audit_failure_rolls_back_state_policy_and_idempotency_without_exposing_sql() {
    let (_directory, state) = fixture();
    state.db(|db| {db.execute_batch("CREATE TRIGGER reject_rest_audit BEFORE INSERT ON audit_events WHEN new.action LIKE 'rest.%' BEGIN SELECT RAISE(ABORT,'PRIVATE_SQL_SENTINEL'); END;")?;Ok(())}).unwrap();
    let saved = call(
        &state,
        "PUT",
        "/api/admin/v1/rest/policy",
        serde_json::to_value(RestPolicy::default()).unwrap(),
        Some('1'),
        &writes("\"1\"", "audit-policy"),
    )
    .await;
    assert_eq!(saved.status, 503);
    assert!(!saved.raw.contains("PRIVATE_SQL_SENTINEL"));
    assert_eq!(saved.value["error"]["code"], "rest_unavailable");
    let slept = call(
        &state,
        "POST",
        "/api/admin/v1/rest/sleep",
        json!({"wake_at":now()+600}),
        Some('1'),
        &writes("\"0\"", "audit-sleep"),
    )
    .await;
    assert_eq!(slept.status, 503);
    assert!(!slept.raw.contains("PRIVATE_SQL_SENTINEL"));
    assert_eq!(scalar(&state, "SELECT revision FROM rest_policies"), 1);
    assert_eq!(
        scalar(&state, "SELECT generation FROM assistant_rest_states"),
        0
    );
    assert_eq!(scalar(&state, "SELECT count(*) FROM rest_cycles"), 0);
    assert_eq!(
        scalar(&state, "SELECT count(*) FROM rest_management_requests"),
        0
    );
}
