use crate::{
    auth::{self, Admin},
    character,
    config::RuntimeConfig,
    error::{ApiError, ApiResult},
    state::{now, private_write, AppState},
    worker,
};
use axum::{
    extract::{DefaultBodyLimit, Path, State},
    http::{header, HeaderMap, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post},
    Json, Router,
};
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::services::{ServeDir, ServeFile};

pub fn router(state: AppState) -> Router {
    let web = state.0.options.web_dir.clone();
    Router::new()
        .merge(crate::character_api::router())
        .merge(crate::runtime_logs::router())
        .merge(crate::conversation_images_api::router())
        .merge(crate::skills_api::router())
        .merge(crate::trigger_api::router())
        .merge(crate::diary_api::router())
        .merge(crate::relationships_api::router())
        .merge(crate::schedule_api::router())
        .merge(crate::media_assets::router())
        .merge(crate::speech_api::router())
        .merge(crate::qq_api::router())
        .merge(crate::reply_delivery_api::router())
        .merge(crate::plugin_factory_api::router())
        .route("/health/live",get(||async{Json(json!({"status":"ok","version":env!("CARGO_PKG_VERSION"),"commit":crate::build_sha()}))}))
        .route("/health/ready",get(ready))
        .route("/api/admin/v1/auth/status",get(auth::auth_status))
        .route("/api/admin/v1/auth/bootstrap",post(auth::bootstrap))
        .route("/api/admin/v1/auth/login",post(auth::login))
        .route("/api/admin/v1/auth/me",get(auth::me))
        .route("/api/admin/v1/auth/logout",post(auth::logout))
        .route("/api/admin/v1/status",get(status))
        .route("/api/admin/v1/capabilities",get(capabilities))
        .route("/api/admin/v1/config",get(get_config))
        .route("/api/admin/v1/config/drafts",post(create_draft))
        .route("/api/admin/v1/config/drafts/{id}",patch(update_draft))
        .route("/api/admin/v1/config/drafts/{id}/validate",post(validate_draft))
        .route("/api/admin/v1/config/drafts/{id}/checks",post(check_draft))
        .route("/api/admin/v1/config/drafts/{id}/apply",post(apply_draft))
        .route("/api/admin/v1/config/revisions",get(revisions))
        .route("/api/admin/v1/config/revisions/{id}/rollback",post(rollback))
        .route("/api/admin/v1/operations/{id}",get(operation))
        .route("/api/admin/v1/secrets",get(list_secrets).post(create_secret))
        .route("/api/admin/v1/secrets/{id}",delete(delete_secret))
        .route("/api/admin/v1/conversations",get(conversations).post(create_conversation))
        .route("/api/admin/v1/conversations/{id}/messages",get(messages).post(send_message))
        .route("/api/admin/v1/runs/{id}",get(run).delete(cancel_run))
        .route("/api/admin/v1/runs",get(runs))
        .merge(crate::memory_api::router())
        .merge(crate::summary_api::router())
        .merge(crate::memory_candidate_api::router())
        .merge(crate::image_analysis_api::router())
        .merge(crate::wasm_workbench_api::router())
        .merge(crate::weixin_api::router())
        .route("/webhooks/qq",post(crate::qq::webhook))
        .route("/",get(||async{axum::response::Redirect::temporary("/admin/")}))
        .route("/admin",get(||async{axum::response::Redirect::permanent("/admin/")}))
        .nest_service("/admin/",ServeDir::new(&web).not_found_service(ServeFile::new(web.join("index.html"))))
        .fallback(||async{ApiError::not_found()})
        .layer(DefaultBodyLimit::max(128*1024))
        .layer(middleware::from_fn_with_state(state.clone(),auth::guard))
        .with_state(state)
}

async fn ready(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    state.db(|db| Ok(db.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?))?;
    Ok(Json(json!({"status":"ready","commit":crate::build_sha()})))
}

