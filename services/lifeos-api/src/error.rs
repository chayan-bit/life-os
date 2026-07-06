//! One error type for every handler. Maps cleanly to an HTTP status + a flat
//! `{ "error": "..." }` body, matching what the frontend's `apiCall` reads.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(Debug)]
pub enum ApiError {
    /// Bad/invalid input from the client.
    BadRequest(String),
    /// Authentication missing or invalid where it is required - e.g. strict
    /// workspace resolution (`trust_workspace_header = false`) with no
    /// verified JWT.
    Unauthorized(String),
    /// Authenticated, but the request asked for something the proven identity
    /// isn't allowed - e.g. an explicit/header workspace that disagrees with
    /// the JWT's own `workspace_id` claim.
    Forbidden(String),
    /// Resource (entity/workspace/...) does not exist.
    NotFound(String),
    /// The request lost a race - e.g. an approval already resolved by another
    /// tap (CAS matched zero rows). Distinct from BadRequest so callers can
    /// treat "already done" as benign, not a client mistake.
    Conflict(String),
    /// Route is intentionally not built yet in the base (honest, not a mock).
    NotImplemented(String),
    /// A downstream/local subprocess (e.g. an agent CLI) failed.
    Upstream(String),
    /// Database or other internal failure.
    Internal(String),
}

impl ApiError {
    fn parts(&self) -> (StatusCode, &str) {
        match self {
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            ApiError::Unauthorized(m) => (StatusCode::UNAUTHORIZED, m),
            ApiError::Forbidden(m) => (StatusCode::FORBIDDEN, m),
            ApiError::NotFound(m) => (StatusCode::NOT_FOUND, m),
            ApiError::Conflict(m) => (StatusCode::CONFLICT, m),
            ApiError::NotImplemented(m) => (StatusCode::NOT_IMPLEMENTED, m),
            ApiError::Upstream(m) => (StatusCode::BAD_GATEWAY, m),
            ApiError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, msg) = self.parts();
        // Only genuine failures are error-level. NotImplemented/4xx are expected.
        if matches!(&self, ApiError::Internal(_) | ApiError::Upstream(_)) {
            tracing::error!("{}: {}", status, msg);
        } else {
            tracing::debug!("{}: {}", status, msg);
        }
        (status, Json(json!({ "error": msg }))).into_response()
    }
}

/// Convenience: turn any libSQL error into an opaque 500. The concrete cause is
/// logged here and never travels to the client - per the "errors don't leak
/// internals" rule (docs/SECURITY.md §1). The client only ever sees a flat,
/// generic `{ "error": "internal database error" }`.
impl From<libsql::Error> for ApiError {
    fn from(e: libsql::Error) -> Self {
        tracing::error!("database error: {e}");
        ApiError::Internal("internal database error".into())
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
