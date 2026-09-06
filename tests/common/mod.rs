//! Shared harness for the integration tests: the whole application served in-process on a
//! random port, a local HTTP target to probe, a fake SMTP sink that captures emails, and a
//! small cookie-aware HTTP client.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use axum::response::Redirect;
use axum::routing::{get, post};
use axum::Router;
use reqwest::header::{HeaderMap, CONTENT_TYPE, COOKIE, SET_COOKIE};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use uptime_medics::api;
use uptime_medics::config::{Config, SmtpConfig, SmtpTls};
use uptime_medics::infrastructure::db;
use uptime_medics::services::scheduler;
use uptime_medics::state::AppState;

pub const TEST_JWT_SECRET: &str = "integration-test-secret-0123456789-abcdefghijklmnopqrstuvwxyz";
pub const WAIT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------------------------
// TestApp

pub struct TestApp {
    pub base_url: String,
    pub state: AppState,
    pub target: TargetServer,
    pub smtp: FakeSmtp,
    shutdown: watch::Sender<bool>,
    scheduler: Option<JoinHandle<()>>,
    server: JoinHandle<()>,
    db_path: PathBuf,
}

impl TestApp {
    /// Fast scheduler, private targets allowed, generous rate limit, fake SMTP configured.
    pub async fn spawn() -> TestApp {
        Self::spawn_with(|_| {}).await
    }

    pub async fn spawn_with(customize: impl FnOnce(&mut Config)) -> TestApp {
        let target = TargetServer::spawn().await;
        let smtp = FakeSmtp::spawn().await;
        let db_path = std::env::temp_dir().join(format!(
            "uptime-medics-test-{}.db",
            uuid::Uuid::new_v4().simple()
        ));

        let mut config = Config {
            database_url: format!("sqlite://{}", db_path.display()),
            jwt_secret: TEST_JWT_SECRET.to_string(),
            allow_private_targets: true,
            scheduler_tick: Duration::from_millis(100),
            log_flush_interval: Duration::from_millis(200),
            rate_limit_per_hour: 1000,
            public_base_url: Some("http://uptime.test".to_string()),
            smtp: Some(SmtpConfig {
                host: "127.0.0.1".to_string(),
                port: smtp.port,
                username: None,
                password: None,
                from_address: "Uptime Medics <alerts@uptime.test>".to_string(),
                tls: SmtpTls::None,
            }),
            ..Config::default()
        };
        customize(&mut config);
        let config = Arc::new(config);

        let pool = db::init(&config.database_url).await.expect("database init");
        let (state, log_rx) = AppState::new(config.clone(), pool).expect("app state");
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let scheduler = tokio::spawn(scheduler::run(state.clone(), log_rx, shutdown_rx));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = api::router(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });

