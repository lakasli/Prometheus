use super::*;
use crate::state::{AppState, Options};

const NOW: i64 = 1_793_000_000;

fn fixture() -> (tempfile::TempDir, AppState, Admin) {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open(Options {
        data_dir: directory.path().into(),
        origin: "http://localhost".into(),
        allow_private_model: false,
        cookie_secure: false,
        web_dir: directory.path().join("web"),
    })
    .unwrap();
    let admin = Admin {
        id: "admin".into(),
        username: "commitment-admin".into(),
        role: "admin".into(),
        csrf_token: "synthetic-commitment-csrf".into(),
    };
    state
        .db(|db| {
            conversation_state::initialize(db)?;
            ensure_schema(db)?;
            db.execute(
                "INSERT INTO admins(id,username,password_hash,created_at)VALUES(?1,?2,'unused',0)",
                params![admin.id, admin.username],
            )?;
            Ok(())
        })
        .unwrap();
    (directory, state, admin)
}

fn source(state: &AppState, id: &str, conversation: &str, owner: &str, input: &str) {
    state.db(|db|{
        let tx=db.transaction()?;
        tx.execute("INSERT OR IGNORE INTO conversations(id,owner_id,title,source_key,created_at)VALUES(?1,?2,'synthetic commitment',?3,?4)",params![conversation,owner,owner.starts_with("qq:").then_some(owner),NOW])?;
        tx.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,created_at,updated_at,run_kind)VALUES(?1,?2,?3,?1,?4,'{}','running',?5,?5,'model')",params![id,conversation,owner,input,NOW])?;
        tx.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at)VALUES(?1,?2,?3,'user',?4,?5)",params![format!("{id}-user"),conversation,id,input,NOW])?;
        conversation_state::scope_for_run_in(&tx,id)?;tx.commit()?;Ok(())
    }).unwrap();
}

fn proposal() -> Proposal {
    Proposal {
        intent: "阅读已授权的合成资料".into(),
        kind: "reading".into(),
        time_text: Some("明天".into()),
        expires_at: None,
        sources: Vec::new(),
    }
}
fn transition(expected: i64, key: &str) -> Transition<'_> {
    Transition {
        expected_revision: expected,
        idempotency_key: key,
        now: NOW,
    }
}

fn candidate(state: &AppState, run: &str) -> String {
    state
        .db(|db| {
            let tx = db.transaction()?;
            let scope = conversation_state::scope_for_run_in(&tx, run)?;
            let value = propose_in(
                &tx,
                &scope,
                &proposal(),
                ProposalOrigin::ModelSuggestion,
                &format!("candidate:{run}"),
                NOW,
            )?;
            tx.commit()?;
            Ok(value["id"].as_str().unwrap().to_string())
        })
        .unwrap()
}

fn confirm(state: &AppState, admin: &Admin, run: &str, id: &str) {
    state
        .db(|db| {
            let tx = db.transaction()?;
            let scope = conversation_state::management_scope_for_source_in(&tx, admin, run)?;
            confirm_in(
                &tx,
                &scope,
                admin,
                id,
                &transition(1, &format!("confirm:{id}")),
            )?;
            tx.commit()?;
            Ok(())
        })
        .unwrap();
}

fn activity(db: &Connection, owner: &str) -> anyhow::Result<ActivityBinding> {
    let resource = schedule::create_resource(
        db,
        owner,
        &json!({"title":"synthetic text","content":"合成资料正文。","type":"text"}),
        NOW,
    )?;
    let value = schedule::create(
        db,
        owner,
        &json!({"title":"已授权阅读","type":"reading","purpose":"验收","start_at":NOW+7200,"end_at":NOW+7800,"time_zone":"Asia/Shanghai","resource_id":resource["id"],"movable":false}),
        "activity",
        false,
        NOW,
    )?;
    Ok(ActivityBinding {
        activity_id: value["id"].as_str().unwrap().into(),
        activity_revision: 1,
        occurrence_id: None,
    })
}

