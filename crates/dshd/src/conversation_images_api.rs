use crate::{
    auth::Admin,
    conversation_images as images,
    error::{ApiError, ApiResult},
    image_analysis,
    state::AppState,
};
use axum::{
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/admin/v1/conversation-images/capabilities",
            get(capabilities),
        )
        .route(
            "/api/admin/v1/conversations/{id}/image-messages",
            post(send).layer(DefaultBodyLimit::max(1_500_000)),
        )
}
async fn capabilities(
    State(state): State<AppState>,
    _admin: Admin,
) -> ApiResult<Json<images::Capabilities>> {
    Ok(Json(images::capabilities(&state)?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageBody {
    content: String,
    client_message_id: String,
    image_base64: String,
}
async fn send(
    State(state): State<AppState>,
    admin: Admin,
    Path(id): Path<String>,
    Json(body): Json<ImageBody>,
) -> ApiResult<Response> {
    state.db(|db| Ok(images::owned(db, &id, &admin.id)))??;
    images::validate_input(&body.client_message_id, &body.content)?;
    if body.image_base64.starts_with("data:") {
        return Err(ApiError::bad(
            "invalid_image_base64",
            "图片必须使用不含data前缀的原始Base64",
        ));
    }
    let permit = crate::image_analysis_api::DECODE_SLOTS
        .try_acquire()
        .map_err(|_| {
            ApiError::new(
                StatusCode::CONFLICT,
                "image_analysis_busy",
                "图片校验正在进行，请稍后再试",
            )
        })?;
    let image = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        image_analysis::canonicalize(&body.image_base64)
    })
    .await
    .map_err(|_| ApiError::internal())??;
    let run = images::enqueue(
        &state,
        &id,
        &admin.id,
        &body.client_message_id,
        &body.content,
        image,
    )?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"run_id":run,"conversation_id":id})),
    )
        .into_response())
}
