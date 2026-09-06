//! Unauthenticated endpoints: health, the landing page, the random pool and stats.

mod common;

use std::collections::HashSet;

use common::TestApp;
use serde_json::json;

#[tokio::test]
async fn health_reports_status_and_alerting() {
    let app = TestApp::spawn().await;
    let body = app
        .client()
        .get("/api/health")
        .await
        .assert_status(200)
        .json();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["alerting_enabled"], true);
    assert!(body["version"].is_string());
    assert!(body["uptime_seconds"].is_number());
    app.finish().await;

    let quiet = TestApp::spawn_with(|c| c.smtp = None).await;
    let body = quiet
        .client()
        .get("/api/health")
        .await
        .assert_status(200)
        .json();
    assert_eq!(body["alerting_enabled"], false);
    quiet.finish().await;
}

#[tokio::test]
async fn security_headers_are_set_on_pages_and_api_responses() {
    let app = TestApp::spawn().await;
    let client = app.client();
    for path in ["/", "/dashboard", "/login", "/api/health", "/css/base.css"] {
        let res = client.get(path).await.assert_status(200);
        assert_eq!(
            res.header("x-content-type-options").as_deref(),
            Some("nosniff"),
            "{path}"
        );
        assert_eq!(
            res.header("x-frame-options").as_deref(),
            Some("DENY"),
            "{path}"
        );
        let csp = res.header("content-security-policy").unwrap();
        assert!(csp.contains("frame-ancestors 'none'"), "{path}: {csp}");
        assert!(
            csp.contains("script-src 'self';"),
            "{path}: scripts only from our origin: {csp}"
        );
        assert!(
            !csp.contains("script-src 'self' 'unsafe-inline'"),
            "{path}: no inline scripts allowed"
        );
    }
    app.finish().await;
}

#[tokio::test]
async fn random_pool_only_contains_active_anonymous_monitors_capped_at_ten() {
    let app = TestApp::spawn().await;
    let anon = app.client();
    let url = app.target.url("/ok");
    let mut public_ids = Vec::new();
    for i in 0..12 {
        let m = anon
            .create_monitor(json!({ "url": url, "name": format!("public {i}") }))
            .await;
        public_ids.push(m["id"].as_str().unwrap().to_string());
    }
    // A deactivated anonymous monitor must be excluded.
    sqlx::query("UPDATE monitors SET is_active = 0 WHERE id = ?")
        .bind(&public_ids[0])
        .execute(&app.state.pool)
        .await
        .unwrap();
    let owner = app.client();
    owner
        .register("owner@example.com", "correct horse battery")
        .await
        .assert_status(201);
    let private = owner
        .create_monitor(json!({ "url": url, "name": "private" }))
        .await;

    let mut seen = HashSet::new();
    for _ in 0..20 {
        let rows = anon
            .get("/api/public/random-monitors")
            .await
            .assert_status(200)
            .json();
        let rows = rows.as_array().unwrap().clone();
        assert_eq!(
            rows.len(),
            10,
            "exactly ten rows when more than ten are eligible"
        );
        for row in &rows {
            let id = row["id"].as_str().unwrap().to_string();
            assert_ne!(id, private["id"], "private monitors never appear");
            assert_ne!(id, public_ids[0], "paused monitors never appear");
            assert!(public_ids.contains(&id));
            for field in [
                "name",
                "url",
                "method",
                "current_status",
                "response_time_ms",
                "last_checked_at",
            ] {
                assert!(row.get(field).is_some(), "missing {field} in {row}");
            }
            assert!(row.get("headers").is_none() && row.get("alert_email").is_none());
            seen.insert(id);
        }
    }
    assert_eq!(seen.len(), 11, "the pool is shuffled between requests");
    app.finish().await;
}

#[tokio::test]
async fn stats_aggregate_monitor_counts() {
    let app = TestApp::spawn().await;
    let client = app.client();
    let before = client
        .get("/api/public/stats")
        .await
        .assert_status(200)
        .json();
    assert_eq!(before["total_monitors"], 0);
    assert_eq!(before["total_checks"], 0);
    assert_eq!(before["alerting_enabled"], true);

    let url = app.target.url("/ok");
    let a = client
        .create_monitor(json!({ "url": url, "name": "a" }))
        .await;
    client
        .create_monitor(json!({ "url": url, "name": "b" }))
        .await;
    let owner = app.client();
    owner
        .register("stats@example.com", "correct horse battery")
        .await
        .assert_status(201);
    owner
        .create_monitor(json!({ "url": url, "name": "private" }))
        .await;
    app.wait_for_state(a["id"].as_str().unwrap(), "UP", 0).await;
    app.wait_for_logs(a["id"].as_str().unwrap(), 1).await;

    // Stats are cached for ten seconds; drop the cache to observe the new totals.
    *app.state.stats_cache.lock().unwrap() = None;
    let after = client
        .get("/api/public/stats")
        .await
        .assert_status(200)
        .json();
    assert_eq!(after["total_monitors"], 3);
    assert_eq!(after["public_monitors"], 2);
    let up = after["up"].as_i64().unwrap();
    let down = after["down"].as_i64().unwrap();
    let pending = after["pending"].as_i64().unwrap();
    assert_eq!(up + down + pending, 3);
    assert!(up >= 1);
    assert!(after["total_checks"].as_i64().unwrap() >= 1);
    app.finish().await;
}
