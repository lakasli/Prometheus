//! Local speech management; global Admin / Origin / CSRF guard applies.
use crate::{
    auth::Admin,
    error::{ApiError, ApiResult},
    speech,
    state::AppState,
};
use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/v1/speech", get(status))
        .route("/api/admin/v1/speech/syntheses", post(synthesize))
        .route(
            "/api/admin/v1/speech/assets/{id}",
            get(audio).delete(delete),
        )
}
async fn status(State(state): State<AppState>, _admin: Admin) -> ApiResult<Json<Value>> {
    let (_, config) = state.current_config()?;
    state.db(|db| {
        speech::prune_in(db)?;
        Ok(())
    })?;
    Ok(Json(speech::status(&state, &config.speech)))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    text: String,
}
async fn synthesize(
    State(state): State<AppState>,
    admin: Admin,
    Json(body): Json<Body>,
) -> ApiResult<Response> {
    let (_, config) = state.current_config()?;
    let asset = speech::synthesize(&state, &config.speech, &admin.id, None, &body.text).await?;
    state.audit(&admin.id, "speech.synthesize", &asset.id)?;
    Ok((StatusCode::CREATED, Json(asset)).into_response())
}
async fn audio(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let (asset, bytes) = speech::read_asset(&state, &admin.id, &id)?;
    let mut response = (StatusCode::OK, bytes).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, "audio/wav".parse().unwrap());
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store, private".parse().unwrap());
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        "inline; filename=\"speech.wav\"".parse().unwrap(),
    );
    response
        .headers_mut()
        .insert("X-Content-Type-Options", "nosniff".parse().unwrap());
    response.headers_mut().insert(
        header::ETAG,
        format!("\"{}\"", asset.sha256)
            .parse()
            .map_err(|_| ApiError::internal())?,
    );
    Ok(response)
}
async fn delete(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    speech::delete_asset(&state, &admin.id, &id)?;
    state.audit(&admin.id, "speech.delete", &id)?;
    Ok(Json(json!({"deleted":true})))
}
