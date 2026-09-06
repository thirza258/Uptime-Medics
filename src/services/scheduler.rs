//! Background work: the probe loop, batched check-log writer, and daily cleanup.

use std::time::Duration;

use sqlx::SqlitePool;
use tokio::sync::{mpsc, watch};
use tokio::time::MissedTickBehavior;

use crate::domain::check_log::CheckLogInsert;
use crate::domain::incident::{self, Transition};
use crate::domain::monitor::Monitor;
use crate::infrastructure::db::now_sql;
use crate::security::ssrf;
use crate::services::probe::{self, ProbeResult, ProbeSpec};
use crate::state::AppState;

pub async fn run(
    state: AppState,
    log_rx: mpsc::Receiver<CheckLogInsert>,
    mut shutdown: watch::Receiver<bool>,
) {
    let writer = tokio::spawn(log_writer(state.clone(), log_rx, shutdown.clone()));
    let cleanup = tokio::spawn(cleanup_loop(state.clone(), shutdown.clone()));

    let mut tick = tokio::time::interval(state.config.scheduler_tick);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    tracing::info!(
        tick_ms = state.config.scheduler_tick.as_millis(),
        concurrency = state.config.probe_concurrency,
        "probe scheduler started"
    );

    loop {
        tokio::select! {
            _ = tick.tick() => {
                if let Err(e) = dispatch_due(&state).await {
                    tracing::error!(error = %e, "failed to load due monitors");
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }

    cleanup.abort();
    if let Err(e) = writer.await {
        tracing::error!(error = %e, "log writer task failed");
    }
    tracing::info!("probe scheduler stopped");
}

async fn dispatch_due(state: &AppState) -> Result<(), sqlx::Error> {
    let due: Vec<Monitor> = sqlx::query_as(
        "SELECT * FROM monitors \
         WHERE is_active = 1 \
           AND (last_checked_at IS NULL \
                OR (strftime('%s', 'now') - strftime('%s', last_checked_at)) >= check_interval_seconds) \
         ORDER BY last_checked_at ASC",
    )
    .fetch_all(&state.pool)
    .await?;

    for monitor in due {
        {
            let mut in_flight = state.in_flight.lock().unwrap_or_else(|e| e.into_inner());
            if !in_flight.insert(monitor.id.clone()) {
                continue; // already being probed
            }
        }
        let state = state.clone();
        tokio::spawn(async move {
            let id = monitor.id.clone();
            run_probe(&state, monitor).await;
            state
                .in_flight
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
        });
    }
    Ok(())
}

async fn run_probe(state: &AppState, monitor: Monitor) {
    let _permit = match state.probe_semaphore.clone().acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => return,
    };

    let outcome = if !state.config.allow_private_targets {
        match ssrf::validate_target_url(&monitor.url, false) {
            Ok(_) => probe_monitor(state, &monitor).await,
            Err(msg) => ProbeResult::failure(0, None, format!("Blocked: {msg}")),
        }
    } else {
        probe_monitor(state, &monitor).await
    };

    handle_outcome(state, monitor, outcome).await;
}

async fn probe_monitor(state: &AppState, monitor: &Monitor) -> ProbeResult {
    let spec = ProbeSpec {
        url: &monitor.url,
        method: &monitor.method,
        headers_json: monitor.headers.as_deref(),
        body: monitor.body.as_deref(),
        timeout_secs: monitor.timeout_seconds.clamp(1, 60) as u64,
        expected_status: monitor.expected_status_code.clamp(100, 599) as u16,
    };
    let client = state.http_client_for(monitor.expected_status_code);
    probe::execute_probe(client, &spec).await
}

async fn handle_outcome(state: &AppState, mut monitor: Monitor, result: ProbeResult) {
    let checked_at = now_sql();
    let evaluation = incident::evaluate(
        &monitor.current_status,
        monitor.consecutive_failures,
        result.is_successful,
    );

    let updated = sqlx::query(
        "UPDATE monitors SET current_status = ?, consecutive_failures = ?, last_checked_at = ? WHERE id = ?",
    )
    .bind(&evaluation.status)
    .bind(evaluation.consecutive_failures)
    .bind(&checked_at)
    .bind(&monitor.id)
    .execute(&state.pool)
    .await;

    match updated {
        Ok(done) if done.rows_affected() == 0 => {
            tracing::debug!(monitor_id = %monitor.id, "monitor deleted during probe; result discarded");
            return;
        }
        Ok(_) => {}
        Err(e) => {
            tracing::error!(monitor_id = %monitor.id, error = %e, "failed to update monitor status");
        }
    }

    tracing::debug!(
        monitor_id = %monitor.id,
        method = %monitor.method,
        url = %monitor.url,
        ok = result.is_successful,
        status_code = ?result.status_code,
        ms = result.response_time_ms,
        error = ?result.error_message,
        "probe finished"
    );

    let row = CheckLogInsert {
        monitor_id: monitor.id.clone(),
        status_code: result.status_code.map(i64::from),
        response_time_ms: result.response_time_ms as i64,
        is_successful: result.is_successful,
        error_message: result.error_message.clone(),
        checked_at,
    };
    if let Err(e) = state.log_tx.send(row).await {
        tracing::error!(error = %e, "check log channel closed");
    }

    if let Some(transition) = evaluation.transition {
        monitor.current_status = evaluation.status;
        monitor.consecutive_failures = evaluation.consecutive_failures;
        let reason = match transition {
            Transition::Down => result
                .error_message
                .clone()
                .unwrap_or_else(|| "Unknown failure".to_string()),
            Transition::Recovered => format!(
                "Status {} in {} ms",
                result.status_code.unwrap_or(0),
                result.response_time_ms
            ),
        };
        tracing::info!(monitor_id = %monitor.id, name = %monitor.name, ?transition, %reason, "status transition");
        state.alerts.notify(monitor, transition, reason);
    }
}

/// Buffers check logs and writes them in batch transactions (up to `LOG_BATCH_SIZE` rows or
/// every `LOG_FLUSH_INTERVAL_SECONDS`, whichever comes first).
async fn log_writer(
    state: AppState,
    mut rx: mpsc::Receiver<CheckLogInsert>,
    mut shutdown: watch::Receiver<bool>,
) {
    let batch_size = state.config.log_batch_size;
    let mut buffer: Vec<CheckLogInsert> = Vec::with_capacity(batch_size);
    let mut flush_tick = tokio::time::interval(state.config.log_flush_interval);
    flush_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Some(row) => {
                    buffer.push(row);
                    if buffer.len() >= batch_size {
                        flush(&state.pool, &mut buffer).await;
                    }
                }
                None => break,
            },
            _ = flush_tick.tick() => {
                if !buffer.is_empty() {
                    flush(&state.pool, &mut buffer).await;
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }

    while let Ok(row) = rx.try_recv() {
        buffer.push(row);
    }
    flush(&state.pool, &mut buffer).await;
}

async fn flush(pool: &SqlitePool, buffer: &mut Vec<CheckLogInsert>) {
    if buffer.is_empty() {
        return;
    }
    let rows = std::mem::take(buffer);
    let total = rows.len();
    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            tracing::error!(error = %e, dropped = total, "could not begin check_logs transaction");
            return;
        }
    };
    let mut inserted = 0u64;
    for row in &rows {
        // INSERT ... SELECT ... WHERE EXISTS skips rows whose monitor was deleted mid-probe
        // instead of failing the whole batch on the foreign key.
        let result = sqlx::query(
            "INSERT INTO check_logs (monitor_id, status_code, response_time_ms, is_successful, error_message, checked_at) \
             SELECT ?1, ?2, ?3, ?4, ?5, ?6 WHERE EXISTS (SELECT 1 FROM monitors WHERE id = ?1)",
        )
        .bind(&row.monitor_id)
        .bind(row.status_code)
        .bind(row.response_time_ms)
        .bind(row.is_successful)
        .bind(&row.error_message)
        .bind(&row.checked_at)
        .execute(&mut *tx)
        .await;
        match result {
            Ok(done) => inserted += done.rows_affected(),
            Err(e) => {
                tracing::warn!(error = %e, monitor_id = %row.monitor_id, "failed to insert check log")
            }
        }
    }
    match tx.commit().await {
        Ok(()) => tracing::debug!(inserted, total, "flushed check logs"),
        Err(e) => tracing::error!(error = %e, dropped = total, "failed to commit check logs"),
    }
}

