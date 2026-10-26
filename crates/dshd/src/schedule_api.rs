use crate::{
    auth::Admin,
    error::{ApiError, ApiResult},
    schedule,
    state::{now, AppState},
    trigger_rule,
};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/v1/schedule/calendar", get(calendar))
        .route("/api/admin/v1/schedule/capabilities", get(capabilities))
        .route("/api/admin/v1/schedule/activities", get(list).post(create))
        .route("/api/admin/v1/schedule/proposals", post(proposal))
        .route(
            "/api/admin/v1/schedule/activities/preview",
            post(preview_new),
        )
        .route(
            "/api/admin/v1/schedule/activities/{id}",
            get(detail).patch(change),
        )
        .route(
            "/api/admin/v1/schedule/activities/{id}/preview",
            post(preview),
        )
        .route("/api/admin/v1/schedule/activities/{id}/pause", post(pause))
        .route(
            "/api/admin/v1/schedule/activities/{id}/approve",
            post(approve),
        )
        .route(
            "/api/admin/v1/schedule/activities/{id}/resume",
            post(resume),
        )
        .route(
            "/api/admin/v1/schedule/activities/{id}/cancel",
            post(cancel),
        )
        .route(
            "/api/admin/v1/schedule/autonomy-grants",
            get(grants).post(grant_create),
        )
        .route(
            "/api/admin/v1/schedule/autonomy-grants/{id}",
            get(grant_detail).patch(grant_change),
        )
        .route(
            "/api/admin/v1/schedule/autonomy-grants/{id}/revoke",
            post(grant_revoke),
        )
        .route(
            "/api/admin/v1/schedule/resources",
            get(resources).post(resource_create),
        )
        .route(
            "/api/admin/v1/schedule/resources/{id}/revoke",
            post(resource_revoke),
        )
        .route("/api/admin/v1/schedule/plans", get(plans).post(plan_create))
        .route(
            "/api/admin/v1/schedule/plans/{id}",
            axum::routing::patch(plan_change),
        )
        .layer(DefaultBodyLimit::max(600 * 1024))
}
fn body(bytes: &[u8], allowed: &[&str]) -> ApiResult<Value> {
    let v = trigger_rule::strict_value(bytes)
        .map_err(|_| ApiError::bad("invalid_schedule_json", "需要有效且不含重复字段的JSON"))?;
    let obj = v
        .as_object()
        .ok_or_else(|| ApiError::bad("invalid_schedule_json", "请求必须是对象"))?;
    if obj.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(ApiError::bad("unknown_schedule_field", "请求包含未知字段"));
    }
    Ok(v)
}
fn activity_body(bytes: &[u8]) -> ApiResult<Value> {
    body(
        bytes,
        &[
            "title",
            "type",
            "purpose",
            "start_at",
            "end_at",
            "time_zone",
            "resource_id",
            "target_ref",
            "repeat",
            "movable",
            "grant_id",
            "restart_reading",
        ],
    )
}
fn expected(v: &Value, headers: &HeaderMap) -> ApiResult<i64> {
    let from_header = headers
        .get("if-match")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim_matches('"').parse().ok());
    let from_body = v["expected_revision"].as_i64();
    if from_header.is_some() && from_body.is_some() && from_header != from_body {
        return Err(ApiError::bad(
            "revision_mismatch",
            "If-Match与正文版本不一致",
        ));
    }
    from_header
        .or(from_body)
        .filter(|n| *n > 0)
        .ok_or_else(|| ApiError::bad("expected_revision_required", "需要当前版本或If-Match"))
}
fn key(headers: &HeaderMap) -> ApiResult<&str> {
    headers
        .get("idempotency-key")
        .and_then(|h| h.to_str().ok())
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .ok_or_else(|| ApiError::bad("idempotency_key_required", "新建活动需要Idempotency-Key"))
}
fn writable_clock(state: &AppState) -> ApiResult<()> {
    if crate::trigger_scheduler::clock_status(state)["trusted"] != true {
        return Err(ApiError::bad(
            "blocked_clock",
            "请先在兼容触发设置确认服务器时钟",
        ));
    }
    Ok(())
}
#[derive(Deserialize)]
struct Range {
    start: i64,
    end: i64,
    time_zone: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    creator: Option<String>,
    state: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
}
async fn calendar(
    State(s): State<AppState>,
    a: Admin,
    Query(q): Query<Range>,
) -> ApiResult<Json<Value>> {
    Ok(Json(s.db(|db| {
        schedule::calendar(
            db,
            &a.id,
            q.start,
            q.end,
            q.time_zone.as_deref().unwrap_or("Asia/Shanghai"),
            q.kind.as_deref(),
            q.creator.as_deref(),
            q.state.as_deref(),
            q.offset.unwrap_or(0),
            q.limit.unwrap_or(500),
        )
    })?))
}
async fn capabilities(State(s): State<AppState>, _: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(
        json!({"text_reading":true,"video":false,"proactive_chat":false,"model_planning":true,"max_range_days":93,"max_page_items":500,"reading_chunk_characters":4096,"legacy_trigger_compatibility":true,"clock":crate::trigger_scheduler::clock_status(&s)}),
    ))
}
async fn list(State(s): State<AppState>, a: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(s.db(|db| schedule::list(db, &a.id))?))
}
async fn detail(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(s.db(|db| schedule::detail(db, &a.id, &id))?))
}
async fn create(
    State(s): State<AppState>,
    a: Admin,
    h: HeaderMap,
    b: Bytes,
) -> ApiResult<Json<Value>> {
    create_inner(s, a, h, b, false).await
}
async fn proposal(
    State(s): State<AppState>,
    a: Admin,
    h: HeaderMap,
    b: Bytes,
) -> ApiResult<Json<Value>> {
    create_inner(s, a, h, b, true).await
}
async fn create_inner(
    s: AppState,
    a: Admin,
    h: HeaderMap,
    b: Bytes,
    autonomous: bool,
) -> ApiResult<Json<Value>> {
    writable_clock(&s)?;
    let v = activity_body(&b)?;
    let k = key(&h)?;
    let result = s.db(|db| {
        let tx = db.transaction()?;
        let result = schedule::create(&tx, &a.id, &v, k, autonomous, now())?;
        tx.commit()?;
        Ok(result)
    })?;
    s.0.notify.notify_waiters();
    Ok(Json(result))
}
async fn preview_new(State(s): State<AppState>, a: Admin, b: Bytes) -> ApiResult<Json<Value>> {
    let v = activity_body(&b)?;
    Ok(Json(
        s.db(|db| schedule::preview(db, &a.id, &v, None, now()))?,
    ))
}
async fn preview(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult<Json<Value>> {
    let v = if b.is_empty() {
        s.db(|db| schedule::detail(db, &a.id, &id))?["definition"].clone()
    } else {
        activity_body(&b)?
    };
    s.db(|db| schedule::detail(db, &a.id, &id))?;
    Ok(Json(s.db(|db| {
        schedule::preview(db, &a.id, &v, Some(&id), now())
    })?))
}
async fn change(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
    b: Bytes,
) -> ApiResult<Json<Value>> {
    writable_clock(&s)?;
    let v = body(
        &b,
        &[
            "expected_revision",
            "scope",
            "nominal_at",
            "definition",
            "state",
        ],
    )?;
    let rev = expected(&v, &h)?;
    let result = s.db(|db| {
        let tx = db.transaction()?;
        let result = schedule::change(
            &tx,
            &a.id,
            &id,
            rev,
            v["scope"].as_str().unwrap_or("series"),
            v["nominal_at"].as_i64(),
            v.get("definition"),
            v["state"].as_str(),
            false,
            now(),
        )?;
        tx.commit()?;
        Ok(result)
    })?;
    s.0.notify.notify_waiters();
    Ok(Json(result))
}
async fn approve(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
    b: Bytes,
) -> ApiResult<Json<Value>> {
    writable_clock(&s)?;
    let v = body(&b, &["confirmed", "scope"])?;
    if h.get("if-match").is_none() {
        return Err(ApiError::bad(
            "if_match_required",
            "批准必须携带精确If-Match版本",
        ));
    }
    let revision = expected(&Value::Null, &h)?;
    let scope = v["scope"].as_str().unwrap_or("");
    let result = s.db(|db| {
        let tx = db.transaction()?;
        let result = schedule::approve_admin_in(
            &tx,
            &a.id,
            &a.id,
            &id,
            revision,
            v["confirmed"] == true,
            scope,
            now(),
        )?;
        tx.commit()?;
        Ok(result)
    })?;
    s.0.notify.notify_waiters();
    Ok(Json(result))
}
async fn pause(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
    b: Bytes,
) -> ApiResult<Json<Value>> {
    state_change(s, a, id, h, b, "paused").await
}
async fn resume(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
    b: Bytes,
) -> ApiResult<Json<Value>> {
    writable_clock(&s)?;
    state_change(s, a, id, h, b, "enabled").await
}
async fn cancel(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
    b: Bytes,
) -> ApiResult<Json<Value>> {
    state_change(s, a, id, h, b, "cancelled").await
}
async fn state_change(
    s: AppState,
    a: Admin,
    id: String,
    h: HeaderMap,
    b: Bytes,
    state: &str,
) -> ApiResult<Json<Value>> {
    let v = body(&b, &["expected_revision", "scope", "nominal_at"])?;
    let rev = expected(&v, &h)?;
    let result = s.db(|db| {
        let tx = db.transaction()?;
        let result = schedule::change(
            &tx,
            &a.id,
            &id,
            rev,
            v["scope"].as_str().unwrap_or("series"),
            v["nominal_at"].as_i64(),
            None,
            Some(state),
            false,
            now(),
        )?;
        tx.commit()?;
        Ok(result)
    })?;
    s.0.notify.notify_waiters();
    Ok(Json(result))
}
async fn grants(State(s): State<AppState>, a: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(s.db(|db| schedule::list_grants(db, &a.id))?))
}
async fn grant_detail(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(s.db(|db| schedule::grant_detail(db, &a.id, &id))?))
}
async fn grant_create(State(s): State<AppState>, a: Admin, b: Bytes) -> ApiResult<Json<Value>> {
    let v = trigger_rule::strict_value(&b)
        .map_err(|_| ApiError::bad("invalid_grant", "授权JSON无效"))?;
    Ok(Json(s.db(|db| {
        let tx = db.transaction()?;
        let result = schedule::create_grant(&tx, &a.id, &v, now())?;
        tx.commit()?;
        Ok(result)
    })?))
}
async fn grant_change(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
    b: Bytes,
) -> ApiResult<Json<Value>> {
    let v = body(&b, &["expected_revision", "policy"])?;
    let rev = expected(&v, &h)?;
    let p = v
        .get("policy")
        .ok_or_else(|| ApiError::bad("invalid_grant", "需要完整policy"))?;
    Ok(Json(s.db(|db| {
        let tx = db.transaction()?;
        let result = schedule::change_grant(&tx, &a.id, &id, rev, Some(p), now())?;
        tx.commit()?;
        Ok(result)
    })?))
}
async fn grant_revoke(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
    b: Bytes,
) -> ApiResult<Json<Value>> {
    let v = body(&b, &["expected_revision"])?;
    let rev = expected(&v, &h)?;
    Ok(Json(s.db(|db| {
        let tx = db.transaction()?;
        let result = schedule::change_grant(&tx, &a.id, &id, rev, None, now())?;
        tx.commit()?;
        Ok(result)
    })?))
}
async fn resources(State(s): State<AppState>, a: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(s.db(|db| schedule::list_resources(db, &a.id))?))
}
async fn resource_create(State(s): State<AppState>, a: Admin, b: Bytes) -> ApiResult<Json<Value>> {
    // Text resources use the larger bounded payload; strict_value's trigger
    // parser intentionally has a smaller 64KiB limit and is not reused here.
    let v: Value = serde_json::from_slice(&b)
        .map_err(|_| ApiError::bad("invalid_resource", "资料JSON无效"))?;
    Ok(Json(s.db(|db| {
        let tx = db.transaction()?;
        let result = schedule::create_resource(&tx, &a.id, &v, now())?;
        tx.commit()?;
        Ok(result)
    })?))
}
async fn resource_revoke(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
    b: Bytes,
) -> ApiResult<Json<Value>> {
    let v = body(&b, &["expected_revision"])?;
    let rev = expected(&v, &h)?;
    Ok(Json(s.db(|db| {
        let tx = db.transaction()?;
        let result = schedule::revoke_resource(&tx, &a.id, &id, rev, now())?;
        tx.commit()?;
        Ok(result)
    })?))
}
async fn plans(State(s): State<AppState>, a: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(s.db(|db| schedule::list_plans(db, &a.id))?))
}
async fn plan_create(State(s): State<AppState>, a: Admin, b: Bytes) -> ApiResult<Json<Value>> {
    let v = body(
        &b,
        &[
            "goal",
            "grant_id",
            "resource_ids",
            "duration_minutes",
            "state",
            "planning_mode",
        ],
    )?;
    Ok(Json(s.db(|db| {
        let tx = db.transaction()?;
        let result = schedule::create_plan(&tx, &a.id, &v, now())?;
        tx.commit()?;
        Ok(result)
    })?))
}
async fn plan_change(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
    b: Bytes,
) -> ApiResult<Json<Value>> {
    let v = body(&b, &["expected_revision", "state"])?;
    let rev = expected(&v, &h)?;
    let state = v["state"]
        .as_str()
        .ok_or_else(|| ApiError::bad("invalid_work_plan", "需要state"))?;
    Ok(Json(s.db(|db| {
        let tx = db.transaction()?;
        let result = schedule::change_plan(&tx, &a.id, &id, rev, state, now())?;
        tx.commit()?;
        Ok(result)
    })?))
}
