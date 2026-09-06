//! Unified API error type rendered as `{"error": <code>, "message": <text>}` JSON.

use axum::extract::rejection::JsonRejection;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::domain::validation::ValidationError;

#[derive(Debug)]
pub enum ApiError {
    BadRequest(String),
    Unauthorized(String),
    NotFound(String),
    Conflict(String),
    TooManyRequests { retry_after_secs: u64 },
    Internal(anyhow::Error),
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }

    pub fn unauthorized() -> Self {
        Self::Unauthorized("Authentication required".into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message, retry_after) = match self {
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, "bad_request", m, None),
            ApiError::Unauthorized(m) => (StatusCode::UNAUTHORIZED, "unauthorized", m, None),
            ApiError::NotFound(m) => (StatusCode::NOT_FOUND, "not_found", m, None),
            ApiError::Conflict(m) => (StatusCode::CONFLICT, "conflict", m, None),
            ApiError::TooManyRequests { retry_after_secs } => (
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                format!(
                    "Too many monitors created from this address. Try again in about {} minute(s), or sign in for unlimited monitors.",
                    retry_after_secs.div_ceil(60).max(1)
                ),
                Some(retry_after_secs),
            ),
            ApiError::Internal(err) => {
                tracing::error!(error = ?err, "internal server error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "Internal server error".to_string(),
                    None,
                )
            }
        };

        let mut response = (
            status,
            axum::Json(json!({ "error": code, "message": message })),
        )
            .into_response();
        if let Some(secs) = retry_after {
            if let Ok(value) = HeaderValue::from_str(&secs.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
        }
        response
    }
}

impl From<ValidationError> for ApiError {
    fn from(e: ValidationError) -> Self {
        ApiError::BadRequest(e.0)
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        ApiError::Internal(e.into())
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::Internal(e)
    }
}

impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        ApiError::BadRequest(format!("Invalid JSON body: {}", rejection.body_text()))
    }
}

/// JSON extractor that reports malformed bodies using the API's JSON error format.
#[derive(axum::extract::FromRequest)]
#[from_request(via(axum::Json), rejection(ApiError))]
pub struct Json<T>(pub T);