        TestApp {
            base_url: format!("http://{addr}"),
            state,
            target,
            smtp,
            shutdown: shutdown_tx,
            scheduler: Some(scheduler),
            server,
            db_path,
        }
    }

    /// A new client with its own cookie jar.
    pub fn client(&self) -> TestClient {
        TestClient::new(self.base_url.clone())
    }

    /// Stop the scheduler (which flushes buffered check logs) but keep the database usable.
    pub async fn stop_scheduler(&mut self) {
        let _ = self.shutdown.send(true);
        if let Some(handle) = self.scheduler.take() {
            tokio::time::timeout(WAIT, handle)
                .await
                .expect("scheduler did not stop in time")
                .expect("scheduler task panicked");
        }
    }

    /// Shut everything down and delete the temporary database.
    pub async fn finish(mut self) {
        self.stop_scheduler().await;
        self.server.abort();
        self.target.stop();
        self.smtp.stop();
        self.state.pool.close().await;
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{}", self.db_path.display(), suffix));
        }
    }

    /// Make a monitor due for its next probe on the next scheduler tick.
    pub async fn make_due(&self, monitor_id: &str) {
        sqlx::query("UPDATE monitors SET last_checked_at = '2000-01-01 00:00:00' WHERE id = ?")
            .bind(monitor_id)
            .execute(&self.state.pool)
            .await
            .unwrap();
    }

    pub async fn monitor_state(&self, id: &str) -> (String, i64) {
        sqlx::query_as::<_, (String, i64)>(
            "SELECT current_status, consecutive_failures FROM monitors WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&self.state.pool)
        .await
        .unwrap()
    }

    pub async fn log_count(&self, id: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM check_logs WHERE monitor_id = ?")
            .bind(id)
            .fetch_one(&self.state.pool)
            .await
            .unwrap()
    }

    /// Poll until the monitor reaches `status` and `consecutive_failures == failures`.
    pub async fn wait_for_state(&self, id: &str, status: &str, failures: i64) -> (String, i64) {
        let deadline = Instant::now() + WAIT;
        loop {
            let current = self.monitor_state(id).await;
            if current.0 == status && current.1 == failures {
                return current;
            }
            assert!(
                Instant::now() < deadline,
                "monitor {id} did not reach ({status}, {failures}) within {WAIT:?}; last = {current:?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub async fn wait_for_logs(&self, id: &str, at_least: i64) -> i64 {
        let deadline = Instant::now() + WAIT;
        loop {
            let count = self.log_count(id).await;
            if count >= at_least {
                return count;
            }
            assert!(
                Instant::now() < deadline,
                "monitor {id} has {count} logs after {WAIT:?}, expected at least {at_least}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// TestClient

pub struct Resp {
    pub status: u16,
    pub headers: HeaderMap,
    pub text: String,
}

impl Resp {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.text)
            .unwrap_or_else(|e| panic!("response is not JSON ({e}): {}", self.text))
    }

    pub fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    }

    pub fn assert_status(self, expected: u16) -> Self {
        assert_eq!(
            self.status, expected,
            "unexpected status; body: {}",
            self.text
        );
        self
    }
}

pub struct TestClient {
    http: reqwest::Client,
    base: String,
    cookie: Mutex<Option<String>>,
}

impl TestClient {
    fn new(base: String) -> Self {
        Self {
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            base,
            cookie: Mutex::new(None),
        }
    }

    pub fn cookie(&self) -> Option<String> {
        self.cookie.lock().unwrap().clone()
    }

    pub fn set_cookie(&self, pair: Option<String>) {
        *self.cookie.lock().unwrap() = pair;
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    pub async fn get(&self, path: &str) -> Resp {
        self.send(self.http.get(self.url(path))).await
    }

    pub async fn delete(&self, path: &str) -> Resp {
        self.send(self.http.delete(self.url(path))).await
    }

    pub async fn post_empty(&self, path: &str) -> Resp {
        self.send(self.http.post(self.url(path))).await
    }

    pub async fn post_json(&self, path: &str, body: Value) -> Resp {
        self.post_raw(path, &body.to_string()).await
    }

    pub async fn post_raw(&self, path: &str, body: &str) -> Resp {
        self.send(
            self.http
                .post(self.url(path))
                .header(CONTENT_TYPE, "application/json")
                .body(body.to_string()),
        )
        .await
    }

    pub async fn patch_json(&self, path: &str, body: Value) -> Resp {
        self.send(
            self.http
                .patch(self.url(path))
                .header(CONTENT_TYPE, "application/json")
                .body(body.to_string()),
        )
        .await
    }

    async fn send(&self, mut request: reqwest::RequestBuilder) -> Resp {
        if let Some(cookie) = self.cookie() {
            request = request.header(COOKIE, cookie);
        }
        let response = request.send().await.expect("request failed");
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        if let Some(set_cookie) = headers.get(SET_COOKIE).and_then(|v| v.to_str().ok()) {
            let pair = set_cookie
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            self.set_cookie(if pair.ends_with('=') {
                None
            } else {
                Some(pair)
            });
        }
        let text = response.text().await.unwrap_or_default();
        Resp {
            status,
            headers,
            text,
        }
    }

    // ---- convenience flows ----

    pub async fn register(&self, email: &str, password: &str) -> Resp {
        self.post_json(
            "/api/auth/register",
            json!({ "email": email, "password": password }),
        )
        .await
    }

    pub async fn login(&self, email: &str, password: &str) -> Resp {
        self.post_json(
            "/api/auth/login",
            json!({ "email": email, "password": password }),
        )
        .await
    }

    /// Create a monitor and return its JSON representation (asserts HTTP 201).
    pub async fn create_monitor(&self, body: Value) -> Value {
        self.post_json("/api/monitors", body)
            .await
            .assert_status(201)
            .json()
    }
}

// ---------------------------------------------------------------------------------------------
// TargetServer: endpoints for probes to hit

pub struct TargetServer {
    pub port: u16,
    pub hits: Arc<AtomicUsize>,
    handle: JoinHandle<()>,
}

impl TargetServer {
    pub async fn spawn() -> Self {
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let app = Router::new()
            .route("/ok", get(|| async { "ok" }))
            .route(
                "/teapot",
                get(|| async { (StatusCode::IM_A_TEAPOT, "short and stout") }),
            )
            .route(
                "/slow",
                get(|| async {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    "late"
                }),
            )
            .route("/redirect", get(|| async { Redirect::temporary("/ok") }))
            .route(
                "/echo",
                post(|headers: axum::http::HeaderMap, body: String| async move {
                    if headers.get("x-ping").map(|v| v.as_bytes()) == Some(b"medic")
                        && body == "{\"check\":true}"
                    {
                        StatusCode::CREATED
                    } else {
                        StatusCode::BAD_REQUEST
                    }
                }),
            )
            .route(
                "/count",
                get(move || {
                    let counter = counter.clone();
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        "counted"
                    }
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { port, hits, handle }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }

    pub fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    pub fn stop(&self) {
        self.handle.abort();
    }
}

// ---------------------------------------------------------------------------------------------
// FakeSmtp: a minimal SMTP server that captures messages

pub struct FakeSmtp {
    pub port: u16,
    messages: Arc<Mutex<Vec<String>>>,
    fail_sessions: Arc<AtomicUsize>,
    handle: JoinHandle<()>,
}

impl FakeSmtp {
    pub async fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let messages = Arc::new(Mutex::new(Vec::new()));
        let fail_sessions = Arc::new(AtomicUsize::new(0));
        let (m, f) = (messages.clone(), fail_sessions.clone());
        let handle = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    break;
                };
                let (m, f) = (m.clone(), f.clone());
                tokio::spawn(async move {
                    let _ = smtp_session(socket, m, f).await;
                });
            }
        });
        Self {
            port,
            messages,
            fail_sessions,
            handle,
        }
    }

    /// Make the next `n` SMTP sessions fail with a 421 greeting.
    pub fn fail_next_sessions(&self, n: usize) {
        self.fail_sessions.store(n, Ordering::SeqCst);
    }

    /// Raw captured messages (headers + body).
    pub fn messages(&self) -> Vec<String> {
        self.messages.lock().unwrap().clone()
    }

    pub async fn wait_for_messages(&self, count: usize) -> Vec<String> {
        let deadline = Instant::now() + WAIT;
        loop {
            let messages = self.messages();
            if messages.len() >= count {
                return messages;
            }
            assert!(
                Instant::now() < deadline,
                "expected {count} emails within {WAIT:?}, got {}",
                messages.len()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub fn stop(&self) {
        self.handle.abort();
    }
}

async fn smtp_session(
    socket: TcpStream,
    messages: Arc<Mutex<Vec<String>>>,
    fail_sessions: Arc<AtomicUsize>,
) -> std::io::Result<()> {
    let (reader, mut writer) = socket.into_split();
    let mut lines = BufReader::new(reader).lines();

    let should_fail = loop {
        let current = fail_sessions.load(Ordering::SeqCst);
        if current == 0 {
            break false;
        }
        if fail_sessions
            .compare_exchange(current, current - 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            break true;
        }
    };
    if should_fail {
        writer
            .write_all(b"421 fake.smtp service not available\r\n")
            .await?;
        return Ok(());
    }

    writer.write_all(b"220 fake.smtp ESMTP ready\r\n").await?;
    let mut data = String::new();
    let mut in_data = false;
    while let Some(line) = lines.next_line().await? {
        if in_data {
            if line == "." {
                in_data = false;
                let message = decode_message(std::mem::take(&mut data));
                messages.lock().unwrap().push(message);
                writer.write_all(b"250 OK queued\r\n").await?;
            } else {
                data.push_str(line.strip_prefix('.').unwrap_or(&line));
                data.push_str("\r\n");
            }
            continue;
        }
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("EHLO") || upper.starts_with("HELO") {
            writer
                .write_all(b"250-fake.smtp\r\n250 8BITMIME\r\n")
                .await?;
        } else if upper.starts_with("MAIL FROM")
            || upper.starts_with("RCPT TO")
            || upper.starts_with("RSET")
            || upper.starts_with("NOOP")
        {
            writer.write_all(b"250 OK\r\n").await?;
        } else if upper.starts_with("DATA") {
            in_data = true;
            writer
                .write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n")
                .await?;
        } else if upper.starts_with("QUIT") {
            writer.write_all(b"221 Bye\r\n").await?;
            break;
        } else {
            writer.write_all(b"500 Unknown command\r\n").await?;
        }
    }
    Ok(())
}

/// Decode a quoted-printable body (lettre picks that encoding for long lines) so tests can
/// assert on plain text.
fn decode_message(raw: String) -> String {
    let Some(split) = raw.find("\r\n\r\n") else {
        return raw;
    };
    let (headers, body) = raw.split_at(split + 4);
    if !headers
        .to_ascii_lowercase()
        .contains("content-transfer-encoding: quoted-printable")
    {
        return raw;
    }
    let unfolded = body.replace("=\r\n", "");
    let bytes = unfolded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'=' && i + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&unfolded[i + 1..i + 3], 16) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    format!("{headers}{}", String::from_utf8_lossy(&out))
}
