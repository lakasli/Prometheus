use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

pub type ApiResult<T> = Result<T, ApiError>;
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}
impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
    pub fn bad(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }
    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", "请先登录")
    }
    pub fn forbidden(message: &str) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "资源不存在")
    }
    pub fn conflict() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "revision_conflict",
            "配置已被修改，请重新加载后合并",
        )
    }
    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "内部操作失败，请检查服务诊断",
        )
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status,Json(json!({"error":{"code":self.code,"message":self.message,"request_id":uuid::Uuid::new_v4().to_string()}}))).into_response()
    }
}
