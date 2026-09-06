//! Monitor creation, validation, visibility rules, ownership, rate limiting and SSRF guards.

mod common;

use std::time::Duration;

use common::TestApp;
use serde_json::{json, Value};

#[tokio::test]
async fn anonymous_monitor_gets_defaults_and_derived_name() {
    // A slow scheduler keeps the first probe from racing the creation response.
    let app = TestApp::spawn_with(|c| c.scheduler_tick = Duration::from_secs(3600)).await;
    let client = app.client();
    let m = client
        .create_monitor(json!({ "url": app.target.url("/ok") }))
        .await;
    assert_eq!(m["method"], "GET");
    assert_eq!(m["name"], "127.0.0.1/ok", "name derives from host and path");
    assert_eq!(m["check_interval_seconds"], 60);
    assert_eq!(m["timeout_seconds"], 10);
    assert_eq!(m["expected_status_code"], 200);
    assert_eq!(m["is_anonymous"], true);
    assert_eq!(m["is_active"], true);
    assert_eq!(m["current_status"], "PENDING");
    assert_eq!(m["consecutive_failures"], 0);
    assert!(m["last_checked_at"].is_null());
    assert!(m.get("user_id").is_none(), "owner ids are never exposed");
    assert!(
        m["created_at"].as_str().unwrap().ends_with('Z'),
        "timestamps are UTC RFC 3339"
    );
    app.finish().await;
}

#[tokio::test]
async fn create_validates_payload() {
    let app = TestApp::spawn().await;
    let client = app.client();
    let url = app.target.url("/ok");
    let cases: Vec<(Value, &str)> = vec![
        (
            json!({ "url": url, "method": "TRACE" }),
            "Unsupported method",
        ),
        (
            json!({ "url": url, "check_interval_seconds": 5 }),
            "check_interval_seconds",
        ),
        (
            json!({ "url": url, "timeout_seconds": 61 }),
            "timeout_seconds",
        ),
        (
            json!({ "url": url, "expected_status_code": 999 }),
            "expected_status_code",
        ),
        (json!({ "url": url, "alert_email": "nope" }), "alert_email"),
        (
            json!({ "url": url, "headers": [1, 2] }),
            "Headers must be a JSON object",
        ),
        (
            json!({ "url": url, "headers": { "bad header": "x" } }),
            "Invalid headers",
        ),
        (
            json!({ "url": url, "body": "x".repeat(70 * 1024) }),
            "too large",
        ),
        (
            json!({ "url": url, "name": "n".repeat(200) }),
            "Name must be at most",
        ),
        (
            json!({ "url": "ftp://example.com/" }),
            "Unsupported URL scheme",
        ),
        (json!({ "url": "not a url" }), "Invalid URL"),
        (json!({ "method": "GET" }), "missing field"),
    ];
    for (body, expected) in cases {
        let res = client.post_json("/api/monitors", body.clone()).await;
        assert_eq!(res.status, 400, "{body} should be rejected: {}", res.text);
        let message = res.json()["message"].as_str().unwrap().to_string();
        assert!(
            message.contains(expected),
            "{body}: message {message:?} should mention {expected:?}"
        );
    }
    client
        .post_raw("/api/monitors", "{oops")
        .await
        .assert_status(400);
    app.finish().await;
}

#[tokio::test]
async fn headers_accept_object_or_string_and_are_normalised() {
    let app = TestApp::spawn().await;
    let client = app.client();
    let url = app.target.url("/echo");

    let as_object = client
        .create_monitor(json!({
            "url": url, "method": "POST",
            "headers": { "X-Ping": "medic", "X-Num": 7 },
            "body": "{\"check\":true}", "expected_status_code": 201
        }))
        .await;
    assert_eq!(
        as_object["headers"],
        "{\"X-Num\":\"7\",\"X-Ping\":\"medic\"}"
    );
    assert_eq!(as_object["body"], "{\"check\":true}");

    let as_string = client
        .create_monitor(json!({
            "url": url, "method": "PUT",
            "headers": "{\"Content-Type\": \"application/json\"}", "body": "{}"
        }))
        .await;
    assert_eq!(
        as_string["headers"],
        "{\"Content-Type\":\"application/json\"}"
    );
    assert_eq!(as_string["method"], "PUT");

    let empty = client
        .create_monitor(json!({ "url": url, "method": "options", "headers": "   ", "body": "" }))
        .await;
    assert_eq!(empty["method"], "OPTIONS", "methods are upper-cased");
    assert!(empty.get("headers").is_none() && empty.get("body").is_none());
    app.finish().await;
}

