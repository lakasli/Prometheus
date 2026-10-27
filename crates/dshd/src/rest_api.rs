//! Instance management of rest. No model result, worker lease, client clock or
//! claimed source identity can be submitted through these routes.
use crate::{
    auth::{self, Admin},
    conversation_commitments, conversation_state,
    error::{ApiError, ApiResult},
    rest_store::{self as rest, GrantPermissions, RestPolicy, VerifiedRequester},
    state::{now, AppState},
};
use axum::{
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

pub fn initialize(db: &Connection) -> anyhow::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS rest_management_requests(
      assistant_id TEXT NOT NULL,actor_id TEXT NOT NULL,operation TEXT NOT NULL,request_key TEXT NOT NULL,
      request_digest TEXT NOT NULL,result_json TEXT NOT NULL,created_at INTEGER NOT NULL,
      PRIMARY KEY(assistant_id,actor_id,operation,request_key));")?;
    Ok(())
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/v1/rest", get(status))
        .route("/api/admin/v1/rest/policy", get(policy).put(save_policy))
        .route("/api/admin/v1/rest/sleep", post(sleep))
        .route("/api/admin/v1/rest/wake", post(wake))
        .route("/api/admin/v1/rest/cycles", get(history))
        .route("/api/admin/v1/rest/cycles/current", get(current_cycle))
        .route("/api/admin/v1/rest/cycles/{id}", get(cycle))
        .route(
            "/api/admin/v1/rest/control-grants/{source_run}",
            get(grant).put(save_grant),
        )
        .layer(DefaultBodyLimit::max(16 * 1024))
}

fn access(state: &AppState, headers: &HeaderMap, admin: &Admin, write: bool) -> ApiResult<()> {
    if admin.role != "admin" {
        return Err(ApiError::forbidden("需要实例管理权限"));
    }
    let expected = url::Url::parse(&state.0.options.origin).map_err(|_| ApiError::internal())?;
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ApiError::forbidden("缺少Host"))?;
    let actual = url::Url::parse(&format!("{}://{host}", expected.scheme()))
        .map_err(|_| ApiError::forbidden("Host无效"))?;
    if actual.host_str() != expected.host_str()
        || actual.port_or_known_default() != expected.port_or_known_default()
    {
        return Err(ApiError::forbidden("Host不在管理来源内"));
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        if origin.to_str().ok() != Some(state.0.options.origin.as_str()) {
            return Err(ApiError::forbidden("Origin不在管理来源内"));
        }
    } else if write {
        return Err(ApiError::forbidden("写操作必须携带Origin"));
    }
    if write
        && !auth::secure_eq(
            headers
                .get("x-csrf-token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or(""),
            &admin.csrf_token,
        )
    {
        return Err(ApiError::forbidden("CSRF校验失败"));
    }
    Ok(())
}

fn expected(headers: &HeaderMap) -> ApiResult<i64> {
    let mut all = headers.get_all(header::IF_MATCH).iter();
    let raw = all
        .next()
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::PRECONDITION_REQUIRED,
                "if_match_required",
                "需要精确If-Match版本",
            )
        })?
        .to_str()
        .unwrap_or("");
    let invalid = || {
        ApiError::bad(
            "invalid_if_match",
            "If-Match须为单个带引号的当前非负整数版本",
        )
    };
    if all.next().is_some() {
        return Err(invalid());
    }
    let parsed = raw
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v >= 0)
        .ok_or_else(invalid)?;
    if raw != format!("\"{parsed}\"") {
        return Err(invalid());
    }
    Ok(parsed)
}

fn key(headers: &HeaderMap) -> ApiResult<&str> {
    headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .filter(|v| {
            !v.is_empty()
                && v.len() <= 128
                && v.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_:-./".contains(c))
        })
        .ok_or_else(|| {
            ApiError::bad(
                "idempotency_key_required",
                "写操作需要有效的Idempotency-Key",
            )
        })
}

