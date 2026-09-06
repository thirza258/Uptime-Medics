//! Adapters: SQLite initialisation and migrations, the SMTP mailer (against a fake server) and
//! the outbound HTTP client.

mod common;

use std::time::Duration;

use common::{FakeSmtp, TargetServer};
use uptime_medics::config::{Config, SmtpConfig, SmtpTls};
use uptime_medics::infrastructure::{db, http_client, mailer::Mailer};

#[tokio::test]
async fn database_init_creates_file_enables_wal_and_migrates() {
    let dir = std::env::temp_dir().join(format!(
        "uptime-medics-db-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let path = dir.join("nested").join("test.db");
    let url = format!("sqlite://{}", path.display());

    let pool = db::init(&url)
        .await
        .expect("init creates missing directories");
    assert!(path.exists());
    let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(journal.to_lowercase(), "wal");
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(foreign_keys, 1);
    let tables: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .fetch_all(&pool)
            .await
            .unwrap();
    for table in ["users", "sessions", "monitors", "check_logs"] {
        assert!(tables.contains(&table.to_string()), "{tables:?}");
    }

    // Schema constraints from the PRD are enforced by the database itself.
    assert!(sqlx::query(
        "INSERT INTO monitors (id, name, url, method) VALUES ('x', 'n', 'http://e', 'TRACE')"
    )
    .execute(&pool)
    .await
    .is_err());
    assert!(sqlx::query("INSERT INTO monitors (id, name, url, method, check_interval_seconds) VALUES ('y', 'n', 'http://e', 'GET', 5)")
        .execute(&pool)
        .await
        .is_err());
    assert!(sqlx::query("INSERT INTO monitors (id, name, url, method, current_status) VALUES ('z', 'n', 'http://e', 'GET', 'MAYBE')")
        .execute(&pool)
        .await
        .is_err());
    assert!(sqlx::query("INSERT INTO check_logs (monitor_id, response_time_ms, is_successful) VALUES ('missing', 1, 1)")
        .execute(&pool)
        .await
        .is_err(), "foreign keys are enforced");

    // Re-running init on an existing database is idempotent.
    pool.close().await;
    let pool = db::init(&url).await.expect("second init");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM monitors")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    pool.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn timestamps_use_the_sqlite_text_format() {
    let now = db::now_sql();
    assert_eq!(now.len(), 19);
    assert_eq!(&now[10..11], " ");
    let dt = chrono::DateTime::parse_from_rfc3339("2026-09-06T06:15:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(db::to_sql(dt), "2026-09-06 06:15:00");
}

#[test]
fn mailer_is_disabled_without_smtp_configuration() {
    assert!(Mailer::from_config(&Config::default()).unwrap().is_none());
    let bad_from = Config {
        smtp: Some(SmtpConfig {
            host: "smtp.example.com".into(),
            port: 587,
            username: None,
            password: None,
            from_address: "not an address".into(),
            tls: SmtpTls::StartTls,
        }),
        ..Config::default()
    };
    assert!(Mailer::from_config(&bad_from).is_err());
}

fn smtp_config(port: u16) -> Config {
    Config {
        smtp: Some(SmtpConfig {
            host: "127.0.0.1".into(),
            port,
            username: None,
            password: None,
            from_address: "Uptime Medics <alerts@uptime.test>".into(),
            tls: SmtpTls::None,
        }),
        ..Config::default()
    }
}

#[tokio::test]
async fn mailer_delivers_through_smtp() {
    let smtp = FakeSmtp::spawn().await;
    let mailer = Mailer::from_config(&smtp_config(smtp.port))
        .unwrap()
        .expect("configured");
    assert!(mailer.description().contains("127.0.0.1"));

    mailer
        .send("someone@example.com", "Hello there", "Body line\n".into())
        .await
        .unwrap();
    let messages = smtp.wait_for_messages(1).await;
    assert!(
        messages[0].contains("Subject: Hello there"),
        "{}",
        messages[0]
    );
    assert!(messages[0].contains("Body line"));
    assert!(messages[0].contains("<alerts@uptime.test>"));
    assert!(messages[0].contains("someone@example.com"));

    assert!(mailer
        .send("not an address", "x", "y".into())
        .await
        .is_err());
    smtp.stop();
}

#[tokio::test]
async fn mailer_retries_with_exponential_backoff() {
    let smtp = FakeSmtp::spawn().await;
    smtp.fail_next_sessions(2);
    let mailer = Mailer::from_config(&smtp_config(smtp.port))
        .unwrap()
        .unwrap()
        .with_initial_backoff(Duration::from_millis(50));
    let started = std::time::Instant::now();
    mailer
        .send_with_backoff("retry@example.com", "Retry", "second".into())
        .await
        .expect("succeeds on the third attempt");
    assert!(
        started.elapsed() >= Duration::from_millis(150),
        "waited 50 ms + 100 ms between attempts"
    );
    let messages = smtp.wait_for_messages(1).await;
    assert!(messages[0].contains("Subject: Retry"));

    // Four failures in a row exhaust the attempts.
    smtp.fail_next_sessions(10);
    let fresh = Mailer::from_config(&smtp_config(smtp.port))
        .unwrap()
        .unwrap()
        .with_initial_backoff(Duration::from_millis(10));
    let err = fresh
        .send_with_backoff("retry@example.com", "Never", "x".into())
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("giving up after 4 attempts"),
        "{err:#}"
    );
    smtp.stop();
}

#[tokio::test]
async fn http_client_redirect_policies_and_user_agent() {
    let target = TargetServer::spawn().await;
    let follow = http_client::build_http_client(true, true).unwrap();
    let res = follow.get(target.url("/redirect")).send().await.unwrap();
    assert_eq!(
        res.status().as_u16(),
        200,
        "redirects are followed by default"
    );

    let no_follow = http_client::build_http_client(true, false).unwrap();
    let res = no_follow.get(target.url("/redirect")).send().await.unwrap();
    assert_eq!(
        res.status().as_u16(),
        307,
        "the no-follow client returns the redirect itself"
    );

    let res = follow.get(target.url("/teapot")).send().await.unwrap();
    assert_eq!(res.status().as_u16(), 418);
    target.stop();
}