#[tokio::test]
async fn public_monitors_are_readable_but_redacted_and_immutable_for_others() {
    let app = TestApp::spawn().await;
    let creator = app.client();
    let m = creator
        .create_monitor(json!({
            "url": app.target.url("/ok"), "name": "Public one",
            "headers": { "Authorization": "Bearer secret" }, "body": "payload",
            "alert_email": "owner@example.com"
        }))
        .await;
    let id = m["id"].as_str().unwrap();
    assert_eq!(
        m["alert_email"], "owner@example.com",
        "the creator sees what was submitted"
    );

    let viewer = app.client();
    let public = viewer
        .get(&format!("/api/monitors/{id}"))
        .await
        .assert_status(200)
        .json();
    assert_eq!(public["name"], "Public one");
    assert!(
        public.get("headers").is_none(),
        "custom headers may contain secrets"
    );
    assert!(public.get("body").is_none());
    assert!(public.get("alert_email").is_none());
    viewer
        .get(&format!("/api/monitors/{id}/logs"))
        .await
        .assert_status(200);

    // Nobody owns an anonymous monitor, so nobody can edit or delete it.
    viewer
        .patch_json(&format!("/api/monitors/{id}"), json!({ "name": "hijack" }))
        .await
        .assert_status(401);
    viewer
        .delete(&format!("/api/monitors/{id}"))
        .await
        .assert_status(401);
    viewer
        .register("someone@example.com", "correct horse battery")
        .await
        .assert_status(201);
    viewer
        .patch_json(&format!("/api/monitors/{id}"), json!({ "name": "hijack" }))
        .await
        .assert_status(404);
    viewer
        .delete(&format!("/api/monitors/{id}"))
        .await
        .assert_status(404);
    viewer
        .get("/api/monitors/does-not-exist")
        .await
        .assert_status(404);
    app.finish().await;
}

#[tokio::test]
async fn registered_monitors_are_private_to_their_owner() {
    let app = TestApp::spawn().await;
    let owner = app.client();
    owner
        .register("owner@example.com", "correct horse battery")
        .await
        .assert_status(201);
    let m = owner
        .create_monitor(json!({
            "url": app.target.url("/ok"), "name": "Mine",
            "headers": { "X-Token": "s3cret" }, "alert_email": "ops@example.com"
        }))
        .await;
    let id = m["id"].as_str().unwrap();
    assert_eq!(m["is_anonymous"], false);

    let mine = owner
        .get("/api/monitors/user")
        .await
        .assert_status(200)
        .json();
    assert_eq!(mine.as_array().unwrap().len(), 1);
    assert_eq!(mine[0]["headers"], "{\"X-Token\":\"s3cret\"}");
    assert_eq!(mine[0]["alert_email"], "ops@example.com");
    owner
        .get(&format!("/api/monitors/{id}"))
        .await
        .assert_status(200);
    owner
        .get(&format!("/api/monitors/{id}/logs"))
        .await
        .assert_status(200);

    let stranger = app.client();
    stranger
        .get(&format!("/api/monitors/{id}"))
        .await
        .assert_status(404);
    stranger
        .get(&format!("/api/monitors/{id}/logs"))
        .await
        .assert_status(404);
    stranger.get("/api/monitors/user").await.assert_status(401);

    stranger
        .register("other@example.com", "correct horse battery")
        .await
        .assert_status(201);
    stranger
        .get(&format!("/api/monitors/{id}"))
        .await
        .assert_status(404);
    stranger
        .patch_json(
            &format!("/api/monitors/{id}"),
            json!({ "is_active": false }),
        )
        .await
        .assert_status(404);
    stranger
        .delete(&format!("/api/monitors/{id}"))
        .await
        .assert_status(404);
    let theirs = stranger
        .get("/api/monitors/user")
        .await
        .assert_status(200)
        .json();
    assert_eq!(theirs.as_array().unwrap().len(), 0);

    let pool = stranger
        .get("/api/public/random-monitors")
        .await
        .assert_status(200)
        .json();
    assert!(
        pool.as_array().unwrap().iter().all(|p| p["id"] != id),
        "private monitors never appear in the public pool"
    );
    app.finish().await;
}

