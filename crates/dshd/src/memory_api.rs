use crate::{
    auth::Admin,
    error::{ApiError, ApiResult},
    memory::{self, CreateMemory, ListOptions, UpdateMemory},
    state::AppState,
};
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};

/// Merge before the root authentication/Origin/CSRF middleware is applied.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/v1/memory", get(list).post(create))
        .route("/api/admin/v1/memory/export", get(export))
        .route(
            "/api/admin/v1/memory/{id}",
            get(show).patch(update).delete(forget),
        )
}

fn etagged<T: serde::Serialize>(status: StatusCode, revision: i64, item: T) -> Response {
    (
        status,
        [(header::ETAG, format!("\"{revision}\""))],
        Json(item),
    )
        .into_response()
}

fn expected_revision(headers: &HeaderMap) -> ApiResult<i64> {
    let raw = headers
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::PRECONDITION_REQUIRED,
                "if_match_required",
                "必须提交记忆当前版本If-Match",
            )
        })?;
    let raw = raw
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(raw);
    raw.parse::<i64>()
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(|| ApiError::bad("invalid_if_match", "If-Match必须为当前记忆的正整数版本"))
}

async fn list(
    State(state): State<AppState>,
    _admin: Admin,
    Query(options): Query<ListOptions>,
) -> ApiResult<Json<memory::MemoryPage>> {
    let page = state.db(|db| Ok(memory::admin_list(db, &options)))??;
    Ok(Json(page))
}

async fn show(
    State(state): State<AppState>,
    _admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let item = state.db(|db| Ok(memory::get(db, &id)))??;
    Ok(etagged(StatusCode::OK, item.revision, item))
}

async fn create(
    State(state): State<AppState>,
    admin: Admin,
    Json(body): Json<CreateMemory>,
) -> ApiResult<Response> {
    let item = state.db(|db| Ok(memory::create(db, &admin.id, &body)))??;
    Ok(etagged(StatusCode::CREATED, item.revision, item))
}

async fn update(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<UpdateMemory>,
) -> ApiResult<Response> {
    let expected = expected_revision(&headers)?;
    let item = state.db(|db| Ok(memory::update(db, &admin.id, &id, expected, &body)))??;
    Ok(etagged(StatusCode::OK, item.revision, item))
}

async fn forget(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let expected = expected_revision(&headers)?;
    let item = state.db(|db| Ok(memory::forget(db, &admin.id, &id, expected)))??;
    Ok(etagged(StatusCode::OK, item.revision, item))
}

async fn export(State(state): State<AppState>, _admin: Admin) -> ApiResult<Response> {
    let result = state.db(|db| Ok(memory::export(db)))??;
    Ok((
        [(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"dsh-assistant-memory.json\"",
        )],
        Json(result),
    )
        .into_response())
}
