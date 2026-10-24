//! Administrator-owned Web/CLI source review. Merge before the global auth guard.
use crate::{
    auth::Admin,
    error::{ApiError, ApiResult},
    memory_candidates as candidates,
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

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/admin/v1/conversations/{id}/memory-candidates",
            get(page),
        )
        .route(
            "/api/admin/v1/conversations/{id}/memory-candidates/extract",
            post(extract),
        )
        .route(
            "/api/admin/v1/memory-candidate-jobs/{id}",
            get(job).delete(cancel),
        )
        .route("/api/admin/v1/memory-candidates/{id}", get(candidate))
        .route("/api/admin/v1/memory-candidates/{id}/accept", post(accept))
        .route("/api/admin/v1/memory-candidates/{id}/reject", post(reject))
}
fn expected(headers: &HeaderMap) -> ApiResult<String> {
    let value = headers
        .get(header::IF_MATCH)
        .and_then(|h| h.to_str().ok())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::PRECONDITION_REQUIRED,
                "if_match_required",
                "请提交当前候选或来源状态If-Match",
            )
        })?;
    let value = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .ok_or_else(|| ApiError::bad("invalid_if_match", "If-Match必须使用当前ETag"))?;
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|b| b.is_ascii_digit() || b == b'.')
    {
        return Err(ApiError::bad("invalid_if_match", "If-Match格式无效"));
    }
    Ok(value.into())
}
fn revision(headers: &HeaderMap) -> ApiResult<i64> {
    expected(headers)?
        .parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| ApiError::bad("invalid_if_match", "If-Match必须为候选当前正整数版本"))
}
fn response<T: serde::Serialize>(
    status: StatusCode,
    tag: impl std::fmt::Display,
    body: T,
) -> Response {
    (status, [(header::ETAG, format!("\"{tag}\""))], Json(body)).into_response()
}
async fn page(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let item = state.db(|db| Ok(candidates::page(db, &id, &admin.id)))??;
    Ok(response(StatusCode::OK, item.source_revision.clone(), item))
}
async fn extract(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<candidates::ExtractBody>,
) -> ApiResult<Response> {
    let expected = expected(&headers)?;
    let item = state.db(|db| Ok(candidates::extract(db, &id, &admin.id, &expected, &body)))??;
    state.0.notify.notify_waiters();
    Ok(response(StatusCode::ACCEPTED, item.revision, item))
}
async fn job(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let item = state.db(|db| Ok(candidates::show_job(db, &id, &admin.id)))??;
    Ok(response(StatusCode::OK, item.revision, item))
}
async fn candidate(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let item = state.db(|db| Ok(candidates::show_candidate(db, &id, &admin.id)))??;
    Ok(response(StatusCode::OK, item.revision, item))
}
async fn cancel(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let expected = revision(&headers)?;
    let item = state.db(|db| Ok(candidates::cancel(db, &id, &admin.id, expected)))??;
    state.0.notify.notify_waiters();
    Ok(response(StatusCode::OK, item.revision, item))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
async fn review(
    state: AppState,
    admin: Admin,
    id: String,
    headers: HeaderMap,
    accept: bool,
) -> ApiResult<Response> {
    let expected = revision(&headers)?;
    let item = state.db(|db| Ok(candidates::review(db, &id, &admin.id, expected, accept)))??;
    Ok(response(StatusCode::OK, item.revision, item))
}
async fn accept(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(_): Json<Empty>,
) -> ApiResult<Response> {
    review(state, admin, id, headers, true).await
}
async fn reject(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(_): Json<Empty>,
) -> ApiResult<Response> {
    review(state, admin, id, headers, false).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{now, random_token, token_hash, Options};
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use rusqlite::params;
    use serde_json::{json, Value};
    use tower::ServiceExt;

    struct Fixture {
        _temp: tempfile::TempDir,
        state: AppState,
        cookie: String,
    }
    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let state = AppState::open(Options {
            data_dir: temp.path().into(),
            origin: "http://127.0.0.1:18429".into(),
            allow_private_model: true,
            cookie_secure: false,
            web_dir: temp.path().join("web"),
        })
        .unwrap();
        let token = random_token();
        state.db(|db| {
            db.execute("INSERT INTO admins(id,username,password_hash,created_at) VALUES('owner','candidate-owner','unused',0)",[])?;
            db.execute("INSERT INTO sessions(token_hash,user_id,csrf,expires_at) VALUES(?1,'owner','candidate-csrf',?2)",params![token_hash(&token),now()+3600])?;
            db.execute("INSERT INTO conversations(id,owner_id,title,created_at) VALUES('owned','owner','own',0),('other','someone','other',0)",[])?;
            db.execute("INSERT INTO runs(id,conversation_id,owner_id,client_message_id,input,config_json,status,response,created_at,updated_at,run_kind) VALUES('source-run','owned','owner','source-run','safe fact','{}','completed','assistant',0,0,'model')",[])?;
            db.execute("INSERT INTO messages(id,conversation_id,run_id,role,content,created_at) VALUES('source-user','owned','source-run','user','用户喜欢蓝色',0),('source-assistant','owned','source-run','assistant','助手文字不应成为候选来源',0)",[])?;
            Ok(())
        }).unwrap();
        Fixture {
            _temp: temp,
            state,
            cookie: format!("dsh_session={token}"),
        }
    }
    async fn request(
        f: &Fixture,
        method: &str,
        path: &str,
        authenticated: bool,
        csrf: bool,
        tag: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value, Option<String>) {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("Host", "127.0.0.1:18429")
            .header("Origin", "http://127.0.0.1:18429")
            .header("Content-Type", "application/json");
        if authenticated {
            builder = builder.header("Cookie", &f.cookie);
        }
        if csrf {
            builder = builder.header("X-CSRF-Token", "candidate-csrf");
        }
        if let Some(tag) = tag {
            builder = builder.header("If-Match", tag);
        }
        let response = crate::api::router(f.state.clone())
            .oneshot(builder.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let tag = response
            .headers()
            .get(header::ETAG)
            .map(|v| v.to_str().unwrap().to_owned());
        let bytes = to_bytes(response.into_body(), 256 * 1024).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            tag,
        )
    }
    #[tokio::test]
    async fn candidate_api_enforces_auth_csrf_ownership_and_explicit_sources() {
        let f = fixture();
        let path = "/api/admin/v1/conversations/owned/memory-candidates";
        assert_eq!(
            request(&f, "GET", path, false, false, None, json!({}))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(
                &f,
                "GET",
                "/api/admin/v1/conversations/other/memory-candidates",
                true,
                false,
                None,
                json!({})
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        let page = request(&f, "GET", path, true, false, None, json!({})).await;
        assert_eq!(page.0, StatusCode::OK);
        assert_eq!(page.1["sources"].as_array().unwrap().len(), 1);
        assert_eq!(page.1["sources"][0]["id"], "source-user");
        let tag = page.2.unwrap();
        let path = format!("{path}/extract");
        let body = json!({"source_message_ids":["source-user"]});
        assert_eq!(
            request(&f, "POST", &path, true, false, Some(&tag), body.clone())
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(&f, "POST", &path, true, true, None, body.clone())
                .await
                .0,
            StatusCode::PRECONDITION_REQUIRED
        );
        assert_eq!(
            request(
                &f,
                "POST",
                &path,
                true,
                true,
                Some("\"0.0.0\""),
                body.clone()
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            request(
                &f,
                "POST",
                &path,
                true,
                true,
                Some(&tag),
                json!({"source_message_ids":[]})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            request(
                &f,
                "POST",
                &path,
                true,
                true,
                Some(&tag),
                json!({"source_message_ids":["source-assistant"]})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(request(&f,"POST",&path,true,true,Some(&tag),json!({"source_message_ids":["source-user"],"owner":"someone","visibility":"shared"})).await.0,StatusCode::UNPROCESSABLE_ENTITY);
        // No model configured is a deterministic error; an API request never
        // silently scans history or creates a chat Run as a fallback.
        let unavailable = request(&f, "POST", &path, true, true, Some(&tag), body).await;
        assert_eq!(unavailable.0, StatusCode::BAD_REQUEST);
        assert_eq!(
            unavailable.1["error"]["code"],
            "candidate_model_unavailable"
        );
        assert_eq!(
            f.state
                .db(|db| Ok(db.query_row(
                    "SELECT count(*) FROM memory_candidate_jobs",
                    [],
                    |r| r.get::<_, i64>(0)
                )?))
                .unwrap(),
            0
        );
    }
    #[tokio::test]
    async fn candidate_accept_is_private_atomic_revision_guarded_and_terminal_bodyless() {
        let f = fixture();
        f.state.db(|db| {
            let assistant=memory_identity(db)?;
            let config=serde_json::to_string(&crate::character::current_config_in(db)?.1)?;
            db.execute("INSERT INTO memory_candidate_jobs(id,assistant_id,conversation_id,source_owner_id,source_epoch,source_floor,source_digest,source_message_ids_json,config_json,status,revision,created_at,expires_at,source_count) VALUES('job',?1,'owned','owner',0,0,'deliberately-stale','[\"source-user\"]',?2,'completed',3,0,?3,1)",params![assistant,config,now()+900])?;
            db.execute("INSERT INTO memory_candidates(id,job_id,status,revision,content,source_message_ids_json,created_at,expires_at) VALUES('candidate','job','pending',1,'用户喜欢蓝色','[\"source-user\"]',0,?1)",[now()+3600])?;
            Ok(())
        }).unwrap();
        let path = "/api/admin/v1/memory-candidates/candidate/accept";
        assert_eq!(
            request(&f, "POST", path, false, true, Some("\"1\""), json!({}))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(&f, "POST", path, true, false, Some("\"1\""), json!({}))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        let stale = request(&f, "POST", path, true, true, Some("\"1\""), json!({})).await;
        assert_eq!(stale.0, StatusCode::CONFLICT);
        assert_eq!(
            stale.1["error"]["code"],
            "memory_candidate_revision_conflict"
        );
        let current = request(
            &f,
            "GET",
            "/api/admin/v1/memory-candidates/candidate",
            true,
            false,
            None,
            json!({}),
        )
        .await;
        assert_eq!(current.1["status"], "invalidated");
        assert!(current.1.get("content").is_none());
        assert_eq!(
            f.state
                .db(|db| Ok(
                    db.query_row("SELECT count(*) FROM memory_records", [], |r| r
                        .get::<_, i64>(0))?
                ))
                .unwrap(),
            0
        );
    }
    fn memory_identity(db: &rusqlite::Connection) -> anyhow::Result<String> {
        crate::memory::identity(db).map_err(|_| anyhow::anyhow!("identity unavailable"))
    }
}
