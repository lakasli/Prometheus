use crate::{
    auth::Admin,
    error::{ApiError, ApiResult},
    state::{now, AppState},
    trigger_calendar, trigger_rule, trigger_scheduler, trigger_store,
};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/admin/v1/task-triggers/capabilities",
            get(capabilities),
        )
        .route(
            "/api/admin/v1/task-triggers/clock/acknowledge",
            post(clock_ack),
        )
        .route("/api/admin/v1/task-triggers/validate", post(validate))
        .route("/api/admin/v1/task-triggers/preview", post(preview))
        .route("/api/admin/v1/task-triggers/jobs", get(list).post(create))
        .route("/api/admin/v1/task-triggers/jobs/{id}", get(detail))
        .route("/api/admin/v1/task-triggers/jobs/{id}/drafts", post(save))
        .route(
            "/api/admin/v1/task-triggers/jobs/{id}/activate",
            post(activate),
        )
        .route("/api/admin/v1/task-triggers/jobs/{id}/pause", post(pause))
        .route("/api/admin/v1/task-triggers/jobs/{id}/stop", post(stop))
        .route("/api/admin/v1/task-triggers/jobs/{id}/resume", post(resume))
        .route(
            "/api/admin/v1/task-triggers/jobs/{id}/archive",
            post(archive),
        )
        .route(
            "/api/admin/v1/task-triggers/jobs/{id}/test-runs",
            post(test_run),
        )
        .route(
            "/api/admin/v1/task-triggers/jobs/{id}/occurrences",
            get(history),
        )
        .route("/api/admin/v1/task-triggers/occurrences/{id}", get(result))
        .route(
            "/api/admin/v1/task-triggers/occurrences/{id}/image",
            get(image),
        )
        .route(
            "/api/admin/v1/task-triggers/occurrences/{id}/retry-delivery",
            post(retry_delivery),
        )
        .route("/api/admin/v1/task-triggers/calendars", get(calendars))
        .route(
            "/api/admin/v1/task-triggers/calendars/drafts",
            post(calendar_draft),
        )
        .route(
            "/api/admin/v1/task-triggers/calendars/{id}/apply",
            post(calendar_apply),
        )
        .layer(DefaultBodyLimit::max(64 * 1024))
}
fn body(bytes: &[u8], allowed: &[&str]) -> ApiResult<Value> {
    let v =
        trigger_rule::strict_value(bytes).map_err(|e| ApiError::bad("invalid_trigger_json", e))?;
    let obj = v
        .as_object()
        .ok_or_else(|| ApiError::bad("invalid_trigger_json", "请求必须为JSON对象"))?;
    if obj.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(ApiError::bad(
            "unknown_trigger_field",
            "请求包含未允许的字段",
        ));
    };
    Ok(v)
}
fn expected(v: &Value) -> ApiResult<i64> {
    v["expected_revision"]
        .as_i64()
        .filter(|i| *i > 0)
        .ok_or_else(|| ApiError::bad("expected_revision_required", "需要当前任务版本"))
}
fn key(headers: &HeaderMap) -> ApiResult<Option<&str>> {
    headers
        .get("Idempotency-Key")
        .map(|h| {
            h.to_str()
                .ok()
                .filter(|s| !s.is_empty() && s.len() <= 128)
                .ok_or_else(|| ApiError::bad("invalid_idempotency_key", "幂等编号无效"))
        })
        .transpose()
}
fn require_clock(state: &AppState) -> ApiResult<()> {
    if trigger_scheduler::clock_status(state)["trusted"] != true {
        return Err(ApiError::bad(
            "blocked_clock",
            "请先确认服务器时间或启用系统时钟同步",
        ));
    };
    Ok(())
}
#[derive(Deserialize, Default)]
struct Paging {
    limit: Option<i64>,
    offset: Option<i64>,
}
fn paging(v: &Paging) -> ApiResult<(i64, i64)> {
    let limit = v.limit.unwrap_or(20);
    let offset = v.offset.unwrap_or(0);
    if !(1..=100).contains(&limit) || !(0..=100000).contains(&offset) {
        return Err(ApiError::bad("invalid_pagination", "分页范围无效"));
    };
    Ok((limit, offset))
}
async fn capabilities(State(state): State<AppState>, admin: Admin) -> ApiResult<Json<Value>> {
    let mut v = state.db(|db| trigger_store::capabilities_in(db, &admin.id, now()))?;
    v["clock"] = trigger_scheduler::clock_status(&state);
    Ok(Json(v))
}
async fn clock_ack(
    State(state): State<AppState>,
    admin: Admin,
    bytes: Bytes,
) -> ApiResult<Json<Value>> {
    let v = body(&bytes, &["observed_now", "confirmed"])?;
    let observed = v["observed_now"]
        .as_i64()
        .ok_or_else(|| ApiError::bad("invalid_clock", "缺少服务器时间"))?;
    let result = trigger_scheduler::acknowledge(&state, observed, v["confirmed"] == true)?;
    state.audit(&admin.id, "trigger.clock.acknowledge", "server-clock")?;
    state.0.notify.notify_waiters();
    Ok(Json(result))
}
async fn validate(
    State(state): State<AppState>,
    admin: Admin,
    bytes: Bytes,
) -> ApiResult<Json<Value>> {
    preview_inner(state, admin, bytes, false).await
}
async fn preview(
    State(state): State<AppState>,
    admin: Admin,
    bytes: Bytes,
) -> ApiResult<Json<Value>> {
    preview_inner(state, admin, bytes, true).await
}
async fn preview_inner(
    state: AppState,
    admin: Admin,
    bytes: Bytes,
    token: bool,
) -> ApiResult<Json<Value>> {
    let v = body(
        &bytes,
        &[
            "task_trigger_draft",
            "job_id",
            "expected_revision",
            "draft_revision",
        ],
    )?;
    let value = v
        .get("task_trigger_draft")
        .ok_or_else(|| ApiError::bad("invalid_trigger", "缺少任务草稿"))?;
    let id = v["job_id"].as_str();
    let result = state.db(|db| {
        trigger_store::preview(
            db,
            &admin.id,
            value,
            id,
            v["expected_revision"].as_i64(),
            v["draft_revision"].as_i64(),
            token,
        )
    });
    match result {
        Ok(v) => Ok(Json(v)),
        Err(error)
            if error.status != StatusCode::NOT_FOUND && error.status != StatusCode::CONFLICT =>
        {
            Ok(Json(
                json!({"valid":false,"errors":[{"field":"task_trigger_draft","message":error.message}],"now":now(),"next_times":[],"preview_token":null,"blocked_reason":error.code}),
            ))
        }
        Err(error) => Err(error),
    }
}
async fn list(
    State(state): State<AppState>,
    admin: Admin,
    Query(p): Query<Paging>,
) -> ApiResult<Json<Value>> {
    let (l, o) = paging(&p)?;
    Ok(Json(
        state.db(|db| trigger_store::list_jobs(db, &admin.id, l, o))?,
    ))
}
async fn detail(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        state.db(|db| trigger_store::owner_job(db, &admin.id, &id))?,
    ))
}
async fn create(
    State(state): State<AppState>,
    admin: Admin,
    headers: HeaderMap,
    bytes: Bytes,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let v = body(&bytes, &["task_trigger_draft"])?;
    let k = key(&headers)?;
    let result = state.db(|db| {
        let tx = db.transaction()?;
        if let Some(v) = trigger_store::cached(&tx, &admin.id, "create", k, &v)? {
            return Ok(v);
        };
        let result = trigger_store::save(&tx, &admin.id, None, None, &v["task_trigger_draft"])?;
        trigger_store::cache(&tx, &admin.id, "create", k, &v, &result)?;
        tx.commit()?;
        Ok(result)
    })?;
    Ok((StatusCode::CREATED, Json(result)))
}
async fn save(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: Bytes,
) -> ApiResult<Json<Value>> {
    let v = body(&bytes, &["task_trigger_draft", "expected_revision"])?;
    let expected = expected(&v)?;
    let k = key(&headers)?;
    let op = format!("draft:{id}");
    let result = state.db(|db| {
        let tx = db.transaction()?;
        if let Some(v) = trigger_store::cached(&tx, &admin.id, &op, k, &v)? {
            return Ok(v);
        };
        let result = trigger_store::save(
            &tx,
            &admin.id,
            Some(&id),
            Some(expected),
            &v["task_trigger_draft"],
        )?;
        trigger_store::cache(&tx, &admin.id, &op, k, &v, &result)?;
        tx.commit()?;
        Ok(result)
    })?;
    Ok(Json(result))
}
async fn activate(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: Bytes,
) -> ApiResult<Json<Value>> {
    require_clock(&state)?;
    let v = body(
        &bytes,
        &["expected_revision", "draft_revision", "preview_token"],
    )?;
    let expected = expected(&v)?;
    let draft = v["draft_revision"]
        .as_i64()
        .ok_or_else(|| ApiError::bad("draft_revision_required", "缺少草稿版本"))?;
    let token = v["preview_token"]
        .as_str()
        .ok_or_else(|| ApiError::bad("preview_required", "请先预演并核对任务"))?;
    let k = key(&headers)?;
    let op = format!("activate:{id}");
    let result = state.db(|db| {
        let tx = db.transaction()?;
        if let Some(v) = trigger_store::cached(&tx, &admin.id, &op, k, &v)? {
            return Ok(v);
        };
        let result = trigger_store::activate(&tx, &admin.id, &id, expected, draft, token)?;
        trigger_store::cache(&tx, &admin.id, &op, k, &v, &result)?;
        tx.commit()?;
        Ok(result)
    })?;
    state.0.notify.notify_waiters();
    Ok(Json(result))
}
macro_rules! operation {
    ($name:ident,$action:expr) => {
        async fn $name(
            State(state): State<AppState>,
            admin: Admin,
            Path(id): Path<String>,
            headers: HeaderMap,
            bytes: Bytes,
        ) -> ApiResult<Json<Value>> {
            if $action == "resume" {
                require_clock(&state)?
            };
            let v = body(&bytes, &["expected_revision"])?;
            let expected = expected(&v)?;
            let k = key(&headers)?;
            let op = format!("{}:{}", $action, id);
            let result = state.db(|db| {
                let tx = db.transaction()?;
                if let Some(v) = trigger_store::cached(&tx, &admin.id, &op, k, &v)? {
                    return Ok(v);
                };
                let result = trigger_store::lifecycle(&tx, &admin.id, &id, expected, $action)?;
                trigger_store::cache(&tx, &admin.id, &op, k, &v, &result)?;
                tx.commit()?;
                Ok(result)
            })?;
            state.0.notify.notify_waiters();
            Ok(Json(result))
        }
    };
}
operation!(pause, "pause");
operation!(stop, "stop");
operation!(resume, "resume");
operation!(archive, "archive");
async fn test_run(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: Bytes,
) -> ApiResult<(StatusCode, Json<Value>)> {
    require_clock(&state)?;
    let v = body(&bytes, &["expected_revision"])?;
    let expected = expected(&v)?;
    let k = key(&headers)?
        .ok_or_else(|| ApiError::bad("idempotency_required", "手动执行需要幂等编号"))?;
    let op = format!("test:{id}");
    let result = state.db(|db| {
        let tx = db.transaction()?;
        if let Some(v) = trigger_store::cached(&tx, &admin.id, &op, Some(k), &v)? {
            return Ok(v);
        };
        let result = trigger_store::test_run(&tx, &admin.id, &id, expected, k)?;
        trigger_store::cache(&tx, &admin.id, &op, Some(k), &v, &result)?;
        tx.commit()?;
        Ok(result)
    })?;
    state.0.notify.notify_waiters();
    Ok((StatusCode::ACCEPTED, Json(result)))
}
async fn history(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    Query(p): Query<Paging>,
) -> ApiResult<Json<Value>> {
    let (l, o) = paging(&p)?;
    Ok(Json(state.db(|db| {
        trigger_store::occurrences(db, &admin.id, &id, l, o)
    })?))
}
async fn result(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        state.db(|db| trigger_store::occurrence(db, &admin.id, &id))?,
    ))
}
async fn image(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let text=state.db(|db|Ok(db.query_row("SELECT response FROM trigger_occurrences WHERE id=?1 AND owner_id=?2 AND status='completed' AND delivery_status='stored' AND response IS NOT NULL AND length(response)<=6291456",params![id,admin.id],|r|r.get::<_,String>(0)).optional()?))?.ok_or_else(ApiError::not_found)?;
    let encoded = text
        .strip_prefix("![绘画](data:image/png;base64,")
        .and_then(|s| s.strip_suffix(')'))
        .ok_or_else(ApiError::not_found)?;
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| ApiError::not_found())?;
    if bytes.len() > 4 * 1024 * 1024 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(ApiError::not_found());
    };
    Ok((
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "private, no-store"),
        ],
        bytes,
    )
        .into_response())
}
async fn retry_delivery(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    bytes: Bytes,
) -> ApiResult<Json<Value>> {
    let _ = body(&bytes, &[])?;
    let result = state.db(|db| {
        let tx = db.transaction()?;
        let original = trigger_store::occurrence(&tx, &admin.id, &id)?;
        if original["status"] != "completed"
            || original["response"].is_null()
            || original["result_expired"] == true
        {
            return Err(trigger_store::fail(
                "result_unavailable",
                "没有可重新保存的完成结果",
            ));
        };
        tx.execute(
            "UPDATE trigger_occurrences SET delivery_status='stored' WHERE id=?1 AND owner_id=?2",
            params![id, admin.id],
        )?;
        let result = trigger_store::occurrence(&tx, &admin.id, &id)?;
        tx.commit()?;
        Ok(result)
    })?;
    Ok(Json(result))
}
async fn calendars(State(state): State<AppState>, _admin: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(
        json!({"items":state.db(|db|trigger_calendar::list(db))?}),
    ))
}
async fn calendar_draft(
    State(state): State<AppState>,
    admin: Admin,
    bytes: Bytes,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let v = body(&bytes, &["document"])?;
    let document: trigger_calendar::CalendarDocument =
        serde_json::from_value(v["document"].clone())
            .map_err(|_| ApiError::bad("invalid_calendar", "日历结构无效"))?;
    trigger_calendar::validate(&document).map_err(|s| ApiError::bad("invalid_calendar", s))?;
    let result=state.db(|db|{let tx=db.transaction()?;let value=trigger_calendar::create_draft(&tx,document)?;tx.execute("INSERT INTO audit_events(actor,action,resource,created_at) VALUES(?1,'trigger.calendar.draft',?2,?3)",params![admin.id,value.id,now()])?;tx.commit()?;Ok(value)})?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(result).map_err(|_| ApiError::internal())?),
    ))
}
async fn calendar_apply(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    bytes: Bytes,
) -> ApiResult<Json<Value>> {
    let v = body(&bytes, &["expected_digest"])?;
    let digest = v["expected_digest"]
        .as_str()
        .ok_or_else(|| ApiError::bad("expected_digest_required", "请核对日历内容摘要"))?;
    let result=state.db(|db|{let tx=db.transaction()?;let current=trigger_calendar::get(&tx,&id)?.ok_or_else(trigger_store::missing)?;if current.sha256!=digest{return Err(trigger_store::conflict())};let value=trigger_calendar::apply(&tx,&id,digest,&admin.id)?;tx.execute("UPDATE trigger_jobs SET next_fire_at=NULL,block_reason=NULL WHERE lifecycle='enabled' AND id IN(SELECT job_id FROM trigger_job_revisions r WHERE r.revision=trigger_jobs.active_revision AND json_extract(r.definition_json,'$.trigger.day_class') IN ('workday','restday'))",[])?;tx.execute("DELETE FROM trigger_previews",[])?;tx.commit()?;Ok(value)})?;
    state.0.notify.notify_waiters();
    Ok(Json(
        serde_json::to_value(result).map_err(|_| ApiError::internal())?,
    ))
}
