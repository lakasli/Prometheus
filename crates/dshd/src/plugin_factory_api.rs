use crate::{
    auth::Admin,
    error::{ApiError, ApiResult},
    plugin_factory as factory, plugin_supervisor,
    state::AppState,
};
use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/v1/plugin-factory", get(page).post(create))
        .route("/api/admin/v1/plugin-factory/check", post(check))
        .route("/api/admin/v1/plugin-factory/versions/{id}", get(detail))
        .route(
            "/api/admin/v1/plugin-factory/versions/{id}/build",
            post(build),
        )
        .route(
            "/api/admin/v1/plugin-factory/versions/{id}/test",
            post(test),
        )
        .route(
            "/api/admin/v1/plugin-factory/versions/{id}/approve",
            post(approve),
        )
        .route(
            "/api/admin/v1/plugin-factory/versions/{id}/activate",
            post(activate),
        )
        .route(
            "/api/admin/v1/plugin-factory/versions/{id}/rollback",
            post(rollback),
        )
        .route(
            "/api/admin/v1/plugin-factory/versions/{id}/quarantine",
            post(quarantine),
        )
        .route("/api/admin/v1/plugin-factory/jobs/{id}", get(job))
        .route(
            "/api/admin/v1/plugin-factory/jobs/{id}/cancel",
            post(cancel),
        )
        .route(
            "/api/admin/v1/plugin-factory/plugins/{id}/invoke",
            post(invoke),
        )
}
fn expected(headers: &HeaderMap) -> ApiResult<i64> {
    let value = headers
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::PRECONDITION_REQUIRED,
                "if_match_required",
                "请提交插件当前If-Match",
            )
        })?;
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v > 0)
        .ok_or_else(|| ApiError::bad("invalid_if_match", "If-Match必须为当前正整数ETag"))
}
fn response<T: serde::Serialize>(status: StatusCode, revision: i64, value: T) -> Response {
    (
        status,
        [(header::ETAG, format!("\"{revision}\""))],
        Json(value),
    )
        .into_response()
}
async fn page(State(state): State<AppState>, admin: Admin) -> ApiResult<Json<factory::Page>> {
    Ok(Json(factory::page(&state, &admin.id)?))
}
async fn create(
    State(state): State<AppState>,
    admin: Admin,
    Json(body): Json<factory::CreateBody>,
) -> ApiResult<Response> {
    let value = factory::create(&state, &admin.id, &body)?;
    state.0.notify.notify_waiters();
    Ok(response(StatusCode::CREATED, value.version.revision, value))
}
async fn detail(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let value = state.db(|db| Ok(factory::detail(db, &id, &admin.id)))??;
    Ok(response(StatusCode::OK, value.version.revision, value))
}
async fn queue(
    state: AppState,
    owner: String,
    id: String,
    headers: HeaderMap,
    body: factory::ActionBody,
    kind: &str,
) -> ApiResult<Response> {
    let value = factory::queue(&state, &owner, &id, expected(&headers)?, kind, &body)?;
    state.0.notify.notify_waiters();
    Ok(response(StatusCode::ACCEPTED, value.revision, value))
}
async fn build(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<factory::ActionBody>,
) -> ApiResult<Response> {
    queue(state, admin.id, id, headers, body, "build").await
}
async fn test(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<factory::ActionBody>,
) -> ApiResult<Response> {
    queue(state, admin.id, id, headers, body, "test").await
}
async fn approve(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<factory::ApprovalBody>,
) -> ApiResult<Response> {
    let revision = expected(&headers)?;
    let value = state.db(|db| Ok(factory::approve(db, &id, &admin.id, revision, &body)))??;
    Ok(response(StatusCode::OK, value.revision, value))
}
async fn route(
    state: AppState,
    owner: String,
    id: String,
    headers: HeaderMap,
    body: factory::RouteBody,
    rollback: bool,
) -> ApiResult<Response> {
    let revision = expected(&headers)?;
    let value = state.db(|db| {
        Ok(factory::activate(
            db, &id, &owner, revision, &body, rollback,
        ))
    })??;
    Ok(response(StatusCode::OK, value.revision, value))
}
async fn activate(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<factory::RouteBody>,
) -> ApiResult<Response> {
    route(state, admin.id, id, headers, body, false).await
}
async fn rollback(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<factory::RouteBody>,
) -> ApiResult<Response> {
    route(state, admin.id, id, headers, body, true).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
async fn quarantine(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(_): Json<Empty>,
) -> ApiResult<Response> {
    let revision = expected(&headers)?;
    let value = state.db(|db| Ok(factory::quarantine(db, &id, &admin.id, revision)))??;
    state.0.notify.notify_waiters();
    Ok(response(StatusCode::OK, value.revision, value))
}
async fn job(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let value = state.db(|db| Ok(factory::job(db, &id, &admin.id)))??;
    Ok(response(StatusCode::OK, value.revision, value))
}
async fn cancel(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(_): Json<Empty>,
) -> ApiResult<Response> {
    let revision = expected(&headers)?;
    let value=state.db(|db|Ok((||->ApiResult<_>{
        let tx=db.transaction().map_err(|_|ApiError::internal())?;let job=factory::job(&tx,&id,&admin.id)?;
        if job.revision!=revision||!matches!(job.status.as_str(),"queued"|"running"){return Err(ApiError::new(StatusCode::CONFLICT,"plugin_revision_conflict","任务状态已变化，请刷新"));}
        tx.execute("UPDATE plugin_factory_jobs SET status='cancelled',revision=revision+1 WHERE id=?1",[&id]).map_err(|_|ApiError::internal())?;
        tx.execute("UPDATE plugin_factory_versions SET status=CASE status WHEN 'generating' THEN 'specified' WHEN 'building' THEN 'generated' WHEN 'testing' THEN 'built' ELSE status END,revision=revision+1 WHERE id=?1 AND status IN ('generating','building','testing')",[&job.version_id]).map_err(|_|ApiError::internal())?;
        // leased stays set until the HTTP future / exact process has exited.
        let value=factory::job(&tx,&id,&admin.id)?;tx.commit().map_err(|_|ApiError::internal())?;Ok(value)
    })()))??;
    state.0.notify.notify_waiters();
    Ok(response(StatusCode::OK, value.revision, value))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Invoke {
    input: Value,
}
async fn invoke(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    Json(body): Json<Invoke>,
) -> ApiResult<Json<Value>> {
    let output = factory::invoke_active(
        &state,
        &admin.id,
        &id,
        body.input,
        &CancellationToken::new(),
    )
    .await?;
    Ok(Json(json!({"output":output})))
}
async fn check(
    State(state): State<AppState>,
    _admin: Admin,
    Json(_): Json<Empty>,
) -> ApiResult<Json<Value>> {
    let response = plugin_supervisor::verify_runtime(&state)
        .await
        .map_err(|error| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                error.code,
                "工厂隔离检查未通过，构建保持不可用",
            )
        })?;
    Ok(Json(
        json!({"passed":response.ok,"isolation":response.isolation,"probes":response.probes,"engine":response.engine,"host_api":response.host_api}),
    ))
}
