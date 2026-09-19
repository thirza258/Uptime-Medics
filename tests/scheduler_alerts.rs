//! The probe loop, incident detection, email notifications, batching, shutdown and cleanup,
//! exercised end to end against a local target server and a fake SMTP sink.

mod common;

use std::time::{Duration, Instant};

use common::{TestApp, WAIT};
use serde_json::json;
use sqlx::SqlitePool;
use uptime_medics::services::scheduler;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probes_run_and_logs_are_recorded() {
    let app = TestApp::spawn().await;
    let client = app.client();
    let m = client
        .create_monitor(json!({ "url": app.target.url("/ok"), "name": "healthy" }))
        .await;
    let id = m["id"].as_str().unwrap();

    app.wait_for_state(id, "UP", 0).await;
    app.wait_for_logs(id, 1).await;

    let logs = client
        .get(&format!("/api/monitors/{id}/logs"))
        .await
        .assert_status(200)
        .json();
    let first = &logs.as_array().unwrap()[0];
    assert_eq!(first["status_code"], 200);
    assert_eq!(first["is_successful"], true);
    assert!(first["error_message"].is_null());
    assert!(first["response_time_ms"].as_i64().unwrap() >= 0);
    assert!(first["checked_at"].as_str().unwrap().ends_with('Z'));

    let fresh = client
        .get(&format!("/api/monitors/{id}"))
        .await
        .assert_status(200)
        .json();
    assert_eq!(fresh["current_status"], "UP");
    assert!(
        fresh["response_time_ms"].is_number(),
        "latest latency is joined in"
    );
    assert!(fresh["last_error"].is_null());
    assert!(!fresh["last_checked_at"].is_null());

    // Not due again until the interval has elapsed...
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(app.log_count(id).await, 1);
    // ...but a due monitor is probed on the next tick.
    app.make_due(id).await;
    app.wait_for_logs(id, 2).await;
    app.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incident_opens_after_two_failures_and_recovers_with_emails() {
    let app = TestApp::spawn().await;
    let owner = app.client();
    owner
        .register("ops@example.com", "correct horse battery")
        .await
        .assert_status(201);
    let m = owner
        .create_monitor(json!({
            "url": app.target.url("/teapot"), "name": "Checkout API",
            "alert_email": "oncall@example.com"
        }))
        .await;
    let id = m["id"].as_str().unwrap().to_string();
    let path = format!("/api/monitors/{id}");

    // One failure is a blip, not an incident.
    app.wait_for_state(&id, "PENDING", 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        app.smtp.messages().is_empty(),
        "a single failure must not alert"
    );

    // The second consecutive failure opens the incident and sends the alert.
    app.make_due(&id).await;
    app.wait_for_state(&id, "DOWN", 2).await;
    let mails = app.smtp.wait_for_messages(1).await;
    let alert = &mails[0];
    assert!(
        alert.contains("Subject: [ALERT] Monitor DOWN: Checkout API"),
        "{alert}"
    );
    assert!(alert.contains("oncall@example.com"), "{alert}");
    assert!(alert.contains("<alerts@uptime.test>"), "{alert}");
    assert!(
        alert.contains("Failure reason: Status 418 I'm a teapot (expected 200)"),
        "{alert}"
    );
    assert!(alert.contains("Consecutive failures: 2"), "{alert}");
    assert!(
        alert.contains(&format!("http://uptime.test/#monitor={id}")),
        "{alert}"
    );
    assert!(alert.contains(" UTC"), "{alert}");

    let details = owner.get(&path).await.assert_status(200).json();
    assert_eq!(details["current_status"], "DOWN");
    assert_eq!(
        details["last_error"],
        "Status 418 I'm a teapot (expected 200)"
    );

    // Further failures while DOWN do not re-alert.
    app.make_due(&id).await;
    app.wait_for_state(&id, "DOWN", 3).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(app.smtp.messages().len(), 1);

    // Recovery: pointing the monitor at a healthy endpoint keeps it DOWN until the next probe
    // succeeds, then a recovery email goes out.
    owner
        .patch_json(&path, json!({ "url": app.target.url("/ok") }))
        .await
        .assert_status(200);
    app.wait_for_state(&id, "UP", 0).await;
    let mails = app.smtp.wait_for_messages(2).await;
    let recovery = &mails[1];
    assert!(
        recovery.contains("Subject: [RESOLVED] Monitor UP: Checkout API"),
        "{recovery}"
    );
    assert!(
        recovery.contains("Latest result: Status 200 in"),
        "{recovery}"
    );
    app.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custom_methods_headers_bodies_redirects_and_timeouts() {
    let app = TestApp::spawn().await;
    let client = app.client();

    let post = client
        .create_monitor(json!({
            "url": app.target.url("/echo"), "method": "POST",
            "headers": { "X-Ping": "medic" }, "body": "{\"check\":true}", "expected_status_code": 201
        }))
        .await;
    app.wait_for_state(post["id"].as_str().unwrap(), "UP", 0)
        .await;

    let wrong_body = client
        .create_monitor(json!({
            "url": app.target.url("/echo"), "method": "POST",
            "headers": { "X-Ping": "medic" }, "body": "nope", "expected_status_code": 201
        }))
        .await;
    app.wait_for_state(wrong_body["id"].as_str().unwrap(), "PENDING", 1)
        .await;

    let teapot = client
        .create_monitor(json!({ "url": app.target.url("/teapot"), "expected_status_code": 418 }))
        .await;
    app.wait_for_state(teapot["id"].as_str().unwrap(), "UP", 0)
        .await;

    let head = client
        .create_monitor(json!({ "url": app.target.url("/ok"), "method": "HEAD" }))
        .await;
    app.wait_for_state(head["id"].as_str().unwrap(), "UP", 0)
        .await;

    let followed = client
        .create_monitor(json!({ "url": app.target.url("/redirect") }))
        .await;
    app.wait_for_state(followed["id"].as_str().unwrap(), "UP", 0)
        .await;
    let not_followed = client
        .create_monitor(json!({ "url": app.target.url("/redirect"), "expected_status_code": 307 }))
        .await;
    app.wait_for_state(not_followed["id"].as_str().unwrap(), "UP", 0)
        .await;

    let slow = client
        .create_monitor(json!({ "url": app.target.url("/slow"), "timeout_seconds": 1 }))
        .await;
    let slow_id = slow["id"].as_str().unwrap();
    app.wait_for_state(slow_id, "PENDING", 1).await;
    app.wait_for_logs(slow_id, 1).await;
    let logs = client
        .get(&format!("/api/monitors/{slow_id}/logs"))
        .await
        .assert_status(200)
        .json();
    let entry = &logs[0];
    assert_eq!(entry["is_successful"], false);
    assert!(entry["status_code"].is_null());
    assert!(
        entry["error_message"]
            .as_str()
            .unwrap()
            .starts_with("TIMEOUT_EXCEEDED"),
        "{entry}"
    );
    assert!(entry["response_time_ms"].as_i64().unwrap() >= 900);

    let refused = client
        .create_monitor(json!({ "url": "http://127.0.0.1:9/" }))
        .await;
    let refused_id = refused["id"].as_str().unwrap();
    app.wait_for_logs(refused_id, 1).await;
    let logs = client
        .get(&format!("/api/monitors/{refused_id}/logs"))
        .await
        .assert_status(200)
        .json();
    assert!(
        logs[0]["error_message"]
            .as_str()
            .unwrap()
            .starts_with("Connection failed"),
        "{}",
        logs[0]
    );
    app.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paused_monitors_are_not_probed_until_resumed() {
    let app = TestApp::spawn().await;
    let owner = app.client();
    owner
        .register("pause@example.com", "correct horse battery")
        .await
        .assert_status(201);
    let m = owner
        .create_monitor(json!({ "url": app.target.url("/count") }))
        .await;
    let id = m["id"].as_str().unwrap().to_string();
    let path = format!("/api/monitors/{id}");
    app.wait_for_state(&id, "UP", 0).await;
    assert_eq!(app.target.hits(), 1);

    owner
        .patch_json(&path, json!({ "is_active": false }))
        .await
        .assert_status(200);
    app.make_due(&id).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(app.target.hits(), 1, "a paused monitor must not be probed");

    owner
        .patch_json(&path, json!({ "is_active": true }))
        .await
        .assert_status(200);
    app.make_due(&id).await;
    let deadline = Instant::now() + WAIT;
    while app.target.hits() < 2 {
        assert!(Instant::now() < deadline, "resumed monitor was not probed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    app.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alerts_need_an_alert_email_and_smtp() {
    // No alert email: the incident opens silently.
    let app = TestApp::spawn().await;
    let m = app
        .client()
        .create_monitor(json!({ "url": app.target.url("/teapot") }))
        .await;
    let id = m["id"].as_str().unwrap().to_string();
    app.wait_for_state(&id, "PENDING", 1).await;
    app.make_due(&id).await;
    app.wait_for_state(&id, "DOWN", 2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(app.smtp.messages().is_empty());
    app.finish().await;

    // SMTP not configured: the incident opens, nothing is sent, nothing breaks.
    let quiet = TestApp::spawn_with(|c| c.smtp = None).await;
    let m = quiet
        .client()
        .create_monitor(
            json!({ "url": quiet.target.url("/teapot"), "alert_email": "x@example.com" }),
        )
        .await;
    let id = m["id"].as_str().unwrap().to_string();
    quiet.wait_for_state(&id, "PENDING", 1).await;
    quiet.make_due(&id).await;
    quiet.wait_for_state(&id, "DOWN", 2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(quiet.smtp.messages().is_empty());
    quiet.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flapping_monitors_stop_sending_notifications() {
    let app = TestApp::spawn().await;
    let owner = app.client();
    owner
        .register("flap@example.com", "correct horse battery")
        .await
        .assert_status(201);
    let m = owner
        .create_monitor(json!({
            "url": app.target.url("/teapot"), "name": "Flappy", "alert_email": "oncall@example.com"
        }))
        .await;
    let id = m["id"].as_str().unwrap().to_string();
    let path = format!("/api/monitors/{id}");

    // Four transitions (DOWN, UP, DOWN, UP) each produce an email.
    let mut mails = 0;
    for round in 0..2 {
        if round > 0 {
            owner
                .patch_json(&path, json!({ "url": app.target.url("/teapot") }))
                .await
                .assert_status(200);
        }
        app.wait_for_state(&id, if round == 0 { "PENDING" } else { "UP" }, 1)
            .await;
        app.make_due(&id).await;
        app.wait_for_state(&id, "DOWN", 2).await;
        mails += 1;
        app.smtp.wait_for_messages(mails).await;

        owner
            .patch_json(&path, json!({ "url": app.target.url("/ok") }))
            .await
            .assert_status(200);
        app.wait_for_state(&id, "UP", 0).await;
        mails += 1;
        app.smtp.wait_for_messages(mails).await;
    }
    assert_eq!(mails, 4);

    // The fifth transition inside the window is flapping: no email.
    owner
        .patch_json(&path, json!({ "url": app.target.url("/teapot") }))
        .await
        .assert_status(200);
    app.wait_for_state(&id, "UP", 1).await;
    app.make_due(&id).await;
    app.wait_for_state(&id, "DOWN", 2).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        app.smtp.messages().len(),
        4,
        "a flapping monitor is silenced"
    );
    app.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_flushes_buffered_logs() {
    let mut app = TestApp::spawn_with(|c| c.log_flush_interval = Duration::from_secs(3600)).await;
    let m = app
        .client()
        .create_monitor(json!({ "url": app.target.url("/ok") }))
        .await;
    let id = m["id"].as_str().unwrap().to_string();
    app.wait_for_state(&id, "UP", 0).await;
    assert_eq!(app.log_count(&id).await, 0, "the log is still buffered");
    app.stop_scheduler().await;
    assert_eq!(
        app.log_count(&id).await,
        1,
        "shutdown flushed the buffered log"
    );
    app.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logs_are_flushed_in_batches_before_the_interval_when_full() {
    let mut app = TestApp::spawn_with(|c| {
        c.log_flush_interval = Duration::from_secs(3600);
        c.log_batch_size = 3;
    })
    .await;
    let client = app.client();
    let mut ids = Vec::new();
    for i in 0..3 {
        let m = client
            .create_monitor(json!({ "url": app.target.url("/ok"), "name": format!("batch {i}") }))
            .await;
        ids.push(m["id"].as_str().unwrap().to_string());
    }
    for id in &ids {
        app.wait_for_state(id, "UP", 0).await;
    }
    // Three results fill the batch, so they land without waiting for the flush interval.
    for id in &ids {
        app.wait_for_logs(id, 1).await;
    }
    app.stop_scheduler().await;
    app.finish().await;
}

async fn insert_log(pool: &SqlitePool, monitor_id: &str, checked_at: &str) {
    sqlx::query(
        "INSERT INTO check_logs (monitor_id, status_code, response_time_ms, is_successful, error_message, checked_at) \
         VALUES (?, 200, 5, 1, NULL, ?)",
    )
    .bind(monitor_id)
    .bind(checked_at)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cleanup_prunes_old_logs_and_expired_sessions() {
    let app = TestApp::spawn_with(|config| config.anon_retention_hours = 48).await;
    let anon = app.client();
    let public = anon
        .create_monitor(json!({ "url": app.target.url("/ok"), "check_interval_seconds": 86400 }))
        .await;
    let owner = app.client();
    owner
        .register("keep@example.com", "correct horse battery")
        .await
        .assert_status(201);
    let private = owner
        .create_monitor(json!({ "url": app.target.url("/ok"), "check_interval_seconds": 86400 }))
        .await;
    let pid = public["id"].as_str().unwrap();
    let rid = private["id"].as_str().unwrap();
    app.wait_for_logs(pid, 1).await;
    app.wait_for_logs(rid, 1).await;

    let ten_days_ago = (chrono::Utc::now() - chrono::Duration::days(10))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    insert_log(&app.state.pool, pid, "2020-01-01 00:00:00").await; // anonymous, ancient -> pruned
    insert_log(&app.state.pool, pid, &ten_days_ago).await; // anonymous, > 48h -> pruned
    insert_log(&app.state.pool, rid, "2020-01-01 00:00:00").await; // registered, > 30d -> pruned
    insert_log(&app.state.pool, rid, &ten_days_ago).await; // registered, < 30d -> kept
    assert_eq!(app.log_count(pid).await, 3);
    assert_eq!(app.log_count(rid).await, 3);

    sqlx::query(
        "INSERT INTO sessions (id, user_id, expires_at) \
         VALUES ('expired-session', (SELECT id FROM users LIMIT 1), '2020-01-01 00:00:00')",
    )
    .execute(&app.state.pool)
    .await
    .unwrap();
    let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&app.state.pool)
        .await
        .unwrap();
    assert_eq!(sessions, 2);

    scheduler::run_cleanup(&app.state).await;

    assert_eq!(
        app.log_count(pid).await,
        1,
        "anonymous logs older than 48h are pruned"
    );
    assert_eq!(
        app.log_count(rid).await,
        2,
        "registered logs are kept for 30 days"
    );
    let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&app.state.pool)
        .await
        .unwrap();
    assert_eq!(sessions, 1, "expired sessions are removed");
    owner.get("/api/auth/me").await.assert_status(200);
    app.finish().await;
}