fn safe_error(error: &anyhow::Error) -> ApiError {
    for cause in error.chain() {
        let text = cause.to_string();
        let (status, code, message) = match text.as_str() {
            "rest_policy_revision_conflict"
            | "rest_grant_revision_conflict"
            | "rest_generation_conflict"
            | "rest_generation_or_phase_conflict"
            | "rest_idempotency_conflict" => (
                StatusCode::CONFLICT,
                "rest_revision_or_request_conflict",
                "状态、版本或幂等请求已变化，请重新读取后核对",
            ),
            "rest_clock_not_trusted_for_sleep" => (
                StatusCode::CONFLICT,
                "rest_clock_untrusted",
                "主机时钟尚未获确认，未受理新的睡眠；唤醒仍可使用",
            ),
            "rest_admin_required"
            | "rest_source_grant_denied"
            | "rest_source_grant_required"
            | "rest_natural_control_disabled" => (
                StatusCode::FORBIDDEN,
                "rest_control_denied",
                "当前身份或来源未获得这项控制许可",
            ),
            "rest_source_not_available" => (
                StatusCode::NOT_FOUND,
                "rest_source_not_available",
                "该来源不存在、不可管理或已失效",
            ),
            "rest_cycle_not_found" => (
                StatusCode::NOT_FOUND,
                "rest_cycle_not_found",
                "整理周期不存在",
            ),
            "rest_wake_time_required"
            | "rest_wake_outside_maximum"
            | "rest_wake_local_time_ambiguous_or_missing"
            | "invalid_rest_duration"
            | "invalid_rest_bounds"
            | "invalid_rest_timezone"
            | "invalid_rest_local_time"
            | "rest_schedule_time_required"
            | "rest_control_ack_window_required"
            | "invalid_rest_grant_expiry" => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_rest_request",
                "策略、授权期限或唤醒时刻无效；未改变当前状态",
            ),
            _ => continue,
        };
        return ApiError::new(status, code, message);
    }
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "rest_unavailable",
        "睡眠状态暂时无法核验或保存，请检查服务后重新读取",
    )
}

fn in_tx<T>(
    state: &AppState,
    operation: impl FnOnce(&Connection) -> anyhow::Result<T>,
) -> ApiResult<T> {
    state.db(|db| {
        let tx = db.transaction()?;
        match operation(&tx) {
            Ok(value) => {
                tx.commit()?;
                Ok(Ok(value))
            }
            Err(error) => Ok(Err(safe_error(&error))),
        }
    })?
}

fn response(value: Value, revision: Option<i64>) -> Response {
    let mut result = Json(value).into_response();
    result
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    if let Some(revision) = revision {
        result
            .headers_mut()
            .insert(header::ETAG, format!("\"{revision}\"").parse().unwrap());
    }
    result
}

fn audit(db: &Connection, admin: &Admin, action: &str, resource: &str) -> anyhow::Result<()> {
    db.execute(
        "INSERT INTO audit_events(actor,action,resource,created_at)VALUES(?1,?2,?3,?4)",
        params![admin.id, action, resource, now()],
    )?;
    Ok(())
}

