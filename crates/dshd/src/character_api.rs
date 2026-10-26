use crate::{
    auth::Admin,
    character::{self, CardRef, CharacterError, CharacterResult},
    character_import,
    error::{ApiError, ApiResult},
    state::{now, private_dir, private_write, AppState},
};
use axum::{
    body::to_bytes,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

static IMPORT_SLOT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/v1/character-cards", get(list).post(create))
        .route(
            "/api/admin/v1/character-cards/from-legacy",
            post(from_legacy),
        )
        .route(
            "/api/admin/v1/character-cards/default",
            get(default_state).put(set_default),
        )
        .route(
            "/api/admin/v1/character-cards/import/inspect",
            post(inspect),
        )
        .route(
            "/api/admin/v1/character-cards/import/{import_id}/confirm",
            post(confirm),
        )
        .route(
            "/api/admin/v1/character-cards/{id}/drafts",
            post(from_revision),
        )
        .route(
            "/api/admin/v1/character-cards/{id}/drafts/{draft_id}",
            get(get_draft).put(update_draft),
        )
        .route(
            "/api/admin/v1/character-cards/{id}/drafts/{draft_id}/validate",
            post(validate),
        )
        .route(
            "/api/admin/v1/character-cards/{id}/drafts/{draft_id}/preview",
            post(preview),
        )
        .route(
            "/api/admin/v1/character-cards/{id}/drafts/{draft_id}/publish",
            post(publish),
        )
        .route(
            "/api/admin/v1/character-cards/{id}/drafts/{draft_id}/bind-current-model",
            post(bind_current_model),
        )
        .route(
            "/api/admin/v1/character-cards/{id}/drafts/{draft_id}/test-conversations",
            post(test_conversation),
        )
        .route(
            "/api/admin/v1/character-cards/{id}/revisions",
            get(revisions),
        )
        .route(
            "/api/admin/v1/character-cards/{id}/revisions/{revision}/export",
            get(export),
        )
        .route("/api/admin/v1/character-cards/{id}/archive", post(archive))
        .route(
            "/api/admin/v1/character-cards/{id}/qq-access",
            put(qq_access),
        )
        .layer(DefaultBodyLimit::max(character_import::JSON_LIMIT))
        .layer(middleware::from_fn(retired_card_writes))
        .route(
            "/api/admin/v1/assistant/personality",
            get(get_personality).put(put_personality),
        )
        .route(
            "/api/admin/v1/assistant/personality/history",
            get(get_personality_history),
        )
}
async fn retired_card_writes(request: Request, next: Next) -> Response {
    if !request.method().is_safe() {
        return ApiError::from(character::single_personality_only()).into_response();
    }
    next.run(request).await
}
async fn get_personality(State(state): State<AppState>, _admin: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(db(&state, |conn| {
        let (_, config) = current(conn)?;
        character::personality_state(conn, &config)
    })?))
}
async fn get_personality_history(
    State(state): State<AppState>,
    _admin: Admin,
) -> ApiResult<Json<Value>> {
    Ok(Json(db(&state, character::personality_history)?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PersonalityBody {
    personality: character::Personality,
}
async fn put_personality(
    State(state): State<AppState>,
    admin: Admin,
    headers: HeaderMap,
    Json(body): Json<PersonalityBody>,
) -> ApiResult<Json<Value>> {
    let expected = headers
        .get(header::IF_MATCH)
        .and_then(|h| h.to_str().ok())
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .map(|s| s.trim_matches('"').to_owned())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::PRECONDITION_REQUIRED,
                "if_match_required",
                "请提供当前助手性格版本If-Match",
            )
        })?;
    Ok(Json(tx(&state, |conn| {
        let (_, config) = current(conn)?;
        let result = character::save_personality(conn, &config, &expected, body.personality)?;
        audit(
            conn,
            &admin.id,
            "assistant.personality.update",
            character::PERSONALITY_ID,
        )?;
        Ok(result)
    })?))
}

