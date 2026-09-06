//! Monitor CRUD and check-log retrieval.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use url::Url;
use uuid::Uuid;

use crate::api::dto::{CreateMonitorRequest, UpdateMonitorRequest};
use crate::api::error::{ApiError, ApiResult, Json};
use crate::api::extractors::{client_ip, AuthUser, OptionalAuthUser};
use crate::domain::check_log::CheckLog;
use crate::domain::monitor::{Monitor, MonitorWithLatest, STATUS_PENDING};
use crate::domain::user::CurrentUser;
use crate::domain::validation as rules;
use crate::infrastructure::db::to_sql;
use crate::security::ssrf;
use crate::state::AppState;

const LOG_PAGE_SIZE: i64 = 50;

pub async fn create(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    OptionalAuthUser(user): OptionalAuthUser,
    Json(req): Json<CreateMonitorRequest>,
) -> ApiResult<(StatusCode, axum::Json<MonitorWithLatest>)> {
    let allow_private = state.config.allow_private_targets;
    let url = ssrf::validate_target_url(&req.url, allow_private).map_err(ApiError::BadRequest)?;
    let method = rules::normalize_method(req.method.as_deref())?;
    let name = rules::normalize_name(req.name.as_deref(), &url)?;
    let headers_json = rules::normalize_headers(req.headers.as_ref())?;
    let body = rules::normalize_body(req.body)?;
    let interval = rules::validate_interval(req.check_interval_seconds.unwrap_or(60))?;
    let timeout = rules::validate_timeout(req.timeout_seconds.unwrap_or(10))?;
    let expected = rules::validate_status(req.expected_status_code.unwrap_or(200))?;
    let alert_email = rules::normalize_email_opt(req.alert_email.as_deref())?;

    if user.is_none() {
        let ip = client_ip(&headers, addr, state.config.trust_proxy_headers);
        state
            .rate_limiter
            .try_acquire(ip)
            .map_err(|retry_after_secs| ApiError::TooManyRequests { retry_after_secs })?;
    }
    if !allow_private {
        ssrf::check_resolves_public(&url)
            .await
            .map_err(ApiError::BadRequest)?;
    }

    let id = Uuid::new_v4().to_string();
    let (user_id, is_anonymous) = match &user {
        Some(u) => (Some(u.id.clone()), false),
        None => (None, true),
    };
    sqlx::query(
        "INSERT INTO monitors (id, user_id, name, url, method, headers, body, check_interval_seconds, \
         timeout_seconds, expected_status_code, alert_email, is_anonymous, is_active, current_status, consecutive_failures) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, 0)",
    )
    .bind(&id)
    .bind(&user_id)
    .bind(&name)
    .bind(url.as_str())
    .bind(&method)
    .bind(&headers_json)
    .bind(&body)
    .bind(interval)
    .bind(timeout)
    .bind(expected)
    .bind(&alert_email)
    .bind(is_anonymous)
    .bind(STATUS_PENDING)
    .execute(&state.pool)
    .await?;
    tracing::info!(monitor_id = %id, %method, url = %url, anonymous = is_anonymous, "monitor created");

    let monitor = fetch_with_latest(&state, &id)
        .await?
        .ok_or_else(not_found)?;
    Ok((StatusCode::CREATED, axum::Json(monitor)))
}