async fn status(State(state): State<AppState>, _admin: Admin) -> ApiResult<Json<Value>> {
    let (revision, config) = state.current_config()?;
    let queue=state.db(|db| {
        let count=|sql:&str|->rusqlite::Result<i64>{db.query_row(sql,[],|r|r.get(0))};
        Ok(json!({"queued":count("SELECT count(*) FROM runs WHERE status='queued'")?+count("SELECT count(*) FROM memory_candidate_jobs WHERE status='queued'")?+count("SELECT count(*) FROM image_analysis_jobs WHERE status='queued'")?+count("SELECT count(*) FROM wasm_workbench_jobs WHERE status='queued'")?+count("SELECT count(*) FROM plugin_factory_jobs WHERE status='queued'")?,"running":worker::running_count(db)?+crate::memory_candidates::running_count(db)?+crate::image_analysis::running_count(db)?+crate::wasm_workbench::running_count(db)?+crate::plugin_factory::running_count(db)?,"failed":count("SELECT count(*) FROM runs WHERE status IN ('failed','interrupted')")?,"outbox_pending":count("SELECT count(*) FROM outbox WHERE status='queued'")?+count("SELECT count(*) FROM weixin_outbox WHERE status='queued'")?,"outbox_unknown":count("SELECT count(*) FROM outbox WHERE status='unknown'")?+count("SELECT count(*) FROM weixin_outbox WHERE status='unknown'")?}))
    })?;
    let last_event = state.db(|db| {
        Ok(db.query_row("SELECT max(received_at) FROM inbox", [], |r| {
            r.get::<_, Option<i64>>(0)
        })?)
    })?;
    let last_error = state.db(|db| {
        Ok(db
            .query_row(
                "SELECT error FROM outbox WHERE error IS NOT NULL ORDER BY created_at DESC LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
    })?;
    let last_error = state
        .0
        .qq_last_error
        .lock()
        .ok()
        .and_then(|e| e.clone())
        .or(last_error);
    let memory = memory();
    let connection_status = state
        .0
        .qq_connection
        .lock()
        .map(|s| s.clone())
        .unwrap_or_else(|_| "unknown".into());
    Ok(Json(
        json!({"version":env!("CARGO_PKG_VERSION"),"commit":crate::build_sha(),"uptime_seconds":state.0.started.elapsed().as_secs(),"platform":{"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"memory_total_bytes":memory.0,"memory_available_bytes":memory.1},"queue":queue,"config":{"desired_revision":revision,"applied_revision":revision},"channels":{"qq":{"enabled":config.qq.enabled,"configured":config.qq.credential_ref.is_some(),"transport":config.qq.transport,"connection_status":connection_status,"webhook_path":"/webhooks/qq","last_event_at":last_event,"last_error":last_error}},"model":{"configured":config.model.credential_ref.is_some(),"model":config.model.model}}),
    ))
}

fn memory() -> (Option<u64>, Option<u64>) {
    let data = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let get = |name: &str| {
        data.lines().find_map(|line| {
            line.strip_prefix(name)
                .and_then(|v| v.split_whitespace().next())
                .and_then(|v| v.parse::<u64>().ok())
                .map(|v| v * 1024)
        })
    };
    (get("MemTotal:"), get("MemAvailable:"))
}
async fn capabilities(_admin: Admin) -> Json<Value> {
    let mut result = Json(
        json!({"api_version":"v1","milestone":"M2-memory","backends":["native"],"channels":["web","cli","qq-c2c","qq-group","qq-channel","weixin-ilink-c2c"],"weixin_ilink":{"text_private":true,"linked_owner_only":true,"groups":false,"media":false,"max_accounts":1},"text":true,"character_cards":true,"character_card_profile":1,"qq_persona_switch":"private_only","memory":true,"assistant_memory":{"identity":"stable-instance","visibility":["shared","private"],"explicit_commands":true,"full_text_search":true,"automatic_extraction":false,"candidate_extraction":{"mode":"explicit_sources","review_required":true,"visibility":"private","max_sources":12,"max_candidates":3},"summaries":true,"summary_policy":{"keep_recent_turns":4,"max_batch_turns":8,"timeout_seconds":20,"max_concurrent":1},"source_tracing":true,"context_invalidation":true},"harness_runtime":false,"image_analysis":{"standalone":true,"formats":["png"],"max_images":1,"max_image_bytes":1048576,"conversation_integration":false},"vision":false,"asr":false,"tts":false,"pure_wasm_workbench":{"abi":"calc(i32,i32)->i32","manual_approval":true,"manual_execution":true,"host_imports":false,"chat_tool_registration":false},"generated_plugins":false,"runtime_limits":{"max_concurrent_runs":2},"configuration":{"hot_reload":true,"restart_required_fields":[]}}),
    );
    result.0["vision"] = json!(true);
    result.0["character_cards"] = json!(false);
    result.0["qq_persona_switch"] = json!(false);
    result.0["assistant_personality"] =
        json!({"mode":"single","versioned":true,"effective_from":"next_run"});
    result.0["runtime_logs"] =
        json!({"persistent":true,"authenticated":true,"subagent":false,"asr":false});
    result.0["conversation_coordination"] = json!({"version":1,"identity_snapshot":2,"delivery_history":"confirmed_parts","correction_policy":"explicit_private_plain_text","frozen_delivery_snapshots":true,"focus":false,"commitments":false});
    result.0["diary"] = json!({"schema_version":2,"hierarchical_reductions":true,"periods":["day","week","month"],"source_authorization":"explicit","model_generation":true,"source_tracing":true,"revisioned":true});
    result.0["relationships"] = json!({"reviewed_fields":true,"directed_edges":true,"identity_confirmation":true,"contact_delivery":false});
    result.0["schedule"] = json!({"calendar_views":["month","week","day","agenda"],"text_reading":true,"video":false,"proactive_delivery":false,"legacy_triggers":true});
    result.0["conversation_images"] = json!({"channels":["web"],"formats":["png"],"max_images":1,"max_image_bytes":1048576,"provider":"deepseek","model":"deepseek-flash","basis":"vision_description","role_vision_module":false,"draft_tests":false});
    result.0["image_analysis"]["conversation_integration"] = json!(true);
    result.0["image_analysis"]["placement"] = json!("api_only");
    result.0["pure_wasm_workbench"]["placement"] = json!("api_only");
    result.0["pure_wasm_workbench"]["experimental"] = json!(true);
    result.0["skills"] = json!({"management":true,"explicit_selection":true,"text_command":"/skill <id> <需求>","native_drawing_channels":["web"],"html_fallback":true,"grants_tool_permissions":false});
    result
}

async fn get_config(State(state): State<AppState>, _admin: Admin) -> ApiResult<Response> {
    let (revision, config) = state.current_config()?;
    Ok((
        [(header::ETAG, format!("\"{revision}\""))],
        Json(json!({"revision":revision,"applied_revision":revision,"config":config})),
    )
        .into_response())
}

#[derive(Deserialize)]
pub struct DraftBody {
    base_revision: i64,
    config: RuntimeConfig,
}
async fn create_draft(
    State(state): State<AppState>,
    admin: Admin,
    Json(body): Json<DraftBody>,
) -> ApiResult<Json<Value>> {
    if state.current_config()?.0 != body.base_revision {
        return Err(ApiError::conflict());
    }
    let id = uuid::Uuid::new_v4().to_string();
    state.db(|db|{db.execute("INSERT INTO config_drafts(id,owner_id,base_revision,config_json,created_at) VALUES(?1,?2,?3,?4,?5)",params![id,admin.id,body.base_revision,serde_json::to_string(&body.config)?,now()])?;Ok(())})?;
    Ok(Json(
        json!({"id":id,"base_revision":body.base_revision,"config":body.config}),
    ))
}
fn draft(state: &AppState, id: &str, owner: &str) -> ApiResult<(i64, RuntimeConfig)> {
    let found=state.db(|db|Ok(db.query_row("SELECT base_revision,config_json FROM config_drafts WHERE id=?1 AND owner_id=?2",params![id,owner],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?))).optional()?))?.ok_or_else(ApiError::not_found)?;
    Ok((
        found.0,
        serde_json::from_str(&found.1).map_err(|_| ApiError::internal())?,
    ))
}
async fn update_draft(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<DraftBody>,
) -> ApiResult<Json<Value>> {
    let (base, _) = draft(&state, &id, &admin.id)?;
    if base != expected_revision(&headers)? || base != body.base_revision {
        return Err(ApiError::conflict());
    }
    state.db(|db| {
        db.execute(
            "UPDATE config_drafts SET config_json=?1 WHERE id=?2 AND owner_id=?3",
            params![serde_json::to_string(&body.config)?, id, admin.id],
        )?;
        Ok(())
    })?;
    Ok(Json(
        json!({"id":id,"base_revision":base,"config":body.config}),
    ))
}
async fn validate_draft(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let (_, config) = draft(&state, &id, &admin.id)?;
    let mut errors = config.validate(state.0.options.allow_private_model);
    if let Err(error) = state.check_config_secrets(&config) {
        errors.push(json!({"field":"credential_ref","message":error.message}));
    }
    Ok(Json(json!({"valid":errors.is_empty(),"errors":errors})))
}
async fn check_draft(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let _slot = state.0.checks.try_acquire().map_err(|_| {
        ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "checks_busy",
            "连接测试正在执行，请稍后再试",
        )
    })?;
    let (_, config) = draft(&state, &id, &admin.id)?;
    if !config
        .validate(state.0.options.allow_private_model)
        .is_empty()
    {
        return Err(ApiError::bad("invalid_config", "请先修复配置校验错误"));
    }
    let model = worker::test_model(&state, &config).await;
    let mut checks = vec![
        json!({"name":"model","ok":model.is_ok(),"message":model.err().unwrap_or_else(||"模型实际返回成功".into())}),
    ];
    if config.qq.enabled {
        let result = crate::qq::access_token(&state, &config).await;
        checks.push(json!({"name":"qq","ok":result.is_ok(),"message":result.err().unwrap_or_else(||"QQ访问凭证获取成功；仍需确认实际连接状态和真实消息收发".into())}));
    }
    state.audit(&admin.id, "config.checks", &id)?;
    Ok(Json(
        json!({"ok":checks.iter().all(|v|v["ok"]==true),"checks":checks}),
    ))
}
fn expected_revision(headers: &HeaderMap) -> ApiResult<i64> {
    headers
        .get(header::IF_MATCH)
        .and_then(|h| h.to_str().ok())
        .and_then(|v| v.trim_matches('"').parse().ok())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::PRECONDITION_REQUIRED,
                "if_match_required",
                "必须提交当前配置版本If-Match",
            )
        })
}

