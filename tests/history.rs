//! Full-window SQLite aggregation, UTC boundaries, retention, persistence and API visibility.

mod common;

use chrono::{DateTime, Duration, Utc};
use common::TestApp;
use serde_json::json;
use sqlx::SqlitePool;
use uptime_medics::domain::history::HistoryRange;
use uptime_medics::infrastructure::db;
use uptime_medics::services::{history, scheduler};

async fn insert(pool: &SqlitePool, id: &str, at: DateTime<Utc>, success: bool, latency: i64) {
    sqlx::query(
        "INSERT INTO check_logs (monitor_id, status_code, response_time_ms, is_successful, checked_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(if success { 200 } else { 503 })
    .bind(latency)
    .bind(success)
    .bind(db::to_sql(at))
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn history_aggregates_all_checks_in_each_range_and_survives_reopening_sqlite() {
    let mut app = TestApp::spawn().await;
    app.stop_scheduler().await;
    let client = app.client();
    let monitor = client
        .create_monitor(json!({"url": app.target.url("/ok")}))
        .await;
    let other = client
        .create_monitor(json!({"url": app.target.url("/ok")}))
        .await;
    let id = monitor["id"].as_str().unwrap();
    let now = Utc::now();
    // More than the 50-row detail limit, with uneven bucket sizes and latencies.
    for i in 0..80 {
        insert(
            &app.state.pool,
            id,
            now - Duration::minutes(2),
            i < 60,
            if i < 60 { 100 } else { 500 },
        )
        .await;
    }
    insert(&app.state.pool, id, now - Duration::days(2), false, 1000).await;
    insert(&app.state.pool, id, now - Duration::days(10), true, 200).await;
    insert(&app.state.pool, id, now - Duration::days(29), false, 400).await;
    insert(&app.state.pool, id, now - Duration::days(31), true, 9000).await;
    insert(&app.state.pool, id, now + Duration::days(1), false, 9000).await;
    insert(
        &app.state.pool,
        other["id"].as_str().unwrap(),
        now - Duration::minutes(2),
        false,
        9000,
    )
    .await;

    for (range, total, successful, latency_sum, buckets) in [
        ("24h", 80, 60, 16000.0, 24),
        ("7d", 81, 60, 17000.0, 28),
        ("30d", 83, 61, 17600.0, 30),
    ] {
        let data = client
            .get(&format!("/api/monitors/{id}/history?range={range}"))
            .await
            .assert_status(200)
            .json();
        assert_eq!(data["range"], range);
        assert_eq!(data["summary"]["total_checks"], total);
        assert_eq!(data["summary"]["successful_checks"], successful);
        assert_eq!(data["summary"]["failed_checks"], total - successful);
        assert!(
            (data["summary"]["uptime_percent"].as_f64().unwrap()
                - 100.0 * successful as f64 / total as f64)
                .abs()
                < 0.0001
        );
        assert!(
            (data["summary"]["avg_response_time_ms"].as_f64().unwrap()
                - latency_sum / total as f64)
                .abs()
                < 0.0001
        );
        assert_eq!(data["buckets"].as_array().unwrap().len(), buckets);
        assert_eq!(data["recent_checks"].as_array().unwrap().len(), 50);
        assert_eq!(data["retention_hours"], 720);
        let count: i64 = data["buckets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["total_checks"].as_i64().unwrap())
            .sum();
        assert_eq!(count, total);
    }

    // Reopen the file after closing the original pool: history is durable, not browser state.
    app.state.pool.close().await;
    let reopened = db::init(&app.state.config.database_url).await.unwrap();
    let data = history::load(&reopened, id, HistoryRange::Month, now, Some(720))
        .await
        .unwrap();
    assert_eq!(data.summary.total_checks, 83);
    reopened.close().await;
    app.finish().await;
}

#[tokio::test]
async fn utc_boundaries_are_counted_once_and_empty_buckets_have_no_uptime() {
    let mut app = TestApp::spawn().await;
    app.stop_scheduler().await;
    let monitor = app
        .client()
        .create_monitor(json!({"url": app.target.url("/ok")}))
        .await;
    let id = monitor["id"].as_str().unwrap();
    // Non-midnight UTC endpoint also covers months of different lengths and a DST transition.
    let now = DateTime::parse_from_rfc3339("2026-03-09T04:17:23Z")
        .unwrap()
        .with_timezone(&Utc);
    for range in [HistoryRange::Day, HistoryRange::Week, HistoryRange::Month] {
        sqlx::query("DELETE FROM check_logs WHERE monitor_id = ?")
            .bind(id)
            .execute(&app.state.pool)
            .await
            .unwrap();
        let start = now - Duration::seconds(range.seconds());
        let first_end = start + Duration::seconds(range.bucket_seconds());
        insert(&app.state.pool, id, start, false, 9000).await; // outside (start, end]
        insert(&app.state.pool, id, start + Duration::seconds(1), true, 100).await;
        insert(&app.state.pool, id, first_end, false, 300).await;
        insert(
            &app.state.pool,
            id,
            first_end + Duration::seconds(1),
            true,
            200,
        )
        .await;
        insert(&app.state.pool, id, now, true, 200).await;
        insert(&app.state.pool, id, now + Duration::seconds(1), false, 9000).await;
        let data = history::load(&app.state.pool, id, range, now, Some(720))
            .await
            .unwrap();
        assert_eq!(data.start, start);
        assert_eq!(data.end, now);
        assert_eq!(data.summary.total_checks, 4);
        assert_eq!(data.summary.uptime_percent, Some(75.0));
        assert_eq!(data.summary.avg_response_time_ms, Some(200.0));
        assert_eq!(
            data.summary.first_checked_at,
            Some(start + Duration::seconds(1))
        );
        assert_eq!(data.summary.last_checked_at, Some(now));
        assert_eq!(data.buckets[0].summary.total_checks, 2);
        assert_eq!(data.buckets[0].summary.uptime_percent, Some(50.0));
        assert_eq!(data.buckets[1].summary.total_checks, 1);
        assert_eq!(data.buckets[2].summary.total_checks, 0);
        assert_eq!(data.buckets[2].summary.uptime_percent, None);
        assert_eq!(data.buckets[2].summary.avg_response_time_ms, None);
        assert_eq!(data.buckets.last().unwrap().summary.total_checks, 1);
        assert_eq!(data.recent_checks.len(), 4);
        assert_eq!(data.recent_checks[0].checked_at, now);
    }
    app.finish().await;
}

#[tokio::test]
async fn history_api_handles_empty_invalid_and_private_requests() {
    let mut app = TestApp::spawn_with(|config| {
        config.anon_retention_hours = 48;
        config.registered_retention_days = 0;
    })
    .await;
    app.stop_scheduler().await;
    let anon = app.client();
    let public = anon
        .create_monitor(json!({"url": app.target.url("/ok")}))
        .await;
    let id = public["id"].as_str().unwrap();
    let data = anon
        .get(&format!("/api/monitors/{id}/history"))
        .await
        .assert_status(200)
        .json();
    assert_eq!(data["range"], "24h");
    assert_eq!(data["summary"]["total_checks"], 0);
    assert!(data["summary"]["uptime_percent"].is_null());
    assert!(data["summary"]["avg_response_time_ms"].is_null());
    assert!(data["summary"]["first_checked_at"].is_null());
    assert!(data["recent_checks"].as_array().unwrap().is_empty());
    assert!(data["buckets"]
        .as_array()
        .unwrap()
        .iter()
        .all(|b| b["uptime_percent"].is_null()));
    assert_eq!(data["retention_hours"], 48);
    for query in ["range=1y", "range=", "range=24h&range=7d"] {
        let response = anon
            .get(&format!("/api/monitors/{id}/history?{query}"))
            .await
            .assert_status(400)
            .json();
        assert_eq!(response["error"], "bad_request");
    }
    anon.get("/api/monitors/missing/history")
        .await
        .assert_status(404);

    let owner = app.client();
    owner
        .register("history-owner@example.com", "correct horse battery")
        .await
        .assert_status(201);
    let private = owner
        .create_monitor(json!({"url": app.target.url("/ok")}))
        .await;
    let private_path = format!(
        "/api/monitors/{}/history?range=30d",
        private["id"].as_str().unwrap()
    );
    let data = owner.get(&private_path).await.assert_status(200).json();
    assert!(data["retention_hours"].is_null());
    anon.get(&private_path).await.assert_status(404);
    let stranger = app.client();
    stranger
        .register("history-stranger@example.com", "correct horse battery")
        .await
        .assert_status(201);
    stranger.get(&private_path).await.assert_status(404);
    stranger
        .get(&format!("/api/monitors/{id}/history?range=7d"))
        .await
        .assert_status(200);

    let private_id = private["id"].as_str().unwrap();
    insert(
        &app.state.pool,
        private_id,
        Utc::now() - Duration::minutes(1),
        true,
        100,
    )
    .await;
    owner
        .delete(&format!("/api/monitors/{private_id}"))
        .await
        .assert_status(204);
    owner.get(&private_path).await.assert_status(404);
    assert_eq!(app.log_count(private_id).await, 0);
    app.finish().await;
}

#[tokio::test]
async fn default_cleanup_preserves_thirty_days_of_public_and_private_history() {
    let mut app = TestApp::spawn().await;
    app.stop_scheduler().await;
    let anon = app.client();
    let public = anon
        .create_monitor(json!({"url": app.target.url("/ok")}))
        .await;
    let owner = app.client();
    owner
        .register("retention@example.com", "correct horse battery")
        .await
        .assert_status(201);
    let private = owner
        .create_monitor(json!({"url": app.target.url("/ok")}))
        .await;
    for monitor in [&public, &private] {
        let id = monitor["id"].as_str().unwrap();
        for days in [1, 7, 29, 31] {
            insert(
                &app.state.pool,
                id,
                Utc::now() - Duration::days(days),
                true,
                100,
            )
            .await;
        }
    }
    scheduler::run_cleanup(&app.state).await;
    for monitor in [&public, &private] {
        let id = monitor["id"].as_str().unwrap();
        assert_eq!(app.log_count(id).await, 3);
        let data = owner
            .get(&format!("/api/monitors/{id}/history?range=30d"))
            .await
            .assert_status(200)
            .json();
        assert_eq!(data["summary"]["total_checks"], 3);
    }
    app.finish().await;
}
