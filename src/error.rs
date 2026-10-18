use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub routing: Option<(String, String, String)>,
}
impl Error {
    pub fn new(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            code,
            message: message.into(),
            routing: None,
        }
    }
    pub fn bad(message: impl Into<String>) -> Self {
        Self::new(400, "invalid_request", message)
    }
    pub fn with_route(mut self, id: &str, model: &str, effort: &str) -> Self {
        self.routing = Some((id.into(), model.into(), effort.into()));
        self
    }
    pub fn internal(_: impl std::fmt::Display) -> Self {
        Self::new(500, "internal_error", "Internal service error")
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let mut response = (
            self.status,
            Json(json!({"error":{"code":self.code,"message":self.message,"request_id":self.routing.as_ref().map(|r|&r.0)}})),
        )
            .into_response();
        if let Some((id, model, effort)) = self.routing {
            for (name, value) in [
                ("x-request-id", id),
                ("x-jev-model", model),
                ("x-jev-effort", effort),
            ] {
                if let Ok(value) = value.parse() {
                    response.headers_mut().insert(name, value);
                }
            }
        }
        response
    }
}
pub type Result<T> = std::result::Result<T, Error>;