fn apply_config(
    state: &AppState,
    admin: &Admin,
    expected: i64,
    config: &RuntimeConfig,
) -> ApiResult<Value> {
    let errors = config.validate(state.0.options.allow_private_model);
    if !errors.is_empty() {
        return Err(ApiError::bad(
            "invalid_config",
            "配置校验失败，请先检查各字段",
        ));
    }
    state.check_config_secrets(config)?;
    let id = uuid::Uuid::new_v4().to_string();
    let outcome=state.db(|db| {
        let tx=db.transaction()?;
        let current:i64=tx.query_row("SELECT CAST(value AS INTEGER) FROM settings WHERE key='active_revision'",[],|r|r.get(0))?;
        if current!=expected{return Ok(Err(ApiError::conflict()));}
        for (reference,kind) in [(config.model.credential_ref.as_deref(),"model_api_key"),(config.qq.credential_ref.as_deref(),"qq_app_secret")] {
            if let Some(reference)=reference {
                let available:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM secrets WHERE id=?1 AND kind=?2)",params![reference,kind],|r|r.get(0))?;
                if !available {return Ok(Err(ApiError::bad("credential_unavailable","凭据已被移除，请重新选择有效凭据")));}
            }
        }
        let next:i64=tx.query_row("SELECT max(revision)+1 FROM config_revisions",[],|r|r.get(0))?;
        let result=json!({"id":id,"status":"completed","desired_revision":next,"applied_revision":next});
        tx.execute("INSERT INTO config_revisions(revision,config_json,created_at) VALUES(?1,?2,?3)",params![next,serde_json::to_string(config)?,now()])?;
        tx.execute("UPDATE settings SET value=?1 WHERE key='active_revision'",[next.to_string()])?;
        tx.execute("INSERT INTO operations(id,owner_id,result_json,created_at) VALUES(?1,?2,?3,?4)",params![id,admin.id,serde_json::to_string(&result)?,now()])?;
        tx.execute("INSERT INTO audit_events(actor,action,resource,created_at) VALUES(?1,'config.apply',?2,?3)",params![admin.id,next.to_string(),now()])?;
        tx.commit()?;Ok(Ok(result))
    })?;
    outcome
}
async fn apply_draft(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let (base, config) = draft(&state, &id, &admin.id)?;
    let expected = expected_revision(&headers)?;
    if base != expected {
        return Err(ApiError::conflict());
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(apply_config(&state, &admin, expected, &config)?),
    )
        .into_response())
}
async fn revisions(State(state): State<AppState>, _admin: Admin) -> ApiResult<Json<Value>> {
    let current = state.current_config()?.0;
    let items=state.db(|db|{let mut stmt=db.prepare("SELECT revision,config_json,created_at FROM config_revisions ORDER BY revision DESC LIMIT 30")?;let rows=stmt.query_map([],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?)))?;let mut result=vec![];for row in rows{let(rev,text,created)=row?;result.push(json!({"revision":rev,"config":serde_json::from_str::<Value>(&text)?,"created_at":created,"current":rev==current}));}Ok(result)})?;
    Ok(Json(json!({"items":items})))
}
async fn rollback(
    State(state): State<AppState>,
    admin: Admin,
    Path(revision): Path<i64>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let config = state
        .db(|db| {
            Ok(db
                .query_row(
                    "SELECT config_json FROM config_revisions WHERE revision=?1",
                    [revision],
                    |r| r.get::<_, String>(0),
                )
                .optional()?)
        })?
        .ok_or_else(ApiError::not_found)?;
    let config = serde_json::from_str(&config).map_err(|_| ApiError::internal())?;
    Ok((
        StatusCode::ACCEPTED,
        Json(apply_config(
            &state,
            &admin,
            expected_revision(&headers)?,
            &config,
        )?),
    )
        .into_response())
}
async fn operation(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let text = state
        .db(|db| {
            Ok(db
                .query_row(
                    "SELECT result_json FROM operations WHERE id=?1 AND owner_id=?2",
                    params![id, admin.id],
                    |r| r.get::<_, String>(0),
                )
                .optional()?)
        })?
        .ok_or_else(ApiError::not_found)?;
    Ok(Json(
        serde_json::from_str(&text).map_err(|_| ApiError::internal())?,
    ))
}