#[test]
fn model_proposal_and_human_confirmation_never_manufacture_a_scheduled_activity() {
    let (_dir, state, admin) = fixture();
    source(&state, "source", "chat", "admin", "我约定明天阅读资料");
    let id = candidate(&state, "source");
    state
        .db(|db| {
            let tx = db.transaction()?;
            let scope = conversation_state::scope_for_run_in(&tx, "source")?;
            let value = detail_in(&tx, &scope, &id, NOW)?;
            assert_eq!(value["confirmation_state"], "proposed");
            assert_eq!(value["execution"]["state"], "unscheduled");
            assert!(confirm_in(
                &tx,
                &scope,
                &admin,
                &id,
                &transition(1, "not-a-management-token")
            )
            .is_err());
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    confirm(&state, &admin, "source", &id);
    state
        .db(|db| {
            let tx = db.transaction()?;
            let scope = conversation_state::management_scope_for_source_in(&tx, &admin, "source")?;
            let value = detail_in(&tx, &scope, &id, NOW)?;
            assert_eq!(value["confirmation_state"], "confirmed");
            assert_eq!(
                value["confirmation_evidence"]["basis"],
                "authenticated_management_review"
            );
            assert_eq!(
                value["confirmation_evidence"]["source_assent_verified"],
                false
            );
            assert_eq!(value["execution"]["state"], "unscheduled");
            assert_eq!(value["recipient_permission"], false);
            assert_eq!(
                tx.query_row("SELECT count(*) FROM schedule_activities", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .unwrap();
    assert!(serde_json::from_value::<Proposal>(
        json!({"intent":"伪造","kind":"other","execution_state":"completed"})
    )
    .is_err());
}

#[test]
fn tentative_or_quoted_time_never_becomes_a_task_and_explicit_time_stays_unresolved() {
    let (_dir, state, _admin) = fixture();
    for (index, text) in [
        "明天可以试试",
        "提醒我明天可以试试",
        "如果明天提醒我",
        "“提醒我明天看书”",
        "不要提醒我明天看书",
    ]
    .iter()
    .enumerate()
    {
        let id = format!("tentative-{index}");
        source(&state, &id, "chat", "admin", text);
        state
            .db(|db| {
                let tx = db.transaction()?;
                let scope = conversation_state::scope_for_run_in(&tx, &id)?;
                assert!(propose_natural_in(&tx, &scope, NOW)?.is_none());
                Ok(())
            })
            .unwrap();
    }
    source(
        &state,
        "explicit",
        "chat",
        "admin",
        "提醒我明天上午有空时看书",
    );
    state
        .db(|db| {
            let tx = db.transaction()?;
            let scope = conversation_state::scope_for_run_in(&tx, "explicit")?;
            let value = propose_natural_in(&tx, &scope, NOW)?.unwrap();
            assert_eq!(value["confirmation_state"], "proposed");
            assert_eq!(value["time"]["precision"], "unresolved");
            assert_eq!(value["time"]["anchor_at"], NOW);
            assert!(value["time"]["resolved_at"].is_null());
            assert_eq!(value["execution"]["state"], "unscheduled");
            Ok(())
        })
        .unwrap();
}

#[test]
fn proposal_idempotency_conflicts_and_source_revocation_never_resurrect_old_content() {
    let (_dir, state, _admin) = fixture();
    source(&state, "source", "chat", "admin", "我约定明天阅读资料");
    state
        .db(|db| {
            let tx = db.transaction()?;
            let scope = conversation_state::scope_for_run_in(&tx, "source")?;
            let first = propose_in(
                &tx,
                &scope,
                &proposal(),
                ProposalOrigin::ModelSuggestion,
                "same-key",
                NOW,
            )?;
            let repeated = propose_in(
                &tx,
                &scope,
                &proposal(),
                ProposalOrigin::ModelSuggestion,
                "same-key",
                NOW,
            )?;
            assert_eq!(first["id"], repeated["id"]);
            let mut other = proposal();
            other.intent = "不同候选".into();
            assert!(propose_in(
                &tx,
                &scope,
                &other,
                ProposalOrigin::ModelSuggestion,
                "same-key",
                NOW
            )
            .is_err());
            let id = first["id"].as_str().unwrap();
            assert_eq!(
                invalidate_source_in(&tx, "run_input", "source", Some(1), NOW + 1)?,
                1
            );
            assert!(detail_in(&tx, &scope, id, NOW + 1).is_err());
            assert!(propose_in(
                &tx,
                &scope,
                &proposal(),
                ProposalOrigin::ModelSuggestion,
                "same-key",
                NOW + 1
            )
            .is_err());
            let retained: String =
                tx.query_row("SELECT input FROM runs WHERE id='source'", [], |r| r.get(0))?;
            assert!(retained.contains("明天"));
            let body: String = tx.query_row(
                "SELECT intent FROM conversation_commitments WHERE id=?1",
                [id],
                |r| r.get(0),
            )?;
            assert!(body.is_empty());
            Ok(())
        })
        .unwrap();
}

#[test]
fn verified_admin_can_review_qq_source_without_changing_owner_or_minting_model_permission() {
    let (_dir, state, admin) = fixture();
    let owner = "qq:synthetic:c2c:private-user";
    source(&state, "qq-source", "qq-chat", owner, "我约定明天阅读资料");
    let id = candidate(&state, "qq-source");
    state
        .db(|db| {
            let tx = db.transaction()?;
            tx.execute(
                "UPDATE runs SET status='completed' WHERE id='qq-source'",
                [],
            )?;
            let scope =
                conversation_state::management_scope_for_source_in(&tx, &admin, "qq-source")?;
            assert_eq!(scope.owner_id(), owner);
            let value = confirm_in(&tx, &scope, &admin, &id, &transition(1, "admin-confirm"))?;
            assert_eq!(value["confirmation_state"], "confirmed");
            assert_eq!(value["execution"]["state"], "unscheduled");
            assert!(propose_in(
                &tx,
                &scope,
                &proposal(),
                ProposalOrigin::ModelSuggestion,
                "not-model",
                NOW
            )
            .is_err());
            let stored: String = tx.query_row(
                "SELECT owner_id FROM conversation_commitments WHERE id=?1",
                [&id],
                |r| r.get(0),
            )?;
            assert_eq!(stored, owner);
            Ok(())
        })
        .unwrap();
}

#[test]
fn real_activity_binding_requires_owner_authorization_and_cancellation_requires_real_result() {
    let (_dir, state, admin) = fixture();
    source(&state, "source", "chat", "admin", "请你阅读明天的资料");
    let id = candidate(&state, "source");
    confirm(&state, &admin, "source", &id);
    state
        .db(|db| {
            let tx = db.transaction()?;
            let scope = conversation_state::management_scope_for_source_in(&tx, &admin, "source")?;
            let binding = activity(&tx, "admin")?;
            let missing = ActivityBinding {
                activity_id: "not-persisted".into(),
                activity_revision: 1,
                occurrence_id: None,
            };
            assert!(bind_activity_in(
                &tx,
                &scope,
                &admin,
                &id,
                &missing,
                &transition(2, "missing")
            )
            .is_err());
            let bound =
                bind_activity_in(&tx, &scope, &admin, &id, &binding, &transition(2, "bind"))?;
            assert_eq!(bound["execution"]["state"], "scheduled");
            assert_eq!(bound["confirmation_state"], "confirmed");
            let cancellation = Cancellation {
                cancel_schedule: true,
                expected_activity_revision: Some(999),
            };
            assert!(cancel_in(
                &tx,
                &scope,
                &admin,
                &id,
                &cancellation,
                &transition(3, "stale-cancel")
            )
            .is_err());
            assert_eq!(
                detail_in(&tx, &scope, &id, NOW)?["confirmation_state"],
                "confirmed"
            );
            assert_eq!(
                schedule::detail(&tx, "admin", &binding.activity_id)?["state"],
                "enabled"
            );
            let cancellation = Cancellation {
                cancel_schedule: true,
                expected_activity_revision: Some(1),
            };
            let cancelled = cancel_in(
                &tx,
                &scope,
                &admin,
                &id,
                &cancellation,
                &transition(3, "cancel"),
            )?;
            assert_eq!(cancelled["confirmation_state"], "cancelled");
            assert_eq!(cancelled["execution"]["state"], "cancelled");
            assert_eq!(
                schedule::detail(&tx, "admin", &binding.activity_id)?["state"],
                "cancelled"
            );
            let replay = cancel_in(
                &tx,
                &scope,
                &admin,
                &id,
                &cancellation,
                &transition(3, "cancel"),
            )?;
            assert_eq!(replay["revision"], 4);
            assert_eq!(
                tx.query_row(
                    "SELECT count(*) FROM audit_events WHERE action='commitment.schedule.cancel'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn unknown_execution_cannot_be_cancelled_or_replayed_and_completion_needs_persisted_result() {
    let (_dir, state, admin) = fixture();
    source(&state, "source", "chat", "admin", "请你阅读明天的资料");
    let id = candidate(&state, "source");
    confirm(&state, &admin, "source", &id);
    state.db(|db|{let tx=db.transaction()?;let scope=conversation_state::management_scope_for_source_in(&tx,&admin,"source")?;let binding=activity(&tx,"admin")?;
        bind_activity_in(&tx,&scope,&admin,&id,&binding,&transition(2,"bind"))?;
        let definition=schedule::detail(&tx,"admin",&binding.activity_id)?["definition"].clone();
        tx.execute("INSERT INTO schedule_occurrences(id,activity_id,owner_id,activity_revision,nominal_at,start_at,end_at,definition_json,status,updated_at) VALUES('occurrence',?1,'admin',1,?2,?2,?3,?4,'unknown',?5)",params![binding.activity_id,NOW+7200,NOW+7800,definition.to_string(),NOW])?;
        assert_eq!(detail_in(&tx,&scope,&id,NOW)?["execution"]["state"],"unknown");
        assert!(cancel_in(&tx,&scope,&admin,&id,&Cancellation{cancel_schedule:true,expected_activity_revision:Some(1)},&transition(3,"unknown-cancel")).is_err());
        tx.execute("UPDATE schedule_occurrences SET status='completed' WHERE id='occurrence'",[])?;
        assert_ne!(detail_in(&tx,&scope,&id,NOW)?["execution"]["state"],"completed");
        tx.execute("UPDATE schedule_occurrences SET completed_at=?1,result_json='{}' WHERE id='occurrence'",[NOW+7800])?;
        let result=detail_in(&tx,&scope,&id,NOW+7900)?;assert_eq!(result["execution"]["state"],"completed");assert_eq!(result["confirmation_state"],"confirmed");assert_eq!(result["execution"]["fulfillment_verified"],false);assert_eq!(result["execution"]["recipient_permission"],false);Ok(())}).unwrap();
}

#[test]
fn confirmation_cannot_grant_reminder_or_contact_delivery() {
    let (_dir, state, admin) = fixture();
    source(&state, "source", "chat", "admin", "提醒我明天看书");
    state
        .db(|db| {
            let tx = db.transaction()?;
            let model = conversation_state::scope_for_run_in(&tx, "source")?;
            let p = propose_natural_in(&tx, &model, NOW)?.unwrap();
            let id = p["id"].as_str().unwrap();
            let management =
                conversation_state::management_scope_for_source_in(&tx, &admin, "source")?;
            confirm_in(&tx, &management, &admin, id, &transition(1, "confirm"))?;
            let binding = activity(&tx, "admin")?;
            assert!(bind_activity_in(
                &tx,
                &management,
                &admin,
                id,
                &binding,
                &transition(2, "bind-reminder")
            )
            .is_err());
            assert_eq!(
                detail_in(&tx, &management, id, NOW)?["execution"]["state"],
                "unscheduled"
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn context_is_bounded_source_private_and_usage_is_immutable_and_invalidated() {
    let (_dir, state, _admin) = fixture();
    source(&state, "source", "chat", "admin", "我约定明天阅读资料");
    let id = candidate(&state, "source");
    source(&state, "next", "chat", "admin", "继续该约定");
    source(
        &state,
        "foreign",
        "other-chat",
        "another-owner",
        "不能看到其他人的约定",
    );
    state
        .db(|db| {
            let tx = db.transaction()?;
            let context = context_for_run_in(&tx, "next", 6, 6000, NOW)?;
            assert_eq!(context.source_refs.len(), 1);
            record_usage_in(&tx, "next", &context.source_refs, NOW)?;
            record_usage_in(&tx, "next", &context.source_refs, NOW)?;
            assert!(record_usage_in(&tx, "next", &[], NOW).is_err());
            assert!(validate_context_in(&tx, "next", NOW)?);
            assert!(context_for_run_in(&tx, "foreign", 6, 6000, NOW)?
                .items
                .is_empty());
            assert!(context_for_run_in(&tx, "next", 6, 1, NOW)?.items.is_empty());
            invalidate_in(&tx, Some("admin"), Some("chat"), NOW + 1)?;
            assert!(!validate_context_in(&tx, "next", NOW + 1)?);
            let scope = conversation_state::scope_for_run_in(&tx, "next")?;
            assert!(detail_in(&tx, &scope, &id, NOW + 1).is_err());
            assert_eq!(
                tx.query_row(
                    "SELECT count(*) FROM messages WHERE run_id='source'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn cross_scope_or_invented_time_sources_fail_and_pagination_is_keyset_bounded() {
    let (_dir, state, _admin) = fixture();
    source(&state, "source", "chat", "admin", "我约定明天阅读资料");
    source(
        &state,
        "other",
        "other-chat",
        "different-owner",
        "后天是别人的资料",
    );
    state
        .db(|db| {
            let tx = db.transaction()?;
            let scope = conversation_state::scope_for_run_in(&tx, "source")?;
            let other = conversation_state::scope_for_run_in(&tx, "other")?;
            let mut bad = proposal();
            bad.time_text = Some("不存在的后天".into());
            assert!(propose_in(
                &tx,
                &scope,
                &bad,
                ProposalOrigin::ModelSuggestion,
                "bad-time",
                NOW
            )
            .is_err());
            let mut bad = proposal();
            bad.sources
                .push(conversation_state::run_input_source_in(&tx, &other)?);
            assert!(propose_in(
                &tx,
                &scope,
                &bad,
                ProposalOrigin::ModelSuggestion,
                "bad-source",
                NOW
            )
            .is_err());
            for index in 0..3 {
                propose_in(
                    &tx,
                    &scope,
                    &proposal(),
                    ProposalOrigin::ModelSuggestion,
                    &format!("page-{index}"),
                    NOW,
                )?;
            }
            let first = list_in(&tx, &scope, 0, 2, NOW)?;
            assert_eq!(first["items"].as_array().unwrap().len(), 2);
            let next = list_in(&tx, &scope, first["next_cursor"].as_i64().unwrap(), 2, NOW)?;
            assert_eq!(next["items"].as_array().unwrap().len(), 1);
            assert!(next["next_cursor"].is_null());
            assert!(list_in(&tx, &other, 0, 2, NOW)?["items"]
                .as_array()
                .unwrap()
                .is_empty());
            assert!(list_in(&tx, &scope, 0, 0, NOW).is_err());
            Ok(())
        })
        .unwrap();
}

async fn api(
    state: &AppState,
    method: &str,
    uri: &str,
    body: Value,
    authenticated: bool,
    headers: &[(&str, &str)],
) -> (u16, Value) {
    use tower::ServiceExt;
    let mut request = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .header("content-type", "application/json");
    if authenticated {
        request = request.header("cookie", format!("dsh_session={}", "1".repeat(64)));
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = crate::conversation_commitments_api::router()
        .with_state(state.clone())
        .oneshot(
            request
                .body(axum::body::Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(json!({"non_json_rejection":true})),
    )
}

fn session(state: &AppState, admin: &Admin) {
    state
        .db(|db| {
            db.execute(
                "INSERT INTO sessions VALUES(?1,?2,?3,?4)",
                params![
                    crate::state::token_hash(&"1".repeat(64)),
                    admin.id,
                    admin.csrf_token,
                    crate::state::now() + 3600
                ],
            )?;
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn management_api_requires_auth_origin_csrf_and_exact_version_without_client_owner() {
    let (_dir, state, admin) = fixture();
    source(&state, "source", "chat", "admin", "我约定明天阅读资料");
    session(&state, &admin);
    let uri = "/api/admin/v1/conversations/chat/commitments?run_id=source";
    let input = serde_json::to_value(proposal()).unwrap();
    assert_eq!(
        api(&state, "POST", uri, input.clone(), false, &[]).await.0,
        401
    );
    assert_eq!(
        api(
            &state,
            "POST",
            uri,
            input.clone(),
            true,
            &[("x-csrf-token", &admin.csrf_token)]
        )
        .await
        .0,
        403
    );
    assert_eq!(
        api(
            &state,
            "POST",
            uri,
            input.clone(),
            true,
            &[
                ("origin", "http://foreign"),
                ("x-csrf-token", &admin.csrf_token)
            ]
        )
        .await
        .0,
        403
    );
    assert_eq!(
        api(
            &state,
            "POST",
            uri,
            input.clone(),
            true,
            &[("origin", "http://localhost")]
        )
        .await
        .0,
        403
    );
    let write = [
        ("origin", "http://localhost"),
        ("x-csrf-token", admin.csrf_token.as_str()),
        ("idempotency-key", "create"),
    ];
    let (status, created) = api(&state, "POST", uri, input.clone(), true, &write).await;
    assert_eq!(status, 200);
    assert_eq!(created["confirmation_state"], "proposed");
    let (status, replayed) = api(&state, "POST", uri, input.clone(), true, &write).await;
    assert_eq!(status, 200);
    assert_eq!(created["id"], replayed["id"]);
    let route = format!(
        "/api/admin/v1/conversations/chat/commitments/{}/confirm?run_id=source",
        created["id"].as_str().unwrap()
    );
    let write = [
        ("origin", "http://localhost"),
        ("x-csrf-token", admin.csrf_token.as_str()),
        ("idempotency-key", "confirm"),
    ];
    assert_eq!(
        api(
            &state,
            "POST",
            &route,
            json!({"confirmed":true}),
            true,
            &write
        )
        .await
        .0,
        428
    );
    let mut stale = write.to_vec();
    stale.push(("if-match", "\"99\""));
    assert_eq!(
        api(
            &state,
            "POST",
            &route,
            json!({"confirmed":true}),
            true,
            &stale
        )
        .await
        .0,
        409
    );
    let mut current = write.to_vec();
    current.push(("if-match", "\"1\""));
    let (status, confirmed) = api(
        &state,
        "POST",
        &route,
        json!({"confirmed":true}),
        true,
        &current,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(confirmed["confirmation_state"], "confirmed");
    assert_eq!(confirmed["execution"]["state"], "unscheduled");
    let (status, replay) = api(
        &state,
        "POST",
        &route,
        json!({"confirmed":true}),
        true,
        &current,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(replay["revision"], 2);
    let mut forged = input;
    forged["owner_id"] = json!("foreign");
    assert_eq!(api(&state, "POST", uri, forged, true, &write).await.0, 422);
    assert_eq!(
        api(
            &state,
            "GET",
            "/api/admin/v1/conversations/other/commitments?run_id=source",
            Value::Null,
            true,
            &[]
        )
        .await
        .0,
        404
    );
    assert_eq!(
        api(
            &state,
            "GET",
            "/api/admin/v1/conversations/chat/commitments?run_id=source&limit=0",
            Value::Null,
            true,
            &[]
        )
        .await
        .0,
        422
    );
}

#[test]
fn user_reading_and_requested_assistant_reading_have_different_verified_subjects() {
    let (_dir, state, admin) = fixture();
    source(&state, "personal", "chat", "admin", "我答应你明天阅读资料");
    let personal = candidate(&state, "personal");
    confirm(&state, &admin, "personal", &personal);
    source(&state, "requested", "chat", "admin", "请你阅读明天的资料");
    let requested = candidate(&state, "requested");
    confirm(&state, &admin, "requested", &requested);
    state
        .db(|db| {
            let tx = db.transaction()?;
            let scope =
                conversation_state::management_scope_for_source_in(&tx, &admin, "requested")?;
            let user = detail_in(&tx, &scope, &personal, NOW)?;
            assert_eq!(user["subject_kind"], "speaker");
            assert_eq!(user["counterparty_kind"], "assistant");
            let assistant = detail_in(&tx, &scope, &requested, NOW)?;
            assert_eq!(assistant["subject_kind"], "assistant");
            assert_eq!(assistant["counterparty_kind"], "speaker");
            let binding = activity(&tx, "admin")?;
            assert!(bind_activity_in(
                &tx,
                &scope,
                &admin,
                &personal,
                &binding,
                &transition(2, "not-personal-execution")
            )
            .is_err());
            let bound = bind_activity_in(
                &tx,
                &scope,
                &admin,
                &requested,
                &binding,
                &transition(2, "requested-execution"),
            )?;
            assert_eq!(bound["execution"]["state"], "scheduled");
            assert_eq!(bound["execution"]["fulfillment_verified"], false);
            assert_eq!(
                detail_in(&tx, &scope, &personal, NOW)?["execution"]["state"],
                "unscheduled"
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn ambiguous_participants_require_explicit_management_review_and_never_accept_recipient_ids() {
    let (_dir, state, admin) = fixture();
    source(&state, "ambiguous", "chat", "admin", "明天阅读资料这件事");
    let id = candidate(&state, "ambiguous");
    state
        .db(|db| {
            let tx = db.transaction()?;
            let model = conversation_state::scope_for_run_in(&tx, "ambiguous")?;
            let value = detail_in(&tx, &model, &id, NOW)?;
            assert_eq!(value["subject_kind"], "unresolved");
            assert_eq!(value["counterparty_kind"], "unresolved");
            let review = ParticipantReview {
                subject_kind: SubjectKind::Assistant,
                counterparty_kind: CounterpartyKind::Speaker,
            };
            assert!(review_participants_in(
                &tx,
                &model,
                &admin,
                &id,
                &review,
                &transition(1, "no-management-token")
            )
            .is_err());
            let scope =
                conversation_state::management_scope_for_source_in(&tx, &admin, "ambiguous")?;
            let reviewed = review_participants_in(
                &tx,
                &scope,
                &admin,
                &id,
                &review,
                &transition(1, "review"),
            )?;
            assert_eq!(reviewed["subject_kind"], "assistant");
            assert_eq!(reviewed["revision"], 2);
            assert_eq!(reviewed["recipient_permission"], false);
            let confirmed =
                confirm_in(&tx, &scope, &admin, &id, &transition(2, "confirm-reviewed"))?;
            assert_eq!(confirmed["execution"]["state"], "unscheduled");
            assert!(review_participants_in(
                &tx,
                &scope,
                &admin,
                &id,
                &review,
                &transition(3, "cannot-rewrite-confirmed-subject")
            )
            .is_err());
            Ok(())
        })
        .unwrap();
    assert!(serde_json::from_value::<ParticipantReview>(
        json!({"subject_kind":"external_user","counterparty_kind":"speaker"})
    )
    .is_err());
    assert!(serde_json::from_value::<ParticipantReview>(
        json!({"subject_kind":"assistant","counterparty_kind":"speaker","recipient_id":"arbitrary"})
    )
    .is_err());
    assert_eq!(
        participants_from_source("我们约定明天一起阅读").0,
        SubjectKind::Joint
    );
}

#[tokio::test]
async fn management_scope_list_is_safe_paginated_and_excludes_other_admin_private_sources() {
    let (_dir, state, admin) = fixture();
    session(&state, &admin);
    state
        .db(|db| {
            db.execute(
                "INSERT INTO admins VALUES('another-admin','another-manager','unused',0)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    source(
        &state,
        "own",
        "own-chat",
        "admin",
        "自己的合成正文不应进入来源列表",
    );
    source(
        &state,
        "foreign-admin",
        "foreign-chat",
        "another-admin",
        "另一个管理员私有正文绝不返回",
    );
    source(
        &state,
        "qq",
        "qq-chat",
        "qq:synthetic:c2c:RAW_PLATFORM_USER",
        "QQ正文绝不返回",
    );
    let (status, page) = api(
        &state,
        "GET",
        "/api/admin/v1/commitments/scopes?limit=1",
        Value::Null,
        true,
        &[],
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["items"][0]["source_kind"], "c2c");
    let encoded = page.to_string();
    assert!(!encoded.contains("RAW_PLATFORM_USER"));
    assert!(!encoded.contains("QQ正文"));
    assert!(!encoded.contains("foreign-chat"));
    let cursor = page["next_cursor"].as_i64().unwrap();
    let (status, next) = api(
        &state,
        "GET",
        &format!("/api/admin/v1/commitments/scopes?limit=1&after={cursor}"),
        Value::Null,
        true,
        &[],
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(next["items"][0]["conversation_id"], "own-chat");
    assert_eq!(
        api(
            &state,
            "GET",
            "/api/admin/v1/conversations/foreign-chat/commitments?run_id=foreign-admin",
            Value::Null,
            true,
            &[]
        )
        .await
        .0,
        404
    );
    state
        .db(|db| {
            let tx = db.transaction()?;
            let scope =
                conversation_state::management_scope_for_source_in(&tx, &admin, "foreign-admin")?;
            assert!(propose_in(
                &tx,
                &scope,
                &proposal(),
                ProposalOrigin::ManagementCandidate,
                "cannot-admin-other-web",
                NOW
            )
            .is_err());
            Ok(())
        })
        .unwrap();
}

#[tokio::test]
async fn group_scope_picker_uses_verified_speaker_separately_and_never_returns_platform_ids() {
    let (_dir, state, admin) = fixture();
    session(&state, &admin);
    let owner = "qq:synthetic:group:RAW_GROUP_ID";
    for (run, sender) in [
        ("a-old", Some("RAW_MEMBER_A")),
        ("a-latest", Some("RAW_MEMBER_A")),
        ("b-latest", Some("RAW_MEMBER_B")),
        ("unknown", None),
    ] {
        source(&state, run, "group-chat", owner, "我们约定明天阅读");
        if let Some(sender) = sender {
            state.db(|db|{let tx=db.transaction()?;
            crate::conversation_identity::record_qq_message_in(&tx,run,"GROUP_AT_MESSAGE_CREATE",&json!({"id":format!("{run}-platform-message"),"group_openid":"RAW_GROUP_ID","author":{"member_openid":sender}}))?;
            tx.commit()?;Ok(())}).unwrap();
        }
    }
    let (status, page) = api(
        &state,
        "GET",
        "/api/admin/v1/commitments/scopes?limit=20",
        Value::Null,
        true,
        &[],
    )
    .await;
    assert_eq!(status, 200);
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert!(items.iter().any(|v| v["run_id"] == "a-latest"));
    assert!(items.iter().any(|v| v["run_id"] == "b-latest"));
    assert_ne!(items[0]["scope_ref"], items[1]["scope_ref"]);
    assert!(items
        .iter()
        .all(|v| v["source_kind"] == "group" && v["title"] == "未命名来源"));
    let encoded = page.to_string();
    for raw in ["RAW_GROUP_ID", "RAW_MEMBER_A", "RAW_MEMBER_B"] {
        assert!(!encoded.contains(raw));
    }
}

#[test]
fn participants_time_and_natural_candidates_use_canonical_scoped_user_message() {
    let (_dir, state, _admin) = fixture();
    source(&state, "canonical", "chat", "admin", "我答应你明天阅读资料");
    source(&state, "tentative", "chat", "admin", "明天可以试试阅读资料");
    state
        .db(|db| {
            let tx = db.transaction()?;
            // A run cache that differs from the actual message must not make the
            // assistant the subject or invent a time absent from that message.
            tx.execute(
                "UPDATE runs SET input='请你下周阅读资料' WHERE id='canonical'",
                [],
            )?;
            let scope = conversation_state::scope_for_run_in(&tx, "canonical")?;
            let value = propose_in(
                &tx,
                &scope,
                &proposal(),
                ProposalOrigin::ModelSuggestion,
                "canonical-message",
                NOW,
            )?;
            assert_eq!(value["subject_kind"], "speaker");
            assert_eq!(value["counterparty_kind"], "assistant");
            assert_eq!(value["time"]["original_text"], "明天");
            let mut invented = proposal();
            invented.time_text = Some("下周".into());
            assert!(propose_in(
                &tx,
                &scope,
                &invented,
                ProposalOrigin::ModelSuggestion,
                "wrong-time-cache",
                NOW
            )
            .is_err());
            tx.execute(
                "UPDATE runs SET input='请提醒我明天阅读资料' WHERE id='tentative'",
                [],
            )?;
            let tentative = conversation_state::scope_for_run_in(&tx, "tentative")?;
            assert!(propose_natural_in(&tx, &tentative, NOW)?.is_none());
            Ok(())
        })
        .unwrap();
}

#[test]
fn activity_binding_and_picker_require_the_same_stable_assistant_identity() {
    let (_dir, state, admin) = fixture();
    source(&state, "source", "chat", "admin", "请你阅读明天的资料");
    let id = candidate(&state, "source");
    confirm(&state, &admin, "source", &id);
    state.db(|db| {
        let tx=db.transaction()?;
        let scope=conversation_state::management_scope_for_source_in(&tx,&admin,"source")?;
        let binding=activity(&tx,"admin")?;
        tx.execute("UPDATE schedule_activities SET assistant_id='different-stable-identity' WHERE id=?1",[&binding.activity_id])?;
        assert!(bind_activity_in(&tx,&scope,&admin,&id,&binding,&transition(2,"wrong-stable-identity")).is_err());
        let page=activity_candidates_in(&tx,&scope,&admin,0,20,NOW)?;
        assert!(page["items"].as_array().unwrap().is_empty());
        assert_eq!(detail_in(&tx,&scope,&id,NOW)?["execution"]["state"],"unscheduled");
        Ok(())
    }).unwrap();
}

#[test]
fn credential_like_natural_and_explicit_candidates_create_no_derived_content_copy() {
    let (_dir, state, _admin) = fixture();
    let original = "请提醒我明天 password = syntheticpass123";
    source(&state, "sensitive", "chat", "admin", original);
    state
        .db(|db| {
            let tx = db.transaction()?;
            let scope = conversation_state::scope_for_run_in(&tx, "sensitive")?;
            assert!(propose_natural_in(&tx, &scope, NOW)?.is_none());
            for (key, intent, time) in [
                ("unsafe-intent", original, None),
                (
                    "unsafe-time",
                    "阅读合成资料",
                    Some("password = syntheticpass123"),
                ),
            ] {
                let error = propose_in(
                    &tx,
                    &scope,
                    &Proposal {
                        intent: intent.into(),
                        kind: "reading".into(),
                        time_text: time.map(str::to_string),
                        expires_at: None,
                        sources: Vec::new(),
                    },
                    ProposalOrigin::ModelSuggestion,
                    key,
                    NOW,
                )
                .unwrap_err();
                let error = error
                    .downcast_ref::<crate::trigger_store::TriggerError>()
                    .unwrap();
                assert_eq!(error.status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);
                assert_eq!(error.code, "sensitive_commitment_content");
                assert!(!error.message.contains("syntheticpass123"));
            }
            for table in [
                "conversation_commitments",
                "conversation_commitment_events",
                "conversation_commitment_sources",
                "conversation_commitment_idempotency",
            ] {
                assert_eq!(
                    tx.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                        .get::<_, i64>(0))?,
                    0
                );
            }
            let retained: String = tx.query_row(
                "SELECT content FROM messages WHERE run_id='sensitive' AND role='user'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(retained, original);
            tx.commit()?;
            Ok(())
        })
        .unwrap();
}