/// Daily maintenance loop (first run 30 s after start-up); see [`run_cleanup`].
async fn cleanup_loop(state: AppState, mut shutdown: watch::Receiver<bool>) {
    let mut next = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut prune_tick = tokio::time::interval(Duration::from_secs(10 * 60));
    prune_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(next) => {
                run_cleanup(&state).await;
                next = tokio::time::Instant::now() + Duration::from_secs(24 * 3600);
            }
            _ = prune_tick.tick() => {
                state.rate_limiter.prune();
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }
}

/// Anonymous log retention (48 h by default), registered log retention, expired sessions and
/// `PRAGMA optimize`.
pub async fn run_cleanup(state: &AppState) {
    let pool = &state.pool;

    let anon_modifier = format!("-{} hours", state.config.anon_retention_hours.max(1));
    match sqlx::query(
        "DELETE FROM check_logs WHERE monitor_id IN (SELECT id FROM monitors WHERE is_anonymous = 1) \
         AND checked_at < datetime('now', ?)",
    )
    .bind(&anon_modifier)
    .execute(pool)
    .await
    {
        Ok(done) => tracing::info!(deleted = done.rows_affected(), retention = %anon_modifier, "pruned anonymous check logs"),
        Err(e) => tracing::error!(error = %e, "failed to prune anonymous check logs"),
    }

    if state.config.registered_retention_days > 0 {
        let modifier = format!("-{} days", state.config.registered_retention_days);
        match sqlx::query(
            "DELETE FROM check_logs WHERE monitor_id IN (SELECT id FROM monitors WHERE is_anonymous = 0) \
             AND checked_at < datetime('now', ?)",
        )
        .bind(&modifier)
        .execute(pool)
        .await
        {
            Ok(done) => tracing::info!(deleted = done.rows_affected(), retention = %modifier, "pruned registered check logs"),
            Err(e) => tracing::error!(error = %e, "failed to prune registered check logs"),
        }
    }

    match sqlx::query("DELETE FROM sessions WHERE expires_at < datetime('now')")
        .execute(pool)
        .await
    {
        Ok(done) => tracing::debug!(deleted = done.rows_affected(), "pruned expired sessions"),
        Err(e) => tracing::error!(error = %e, "failed to prune sessions"),
    }

    if let Err(e) = sqlx::query("PRAGMA optimize").execute(pool).await {
        tracing::warn!(error = %e, "PRAGMA optimize failed");
    }
}
