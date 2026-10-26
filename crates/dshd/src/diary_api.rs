use crate::{
    auth::Admin,
    diary::{self, ListOptions, Note, SourcePolicy},
    error::{ApiError, ApiResult},
    state::AppState,
};
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/v1/diary/status", get(status))
        .route("/api/admin/v1/diary/sources", get(available_sources))
        .route("/api/admin/v1/diary/policies", get(policies).put(policy))
        .route("/api/admin/v1/diary/events", get(events).post(note))
        .route(
            "/api/admin/v1/diary/events/{id}",
            get(event).patch(correct).delete(forget),
        )
        .route("/api/admin/v1/diary/entries", get(entries))
        .route("/api/admin/v1/diary/entries/{id}", get(entry))
        .route(
            "/api/admin/v1/diary/entries/{id}/sources",
            get(entry_sources),
        )
        .route("/api/admin/v1/diary/consolidate", post(consolidate))
        .route("/api/admin/v1/diary/export", get(export))
}
fn response<T: serde::Serialize>(status: StatusCode, revision: i64, value: T) -> Response {
    (
        status,
        [(header::ETAG, format!("\"{revision}\""))],
        Json(value),
    )
        .into_response()
}
fn expected(headers: &HeaderMap, allow_zero: bool) -> ApiResult<i64> {
    let value = headers
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::PRECONDITION_REQUIRED,
                "if_match_required",
                "须提交当前版本If-Match",
            )
        })?;
    value
        .trim_matches('"')
        .parse::<i64>()
        .ok()
        .filter(|v| *v >= if allow_zero { 0 } else { 1 })
        .ok_or_else(|| ApiError::bad("invalid_if_match", "If-Match必须为当前版本整数"))
}
async fn status(State(s): State<AppState>, _: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(s.db(|db| Ok(diary::status(db)))??))
}
async fn policies(State(s): State<AppState>, _: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(s.db(|db| Ok(diary::policies(db)))??))
}
async fn policy(
    State(s): State<AppState>,
    a: Admin,
    h: HeaderMap,
    Json(body): Json<SourcePolicy>,
) -> ApiResult<Response> {
    let revision = expected(&h, true)?;
    let value = s.db(|db| Ok(diary::save_policy(db, &a.id, revision, &body)))??;
    s.0.notify.notify_waiters();
    Ok(response(StatusCode::OK, revision + 1, value))
}
async fn events(
    State(s): State<AppState>,
    _: Admin,
    Query(q): Query<ListOptions>,
) -> ApiResult<Json<diary::Page<diary::Event>>> {
    Ok(Json(s.db(|db| Ok(diary::events(db, &q, None)))??))
}
async fn entries(
    State(s): State<AppState>,
    _: Admin,
    Query(q): Query<ListOptions>,
) -> ApiResult<Json<diary::Page<diary::Entry>>> {
    Ok(Json(s.db(|db| Ok(diary::list(db, &q, None)))??))
}
async fn event(State(s): State<AppState>, _: Admin, Path(id): Path<String>) -> ApiResult<Response> {
    let item = s.db(|db| Ok(diary::event(db, &id, None)))??;
    Ok(response(StatusCode::OK, item.revision, item))
}
async fn entry(State(s): State<AppState>, _: Admin, Path(id): Path<String>) -> ApiResult<Response> {
    let item = s.db(|db| Ok(diary::entry(db, &id, None)))??;
    Ok(response(StatusCode::OK, item.revision, item))
}
async fn entry_sources(
    State(s): State<AppState>,
    _: Admin,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let items = s.db(|db| Ok(diary::sources(db, &id, None)))??;
    Ok(Json(json!({"items":items})))
}
async fn note(State(s): State<AppState>, a: Admin, Json(body): Json<Note>) -> ApiResult<Response> {
    let item = s.db(|db| Ok(diary::note(db, &a.id, &body)))??;
    s.0.notify.notify_waiters();
    Ok(response(StatusCode::CREATED, item.revision, item))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Correction {
    content: String,
}
async fn correct(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(body): Json<Correction>,
) -> ApiResult<Response> {
    let revision = expected(&h, false)?;
    let item = s.db(|db| Ok(diary::correct(db, &a.id, &id, revision, &body.content)))??;
    s.0.notify.notify_waiters();
    Ok(response(StatusCode::OK, item.revision, item))
}
async fn forget(
    State(s): State<AppState>,
    a: Admin,
    Path(id): Path<String>,
    h: HeaderMap,
) -> ApiResult<Response> {
    let revision = expected(&h, false)?;
    let item = s.db(|db| Ok(diary::forget(db, &a.id, &id, revision)))??;
    s.0.notify.notify_waiters();
    Ok(response(StatusCode::OK, revision + 1, item))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Consolidate {
    period: String,
    day: String,
    timezone: String,
    visibility: String,
    source_owner_id: Option<String>,
}
async fn consolidate(
    State(s): State<AppState>,
    a: Admin,
    Json(body): Json<Consolidate>,
) -> ApiResult<Response> {
    let owner = body
        .source_owner_id
        .as_deref()
        .or(if body.visibility == "private" {
            Some(a.id.as_str())
        } else {
            None
        });
    let item=s.db(|db|{let tx=db.transaction()?;let item=diary::consolidate_in(&tx,&body.visibility,owner,&body.period,&body.day,&body.timezone,crate::state::now());if item.is_ok(){tx.execute("INSERT INTO audit_events(actor,action,resource,created_at) VALUES(?1,'diary.consolidate',?2,?3)",rusqlite::params![a.id,body.day,crate::state::now()])?;tx.commit()?;}Ok(item)})??;
    s.0.notify.notify_waiters();
    Ok(response(StatusCode::OK, item.revision, item))
}
async fn export(
    State(s): State<AppState>,
    _: Admin,
    Query(q): Query<ListOptions>,
) -> ApiResult<Response> {
    let value = s.db(|db| {
        Ok((
            diary::list(
                db,
                &ListOptions {
                    limit: 100,
                    ..q.clone()
                },
                None,
            ),
            diary::events(db, &ListOptions { limit: 100, ..q }, None),
        ))
    })?;
    Ok(([(header::CONTENT_DISPOSITION,"attachment; filename=\"dsh-diary.json\"")],Json(json!({"schema":"dsh-diary-export-v1","entries":value.0?,"events":value.1?,"pagination":"explicit_limit_100_per_section","generated_at":crate::state::now()}))).into_response())
}
async fn available_sources(State(s): State<AppState>, _: Admin) -> ApiResult<Json<Value>> {
    let items=s.db(|db|{let mut stmt=db.prepare("SELECT owner_id,title,conversation_count,revision,enabled,allow_model,timezone FROM (SELECT c.owner_id AS owner_id,min(c.title) AS title,count(*) AS conversation_count,coalesce(p.revision,0) AS revision,coalesce(p.enabled,0) AS enabled,coalesce(p.allow_model,0) AS allow_model,coalesce(p.timezone,'Asia/Shanghai') AS timezone,max(c.created_at) AS created_at FROM conversations c LEFT JOIN diary_source_policies p ON p.source_owner_id=c.owner_id WHERE NOT EXISTS(SELECT 1 FROM conversation_character_bindings b WHERE b.conversation_id=c.id AND b.draft_card_json IS NOT NULL) AND NOT EXISTS(SELECT 1 FROM runs r WHERE r.conversation_id=c.id AND r.run_kind IN ('trigger_model','trigger_image')) GROUP BY c.owner_id UNION ALL SELECT a.id,'管理记录',0,coalesce(p.revision,0),coalesce(p.enabled,0),coalesce(p.allow_model,0),coalesce(p.timezone,'Asia/Shanghai'),a.created_at FROM admins a LEFT JOIN diary_source_policies p ON p.source_owner_id=a.id WHERE NOT EXISTS(SELECT 1 FROM conversations c WHERE c.owner_id=a.id)) ORDER BY created_at DESC LIMIT 200")?;
        let rows=stmt.query_map([],|r|Ok(json!({"source_owner_id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"conversation_count":r.get::<_,i64>(2)?,"policy_revision":r.get::<_,i64>(3)?,"enabled":r.get::<_,bool>(4)?,"allow_model":r.get::<_,bool>(5)?,"timezone":r.get::<_,String>(6)?})))?;Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)})?;
    Ok(Json(json!({"items":items})))
}