fn cached(
    db: &Connection,
    admin: &Admin,
    operation: &str,
    key: &str,
    request: &Value,
) -> anyhow::Result<Option<Value>> {
    let row:Option<(String,String)>=db.query_row("SELECT request_digest,result_json FROM rest_management_requests WHERE assistant_id=(SELECT value FROM settings WHERE key='assistant.identity.v1') AND actor_id=?1 AND operation=?2 AND request_key=?3",params![admin.id,operation,key],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((digest, text)) = row {
        anyhow::ensure!(
            digest == crate::trigger_store::digest(request),
            "rest_idempotency_conflict"
        );
        let mut value: Value = serde_json::from_str(&text)?;
        value["idempotent_replay"] = json!(true);
        return Ok(Some(value));
    }
    Ok(None)
}

fn remember(
    db: &Connection,
    admin: &Admin,
    operation: &str,
    key: &str,
    request: &Value,
    result: &Value,
) -> anyhow::Result<()> {
    db.execute("INSERT INTO rest_management_requests VALUES((SELECT value FROM settings WHERE key='assistant.identity.v1'),?1,?2,?3,?4,?5,?6)",params![admin.id,operation,key,crate::trigger_store::digest(request),result.to_string(),now()])?;
    Ok(())
}

async fn status(
    State(state): State<AppState>,
    admin: Admin,
    headers: HeaderMap,
) -> ApiResult<Response> {
    access(&state, &headers, &admin, false)?;
    let value = in_tx(&state, |db| {
        rest::requester_for_admin_in(db, &admin)?;
        let state = rest::state_in(db)?;
        let (revision, _) = rest::policy_in(db)?;
        let at = state.logical_at;
        let clock = crate::rest_clock::status_in(db);
        Ok(
            json!({"state":state,"policy_revision":revision,"clock":clock,
            "admission":{"receiver":rest::admission_in(db,rest::RestWork::Receiver,at)?,"management":rest::admission_in(db,rest::RestWork::Management,at)?,"wake":rest::admission_in(db,rest::RestWork::WakeControl,at)?,"automatic":rest::admission_in(db,rest::RestWork::Automatic,at)?},
            "physical_requests":rest::physical_work_in(db)?,"rest_admission_is_not_resource_or_contact_permission":true}),
        )
    })?;
    let revision = value["state"]["generation"].as_i64();
    Ok(response(value, revision))
}

async fn policy(
    State(state): State<AppState>,
    admin: Admin,
    headers: HeaderMap,
) -> ApiResult<Response> {
    access(&state, &headers, &admin, false)?;
    let value = in_tx(&state, |db| {
        rest::requester_for_admin_in(db, &admin)?;
        let (revision, policy) = rest::policy_in(db)?;
        Ok(
            json!({"revision":revision,"policy":policy,"independent_of_runtime_config_and_personality":true}),
        )
    })?;
    let revision = value["revision"].as_i64();
    Ok(response(value, revision))
}

async fn save_policy(
    State(state): State<AppState>,
    admin: Admin,
    headers: HeaderMap,
    Json(policy): Json<RestPolicy>,
) -> ApiResult<Response> {
    access(&state, &headers, &admin, true)?;
    let revision = expected(&headers)?;
    let key = key(&headers)?;
    let value = in_tx(&state, |db| {
        let requester = rest::requester_for_admin_in(db, &admin)?;
        let request = json!({"expected_revision":revision,"policy":policy});
        if let Some(value) = cached(db, &admin, "policy", key, &request)? {
            return Ok(value);
        }
        let next = rest::save_policy_in(db, &requester, revision, &policy, now())?;
        let value = json!({"revision":next,"policy":policy,"idempotent_replay":false,"independent_of_runtime_config_and_personality":true});
        audit(db, &admin, "rest.policy.update", &format!("policy:{next}"))?;
        remember(db, &admin, "policy", key, &request, &value)?;
        Ok(value)
    })?;
    state.0.notify.notify_waiters();
    let revision = value["revision"].as_i64();
    Ok(response(value, revision))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WakeBody {}

fn control(
    state: &AppState,
    admin: &Admin,
    revision: i64,
    key: &str,
    sleep: Option<&rest::SleepRequest>,
) -> ApiResult<Value> {
    in_tx(state, |db| {
        let requester = rest::requester_for_admin_in(db, admin)?;
        let replay:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM rest_operations WHERE assistant_id=(SELECT value FROM settings WHERE key='assistant.identity.v1') AND actor_ref=?1 AND idempotency_key=?2)",params![requester.audit_ref(),key],|r|r.get(0))?;
        let clock = crate::rest_clock::sample_in(db)?;
        let operation = if let Some(body) = sleep {
            rest::sleep_in(db, &requester, revision, key, body, &clock)?
        } else {
            rest::wake_in(db, &requester, revision, key, &clock)?
        };
        if sleep.is_none() {
            let current = rest::state_in(db)?;
            if current.generation == operation.generation
                && current.phase == rest::RestPhase::Waking
            {
                rest::advance_in(db, &clock)?;
            }
        }
        if !replay {
            audit(
                db,
                admin,
                if sleep.is_some() {
                    "rest.sleep"
                } else {
                    "rest.wake"
                },
                &operation.id,
            )?;
        }
        Ok(
            json!({"operation":operation,"state":rest::state_in(db)?,"idempotent_replay":replay,"operation_is_original_receipt":true,"physical_leases_released":false}),
        )
    })
}

async fn sleep(
    State(state): State<AppState>,
    admin: Admin,
    headers: HeaderMap,
    Json(body): Json<rest::SleepRequest>,
) -> ApiResult<Response> {
    access(&state, &headers, &admin, true)?;
    let value = control(
        &state,
        &admin,
        expected(&headers)?,
        key(&headers)?,
        Some(&body),
    )?;
    state.0.notify.notify_waiters();
    let revision = value["state"]["generation"].as_i64();
    Ok(response(value, revision))
}
async fn wake(
    State(state): State<AppState>,
    admin: Admin,
    headers: HeaderMap,
    Json(_): Json<WakeBody>,
) -> ApiResult<Response> {
    access(&state, &headers, &admin, true)?;
    let value = control(&state, &admin, expected(&headers)?, key(&headers)?, None)?;
    state.0.notify.notify_waiters();
    let revision = value["state"]["generation"].as_i64();
    Ok(response(value, revision))
}

fn source(db: &Connection, admin: &Admin, run: &str) -> anyhow::Result<VerifiedRequester> {
    let unavailable = || anyhow::anyhow!("rest_source_not_available");
    if run.is_empty() || run.len() > 256 {
        return Err(unavailable());
    }
    let scope = conversation_state::management_scope_for_source_in(db, admin, run)
        .map_err(|_| unavailable())?;
    conversation_commitments::management_access_in(db, &scope, admin).map_err(|_| unavailable())?;
    let reference =
        conversation_state::run_input_source_in(db, &scope).map_err(|_| unavailable())?;
    if !conversation_state::validate_source_in(db, &scope, &reference).map_err(|_| unavailable())? {
        return Err(unavailable());
    }
    let requester = rest::requester_for_run_in(db, run).map_err(|_| unavailable())?;
    if requester.owner_id() != Some(scope.owner_id())
        || requester.conversation_id() != Some(scope.conversation_id())
        || requester.speaker_ref() != scope.speaker_scope()
    {
        return Err(unavailable());
    }
    Ok(requester)
}

fn grant_view(db: &Connection, source: &VerifiedRequester) -> anyhow::Result<Value> {
    let value:Option<(i64,bool,bool,i64)>=db.query_row("SELECT revision,can_sleep,can_wake,valid_until FROM rest_control_grants WHERE assistant_id=(SELECT value FROM settings WHERE key='assistant.identity.v1') AND scope_ref=?1",[source.audit_ref()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let (revision, sleep, wake, until) = value.unwrap_or((0, false, false, 0));
    Ok(
        json!({"source_run_id":source.source_run_id(),"source_ref":source.audit_ref(),"revision":revision,
        "permissions":{"can_sleep":sleep,"can_wake":wake,"valid_until":until},"natural_control_enabled":rest::policy_in(db)?.1.natural_enabled,
        "grant_present":revision>0,"grant_expired":revision>0&&until<=now(),"current_source_and_policy_still_required":true,
        "recipient_or_model_permission":false,"idempotent_replay":false}),
    )
}

async fn grant(
    State(state): State<AppState>,
    admin: Admin,
    Path(run): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    access(&state, &headers, &admin, false)?;
    let value = in_tx(&state, |db| {
        rest::requester_for_admin_in(db, &admin)?;
        let source = source(db, &admin, &run)?;
        grant_view(db, &source)
    })?;
    let revision = value["revision"].as_i64();
    Ok(response(value, revision))
}
async fn save_grant(
    State(state): State<AppState>,
    admin: Admin,
    Path(run): Path<String>,
    headers: HeaderMap,
    Json(permissions): Json<GrantPermissions>,
) -> ApiResult<Response> {
    access(&state, &headers, &admin, true)?;
    let revision = expected(&headers)?;
    let key = key(&headers)?;
    let value = in_tx(&state, |db| {
        let admin_requester = rest::requester_for_admin_in(db, &admin)?;
        let source = source(db, &admin, &run)?;
        let request = json!({"run":run,"expected_revision":revision,"permissions":permissions});
        let operation = format!("grant:{}", source.audit_ref());
        if let Some(value) = cached(db, &admin, &operation, key, &request)? {
            return Ok(value);
        }
        let next = rest::set_source_grant_in(
            db,
            &admin_requester,
            &source,
            revision,
            &permissions,
            now(),
        )?;
        let value = grant_view(db, &source)?;
        audit(
            db,
            &admin,
            "rest.control_grant.update",
            &format!("{}:{next}", source.audit_ref()),
        )?;
        remember(db, &admin, &operation, key, &request, &value)?;
        Ok(value)
    })?;
    state.0.notify.notify_waiters();
    let revision = value["revision"].as_i64();
    Ok(response(value, revision))
}

fn progress(db: &Connection, id: &str) -> anyhow::Result<Value> {
    let exists:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM rest_cycles WHERE id=?1 AND assistant_id=(SELECT value FROM settings WHERE key='assistant.identity.v1'))",[id],|r|r.get(0))?;
    anyhow::ensure!(exists, "rest_cycle_not_found");
    let progress = rest::progress_in(db, id)?;
    let steps=progress.steps.iter().map(|step|json!({"kind":step.kind,"status":step.status,
        "checkpoint":{"cursor_digest":step.checkpoint.cursor_ref.as_ref().map(|s|crate::character::digest(s.as_bytes())),"processed_refs":step.checkpoint.processed_refs,"result_digest":step.checkpoint.result_digest},
        "result_digest":step.result_digest,"error_code":step.error_code})).collect::<Vec<_>>();
    Ok(
        json!({"cycle_id":progress.cycle_id,"generation":progress.generation,"wake_at":progress.wake_at,"partial":progress.partial,"error_code":progress.error_code,"steps":steps,"jobs":progress.jobs,"metadata_only":true}),
    )
}
async fn current_cycle(
    State(state): State<AppState>,
    admin: Admin,
    headers: HeaderMap,
) -> ApiResult<Response> {
    access(&state, &headers, &admin, false)?;
    let value = in_tx(&state, |db| {
        rest::requester_for_admin_in(db, &admin)?;
        let state = rest::state_in(db)?;
        Ok(json!({"current":state.cycle_id.as_deref().map(|id|progress(db,id)).transpose()?}))
    })?;
    Ok(response(value, None))
}
async fn cycle(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    access(&state, &headers, &admin, false)?;
    let value = in_tx(&state, |db| {
        rest::requester_for_admin_in(db, &admin)?;
        progress(db, &id)
    })?;
    Ok(response(value, None))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct History {
    after: Option<i64>,
    limit: Option<usize>,
}
async fn history(
    State(state): State<AppState>,
    admin: Admin,
    headers: HeaderMap,
    Query(page): Query<History>,
) -> ApiResult<Response> {
    access(&state, &headers, &admin, false)?;
    let limit = page.limit.unwrap_or(20);
    if !(1..=50).contains(&limit) || page.after.is_some_and(|v| v <= 0) {
        return Err(ApiError::bad(
            "invalid_rest_page",
            "分页大小须为1到50，游标须为正整数",
        ));
    }
    let value = in_tx(&state, |db| {
        rest::requester_for_admin_in(db, &admin)?;
        let mut query=db.prepare("SELECT rowid,id,generation,trigger_kind,policy_revision,wake_at,created_at,finished_at,partial,calls,reserved_tokens FROM rest_cycles WHERE assistant_id=(SELECT value FROM settings WHERE key='assistant.identity.v1') AND (?1 IS NULL OR rowid<?1) ORDER BY rowid DESC LIMIT ?2")?;
        let rows=query.query_map(params![page.after,(limit+1) as i64],|r|Ok((r.get::<_,i64>(0)?,json!({"cycle_id":r.get::<_,String>(1)?,"generation":r.get::<_,i64>(2)?,"trigger_kind":r.get::<_,String>(3)?,"policy_revision":r.get::<_,i64>(4)?,"wake_at":r.get::<_,i64>(5)?,"created_at":r.get::<_,i64>(6)?,"finished_at":r.get::<_,Option<i64>>(7)?,"partial":r.get::<_,bool>(8)?,"model_calls":r.get::<_,i64>(9)?,"reserved_tokens":r.get::<_,i64>(10)?}))))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let more = rows.len() > limit;
        let cursor = rows
            .get(limit.saturating_sub(1))
            .or_else(|| rows.last())
            .map(|r| r.0);
        let items = rows
            .into_iter()
            .take(limit)
            .map(|r| r.1)
            .collect::<Vec<_>>();
        Ok(
            json!({"items":items,"next_cursor":if more{cursor}else{None},"limit":limit,"metadata_only":true}),
        )
    })?;
    Ok(response(value, None))
}

#[cfg(test)]
#[path = "rest_api_tests.rs"]
mod tests;
