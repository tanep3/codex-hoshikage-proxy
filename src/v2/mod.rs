//! Managed conversation and immutable output services. Isolated from legacy v1 state.
pub mod files;
pub mod interactions;
pub mod store;
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug)]
pub struct Error {
    pub status: u16,
    pub code: &'static str,
}
impl Error {
    pub fn code(status: u16, code: &'static str) -> Self {
        Self { status, code }
    }
}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self::code(503, "store_unavailable")
    }
}
impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        tracing::error!(%error,"v2 SQLite operation failed");
        Self::code(503, "store_unavailable")
    }
}
impl From<serde_json::Error> for Error {
    fn from(_: serde_json::Error) -> Self {
        Self::code(503, "store_corrupt")
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let retry = match self.code {
            "capture_capacity_busy" | "download_capacity_busy" => "repeat_same_request",
            "source_changed" | "capture_timeout" => "new_operation",
            "output_not_ready" | "interaction_binding_pending" => "poll_operation",
            "store_unavailable"
            | "store_corrupt"
            | "content_corrupt"
            | "storage_capacity_exceeded"
            | "recovery_blocked"
            | "instance_mismatch"
            | "recovery_generation_mismatch" => "operator_action",
            _ => "none",
        };
        let mut response = (
            StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({
                "error":{ "code":self.code,"message":self.code,"retry":{ "action":retry} }
            })),
        )
            .into_response();
        if self.status == 429 {
            response
                .headers_mut()
                .insert("retry-after", "2".parse().unwrap());
        }
        response
    }
}
pub fn id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4())
}
pub fn now() -> u64 {
    crate::journal::now_ms() as u64
}

pub mod service;

pub mod engine;

pub mod api;

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.code)
    }
}
impl std::error::Error for Error {}

pub mod admin;

pub mod retention;

pub mod download;

pub mod coordination;

pub mod recovery;

pub mod events;

pub mod limits;

pub mod backup;

pub mod listing;

pub mod migration;

pub mod images;