fn db<T>(state: &AppState, f: impl FnOnce(&Connection) -> CharacterResult<T>) -> ApiResult<T> {
    let conn = state.0.db.lock().map_err(|_| ApiError::internal())?;
    f(&conn).map_err(Into::into)
}
fn tx<T>(state: &AppState, f: impl FnOnce(&Connection) -> CharacterResult<T>) -> ApiResult<T> {
    let mut conn = state.0.db.lock().map_err(|_| ApiError::internal())?;
    let transaction = conn.transaction().map_err(|_| ApiError::internal())?;
    let result = f(&transaction).map_err(ApiError::from)?;
    transaction.commit().map_err(|_| ApiError::internal())?;
    Ok(result)
}
fn audit(db: &Connection, actor: &str, action: &str, resource: &str) -> CharacterResult<()> {
    db.execute(
        "INSERT INTO audit_events(actor,action,resource,created_at) VALUES(?1,?2,?3,?4)",
        params![actor, action, resource, now()],
    )?;
    Ok(())
}
fn expected(headers: &HeaderMap) -> ApiResult<String> {
    headers
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .map(|v| v.trim_matches('"').to_string())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::PRECONDITION_REQUIRED,
                "if_match_required",
                "请提供当前角色卡版本If-Match",
            )
        })
}
fn current(db: &Connection) -> CharacterResult<(i64, crate::config::RuntimeConfig)> {
    character::current_config_in(db)
        .map_err(|_| CharacterError::new("invalid_active_config", "无法读取当前配置"))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CardBody {
    card: Option<Value>,
}
async fn list(State(state): State<AppState>, admin: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(db(&state, |conn| character::list(conn, &admin.id))?))
}
async fn create(
    State(state): State<AppState>,
    admin: Admin,
    Json(body): Json<CardBody>,
) -> ApiResult<Json<Value>> {
    let result = tx(&state, |conn| {
        let draft = character::create(
            conn,
            &admin.id,
            body.card
                .unwrap_or_else(|| character::template("新角色", "")),
        )?;
        audit(conn, &admin.id, "character.create", &draft.card_id)?;
        Ok(draft)
    })?;
    Ok(Json(
        serde_json::to_value(result).map_err(|_| ApiError::internal())?,
    ))
}
async fn from_legacy(State(state): State<AppState>, admin: Admin) -> ApiResult<Json<Value>> {
    let result = tx(&state, |conn| {
        let (_, config) = current(conn)?;
        let draft = character::create(
            conn,
            &admin.id,
            character::template(&config.assistant.name, &config.assistant.system_prompt),
        )?;
        audit(conn, &admin.id, "character.from_legacy", &draft.card_id)?;
        Ok(draft)
    })?;
    Ok(Json(
        serde_json::to_value(result).map_err(|_| ApiError::internal())?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevisionBody {
    revision: Option<i64>,
    base_revision: Option<i64>,
}
async fn from_revision(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    Json(body): Json<RevisionBody>,
) -> ApiResult<Json<Value>> {
    let revision = body
        .revision
        .or(body.base_revision)
        .ok_or_else(|| ApiError::bad("revision_required", "请指定角色版本"))?;
    let result = tx(&state, |conn| {
        character::from_revision(conn, &id, revision, &admin.id)
    })?;
    Ok(Json(
        serde_json::to_value(result).map_err(|_| ApiError::internal())?,
    ))
}
async fn get_draft(
    State(state): State<AppState>,
    admin: Admin,
    Path((id, draft_id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let result = db(&state, |conn| {
        character::get_draft(conn, &id, &draft_id, &admin.id)
    })?;
    Ok(Json(
        serde_json::to_value(result).map_err(|_| ApiError::internal())?,
    ))
}
async fn update_draft(
    State(state): State<AppState>,
    admin: Admin,
    Path((id, draft_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<CardBody>,
) -> ApiResult<Json<Value>> {
    let expected = expected(&headers)?;
    let card = body
        .card
        .ok_or_else(|| ApiError::bad("card_required", "请提供角色卡"))?;
    let result = tx(&state, |conn| {
        let draft = character::update_draft(conn, &id, &draft_id, &admin.id, &expected, card)?;
        audit(conn, &admin.id, "character.draft.update", &draft_id)?;
        Ok(draft)
    })?;
    Ok(Json(
        serde_json::to_value(result).map_err(|_| ApiError::internal())?,
    ))
}
async fn validate(
    State(state): State<AppState>,
    admin: Admin,
    Path((id, draft_id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let result = db(&state, |conn| {
        let draft = character::get_draft(conn, &id, &draft_id, &admin.id)?;
        let (_, config) = current(conn)?;
        let binding = character::draft_binding(conn, &draft_id)?;
        let mut errors = character::validate(&draft.card);
        let valid = errors.is_empty();
        let binding_errors = character::binding_errors(&draft.card, &config, binding.as_deref());
        let bindable = valid && binding_errors.is_empty();
        errors.extend(binding_errors);
        let warnings = character::compile(&draft.card, &admin.username)
            .map(|v| v.1)
            .unwrap_or_default();
        Ok(json!({"valid":valid,"bindable":bindable,"errors":errors,"warnings":warnings}))
    })?;
    Ok(Json(result))
}
async fn preview(
    State(state): State<AppState>,
    admin: Admin,
    Path((id, draft_id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let result = db(&state, |conn| {
        let draft = character::get_draft(conn, &id, &draft_id, &admin.id)?;
        let (system_prompt, warnings) = character::compile(&draft.card, &admin.username)?;
        Ok(
            json!({"bytes":system_prompt.len(),"system_prompt":system_prompt,"warnings":warnings,"ignored_fields":["creator_notes","first_mes","alternate_greetings","group_only_greetings","post_history_instructions","tags","creator","character_version"]}),
        )
    })?;
    Ok(Json(result))
}
async fn bind_current_model(
    State(state): State<AppState>,
    admin: Admin,
    Path((id, draft_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    let expected = expected(&headers)?;
    let result = tx(&state, |conn| {
        let (_, config) = current(conn)?;
        let draft =
            character::map_draft_model(conn, &id, &draft_id, &admin.id, &expected, &config)?;
        audit(conn, &admin.id, "character.model.bind", &draft_id)?;
        Ok(draft)
    })?;
    Ok(Json(
        serde_json::to_value(result).map_err(|_| ApiError::internal())?,
    ))
}
async fn publish(
    State(state): State<AppState>,
    admin: Admin,
    Path((id, draft_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    let expected = expected(&headers)?;
    let key = headers
        .get("idempotency-key")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let result = tx(&state, |conn| {
        let (_, config) = current(conn)?;
        let result = character::publish(conn, &id, &draft_id, &admin.id, &expected, key, &config)?;
        audit(conn, &admin.id, "character.publish", &id)?;
        Ok(result)
    })?;
    Ok(Json(result))
}
async fn default_state(State(state): State<AppState>, _admin: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(db(&state, character::default_state)?))
}
async fn set_default(
    State(state): State<AppState>,
    admin: Admin,
    headers: HeaderMap,
    Json(reference): Json<CardRef>,
) -> ApiResult<Json<Value>> {
    let expected = expected(&headers)?;
    let result = tx(&state, |conn| {
        let (_, config) = current(conn)?;
        let result = character::set_default(conn, &reference, &expected, &config)?;
        audit(conn, &admin.id, "character.default", &reference.card_id)?;
        Ok(result)
    })?;
    Ok(Json(result))
}
async fn revisions(
    State(state): State<AppState>,
    _admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    Ok(Json(db(&state, |conn| character::revisions(conn, &id))?))
}
async fn export(
    State(state): State<AppState>,
    admin: Admin,
    Path((id, revision)): Path<(String, i64)>,
) -> ApiResult<Response> {
    let card = tx(&state, |conn| {
        let card = character::revision_card(
            conn,
            &CardRef {
                card_id: id.clone(),
                revision,
            },
            true,
        )?;
        audit(conn, &admin.id, "character.export", &id)?;
        Ok(card)
    })?;
    Ok((
        [
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"character-card.json\"",
            ),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Json(card),
    )
        .into_response())
}
async fn archive(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    let expected = expected(&headers)?;
    Ok(Json(tx(&state, |conn| {
        let result = character::archive(conn, &id, &expected)?;
        audit(conn, &admin.id, "character.archive", &id)?;
        Ok(result)
    })?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccessBody {
    enabled: bool,
}
async fn qq_access(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<AccessBody>,
) -> ApiResult<Json<Value>> {
    let expected = expected(&headers)?;
    Ok(Json(tx(&state, |conn| {
        let result = character::set_qq_access(conn, &id, &expected, body.enabled)?;
        audit(conn, &admin.id, "character.qq_access", &id)?;
        Ok(result)
    })?))
}
async fn test_conversation(
    State(state): State<AppState>,
    admin: Admin,
    Path((id, draft_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    let expected = expected(&headers)?;
    Ok(Json(tx(&state, |conn| {
        let (_, config) = current(conn)?;
        let conversation = uuid::Uuid::new_v4().to_string();
        let title = "角色卡测试对话";
        conn.execute(
            "INSERT INTO conversations(id,owner_id,title,created_at) VALUES(?1,?2,?3,?4)",
            params![conversation, admin.id, title, now()],
        )?;
        character::bind_test_conversation(
            conn,
            &conversation,
            &id,
            &draft_id,
            &admin.id,
            &expected,
            &config,
        )?;
        audit(
            conn,
            &admin.id,
            "character.test_conversation",
            &conversation,
        )?;
        Ok(
            json!({"id":conversation,"title":title,"is_test":true,"character":character::connection_character(conn,&conversation)?}),
        )
    })?))
}
async fn inspect(
    State(state): State<AppState>,
    admin: Admin,
    request: Request,
) -> ApiResult<Json<Value>> {
    let _permit = IMPORT_SLOT.try_acquire().map_err(|_| {
        ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "character_import_busy",
            "已有角色卡导入正在处理，请稍后重试",
        )
    })?;
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = to_bytes(request.into_body(), character_import::ZIP_LIMIT)
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "character_import_too_large",
                "角色卡文件超过8MiB",
            )
        })?;
    let report = character_import::inspect(&bytes, &content_type).map_err(ApiError::from)?;
    cleanup_expired_imports(&state)?;
    db(&state, |conn| {
        let(owner_count,total):(i64,i64)=conn.query_row("SELECT sum(CASE WHEN owner_id=?1 THEN 1 ELSE 0 END),count(*) FROM character_imports WHERE confirmed_json IS NULL AND expires_at>=?2",params![admin.id,now()],|r|Ok((r.get::<_,Option<i64>>(0)?.unwrap_or(0),r.get(1)?)))?;
        if owner_count >= 4 || total >= 16 {
            return Err(CharacterError {
                code: "character_import_quota",
                message: "未确认的导入预览过多，请先确认现有预览或等待过期".into(),
                status: StatusCode::TOO_MANY_REQUESTS,
            });
        }
        Ok(())
    })?;
    let id = uuid::Uuid::new_v4().to_string();
    let directory = state.0.options.data_dir.join("character-imports");
    private_dir(&directory).map_err(|_| ApiError::internal())?;
    // Raw input stays in an owner-only temporary file; only normalized preview metadata goes to SQLite.
    private_write(&directory.join(&id), &bytes).map_err(|_| ApiError::internal())?;
    let result = tx(&state, |conn| {
        let mut expired = conn.prepare("SELECT id FROM character_imports WHERE expires_at<?1")?;
        let expired = expired
            .query_map([now()], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for old in expired {
            if uuid::Uuid::parse_str(&old).is_ok() {
                let _ = std::fs::remove_file(directory.join(&old));
            }
        }
        conn.execute("DELETE FROM character_imports WHERE expires_at<?1", [now()])?;
        conn.execute("INSERT INTO character_imports(id,owner_id,input_digest,card_json,report_json,expires_at) VALUES(?1,?2,?3,?4,?5,?6)",params![id,admin.id,report.digest,report.card.to_string(),serde_json::to_string(&report)?,now()+900])?;
        audit(conn, &admin.id, "character.import.inspect", &id)?;
        let mut result = serde_json::to_value(report)?;
        result["import_id"] = json!(id);
        Ok(result)
    });
    if result.is_err() {
        let _ = std::fs::remove_file(directory.join(&id));
    }
    Ok(Json(result?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfirmBody {
    card: Value,
    digest: String,
}
async fn confirm(
    State(state): State<AppState>,
    admin: Admin,
    Path(import_id): Path<String>,
    Json(body): Json<ConfirmBody>,
) -> ApiResult<Json<Value>> {
    if uuid::Uuid::parse_str(&import_id).is_err() {
        return Err(ApiError::not_found());
    }
    let result = tx(&state, |conn| {
        let row:Option<(String,i64,Option<String>,String)>=conn.query_row("SELECT input_digest,expires_at,confirmed_json,report_json FROM character_imports WHERE id=?1 AND owner_id=?2",params![import_id,admin.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        let (digest, expires, confirmed, report_json) = row.ok_or_else(CharacterError::missing)?;
        if expires < now() {
            return Err(CharacterError::new(
                "character_import_expired",
                "导入预览已过期，请重新上传",
            ));
        }
        if digest != body.digest {
            return Err(CharacterError::conflict());
        }
        require_current_converter(&report_json)?;
        let request_digest = character::card_digest(&body.card)?;
        if let Some(stored) = confirmed {
            let stored: Value = serde_json::from_str(&stored)?;
            if stored["request_digest"] != request_digest {
                return Err(CharacterError::conflict());
            }
            return Ok(stored["result"].clone());
        }
        let draft = character::create(conn, &admin.id, body.card)?;
        let result = serde_json::to_value(draft)?;
        conn.execute(
            "UPDATE character_imports SET confirmed_json=?1 WHERE id=?2",
            params![
                json!({"request_digest":request_digest,"result":result}).to_string(),
                import_id
            ],
        )?;
        audit(conn, &admin.id, "character.import.confirm", &import_id)?;
        Ok(result)
    })?;
    let _ = std::fs::remove_file(
        state
            .0
            .options
            .data_dir
            .join("character-imports")
            .join(&import_id),
    );
    Ok(Json(result))
}

/// Called at startup and every minute by the managed application lifecycle task.
pub fn cleanup_expired_imports(state: &AppState) -> ApiResult<()> {
    let directory = state.0.options.data_dir.join("character-imports");
    let conn = state.0.db.lock().map_err(|_| ApiError::internal())?;
    let mut stmt = conn
        .prepare("SELECT id FROM character_imports WHERE expires_at<?1")
        .map_err(|_| ApiError::internal())?;
    let expired = stmt
        .query_map([now()], |r| r.get::<_, String>(0))
        .map_err(|_| ApiError::internal())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ApiError::internal())?;
    drop(stmt);
    for id in expired {
        if uuid::Uuid::parse_str(&id).is_ok() {
            match std::fs::remove_file(directory.join(&id)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(ApiError::internal()),
            }
        }
        conn.execute("DELETE FROM character_imports WHERE id=?1", [id])
            .map_err(|_| ApiError::internal())?;
    }
    if let Ok(entries) = std::fs::read_dir(&directory) {
        for entry in entries.take(128) {
            let entry = entry.map_err(|_| ApiError::internal())?;
            let name = entry.file_name();
            let Some(id) = name.to_str() else { continue };
            if uuid::Uuid::parse_str(id).is_err() || !entry.file_type().is_ok_and(|ty| ty.is_file())
            {
                continue;
            }
            let old = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age.as_secs() >= 900);
            if old {
                let exists: bool = conn
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM character_imports WHERE id=?1)",
                        [id],
                        |r| r.get(0),
                    )
                    .map_err(|_| ApiError::internal())?;
                if !exists {
                    std::fs::remove_file(entry.path()).map_err(|_| ApiError::internal())?;
                }
            }
        }
    }
    Ok(())
}

fn require_current_converter(report_json: &str) -> CharacterResult<()> {
    let report: Value = serde_json::from_str(report_json)?;
    if report["converter_version"] != character_import::CONVERTER_VERSION {
        return Err(CharacterError {
            code: "character_import_version_changed",
            message: "导入转换器版本已改变，请重新上传并检查转换结果".into(),
            status: StatusCode::CONFLICT,
        });
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn import_confirmation_requires_the_inspected_converter_version() {
        assert!(require_current_converter(
            &json!({"converter_version":character_import::CONVERTER_VERSION}).to_string()
        )
        .is_ok());
        let old =
            require_current_converter(r#"{"converter_version":"dsh-airi-text-old"}"#).unwrap_err();
        assert_eq!(old.status, StatusCode::CONFLICT);
        assert_eq!(old.code, "character_import_version_changed");
        assert!(require_current_converter("{}").is_err());
    }
}
