//! Registration, login, logout and current-user endpoints.

use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use uuid::Uuid;

use crate::api::dto::{LoginRequest, RegisterRequest};
use crate::api::error::{ApiError, ApiResult, Json};
use crate::api::extractors::{cookie_value, AuthUser};
use crate::domain::user::{User, UserResponse};
use crate::domain::validation as rules;
use crate::security::auth::{self, COOKIE_NAME};
use crate::services::session;
use crate::state::AppState;

pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> ApiResult<Response> {
    let email = rules::normalize_email(&req.email)?;
    rules::validate_password(&req.password)?;

    let password = req.password;
    let hash = tokio::task::spawn_blocking(move || auth::hash_password(&password))
        .await
        .map_err(|e| ApiError::Internal(e.into()))??;

    let id = Uuid::new_v4().to_string();
    if let Err(e) = sqlx::query("INSERT INTO users (id, email, password_hash) VALUES (?, ?, ?)")
        .bind(&id)
        .bind(&email)
        .bind(&hash)
        .execute(&state.pool)
        .await
    {
        if let sqlx::Error::Database(db) = &e {
            if db.is_unique_violation() {
                return Err(ApiError::Conflict(
                    "An account with this email already exists".into(),
                ));
            }
        }
        return Err(e.into());
    }
    tracing::info!(user_id = %id, "user registered");

    let cookie = set_cookie(session::create_session(&state.pool, &state.config, &id).await?)?;
    Ok((
        StatusCode::CREATED,
        [(header::SET_COOKIE, cookie)],
        axum::Json(UserResponse { id, email }),
    )
        .into_response())
}

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> ApiResult<Response> {
    let email = rules::normalize_email(&req.email)?;
    let user: Option<User> =
        sqlx::query_as("SELECT id, email, password_hash FROM users WHERE email = ?")
            .bind(&email)
            .fetch_optional(&state.pool)
            .await?;
    let Some(user) = user else {
        return Err(invalid_credentials());
    };

    let password = req.password;
    let hash = user.password_hash.clone();
    let valid = tokio::task::spawn_blocking(move || auth::verify_password(&password, &hash))
        .await
        .map_err(|e| ApiError::Internal(e.into()))??;
    if !valid {
        return Err(invalid_credentials());
    }

    let cookie = set_cookie(session::create_session(&state.pool, &state.config, &user.id).await?)?;
    Ok((
        StatusCode::OK,
        [(header::SET_COOKIE, cookie)],
        axum::Json(UserResponse {
            id: user.id,
            email: user.email,
        }),
    )
        .into_response())
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    if let Some(token) = cookie_value(&headers, COOKIE_NAME) {
        session::revoke_session(&state.pool, &state.config, &token).await?;
    }
    let cookie = set_cookie(auth::clear_cookie(&state.config))?;
    Ok((StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie)]).into_response())
}

pub async fn me(user: AuthUser) -> axum::Json<UserResponse> {
    axum::Json(UserResponse::from(user.0))
}

fn invalid_credentials() -> ApiError {
    ApiError::Unauthorized("Invalid email or password".into())
}

fn set_cookie(value: String) -> ApiResult<HeaderValue> {
    HeaderValue::from_str(&value).map_err(|e| ApiError::Internal(e.into()))
}
