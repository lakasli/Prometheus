//! Owner-scoped manual WAT generation, artifact review and pure calculation.
use crate::{
    auth::Admin,
    error::{ApiError, ApiResult},
    state::AppState,
    wasm_workbench as workbench,
};
use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/v1/wasm-workbench", get(page))
        .route("/api/admin/v1/wasm-workbench/generate", post(generate))
        .route(
            "/api/admin/v1/wasm-workbench/jobs/{id}",
            get(show).delete(delete),
        )
        .route("/api/admin/v1/wasm-workbench/artifacts/{id}", get(artifact))
        .route(
            "/api/admin/v1/wasm-workbench/artifacts/{id}/approve",
            post(approve),
        )
        .route(
            "/api/admin/v1/wasm-workbench/artifacts/{id}/reject",
            post(reject),
        )
        .route(
            "/api/admin/v1/wasm-workbench/artifacts/{id}/revoke",
            post(revoke),
        )
        .route(
            "/api/admin/v1/wasm-workbench/artifacts/{id}/execute",
            post(execute),
        )
}
fn expected(headers: &HeaderMap) -> ApiResult<i64> {
    let value = headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::PRECONDITION_REQUIRED,
                "if_match_required",
                "请提交工坊任务或制品当前If-Match",
            )
        })?;
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| ApiError::bad("invalid_if_match", "If-Match必须为当前正整数ETag"))
}
fn response<T: serde::Serialize>(status: StatusCode, revision: i64, item: T) -> Response {
    (
        status,
        [(header::ETAG, format!("\"{revision}\""))],
        Json(item),
    )
        .into_response()
}
async fn page(State(state): State<AppState>, admin: Admin) -> ApiResult<Json<workbench::Page>> {
    Ok(Json(workbench::page(&state, &admin.id)?))
}
async fn generate(
    State(state): State<AppState>,
    admin: Admin,
    Json(body): Json<workbench::GenerateBody>,
) -> ApiResult<Response> {
    let item = workbench::generate(&state, &admin.id, &body)?;
    state.0.notify.notify_waiters();
    Ok(response(StatusCode::ACCEPTED, item.revision, item))
}
async fn show(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let item = state.db(|db| Ok(workbench::show(db, &id, &admin.id)))??;
    Ok(response(StatusCode::OK, item.revision, item))
}
async fn delete(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let revision = expected(&headers)?;
    let item = state.db(|db| Ok(workbench::delete(db, &id, &admin.id, revision)))??;
    state.0.notify.notify_waiters();
    Ok(response(StatusCode::OK, item.revision, item))
}
async fn artifact(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let item = state.db(|db| Ok(workbench::artifact(db, &id, &admin.id)))??;
    Ok(response(StatusCode::OK, item.summary.revision, item))
}
async fn approve(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<workbench::ApproveBody>,
) -> ApiResult<Response> {
    let revision = expected(&headers)?;
    let item = state.db(|db| Ok(workbench::approve(db, &id, &admin.id, revision, &body)))??;
    Ok(response(StatusCode::OK, item.revision, item))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
async fn change(
    state: AppState,
    admin: Admin,
    id: String,
    headers: HeaderMap,
    revoke: bool,
) -> ApiResult<Response> {
    let revision = expected(&headers)?;
    let item = state.db(|db| {
        Ok(workbench::change_artifact(
            db, &id, &admin.id, revision, revoke,
        ))
    })??;
    state.0.notify.notify_waiters();
    Ok(response(StatusCode::OK, item.revision, item))
}
async fn reject(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(_): Json<Empty>,
) -> ApiResult<Response> {
    change(state, admin, id, headers, false).await
}
async fn revoke(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(_): Json<Empty>,
) -> ApiResult<Response> {
    change(state, admin, id, headers, true).await
}
async fn execute(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<workbench::ExecuteBody>,
) -> ApiResult<Response> {
    let revision = expected(&headers)?;
    let item = workbench::execute(&state, &id, &admin.id, revision, &body)?;
    state.0.notify.notify_waiters();
    Ok(response(StatusCode::ACCEPTED, item.revision, item))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{now, token_hash, Options};
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use serde_json::{json, Value};
    use tower::ServiceExt;

    #[allow(clippy::too_many_arguments)]
    async fn request(
        state: &AppState,
        method: &str,
        path: &str,
        body: Value,
        cookie: bool,
        csrf: bool,
        revision: Option<&str>,
        origin: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1:18435")
            .header("content-type", "application/json")
            .header("origin", origin.unwrap_or(&state.0.options.origin));
        if cookie {
            request = request.header("cookie", format!("dsh_session={}", "c".repeat(64)));
        }
        if csrf {
            request = request.header("x-csrf-token", "csrf");
        }
        if let Some(value) = revision {
            request = request.header("if-match", value);
        }
        let response = crate::api::router(state.clone())
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 262144).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    #[tokio::test]
    async fn administrator_auth_owner_origin_csrf_revisions_and_unavailable_gate() {
        let temporary = tempfile::tempdir().unwrap();
        let state = AppState::open(Options {
            data_dir: temporary.path().to_owned(),
            origin: "http://127.0.0.1:18435".into(),
            allow_private_model: true,
            cookie_secure: false,
            web_dir: temporary.path().to_owned(),
        })
        .unwrap();
        state.db(|db|{db.execute("INSERT INTO admins(id,username,password_hash,created_at) VALUES('owner','owner','unused',0)",[])?;db.execute("INSERT INTO sessions(token_hash,user_id,csrf,expires_at) VALUES(?1,'owner','csrf',?2)",rusqlite::params![token_hash(&"c".repeat(64)),now()+3600])?;db.execute("INSERT INTO wasm_workbench_jobs(id,owner_id,kind,client_request_id,request_digest,name,status,revision,created_at,expires_at) VALUES('own','owner','generate','own-client','digest','own','cancelled',1,0,?1),('foreign','another','generate','foreign-client','digest','foreign','cancelled',1,0,?1)",[now()+3600])?;Ok(())}).unwrap();
        let root = "/api/admin/v1/wasm-workbench";
        assert_eq!(
            request(&state, "GET", root, json!({}), false, false, None, None)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        let page = request(&state, "GET", root, json!({}), true, false, None, None).await;
        assert_eq!(page.0, StatusCode::OK);
        assert_eq!(page.1["available"], false);
        assert_eq!(page.1["jobs"].as_array().unwrap().len(), 1);
        assert_eq!(page.1["engine_version"], "wasmi-2.0.0");
        assert_eq!(page.1["policy_version"], "pure-i32-v1");
        assert!(!page.1.to_string().contains("config_json"));
        let path = format!("{root}/jobs/own");
        assert_eq!(
            request(
                &state,
                "GET",
                &format!("{root}/jobs/foreign"),
                json!({}),
                true,
                false,
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
                "DELETE",
                &path,
                json!({}),
                true,
                false,
                Some("\"1\""),
                None
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(
                &state,
                "DELETE",
                &path,
                json!({}),
                true,
                true,
                Some("\"1\""),
                Some("https://example.invalid")
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(&state, "DELETE", &path, json!({}), true, true, None, None)
                .await
                .0,
            StatusCode::PRECONDITION_REQUIRED
        );
        assert_eq!(
            request(
                &state,
                "DELETE",
                &path,
                json!({}),
                true,
                true,
                Some("\"0\""),
                None
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            request(
                &state,
                "DELETE",
                &path,
                json!({}),
                true,
                true,
                Some("\"9\""),
                None
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            request(
                &state,
                "DELETE",
                &path,
                json!({}),
                true,
                true,
                Some("\"1\""),
                None
            )
            .await
            .0,
            StatusCode::OK
        );
        let input = json!({"client_request_id":uuid::Uuid::new_v4().to_string(),"name":"合成加法","spec":"返回整数和","tests":[{"a":1,"b":2,"expected":3},{"a":3,"b":4,"expected":7}]});
        assert_eq!(
            request(
                &state,
                "POST",
                &format!("{root}/generate"),
                input.clone(),
                true,
                true,
                None,
                None
            )
            .await
            .0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        let mut invalid = input;
        invalid["tools"] = json!([]);
        assert_eq!(
            request(
                &state,
                "POST",
                &format!("{root}/generate"),
                invalid,
                true,
                true,
                None,
                None
            )
            .await
            .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
}
