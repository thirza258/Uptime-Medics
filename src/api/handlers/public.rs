//! Unauthenticated endpoints: health, the "10 random juggle" pool and landing-page stats.

use std::time::{Duration, Instant};

use axum::extract::State;

use crate::api::error::ApiResult;
use crate::domain::monitor::RandomMonitor;
use crate::domain::stats::StatsResponse;
use crate::state::AppState;

const STATS_CACHE_TTL: Duration = Duration::from_secs(10);

pub async fn health(State(state): State<AppState>) -> ApiResult<axum::Json<serde_json::Value>> {
    sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&state.pool)
        .await?;
    Ok(axum::Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_seconds": state.started_at.elapsed().as_secs(),
        "alerting_enabled": state.alerts.enabled(),
    })))
}

pub async fn random_monitors(
    State(state): State<AppState>,
) -> ApiResult<axum::Json<Vec<RandomMonitor>>> {
    let rows = sqlx::query_as::<_, RandomMonitor>(
        "SELECT m.id, m.name, m.url, m.method, m.current_status, m.last_checked_at, \
         (SELECT c.response_time_ms FROM check_logs c WHERE c.monitor_id = m.id ORDER BY c.checked_at DESC LIMIT 1) AS response_time_ms, \
         (SELECT c.error_message FROM check_logs c WHERE c.monitor_id = m.id ORDER BY c.checked_at DESC LIMIT 1) AS last_error \
         FROM monitors m WHERE m.is_anonymous = 1 AND m.is_active = 1 ORDER BY RANDOM() LIMIT 10",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(axum::Json(rows))
}

pub async fn stats(State(state): State<AppState>) -> ApiResult<axum::Json<StatsResponse>> {
    let cached = state
        .stats_cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if let Some((at, stats)) = cached {
        if at.elapsed() < STATS_CACHE_TTL {
            return Ok(axum::Json(stats));
        }
    }

    let (total_monitors, public_monitors, up, down, pending): (i64, i64, i64, i64, i64) =
        sqlx::query_as(
            "SELECT COUNT(*), \
             COALESCE(SUM(CASE WHEN is_anonymous = 1 AND is_active = 1 THEN 1 ELSE 0 END), 0), \
             COALESCE(SUM(CASE WHEN is_active = 1 AND current_status = 'UP' THEN 1 ELSE 0 END), 0), \
             COALESCE(SUM(CASE WHEN is_active = 1 AND current_status = 'DOWN' THEN 1 ELSE 0 END), 0), \
             COALESCE(SUM(CASE WHEN is_active = 1 AND current_status = 'PENDING' THEN 1 ELSE 0 END), 0) \
             FROM monitors",
        )
        .fetch_one(&state.pool)
        .await?;
    let total_checks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM check_logs")
        .fetch_one(&state.pool)
        .await?;

    let stats = StatsResponse {
        total_monitors,
        public_monitors,
        up,
        down,
        pending,
        total_checks,
        alerting_enabled: state.alerts.enabled(),
    };
    *state.stats_cache.lock().unwrap_or_else(|e| e.into_inner()) =
        Some((Instant::now(), stats.clone()));
    Ok(axum::Json(stats))
}
