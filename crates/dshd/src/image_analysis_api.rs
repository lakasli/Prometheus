//! Single PNG analysis API; global Admin/Origin/CSRF middleware still applies.
use crate::{
    auth::Admin,
    error::{ApiError, ApiResult},
    image_analysis as analysis,
    state::AppState,
};
use axum::{
    extract::{DefaultBodyLimit, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
pub(crate) static DECODE_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/v1/image-analysis", get(page))
        .route(
            "/api/admin/v1/image-analysis/jobs",
            post(create).layer(DefaultBodyLimit::max(1_500_000)),
        )
        .route(
            "/api/admin/v1/image-analysis/jobs/{id}",
            get(show).delete(delete),
        )
}
fn etag(item: analysis::Job, status: StatusCode) -> Response {
    (
        status,
        [(header::ETAG, format!("\"{}\"", item.revision))],
        Json(item),
    )
        .into_response()
}
fn expected(headers: &HeaderMap) -> ApiResult<i64> {
    let value = headers
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::PRECONDITION_REQUIRED,
                "if_match_required",
                "请提交当前图片分析版本If-Match",
            )
        })?;
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| ApiError::bad("invalid_if_match", "If-Match必须为图片分析当前整数ETag"))
}
async fn page(State(state): State<AppState>, admin: Admin) -> ApiResult<Json<analysis::Page>> {
    Ok(Json(analysis::page(&state, &admin.id)?))
}
async fn create(
    State(state): State<AppState>,
    admin: Admin,
    Json(body): Json<analysis::CreateBody>,
) -> ApiResult<Response> {
    let permit = DECODE_SLOTS.try_acquire().map_err(|_| {
        ApiError::new(
            StatusCode::CONFLICT,
            "image_analysis_busy",
            "图片校验正在进行，请稍后再试",
        )
    })?;
    let image = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        analysis::canonicalize(&body.image_base64)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    let item = state.db(|db| {
        Ok(analysis::create(
            db,
            &admin.id,
            &body.client_request_id,
            &body.question,
            image,
            state.0.options.allow_private_model,
        ))
    })??;
    state.0.notify.notify_waiters();
    Ok(etag(item, StatusCode::ACCEPTED))
}
async fn show(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let item = state.db(|db| Ok(analysis::show(db, &id, &admin.id)))??;
    Ok(etag(item, StatusCode::OK))
}
async fn delete(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let expected = expected(&headers)?;
    let item = state.db(|db| Ok(analysis::delete(db, &id, &admin.id, expected)))??;
    state.0.notify.notify_waiters();
    Ok(etag(item, StatusCode::OK))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{now, random_token, token_hash, Options};
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use base64::{engine::general_purpose::STANDARD, Engine};
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
            origin: "http://127.0.0.1:18431".into(),
            allow_private_model: true,
            cookie_secure: false,
            web_dir: temp.path().join("web"),
        })
        .unwrap();
        let token = random_token();
        let key = uuid::Uuid::new_v4().to_string();
        crate::state::private_write(&temp.path().join("secrets").join(&key), b"fixture-key")
            .unwrap();
        state.db(|db|{
            db.execute("INSERT INTO admins(id,username,password_hash,created_at) VALUES('owner','image-owner','unused',0)",[])?;
            db.execute("INSERT INTO sessions(token_hash,user_id,csrf,expires_at) VALUES(?1,'owner','image-csrf',?2)",params![token_hash(&token),now()+3600])?;
            db.execute("INSERT INTO secrets(id,kind,created_at) VALUES(?1,'model_api_key',0)",[&key])?;
            let (_,mut config)=crate::character::current_config_in(db)?;config.model.base_url="http://127.0.0.1:9".into();config.model.credential_ref=Some(key);
            db.execute("UPDATE config_revisions SET config_json=?1 WHERE revision=1",[serde_json::to_string(&config)?])?;Ok(())
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
        auth: bool,
        csrf: bool,
        tag: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value, Option<String>) {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("Host", "127.0.0.1:18431")
            .header("Origin", "http://127.0.0.1:18431")
            .header("Content-Type", "application/json");
        if auth {
            builder = builder.header("Cookie", &f.cookie);
        }
        if csrf {
            builder = builder.header("X-CSRF-Token", "image-csrf");
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
        let bytes = to_bytes(response.into_body(), 128 * 1024).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            tag,
        )
    }
    fn noisy_png() -> String {
        let mut pixels = vec![0; 256 * 256 * 4];
        let mut random = 0x9265_1467u32;
        for byte in &mut pixels {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            *byte = random as u8;
        }
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 256, 256);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_compression(png::Compression::Fast);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&pixels).unwrap();
            writer.finish().unwrap();
        }
        assert!(bytes.len() > 128 * 1024 && bytes.len() < analysis::MAX_IMAGE_BYTES);
        STANDARD.encode(bytes)
    }
    #[tokio::test]
    async fn image_api_auth_origin_csrf_upload_limit_owner_revision_and_bodyless_deletion() {
        let f = fixture();
        let path = "/api/admin/v1/image-analysis";
        assert_eq!(
            request(&f, "GET", path, false, false, None, json!({}))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        let page = request(&f, "GET", path, true, false, None, json!({})).await;
        assert_eq!(page.0, StatusCode::OK);
        assert_eq!(page.1["available"], true);
        assert_eq!(page.1["model"], "deepseek-flash");
        assert!(page.1.get("credential_ref").is_none());
        let body = json!({"client_request_id":uuid::Uuid::new_v4().to_string(),"image_base64":noisy_png(),"question":"说明合成像素"});
        let create = "/api/admin/v1/image-analysis/jobs";
        assert_eq!(
            request(&f, "POST", create, false, true, None, body.clone())
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(&f, "POST", create, true, false, None, body.clone())
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        let hostile = Request::builder()
            .method("POST")
            .uri(create)
            .header("Host", "127.0.0.1:18431")
            .header("Origin", "http://evil.example")
            .header("Cookie", &f.cookie)
            .header("X-CSRF-Token", "image-csrf")
            .header("Content-Type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        assert_eq!(
            crate::api::router(f.state.clone())
                .oneshot(hostile)
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let accepted = request(&f, "POST", create, true, true, None, body.clone()).await;
        assert_eq!(accepted.0, StatusCode::ACCEPTED);
        assert_eq!(accepted.1["status"], "queued");
        assert_eq!(accepted.2.as_deref(), Some("\"1\""));
        assert!(accepted.1.get("image_base64").is_none());
        assert!(accepted.1.get("image").is_none());
        let again = request(&f, "POST", create, true, true, None, body.clone()).await;
        assert_eq!(again.1["id"], accepted.1["id"]);
        let id = accepted.1["id"].as_str().unwrap();
        let job = format!("{create}/{id}");
        assert_eq!(
            request(&f, "DELETE", &job, true, true, None, json!({}))
                .await
                .0,
            StatusCode::PRECONDITION_REQUIRED
        );
        assert_eq!(
            request(&f, "DELETE", &job, true, true, Some("\"2\""), json!({}))
                .await
                .0,
            StatusCode::CONFLICT
        );
        let deleted = request(&f, "DELETE", &job, true, true, Some("\"1\""), json!({})).await;
        assert_eq!(deleted.0, StatusCode::OK);
        assert_eq!(deleted.1["status"], "cancelled");
        assert!(deleted.1.get("question").is_none());
        assert!(deleted.1.get("result").is_none());
        f.state
            .db(|db| {
                db.execute(
                    "UPDATE image_analysis_jobs SET owner_id='someone-else' WHERE id=?1",
                    [id],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            request(&f, "GET", &job, true, false, None, json!({}))
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            request(&f, "DELETE", &job, true, true, Some("\"2\""), json!({}))
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        let mut unknown = body.clone();
        unknown["image_url"] = json!("https://example.com/photo.png");
        assert_eq!(
            request(&f, "POST", create, true, true, None, unknown)
                .await
                .0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        let mut invalid = body;
        invalid["image_base64"] = json!("data:image/png;base64,not-accepted");
        assert_eq!(
            request(&f, "POST", create, true, true, None, invalid)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(f.state.db(|db|Ok(db.query_row("SELECT count(*) FROM image_analysis_jobs WHERE image IS NOT NULL OR question IS NOT NULL OR result IS NOT NULL",[],|r|r.get::<_,i64>(0))?)).unwrap(),0);
    }
}
