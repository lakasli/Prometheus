//! Authenticated management projections. Never serialize account snapshots or
//! private login rows directly; only owner GET may return display QR content.
use crate::{
    auth::Admin,
    error::{ApiError, ApiResult},
    state::AppState,
    weixin_login::{self, LoginView},
    weixin_protocol::SecretToken,
    weixin_store::{self, AccountSnapshot},
};
use axum::{
    extract::{rejection::JsonRejection, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/v1/weixin", get(status))
        .route("/api/admin/v1/weixin/login", post(start_login))
        .route(
            "/api/admin/v1/weixin/login/{id}",
            get(get_login).delete(cancel_login),
        )
        .route(
            "/api/admin/v1/weixin/login/{id}/verification",
            post(verification),
        )
        .route("/api/admin/v1/weixin/enabled", post(enabled))
        .route("/api/admin/v1/weixin/check", post(check))
}

fn expected(headers: &HeaderMap) -> ApiResult<i64> {
    let value = headers
        .get(header::IF_MATCH)
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::PRECONDITION_REQUIRED,
                "if_match_required",
                "请提交当前微信配置或扫码任务版本If-Match",
            )
        })?;
    let value = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value);
    value
        .parse::<i64>()
        .ok()
        .filter(|r| *r >= 0)
        .ok_or_else(|| ApiError::bad("invalid_if_match", "微信版本必须为非负整数"))
}

fn signal(state: &AppState) {
    state
        .0
        .weixin_signal
        .send_modify(|revision| *revision = revision.wrapping_add(1));
}

fn account_view(state: &AppState, account: &AccountSnapshot) -> Value {
    let configured = state
        .secret(&account.credential_ref, "weixin_bot_token")
        .ok()
        .and_then(|raw| SecretToken::new(raw).ok())
        .is_some_and(|token| token.expose().bytes().all(|b| b.is_ascii_graphic()));
    json!({"account_id":account.account_id,"enabled":account.enabled,"credential_configured":configured,
        "connection_status":account.connection_status,"last_error_code":account.last_error_code,"last_event_at":account.last_event_at})
}

fn login_response(code: StatusCode, view: LoginView) -> Response {
    (
        code,
        [(header::ETAG, format!("\"{}\"", view.revision))],
        Json(view),
    )
        .into_response()
}

