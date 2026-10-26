//! Management of estimated context budgets. These routes neither modify the
//! runtime model configuration nor mint historical prompt/authorization proof.
use crate::{
    auth::{self, Admin},
    character,
    context_budget::{self as budget, BudgetPolicy},
    conversation_state,
    error::{ApiError, ApiResult},
    state::{now, AppState},
};
use axum::{
    extract::{DefaultBodyLimit, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/v1/context-policy", get(current).put(update))
        .route("/api/admin/v1/runs/{id}/context-selection", get(selection))
        .layer(DefaultBodyLimit::max(16 * 1024))
}

// Enforce the same checks with the standalone router used by tests as with the
// enclosing application's auth::guard. A hidden button is not authorization.
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

fn expected_revision(headers: &HeaderMap) -> ApiResult<i64> {
    let mut values = headers.get_all(header::IF_MATCH).iter();
    let value = values.next().ok_or_else(|| {
        ApiError::new(
            StatusCode::PRECONDITION_REQUIRED,
            "if_match_required",
            "须提交精确If-Match版本",
        )
    })?;
    let invalid = || {
        ApiError::bad(
            "invalid_if_match",
            "If-Match须为单个带引号的当前非负整数版本",
        )
    };
    if values.next().is_some() {
        return Err(invalid());
    }
    let raw = value.to_str().map_err(|_| invalid())?;
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

fn response(value: Value, etag: Option<String>) -> Response {
    let mut response = Json(value).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    if let Some(etag) = etag {
        response
            .headers_mut()
            .insert(header::ETAG, etag.parse().expect("host-created entity tag"));
    }
    response
}

fn policy_view(policy: BudgetPolicy, provider: String, config_revision: i64) -> Value {
    let matches = policy
        .provider_fingerprint
        .as_deref()
        .is_none_or(|fp| fp == provider);
    let revision = policy.revision;
    let window_status = if policy.provider_context_window.is_none() {
        "unknown"
    } else if matches {
        "administrator_declared"
    } else {
        "provider_binding_changed"
    };
    json!({"policy":policy,"etag":format!("\"{revision}\""),"config_revision":config_revision,
        "current_provider_fingerprint":provider,"provider_binding_matches":matches,
        "window_status":window_status,"token_counts_exact":false,
        "saved":revision>0,"applied":matches,"effective_from":"next_run",
        "host_limits":{"max_input_bytes":budget::MAX_INPUT_BYTES,"max_response_tokens":budget::MAX_RESPONSE_TOKENS}})
}

async fn current(
    State(state): State<AppState>,
    admin: Admin,
    headers: HeaderMap,
) -> ApiResult<Response> {
    access(&state, &headers, &admin, false)?;
    let value = state.db(|db| {
        let tx = db.transaction()?;
        let (revision, config) = character::current_config_in(&tx)?;
        let policy = budget::policy_in(&tx, &config.model.model)?;
        let mut value = policy_view(
            policy,
            budget::provider_fingerprint(&config.model.base_url),
            revision,
        );
        value["current_model_id"] = json!(config.model.model);
        tx.commit()?;
        Ok(value)
    })?;
    let etag = value["etag"].as_str().map(str::to_owned);
    Ok(response(value, etag))
}

async fn update(
    State(state): State<AppState>,
    admin: Admin,
    headers: HeaderMap,
    Json(proposed): Json<BudgetPolicy>,
) -> ApiResult<Response> {
    access(&state, &headers, &admin, true)?;
    let expected = expected_revision(&headers)?;
    budget::validate_policy(&proposed).map_err(|error| {
        ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_context_policy",
            error.to_string(),
        )
    })?;
    let value=state.db(|db|{
        let tx=db.transaction()?;
        let (config_revision,config)=character::current_config_in(&tx)?;
        if proposed.model_id!=config.model.model {
            return Ok(Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY,"context_policy_model_mismatch","只能修改当前已配置模型的上下文策略")));
        }
        let provider=budget::provider_fingerprint(&config.model.base_url);
        let binding_valid=match proposed.provider_context_window {
            Some(_)=>proposed.window_basis=="administrator_declared" && proposed.provider_fingerprint.as_deref()==Some(provider.as_str()),
            None=>proposed.window_basis=="unknown" && proposed.provider_fingerprint.is_none(),
        };
        if !binding_valid {
            return Ok(Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY,"context_policy_provider_mismatch","窗口声明须绑定当前服务指纹；未知窗口须保持unknown且不绑定虚构能力")));
        }
        let current=budget::policy_in(&tx,&config.model.model)?;
        if current.revision!=expected || proposed.revision!=expected {
            return Ok(Err(ApiError::new(StatusCode::CONFLICT,"context_policy_conflict","上下文策略已更新，请重新读取当前版本")));
        }
        let saved=budget::save_policy_in(&tx,&proposed,expected,crate::reply_delivery::now_ms())?;
        // The audit insertion and policy revision are one atomic operation.
        tx.execute("INSERT INTO audit_events(actor,action,resource,created_at) VALUES(?1,'context_policy.update',?2,?3)",params![admin.id,format!("{}:{}",character::digest(saved.model_id.as_bytes()),saved.revision),now()])?;
        let mut value=policy_view(saved,provider,config_revision);
        value["current_model_id"]=json!(config.model.model);
        tx.commit()?;
        Ok(Ok(value))
    })??;
    let etag = value["etag"].as_str().map(str::to_owned);
    state.0.notify.notify_waiters();
    Ok(response(value, etag))
}

async fn selection(
    State(state): State<AppState>,
    admin: Admin,
    Path(run): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    access(&state, &headers, &admin, false)?;
    let value=state.db(|db|{
        let tx=db.transaction()?;
        // Match the established runtime-log audit boundary, including the
        // exclusion of an administrator ID disguised as a channel owner.
        let allowed:Option<(String,String)>=tx.query_row(
            "SELECT r.owner_id,r.conversation_id FROM runs r JOIN conversations c ON c.id=r.conversation_id AND c.owner_id=r.owner_id WHERE r.id=?1 AND (r.owner_id=?2 OR ((r.owner_id LIKE 'qq:%' OR r.owner_id LIKE 'weixin:%') AND NOT EXISTS(SELECT 1 FROM admins a WHERE a.id=r.owner_id)))",
            params![run,admin.id],|r|Ok((r.get(0)?,r.get(1)?)),
        ).optional()?;
        let Some((owner,conversation))=allowed else{return Ok(Err(ApiError::not_found()))};
        let Some(snapshot)=budget::snapshot_in(&tx,&run)? else {
            // Absence remains unknown. Reading old Runs must not backfill a
            // budget snapshot, binding or evidence of an old provider request.
            tx.commit()?;
            return Ok(Ok(json!({"run_id":run,"available":false,"reason":"context_budget_not_recorded","selection":null,"token_counts_exact":false})));
        };
        let scope=match conversation_state::management_scope_for_source_in(&tx,&admin,&run) {
            Ok(scope) if scope.owner_id()==owner && scope.conversation_id()==conversation=>scope,
            _=>return Ok(Err(ApiError::not_found())),
        };
        let _=scope;
        tx.commit()?;
        Ok(Ok(json!({"run_id":run,"available":true,"selection":snapshot,"scope":"own_web_and_instance_channels","token_counts_exact":false})))
    })??;
    Ok(response(value, None))
}

#[cfg(test)]
#[path = "context_budget_api_tests.rs"]
mod tests;