pub async fn list_mine(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<axum::Json<Vec<MonitorWithLatest>>> {
    let sql = select_with_latest("WHERE m.user_id = ? ORDER BY m.created_at DESC, m.rowid DESC");
    let rows = sqlx::query_as::<_, MonitorWithLatest>(&sql)
        .bind(&user.id)
        .fetch_all(&state.pool)
        .await?;
    Ok(axum::Json(rows))
}

pub async fn get_one(
    State(state): State<AppState>,
    Path(id): Path<String>,
    OptionalAuthUser(user): OptionalAuthUser,
) -> ApiResult<axum::Json<MonitorWithLatest>> {
    let monitor = fetch_with_latest(&state, &id)
        .await?
        .ok_or_else(not_found)?;
    match visibility(&monitor.monitor, user.as_ref()) {
        Visibility::Owner => Ok(axum::Json(monitor)),
        Visibility::Public => Ok(axum::Json(monitor.redact())),
        Visibility::Hidden => Err(not_found()),
    }
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: AuthUser,
    Json(req): Json<UpdateMonitorRequest>,
) -> ApiResult<axum::Json<MonitorWithLatest>> {
    let allow_private = state.config.allow_private_targets;
    let existing: Option<Monitor> = sqlx::query_as("SELECT * FROM monitors WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?;
    let Some(existing) = existing.filter(|m| m.is_owned_by(&user.id)) else {
        return Err(not_found());
    };

    let mut m = existing.clone();
    if let Some(url) = req.url.as_deref() {
        let parsed = ssrf::validate_target_url(url, allow_private).map_err(ApiError::BadRequest)?;
        if !allow_private {
            ssrf::check_resolves_public(&parsed)
                .await
                .map_err(ApiError::BadRequest)?;
        }
        m.url = parsed.to_string();
    }
    if let Some(method) = req.method.as_deref() {
        m.method = rules::normalize_method(Some(method))?;
    }
    if let Some(name) = req.name.as_deref() {
        let url = Url::parse(&m.url).map_err(|e| ApiError::Internal(e.into()))?;
        m.name = rules::normalize_name(Some(name), &url)?;
    }
    if let Some(headers) = &req.headers {
        m.headers = rules::normalize_headers(headers.as_ref())?;
    }
    if let Some(body) = req.body {
        m.body = rules::normalize_body(body)?;
    }
    if let Some(interval) = req.check_interval_seconds {
        m.check_interval_seconds = rules::validate_interval(interval)?;
    }
    if let Some(timeout) = req.timeout_seconds {
        m.timeout_seconds = rules::validate_timeout(timeout)?;
    }
    if let Some(status) = req.expected_status_code {
        m.expected_status_code = rules::validate_status(status)?;
    }
    if let Some(email) = &req.alert_email {
        m.alert_email = rules::normalize_email_opt(email.as_deref())?;
    }
    if let Some(active) = req.is_active {
        m.is_active = active;
    }

    let target_changed = m.url != existing.url
        || m.method != existing.method
        || m.headers != existing.headers
        || m.body != existing.body
        || m.expected_status_code != existing.expected_status_code;
    if target_changed {
        // The definition of "up" changed: reset the failure streak and probe right away. The
        // current status is kept so a DOWN monitor still gets a recovery email once the (new)
        // target answers correctly.
        m.consecutive_failures = 0;
        m.last_checked_at = None;
    }

    sqlx::query(
        "UPDATE monitors SET name = ?, url = ?, method = ?, headers = ?, body = ?, check_interval_seconds = ?, \
         timeout_seconds = ?, expected_status_code = ?, alert_email = ?, is_active = ?, current_status = ?, \
         consecutive_failures = ?, last_checked_at = ? WHERE id = ? AND user_id = ?",
    )
    .bind(&m.name)
    .bind(&m.url)
    .bind(&m.method)
    .bind(&m.headers)
    .bind(&m.body)
    .bind(m.check_interval_seconds)
    .bind(m.timeout_seconds)
    .bind(m.expected_status_code)
    .bind(&m.alert_email)
    .bind(m.is_active)
    .bind(&m.current_status)
    .bind(m.consecutive_failures)
    .bind(m.last_checked_at.map(to_sql))
    .bind(&id)
    .bind(&user.id)
    .execute(&state.pool)
    .await?;
    tracing::info!(monitor_id = %id, target_changed, active = m.is_active, "monitor updated");

    let updated = fetch_with_latest(&state, &id)
        .await?
        .ok_or_else(not_found)?;
    Ok(axum::Json(updated))
}

pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: AuthUser,
) -> ApiResult<StatusCode> {
    let done = sqlx::query("DELETE FROM monitors WHERE id = ? AND user_id = ?")
        .bind(&id)
        .bind(&user.id)
        .execute(&state.pool)
        .await?;
    if done.rows_affected() == 0 {
        return Err(not_found());
    }
    tracing::info!(monitor_id = %id, "monitor deleted");
    Ok(StatusCode::NO_CONTENT)
}

pub async fn logs(
    State(state): State<AppState>,
    Path(id): Path<String>,
    OptionalAuthUser(user): OptionalAuthUser,
) -> ApiResult<axum::Json<Vec<CheckLog>>> {
    let monitor: Option<Monitor> = sqlx::query_as("SELECT * FROM monitors WHERE id = ?")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?;
    let Some(monitor) = monitor else {
        return Err(not_found());
    };
    if matches!(visibility(&monitor, user.as_ref()), Visibility::Hidden) {
        return Err(not_found());
    }
    let logs = sqlx::query_as::<_, CheckLog>(
        "SELECT id, monitor_id, status_code, response_time_ms, is_successful, error_message, checked_at \
         FROM check_logs WHERE monitor_id = ? ORDER BY checked_at DESC LIMIT ?",
    )
    .bind(&id)
    .bind(LOG_PAGE_SIZE)
    .fetch_all(&state.pool)
    .await?;
    Ok(axum::Json(logs))
}

// ---------------------------------------------------------------------------------------------
// helpers

enum Visibility {
    Owner,
    Public,
    Hidden,
}

fn visibility(monitor: &Monitor, user: Option<&CurrentUser>) -> Visibility {
    if let Some(u) = user {
        if monitor.is_owned_by(&u.id) {
            return Visibility::Owner;
        }
    }
    if monitor.is_anonymous {
        Visibility::Public
    } else {
        Visibility::Hidden
    }
}

fn not_found() -> ApiError {
    ApiError::not_found("Monitor not found")
}

fn select_with_latest(suffix: &str) -> String {
    format!(
        "SELECT m.*, \
         (SELECT c.response_time_ms FROM check_logs c WHERE c.monitor_id = m.id ORDER BY c.checked_at DESC LIMIT 1) AS response_time_ms, \
         (SELECT c.error_message FROM check_logs c WHERE c.monitor_id = m.id ORDER BY c.checked_at DESC LIMIT 1) AS last_error \
         FROM monitors m {suffix}"
    )
}

async fn fetch_with_latest(state: &AppState, id: &str) -> ApiResult<Option<MonitorWithLatest>> {
    let sql = select_with_latest("WHERE m.id = ?");
    Ok(sqlx::query_as::<_, MonitorWithLatest>(&sql)
        .bind(id)
        .fetch_optional(&state.pool)
        .await?)
}