async fn status(State(state): State<AppState>, admin: Admin) -> ApiResult<Json<Value>> {
    let (account, login, check, delivery) = state.db(|db| {
        let count=|status:&str|->rusqlite::Result<i64>{db.query_row("SELECT count(*) FROM weixin_outbox WHERE status=?1",[status],|r|r.get(0))};
        Ok((
            weixin_store::snapshot(db)?,
            weixin_login::pending_login(db, &admin.id),
            weixin_login::latest_check(db, &admin.id)?,
            json!({"queued":count("queued")?,"sending":count("sending")?,"accepted":count("accepted")?,"failed":count("failed")?,"unknown":count("unknown")?}),
        ))
    })?;
    let login = login?;
    let revision = account.as_ref().map_or(0, |account| account.revision);
    let account = account
        .as_ref()
        .map(|account| account_view(&state, account));
    let pending=login.map(|view|json!({"id":view.id,"status":view.status,"revision":view.revision,"expires_at":view.expires_at}));
    Ok(Json(
        json!({"revision":revision,"account":account,"pending_login":pending,"check":check,"delivery":delivery,
        "capabilities":{"text_private":true,"linked_owner_only":true,"groups":false,"media":false,"max_accounts":1}}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

async fn start_login(
    State(state): State<AppState>,
    admin: Admin,
    headers: HeaderMap,
    Json(_): Json<Empty>,
) -> ApiResult<Response> {
    let expected = expected(&headers)?;
    let result = state.db(|db| Ok(weixin_login::start_login(db, &admin.id, expected)))??;
    signal(&state);
    Ok(login_response(StatusCode::ACCEPTED, result))
}

async fn get_login(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let result = state.db(|db| Ok(weixin_login::login_view(db, &admin.id, &id)))??;
    Ok(login_response(StatusCode::OK, result))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Verification {
    code: String,
}

async fn verification(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<Verification>, JsonRejection>,
) -> ApiResult<Response> {
    let Json(body) = body
        .map_err(|_| ApiError::bad("weixin_invalid_verification", "验证码须为1到16位数字字符串"))?;
    let expected = expected(&headers)?;
    let result = state.db(|db| {
        Ok(weixin_login::submit_verification(
            db, &admin.id, &id, expected, &body.code,
        ))
    })??;
    signal(&state);
    Ok(login_response(StatusCode::ACCEPTED, result))
}

async fn cancel_login(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let expected = expected(&headers)?;
    let result = state.db(|db| Ok(weixin_login::cancel_login(db, &admin.id, &id, expected)))??;
    signal(&state);
    Ok(login_response(StatusCode::OK, result))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Enabled {
    enabled: bool,
}

async fn enabled(
    State(state): State<AppState>,
    admin: Admin,
    headers: HeaderMap,
    Json(body): Json<Enabled>,
) -> ApiResult<Response> {
    let expected = expected(&headers)?;
    let account = state
        .db(|db| {
            Ok(weixin_store::set_enabled(
                db,
                expected,
                &admin.id,
                body.enabled,
            ))
        })?
        .map_err(weixin_login::store_error)?;
    signal(&state);
    Ok((
        [(header::ETAG, format!("\"{}\"", account.revision))],
        Json(json!({"revision":account.revision,"account":account_view(&state,&account)})),
    )
        .into_response())
}

async fn check(
    State(state): State<AppState>,
    admin: Admin,
    headers: HeaderMap,
    Json(_): Json<Empty>,
) -> ApiResult<Response> {
    let expected = expected(&headers)?;
    let result = state.db(|db| Ok(weixin_login::start_check(db, &admin.id, expected)))??;
    signal(&state);
    Ok((StatusCode::ACCEPTED, Json(result)).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{now, private_write, token_hash, Options};
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use rusqlite::params;
    use tempfile::TempDir;
    use tower::ServiceExt;

    const ORIGIN: &str = "http://127.0.0.1:18419";
    fn setup() -> (TempDir, AppState) {
        let temporary = TempDir::new().unwrap();
        let state = AppState::open(Options {
            data_dir: temporary.path().to_owned(),
            origin: ORIGIN.into(),
            allow_private_model: false,
            cookie_secure: false,
            web_dir: temporary.path().join("web"),
        })
        .unwrap();
        state.db(|db| {
            for (id,token) in [("owner-a",'a'),("owner-b",'b')] {
                db.execute("INSERT INTO admins(id,username,password_hash,created_at) VALUES(?1,?1,'unused-fixture',?2)",params![id,now()])?;
                db.execute("INSERT INTO sessions(token_hash,user_id,csrf,expires_at) VALUES(?1,?2,'weixin-fixture-csrf',?3)",params![token_hash(&token.to_string().repeat(64)),id,now()+3600])?;
            }
            Ok(())
        }).unwrap();
        (temporary, state)
    }

    async fn request(
        state: &AppState,
        method: &str,
        path: &str,
        body: Value,
        owner: Option<char>,
        csrf: bool,
        revision: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("Host", "127.0.0.1:18419")
            .header("Origin", ORIGIN)
            .header("Content-Type", "application/json");
        if let Some(owner) = owner {
            builder = builder.header(
                "Cookie",
                format!("dsh_session={}", owner.to_string().repeat(64)),
            );
        }
        if csrf {
            builder = builder.header("X-CSRF-Token", "weixin-fixture-csrf");
        }
        if let Some(revision) = revision {
            builder = builder.header("If-Match", revision);
        }
        let response = crate::api::router(state.clone())
            .oneshot(builder.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    #[tokio::test]
    async fn login_api_enforces_auth_owner_csrf_and_revision_without_exposing_private_transport_values(
    ) {
        let (_temporary, state) = setup();
        let base = "/api/admin/v1/weixin";
        assert_eq!(
            request(&state, "GET", base, Value::Null, None, false, None)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(
                &state,
                "POST",
                &format!("{base}/login"),
                json!({}),
                Some('a'),
                false,
                Some("\"0\"")
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(
                &state,
                "POST",
                &format!("{base}/login"),
                json!({}),
                Some('a'),
                true,
                None
            )
            .await
            .0,
            StatusCode::PRECONDITION_REQUIRED
        );
        let stale = request(
            &state,
            "POST",
            &format!("{base}/login"),
            json!({}),
            Some('a'),
            true,
            Some("\"1\""),
        )
        .await;
        assert_eq!(stale.0, StatusCode::CONFLICT);
        assert_eq!(stale.1["error"]["code"], "weixin_revision_conflict");
        let (status, created) = request(
            &state,
            "POST",
            &format!("{base}/login"),
            json!({}),
            Some('a'),
            true,
            Some("\"0\""),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(created["status"], "creating");
        assert_eq!(created["revision"], 1);
        let duplicate = request(
            &state,
            "POST",
            &format!("{base}/login"),
            json!({}),
            Some('b'),
            true,
            Some("\"0\""),
        )
        .await;
        assert_eq!(duplicate.0, StatusCode::CONFLICT);
        assert_eq!(duplicate.1["error"]["code"], "weixin_revision_conflict");
        let id = created["id"].as_str().unwrap();
        let path = format!("{base}/login/{id}");
        assert_eq!(
            request(&state, "GET", &path, Value::Null, Some('b'), false, None)
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        state.db(|db| {db.execute("UPDATE weixin_login_jobs SET status='waiting',revision=2,qrcode='private-identifier',display_content='private-display-content' WHERE id=?1",[id])?;Ok(())}).unwrap();
        let (_, owner_view) =
            request(&state, "GET", &path, Value::Null, Some('a'), false, None).await;
        assert_eq!(owner_view["qr_content"], "private-display-content");
        assert!(!owner_view.to_string().contains("private-identifier"));
        let (_, page) = request(&state, "GET", base, Value::Null, Some('a'), false, None).await;
        assert_eq!(page["revision"], 0);
        assert!(page["account"].is_null());
        assert!(!page.to_string().contains("private-display"));
        assert!(!page.to_string().contains("private-identifier"));
        assert!(
            request(&state, "GET", base, Value::Null, Some('b'), false, None)
                .await
                .1["pending_login"]
                .is_null()
        );
        state.db(|db| {db.execute("UPDATE weixin_login_jobs SET status='need_verification',revision=3 WHERE id=?1",[id])?;Ok(())}).unwrap();
        assert_eq!(
            request(
                &state,
                "POST",
                &format!("{path}/verification"),
                json!({"code":"12a"}),
                Some('a'),
                true,
                Some("\"3\"")
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        let invalid_type = request(
            &state,
            "POST",
            &format!("{path}/verification"),
            json!({"code":123456}),
            Some('a'),
            true,
            Some("\"3\""),
        )
        .await;
        assert_eq!(invalid_type.0, StatusCode::BAD_REQUEST);
        assert!(!invalid_type.1.to_string().contains("123456"));
        let verified = request(
            &state,
            "POST",
            &format!("{path}/verification"),
            json!({"code":"123456"}),
            Some('a'),
            true,
            Some("\"3\""),
        )
        .await;
        assert_eq!(verified.0, StatusCode::ACCEPTED);
        assert_eq!(verified.1["revision"], 4);
        assert!(!verified.1.to_string().contains("123456"));
        assert_eq!(
            request(
                &state,
                "DELETE",
                &path,
                Value::Null,
                Some('a'),
                true,
                Some("\"3\"")
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        let cancelled = request(
            &state,
            "DELETE",
            &path,
            Value::Null,
            Some('a'),
            true,
            Some("\"4\""),
        )
        .await;
        assert_eq!(cancelled.0, StatusCode::OK);
        assert_eq!(cancelled.1["status"], "cancelled");
        assert!(cancelled.1.get("qr_content").is_none());
        state.db(|db| {assert_eq!(db.query_row("SELECT count(*) FROM weixin_login_jobs WHERE qrcode IS NOT NULL OR display_content IS NOT NULL OR verification_code IS NOT NULL",[],|r|r.get::<_,i64>(0))?,0);Ok(())}).unwrap();
        assert_eq!(
            request(
                &state,
                "POST",
                &format!("{base}/check"),
                json!({}),
                Some('a'),
                true,
                Some("\"0\"")
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn account_api_hides_credential_refs_and_check_is_a_single_generation_bound_receiver_request(
    ) {
        let (_temporary, state) = setup();
        let base = "/api/admin/v1/weixin";
        let reference = uuid::Uuid::new_v4().to_string();
        private_write(
            &state.0.options.data_dir.join("secrets").join(&reference),
            b"private-existing-bot-token",
        )
        .unwrap();
        let account = state
            .db(|db| {
                db.execute(
                    "INSERT INTO secrets(id,kind,created_at) VALUES(?1,'weixin_bot_token',?2)",
                    params![reference, now()],
                )?;
                Ok(weixin_store::replace_account(
                    db,
                    0,
                    "owner-a",
                    &weixin_store::AccountInput {
                        account_id: "bot@im.bot".into(),
                        base_url: crate::weixin_protocol::DEFAULT_BASE.into(),
                        credential_ref: reference.clone(),
                        linked_user_id: "private-owner@im.wechat".into(),
                        enabled: true,
                    },
                ))
            })
            .unwrap()
            .unwrap();
        let (_, page) = request(&state, "GET", base, Value::Null, Some('a'), false, None).await;
        assert_eq!(page["revision"], 1);
        assert_eq!(page["account"]["credential_configured"], true);
        assert_eq!(page["account"]["connection_status"], "connecting");
        for private in [
            &reference,
            "private-existing-bot-token",
            "private-owner@im.wechat",
            "credential_ref",
            "base_url",
        ] {
            assert!(!page.to_string().contains(private));
        }
        assert_eq!(page["capabilities"]["groups"], false);
        assert_eq!(page["capabilities"]["media"], false);
        let pending = request(
            &state,
            "POST",
            &format!("{base}/check"),
            json!({}),
            Some('a'),
            true,
            Some("\"1\""),
        )
        .await;
        assert_eq!(pending.0, StatusCode::ACCEPTED);
        assert_eq!(pending.1["status"], "pending");
        assert_eq!(
            request(
                &state,
                "POST",
                &format!("{base}/check"),
                json!({}),
                Some('a'),
                true,
                Some("\"1\"")
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        state
            .db(|db| {
                assert!(weixin_login::pending_check(db)?.is_some());
                assert!(weixin_login::complete_check(
                    db,
                    &account,
                    weixin_login::CheckOutcome::Passed
                )?);
                Ok(())
            })
            .unwrap();
        let (_, checked) = request(&state, "GET", base, Value::Null, Some('a'), false, None).await;
        assert_eq!(checked["check"]["status"], "passed");
        assert_eq!(
            checked["account"]["connection_status"], "connecting",
            "a check result alone does not invent receiver connectivity"
        );
        let disabled = request(
            &state,
            "POST",
            &format!("{base}/enabled"),
            json!({"enabled":false}),
            Some('a'),
            true,
            Some("\"1\""),
        )
        .await;
        assert_eq!(disabled.0, StatusCode::OK);
        assert_eq!(disabled.1["revision"], 2);
        assert_eq!(disabled.1["account"]["enabled"], false);
        let stale = request(
            &state,
            "POST",
            &format!("{base}/enabled"),
            json!({"enabled":true}),
            Some('a'),
            true,
            Some("\"1\""),
        )
        .await;
        assert_eq!(stale.0, StatusCode::CONFLICT);
        assert_eq!(stale.1["error"]["code"], "weixin_revision_conflict");
        assert_eq!(
            request(
                &state,
                "POST",
                &format!("{base}/check"),
                json!({}),
                Some('a'),
                true,
                Some("\"2\"")
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
}
