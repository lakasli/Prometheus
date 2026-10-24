//! Authenticated views and explicit rebuilding of derived conversation summaries.
use crate::{
    auth::Admin,
    error::{ApiError, ApiResult},
    state::{now, AppState},
};
use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::json;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/v1/conversations/{id}/summary", get(show))
        .route(
            "/api/admin/v1/conversations/{id}/summary/rebuild",
            post(rebuild),
        )
}

fn owned(db: &Connection, id: &str, owner: &str) -> anyhow::Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM conversations WHERE id=?1 AND owner_id=?2)",
        params![id, owner],
        |r| r.get(0),
    )?)
}

fn revision(db: &Connection, id: &str) -> anyhow::Result<String> {
    let epoch: i64 = db
        .query_row(
            "SELECT epoch FROM summary_context_epochs WHERE conversation_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let revision: i64 = db
        .query_row(
            "SELECT revision FROM conversation_summaries WHERE conversation_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    Ok(format!("{epoch}.{revision}"))
}

async fn show(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let result=state.db(|db| {
        if !owned(db,&id,&admin.id)? {return Ok(None);}
        let summary=crate::conversation_summary::context(db,&id,&admin.id)?;
        let last_job=db.query_row("SELECT status,created_at,error_code FROM summary_jobs WHERE conversation_id=?1 AND source_owner_id=?2 ORDER BY rowid DESC LIMIT 1",params![id,admin.id],|r|Ok(json!({"status":r.get::<_,String>(0)?,"created_at":r.get::<_,i64>(1)?,"error_code":r.get::<_,Option<String>>(2)?}))).optional()?;
        let tag=revision(db,&id)?;
        Ok(Some((tag.clone(),json!({"conversation_id":id,"revision":tag,"summary":summary,"last_job":last_job,"policy":{"keep_recent_turns":4,"max_batch_turns":8,"max_source_bytes":24000,"max_summary_bytes":6000,"timeout_seconds":20}}))))
    })?.ok_or_else(ApiError::not_found)?;
    Ok((
        [(header::ETAG, format!("\"{}\"", result.0))],
        Json(result.1),
    )
        .into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RebuildBody {}

async fn rebuild(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(_body): Json<RebuildBody>,
) -> ApiResult<Response> {
    let expected = headers
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::PRECONDITION_REQUIRED,
                "if_match_required",
                "请提交当前摘要状态If-Match",
            )
        })?;
    // Keep epoch and revision separate; a second invalidation at the same history
    // watermark must invalidate a stale management request as well.
    let result=state.db(|db| {
        let tx=db.transaction()?;
        if !owned(&tx,&id,&admin.id)? {return Ok((false,false,None));}
        if expected!=format!("\"{}\"",revision(&tx,&id)?) {return Ok((true,false,None));}
        let is_test:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM conversation_character_bindings WHERE conversation_id=?1 AND draft_card_json IS NOT NULL)",[&id],|r|r.get(0))?;
        if is_test {return Ok((true,true,None));}
        let assistant=crate::memory::identity(&tx).map_err(|_|anyhow::anyhow!("assistant identity unavailable"))?;
        crate::conversation_summary::invalidate(&tx,&assistant,&id)?;
        tx.execute("DELETE FROM run_prompt_snapshots WHERE run_id IN (SELECT run_id FROM run_summary_uses WHERE conversation_id=?1)",[&id])?;
        tx.execute("INSERT INTO audit_events(actor,action,resource,created_at) VALUES(?1,'summary.rebuild',?2,?3)",params![admin.id,id,now()])?;
        let tag=revision(&tx,&id)?;
        tx.commit()?;
        Ok((true,false,Some(tag)))
    })?;
    if !result.0 {
        return Err(ApiError::not_found());
    }
    if result.1 {
        return Err(ApiError::bad(
            "summary_draft_test",
            "角色草稿测试会话不生成长期摘要",
        ));
    }
    let tag = result.2.ok_or_else(|| {
        ApiError::new(
            StatusCode::CONFLICT,
            "summary_revision_conflict",
            "摘要状态已变化，请刷新后再重建",
        )
    })?;
    Ok((
        StatusCode::ACCEPTED,
        [(header::ETAG, format!("\"{tag}\""))],
        Json(json!({"status":"pending_next_run","revision":tag})),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{random_token, token_hash, Options};
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use serde_json::Value;
    use tower::ServiceExt;

    async fn request(
        state: &AppState,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        csrf: bool,
        tag: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("Host", "127.0.0.1:18428")
            .header("Origin", "http://127.0.0.1:18428")
            .header("Content-Type", "application/json");
        if let Some(cookie) = cookie {
            request = request.header("Cookie", cookie);
        }
        if csrf {
            request = request.header("X-CSRF-Token", "summary-test-csrf");
        }
        if let Some(tag) = tag {
            request = request.header("If-Match", tag);
        }
        let response = crate::api::router(state.clone())
            .oneshot(request.body(Body::from("{}")).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 128 * 1024).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn summary_management_requires_owner_csrf_and_epoch_aware_revision() {
        let temp = tempfile::tempdir().unwrap();
        let state = AppState::open(Options {
            data_dir: temp.path().to_owned(),
            origin: "http://127.0.0.1:18428".into(),
            allow_private_model: true,
            cookie_secure: false,
            web_dir: temp.path().join("web"),
        })
        .unwrap();
        let token = random_token();
        state.db(|db| {
            db.execute("INSERT INTO admins(id,username,password_hash,created_at) VALUES('admin','admin','unused',?1)",[now()])?;
            db.execute("INSERT INTO sessions(token_hash,user_id,csrf,expires_at) VALUES(?1,'admin','summary-test-csrf',?2)",params![token_hash(&token),now()+3600])?;
            db.execute("INSERT INTO conversations(id,owner_id,title,created_at) VALUES('owned','admin','own',1),('other','someone-else','other',1)",[])?;
            db.execute("INSERT INTO messages(id,conversation_id,role,content,created_at) VALUES('original','owned','user','synthetic retained source',1)",[])?;
            Ok(())
        }).unwrap();
        let cookie = format!("dsh_session={token}");
        let path = "/api/admin/v1/conversations/owned/summary";
        assert_eq!(
            request(&state, "GET", path, None, false, None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(
                &state,
                "GET",
                "/api/admin/v1/conversations/other/summary",
                Some(&cookie),
                false,
                None
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        let first = request(&state, "GET", path, Some(&cookie), false, None).await;
        assert_eq!(first.0, StatusCode::OK);
        assert_eq!(first.1["revision"], "0.0");
        assert!(first.1["summary"].is_null());
        let rebuild = format!("{path}/rebuild");
        assert_eq!(
            request(
                &state,
                "POST",
                &rebuild,
                Some(&cookie),
                false,
                Some("\"0.0\"")
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(&state, "POST", &rebuild, Some(&cookie), true, None)
                .await
                .0,
            StatusCode::PRECONDITION_REQUIRED
        );
        assert_eq!(
            request(
                &state,
                "POST",
                &rebuild,
                Some(&cookie),
                true,
                Some("\"wrong\"")
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        let accepted = request(
            &state,
            "POST",
            &rebuild,
            Some(&cookie),
            true,
            Some("\"0.0\""),
        )
        .await;
        assert_eq!(accepted.0, StatusCode::ACCEPTED);
        assert_eq!(accepted.1["revision"], "1.0");
        assert_eq!(
            request(
                &state,
                "POST",
                &rebuild,
                Some(&cookie),
                true,
                Some("\"0.0\"")
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        assert_eq!(
            state
                .db(|db| Ok(db.query_row(
                    "SELECT content FROM messages WHERE id='original'",
                    [],
                    |r| r.get::<_, String>(0)
                )?))
                .unwrap(),
            "synthetic retained source"
        );
        assert_eq!(
            state
                .db(|db| Ok(db.query_row(
                    "SELECT count(*) FROM audit_events WHERE action='summary.rebuild'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?))
                .unwrap(),
            1
        );
    }
}