#[derive(Deserialize)]
struct SecretBody {
    kind: String,
    value: String,
}
async fn create_secret(
    State(state): State<AppState>,
    admin: Admin,
    Json(body): Json<SecretBody>,
) -> ApiResult<Json<Value>> {
    if !["model_api_key", "qq_app_secret"].contains(&body.kind.as_str())
        || body.value.is_empty()
        || body.value.len() > 8192
        || body.value.contains(['\r', '\n'])
    {
        return Err(ApiError::bad("invalid_secret", "不支持的凭证类型或值"));
    }
    let id = uuid::Uuid::new_v4().to_string();
    private_write(
        &state.0.options.data_dir.join("secrets").join(&id),
        body.value.as_bytes(),
    )
    .map_err(|_| ApiError::internal())?;
    state.db(|db| {
        db.execute(
            "INSERT INTO secrets(id,kind,created_at) VALUES(?1,?2,?3)",
            params![id, body.kind, now()],
        )?;
        Ok(())
    })?;
    state.audit(&admin.id, "secret.create", &id)?;
    Ok(Json(json!({"id":id,"kind":body.kind,"configured":true})))
}
async fn list_secrets(State(state): State<AppState>, _admin: Admin) -> ApiResult<Json<Value>> {
    let items=state.db(|db|{let mut stmt=db.prepare("SELECT id,kind,created_at FROM secrets ORDER BY created_at DESC LIMIT 100")?;let rows=stmt.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"kind":r.get::<_,String>(1)?,"created_at":r.get::<_,i64>(2)?,"configured":true})))?;Ok(rows.collect::<Result<Vec<_>,_>>()?)})?;
    Ok(Json(json!({"items":items})))
}
async fn delete_secret(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    if uuid::Uuid::parse_str(&id).is_err() {
        return Err(ApiError::not_found());
    }
    state.db(|db| {
        let tx=db.transaction()?;
        let (_,config)=character::current_config_in(&tx)?;
        let pending:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE (status IN ('queued','running') OR EXISTS(SELECT 1 FROM native_worker_leases l WHERE l.run_id=runs.id)) AND instr(config_json,?1)>0) OR EXISTS(SELECT 1 FROM outbox WHERE credential_ref=?1 AND status IN ('queued','sending'))",[&id],|r|r.get(0))?;
        let weixin_in_use=crate::weixin_store::credential_in_use(&tx,&id).map_err(|_|anyhow::anyhow!("weixin credential references unavailable"))?;
        let candidate_in_use=crate::memory_candidates::credential_in_use(&tx,&id)?;
        let image_in_use=crate::image_analysis::credential_in_use(&tx,&id)?;
        let wasm_in_use=crate::wasm_workbench::credential_in_use(&tx,&id)?;
        let factory_in_use=crate::plugin_factory::references_secret(&tx,&id)?;
        let diary_in_use=crate::diary_model::credential_in_use(&tx,&id)?;
        let schedule_in_use=crate::schedule_model::credential_in_use(&tx,&id)?;
        let relationships_in_use=crate::relationships_model::credential_in_use(&tx,&id)?;
        if config.model.credential_ref.as_ref()==Some(&id) || config.qq.credential_ref.as_ref()==Some(&id) || pending || weixin_in_use || candidate_in_use || image_in_use || wasm_in_use || factory_in_use || diary_in_use || schedule_in_use || relationships_in_use {
            return Ok(Err(ApiError::new(StatusCode::CONFLICT,"credential_in_use","凭据仍被配置、任务或渠道发送引用，请先停止引用再删除")));
        }
        if tx.execute("DELETE FROM secrets WHERE id=?1",[&id])?==0 {return Ok(Err(ApiError::not_found()));}
        tx.execute("INSERT INTO audit_events(actor,action,resource,created_at) VALUES(?1,'secret.delete',?2,?3)",params![admin.id,id,now()])?;
        tx.commit()?;
        Ok(Ok(()))
    })??;
    let _ = std::fs::remove_file(state.0.options.data_dir.join("secrets").join(&id));
    Ok(Json(json!({"ok":true})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversationBody {
    title: Option<String>,
    character: Option<character::CardRef>,
}
async fn create_conversation(
    State(state): State<AppState>,
    admin: Admin,
    Json(body): Json<ConversationBody>,
) -> ApiResult<Json<Value>> {
    let id = uuid::Uuid::new_v4().to_string();
    let title = body.title.unwrap_or_else(|| "新对话".into());
    if title.len() > 200 {
        return Err(ApiError::bad("invalid_title", "标题过长"));
    }
    let character = state.db(|db| {
        let tx = db.transaction()?;
        let (_, config) = character::current_config_in(&tx)?;
        tx.execute(
            "INSERT INTO conversations(id,owner_id,title,created_at) VALUES(?1,?2,?3,?4)",
            params![id, admin.id, title, now()],
        )?;
        character::bind_new_conversation(&tx, &id, body.character.as_ref(), &config)?;
        let result = character::connection_character(&tx, &id)?;
        tx.commit()?;
        Ok(result)
    })?;
    Ok(Json(
        json!({"id":id,"title":title,"legacy":character.is_null(),"character":character}),
    ))
}
async fn conversations(State(state): State<AppState>, admin: Admin) -> ApiResult<Json<Value>> {
    let items=state.db(|db|{let mut stmt=db.prepare("SELECT id,title,created_at FROM conversations WHERE owner_id=?1 AND NOT EXISTS(SELECT 1 FROM runs automatic WHERE automatic.conversation_id=conversations.id AND automatic.run_kind IN ('trigger_model','trigger_image')) ORDER BY created_at DESC,rowid DESC LIMIT 100")?;let rows=stmt.query_map([admin.id],|r|Ok(json!({"id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"created_at":r.get::<_,i64>(2)?})))?;let mut items=rows.collect::<Result<Vec<_>,_>>()?;for item in &mut items {let c=character::connection_character(db,item["id"].as_str().unwrap_or(""))?;item["legacy"]=json!(c.is_null());item["test"]=json!(c["is_test"].as_bool().unwrap_or(false));item["character"]=c;}Ok(items)})?;
    Ok(Json(json!({"items":items})))
}
fn own_conversation(state: &AppState, id: &str, owner: &str) -> ApiResult<()> {
    let exists = state.db(|db| {
        Ok(db
            .query_row(
                "SELECT 1 FROM conversations WHERE id=?1 AND owner_id=?2",
                params![id, owner],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .is_some())
    })?;
    if exists {
        Ok(())
    } else {
        Err(ApiError::not_found())
    }
}
async fn messages(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    own_conversation(&state, &id, &admin.id)?;
    let items=state.db(|db| {
        let mut stmt=db.prepare("SELECT id,role,content,created_at,run_id,native_image FROM (SELECT m.rowid,m.id,m.role,CASE WHEN r.run_kind IN ('skill_image','trigger_image') AND m.role='assistant' THEN '[原生生图结果]' ELSE m.content END AS content,m.created_at,m.run_id,(r.run_kind IN ('skill_image','trigger_image') AND m.role='assistant') AS native_image FROM messages m LEFT JOIN runs r ON r.id=m.run_id WHERE m.conversation_id=?1 ORDER BY m.rowid DESC LIMIT 200) ORDER BY rowid")?;
        let rows=stmt.query_map([id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,Option<bool>>(5)?.unwrap_or(false))))?.collect::<Result<Vec<_>,_>>()?;
        let mut items=Vec::new();
        for(message,role,content,created,run,native_image)in rows {
            let mut item=json!({"id":message,"role":role,"content":content,"created_at":created});
            if let Some(run)=run {
                let publication=db.query_row("SELECT decision,revision,reason FROM run_publication_decisions WHERE run_id=?1",[&run],|r|Ok(json!({"state":r.get::<_,String>(0)?,"revision":r.get::<_,i64>(1)?,"reason":r.get::<_,String>(2)?}))).optional()?.unwrap_or(json!({"state":"active","revision":0,"reason":"legacy"}));
                if role=="assistant" && publication["state"]=="suppressed" {
                    item["content"]=json!("[该候选因后续更正未发布；原始生成结果保留于运行审计]");
                    item["publication"]=publication;
                    items.push(item);continue;
                }
                item["publication"]=publication;
                if role=="assistant" {item["reply_plan"]=json!(crate::chat_reply::get_in(db,&run)?);}
                if native_image {item["generated_image_url"]=json!(crate::skills_api::image_url(&run));}
                if role=="user" {if let Some(image)=crate::conversation_images::metadata(db,&run)? {item["image"]=image;}}
                else if role=="assistant" && crate::conversation_images::used_vision(db,&run)? {item["basis"]=json!("vision_description");}
            }
            items.push(item);
        }
        Ok(items)
    })?;
    Ok(Json(json!({"items":items})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageBody {
    content: String,
    client_message_id: String,
    skill_id: Option<String>,
}
async fn send_message(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    Json(body): Json<MessageBody>,
) -> ApiResult<Response> {
    own_conversation(&state, &id, &admin.id)?;
    let automatic=state.db(|db|Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE conversation_id=?1 AND run_kind IN ('trigger_model','trigger_image'))",[&id],|r|r.get::<_,bool>(0))?))?;
    if automatic {
        return Err(ApiError::forbidden("自动任务会话只读，请在任务触发中管理"));
    }
    let run = worker::enqueue_with_skill(
        &state,
        &id,
        &admin.id,
        &body.client_message_id,
        &body.content,
        None,
        body.skill_id.as_deref(),
    )?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"run_id":run,"conversation_id":id})),
    )
        .into_response())
}
async fn run(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let record=state.db(|db|Ok(db.query_row("SELECT id,status,CASE WHEN run_kind IN ('skill_image','trigger_image') AND response IS NOT NULL THEN '[原生生图结果]' ELSE response END,error,conversation_id,created_at,run_kind FROM runs WHERE id=?1 AND owner_id=?2",params![id,admin.id],|r|{let mut item=json!({"id":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,"response":r.get::<_,Option<String>>(2)?,"error":r.get::<_,Option<String>>(3)?,"conversation_id":r.get::<_,String>(4)?,"created_at":r.get::<_,i64>(5)?});if matches!(r.get::<_,String>(6)?.as_str(),"skill_image"|"trigger_image") && item["status"]=="completed" && !item["response"].is_null() {item["generated_image_url"]=json!(crate::skills_api::image_url(&id));}Ok(item)}).optional()?))?.ok_or_else(ApiError::not_found)?;
    let mut record = record;
    record["reply_plan"] = state.db(|db| Ok(json!(crate::chat_reply::get_in(db, &id)?)))?;
    record["deliveries"] = state.db(|db| crate::reply_delivery::status_in(db, &id))?;
    record["character"] = run_character(&state, &id)?;
    record["legacy"] = json!(record["character"].is_null());
    record["publication"] = state.db(|db| {
        Ok(db.query_row("SELECT decision,revision,reason FROM run_publication_decisions WHERE run_id=?1",[&id],|r|Ok(json!({"state":r.get::<_,String>(0)?,"revision":r.get::<_,i64>(1)?,"reason":r.get::<_,String>(2)?}))).optional()?.unwrap_or(json!({"state":"active","revision":0,"reason":"legacy"})))
    })?;
    if record["publication"]["state"] == "suppressed" {
        record["response"] = Value::Null;
        record["reply_plan"] = Value::Null;
        record
            .as_object_mut()
            .expect("run object")
            .remove("generated_image_url");
    }
    if record["status"] == "completed"
        && state.db(|db| crate::conversation_images::used_vision(db, &id))?
    {
        record["basis"] = json!("vision_description");
    }
    Ok(Json(record))
}

fn run_character(state: &AppState, id: &str) -> ApiResult<Value> {
    let text = state.db(|db| {
        Ok(db.query_row(
            "SELECT character_snapshot_json FROM runs WHERE id=?1",
            [id],
            |r| r.get::<_, Option<String>>(0),
        )?)
    })?;
    let Some(text) = text else {
        return Ok(Value::Null);
    };
    let value: Value = serde_json::from_str(&text).map_err(|_| ApiError::internal())?;
    Ok(
        json!({"card_id":value["card_id"],"revision":value["revision"],"name":value["name"],"digest":value["card_digest"],"projection_digest":value["projection_digest"],"is_test":value["is_test"]}),
    )
}
async fn runs(State(state): State<AppState>, admin: Admin) -> ApiResult<Json<Value>> {
    let items=state.db(|db|{let mut stmt=db.prepare("SELECT id,status,CASE WHEN EXISTS(SELECT 1 FROM run_publication_decisions p WHERE p.run_id=runs.id AND p.decision='suppressed') THEN NULL WHEN run_kind IN ('skill_image','trigger_image') AND response IS NOT NULL THEN '[原生生图结果]' ELSE response END,error,conversation_id,created_at FROM runs WHERE owner_id=?1 ORDER BY created_at DESC LIMIT 100")?;let rows=stmt.query_map([admin.id],|r|Ok(json!({"id":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,"response":r.get::<_,Option<String>>(2)?,"error":r.get::<_,Option<String>>(3)?,"conversation_id":r.get::<_,String>(4)?,"created_at":r.get::<_,i64>(5)?})))?;Ok(rows.collect::<Result<Vec<_>,_>>()?)})?;
    Ok(Json(json!({"items":items})))
}
async fn cancel_run(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let automatic=state.db(|db|Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND owner_id=?2 AND run_kind IN ('trigger_model','trigger_image'))",params![id,admin.id],|r|r.get::<_,bool>(0))?))?;
    if automatic {
        return Err(ApiError::forbidden(
            "请在任务触发中停止自动任务，以同步更新执行与投递状态",
        ));
    }
    let changed=state.db(|db|{let tx=db.transaction()?;let changed=tx.execute("UPDATE runs SET status='cancelled',updated_at=?1 WHERE id=?2 AND owner_id=?3 AND status IN ('queued','running')",params![now(),id,admin.id])?;if changed!=0 {crate::conversation_images::finish_run_in(&tx,&id,"cancelled")?;}tx.commit()?;Ok(changed)})?;
    if changed == 0 {
        return Err(ApiError::not_found());
    }
    Ok(Json(json!({"id":id,"status":"cancelled"})))
}