#[tokio::test]
async fn owner_can_update_pause_and_delete() {
    let app = TestApp::spawn().await;
    let owner = app.client();
    owner
        .register("editor@example.com", "correct horse battery")
        .await
        .assert_status(201);
    let m = owner
        .create_monitor(json!({
            "url": app.target.url("/ok"), "name": "Editable", "alert_email": "ops@example.com"
        }))
        .await;
    let id = m["id"].as_str().unwrap().to_string();
    let path = format!("/api/monitors/{id}");
    app.wait_for_state(&id, "UP", 0).await;

    let updated = owner
        .patch_json(
            &path,
            json!({ "name": "Renamed", "check_interval_seconds": 300, "timeout_seconds": 20, "alert_email": null }),
        )
        .await
        .assert_status(200)
        .json();
    assert_eq!(updated["name"], "Renamed");
    assert_eq!(updated["check_interval_seconds"], 300);
    assert_eq!(updated["timeout_seconds"], 20);
    assert!(
        updated.get("alert_email").is_none(),
        "null clears the alert email"
    );
    assert_eq!(
        updated["current_status"], "UP",
        "non-target edits keep the status"
    );
    assert!(!updated["last_checked_at"].is_null());

    // Changing the target resets the failure streak and schedules an immediate probe.
    sqlx::query("UPDATE monitors SET consecutive_failures = 1 WHERE id = ?")
        .bind(&id)
        .execute(&app.state.pool)
        .await
        .unwrap();
    let retargeted = owner
        .patch_json(
            &path,
            json!({ "url": app.target.url("/teapot"), "expected_status_code": 418 }),
        )
        .await
        .assert_status(200)
        .json();
    assert_eq!(retargeted["consecutive_failures"], 0);
    assert_eq!(retargeted["expected_status_code"], 418);
    app.wait_for_logs(&id, 2).await;
    app.wait_for_state(&id, "UP", 0).await;

    owner
        .patch_json(&path, json!({ "check_interval_seconds": 1 }))
        .await
        .assert_status(400);
    owner
        .patch_json(&path, json!({ "url": "ftp://x" }))
        .await
        .assert_status(400);
    owner
        .patch_json(&path, json!({ "method": "TRACE" }))
        .await
        .assert_status(400);
    owner
        .patch_json(&path, json!({ "alert_email": "not-an-email" }))
        .await
        .assert_status(400);

    let paused = owner
        .patch_json(&path, json!({ "is_active": false }))
        .await
        .assert_status(200)
        .json();
    assert_eq!(paused["is_active"], false);
    let resumed = owner
        .patch_json(&path, json!({ "is_active": true }))
        .await
        .assert_status(200)
        .json();
    assert_eq!(resumed["is_active"], true);

    assert!(app.log_count(&id).await >= 1);
    owner.delete(&path).await.assert_status(204);
    owner.get(&path).await.assert_status(404);
    owner.delete(&path).await.assert_status(404);
    assert_eq!(
        app.log_count(&id).await,
        0,
        "deleting a monitor cascades to its logs"
    );
    app.finish().await;
}

#[tokio::test]
async fn ssrf_protection_blocks_internal_targets() {
    let app = TestApp::spawn_with(|c| c.allow_private_targets = false).await;
    let client = app.client();
    for url in [
        "http://127.0.0.1/",
        "http://localhost:8080/health",
        "http://169.254.169.254/latest/meta-data",
        "http://10.0.0.1/",
        "http://[::1]/",
        "http://192.168.1.1/admin",
        "http://metadata.google.internal/",
        "http://printer.local/",
    ] {
        let res = client
            .post_json("/api/monitors", json!({ "url": url }))
            .await;
        assert_eq!(res.status, 400, "{url} should be blocked: {}", res.text);
        assert_eq!(res.json()["error"], "bad_request");
    }
    // The local target is unreachable with protection on, but allowed with the escape hatch.
    let allowed = TestApp::spawn().await;
    allowed
        .client()
        .create_monitor(json!({ "url": allowed.target.url("/ok") }))
        .await;
    allowed.finish().await;
    app.finish().await;
}

#[tokio::test]
async fn anonymous_creation_is_rate_limited_per_ip() {
    let app = TestApp::spawn_with(|c| c.rate_limit_per_hour = 2).await;
    let client = app.client();
    let url = app.target.url("/ok");
    client.create_monitor(json!({ "url": url })).await;
    client.create_monitor(json!({ "url": url })).await;
    let limited = client
        .post_json("/api/monitors", json!({ "url": url }))
        .await
        .assert_status(429);
    assert_eq!(limited.json()["error"], "rate_limited");
    let retry_after: u64 = limited.header("retry-after").unwrap().parse().unwrap();
    assert!(retry_after >= 1);

    // Signed-in users are exempt from the anonymous limit.
    let member = app.client();
    member
        .register("member@example.com", "correct horse battery")
        .await
        .assert_status(201);
    for _ in 0..3 {
        member.create_monitor(json!({ "url": url })).await;
    }
    app.finish().await;
}

#[tokio::test]
async fn unknown_routes_and_methods() {
    let app = TestApp::spawn().await;
    let client = app.client();
    let api = client.get("/api/nope").await.assert_status(404);
    assert_eq!(api.json()["error"], "not_found");
    let page = client.get("/nope").await.assert_status(404);
    assert!(page.text.contains("404"));
    client
        .post_empty("/api/monitors/user")
        .await
        .assert_status(405);
    app.finish().await;
}
