//! HTTP probe execution: any supported verb, custom headers, payload, strict timeout, and an
//! exact expected status code.

use std::time::{Duration, Instant};

use reqwest::{Client, Method};

use crate::domain::validation::parse_headers;

pub const MAX_ERROR_LEN: usize = 500;

#[derive(Debug, Clone)]
pub struct ProbeSpec<'a> {
    pub url: &'a str,
    pub method: &'a str,
    pub headers_json: Option<&'a str>,
    pub body: Option<&'a str>,
    pub timeout_secs: u64,
    pub expected_status: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    pub status_code: Option<u16>,
    pub response_time_ms: u64,
    pub is_successful: bool,
    pub error_message: Option<String>,
}

impl ProbeResult {
    pub fn failure(response_time_ms: u64, status_code: Option<u16>, message: String) -> Self {
        Self {
            status_code,
            response_time_ms,
            is_successful: false,
            error_message: Some(truncate(message, MAX_ERROR_LEN)),
        }
    }
}

pub async fn execute_probe(client: &Client, spec: &ProbeSpec<'_>) -> ProbeResult {
    let method = Method::from_bytes(spec.method.as_bytes()).unwrap_or(Method::GET);
    let timeout = Duration::from_secs(spec.timeout_secs.clamp(1, 60));

    let mut request = client.request(method.clone(), spec.url).timeout(timeout);
    if let Some(json) = spec.headers_json {
        match parse_headers(json) {
            Ok(headers) => request = request.headers(headers),
            Err(e) => return ProbeResult::failure(0, None, format!("Invalid custom headers: {e}")),
        }
    }
    if let Some(body) = spec.body {
        if !body.is_empty() && method != Method::HEAD {
            request = request.body(body.to_owned());
        }
    }

    let started = Instant::now();
    match request.send().await {
        Ok(response) => {
            // Headers have arrived: that is the success criterion. Drop the body unread.
            let elapsed = elapsed_ms(started);
            let status = response.status();
            drop(response);
            let code = status.as_u16();
            if code == spec.expected_status {
                ProbeResult {
                    status_code: Some(code),
                    response_time_ms: elapsed,
                    is_successful: true,
                    error_message: None,
                }
            } else {
                let reason = status.canonical_reason().unwrap_or("Unexpected status");
                ProbeResult::failure(
                    elapsed,
                    Some(code),
                    format!("Status {code} {reason} (expected {})", spec.expected_status),
                )
            }
        }
        Err(err) => {
            let elapsed = elapsed_ms(started);
            let message = if err.is_timeout() {
                format!(
                    "TIMEOUT_EXCEEDED (no response within {}s)",
                    timeout.as_secs()
                )
            } else if err.is_connect() {
                format!("Connection failed: {}", describe(&err))
            } else if err.is_redirect() {
                format!("Redirect error: {}", describe(&err))
            } else {
                describe(&err)
            };
            ProbeResult::failure(elapsed, None, message)
        }
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

/// Human-readable root cause of a reqwest error (the deepest one or two sources).
pub fn describe(err: &reqwest::Error) -> String {
    let mut chain: Vec<String> = Vec::new();
    let mut current = std::error::Error::source(err);
    while let Some(e) = current {
        chain.push(e.to_string());
        current = e.source();
    }
    match chain.len() {
        0 => {
            let text = err.to_string();
            match text.find(" for url (") {
                Some(index) => text[..index].to_string(),
                None => text,
            }
        }
        1 => chain[0].clone(),
        n => {
            let (outer, inner) = (&chain[n - 2], &chain[n - 1]);
            if outer.contains(inner.as_str()) {
                outer.clone()
            } else {
                format!("{outer}: {inner}")
            }
        }
    }
}

fn truncate(mut s: String, max: usize) -> String {
    if s.chars().count() > max {
        s = s.chars().take(max - 1).collect::<String>() + "…";
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, Router};

    async fn spawn_target() -> u16 {
        let app = Router::new()
            .route("/ok", get(|| async { "ok" }))
            .route(
                "/teapot",
                get(|| async { (axum::http::StatusCode::IM_A_TEAPOT, "short and stout") }),
            )
            .route(
                "/slow",
                get(|| async {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    "late"
                }),
            )
            .route(
                "/echo",
                axum::routing::post(|headers: axum::http::HeaderMap, body: String| async move {
                    if headers.get("x-ping").map(|v| v.as_bytes()) == Some(b"medic")
                        && body == "{\"check\":true}"
                    {
                        axum::http::StatusCode::CREATED
                    } else {
                        axum::http::StatusCode::BAD_REQUEST
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        port
    }

    #[tokio::test]
    async fn probes_local_targets() {
        let port = spawn_target().await;
        let client = crate::infrastructure::http_client::build_http_client(true, true).unwrap();
        let base = format!("http://127.0.0.1:{port}");

        let ok = execute_probe(
            &client,
            &ProbeSpec {
                url: &format!("{base}/ok"),
                method: "GET",
                headers_json: None,
                body: None,
                timeout_secs: 5,
                expected_status: 200,
            },
        )
        .await;
        assert!(ok.is_successful, "{ok:?}");
        assert_eq!(ok.status_code, Some(200));

        let head = execute_probe(
            &client,
            &ProbeSpec {
                url: &format!("{base}/ok"),
                method: "HEAD",
                headers_json: None,
                body: None,
                timeout_secs: 5,
                expected_status: 200,
            },
        )
        .await;
        assert!(head.is_successful, "{head:?}");

        let mismatch = execute_probe(
            &client,
            &ProbeSpec {
                url: &format!("{base}/teapot"),
                method: "GET",
                headers_json: None,
                body: None,
                timeout_secs: 5,
                expected_status: 200,
            },
        )
        .await;
        assert!(!mismatch.is_successful);
        assert_eq!(mismatch.status_code, Some(418));
        assert!(mismatch
            .error_message
            .as_deref()
            .unwrap()
            .starts_with("Status 418"));

        let expected_teapot = execute_probe(
            &client,
            &ProbeSpec {
                url: &format!("{base}/teapot"),
                method: "GET",
                headers_json: None,
                body: None,
                timeout_secs: 5,
                expected_status: 418,
            },
        )
        .await;
        assert!(expected_teapot.is_successful);

        let timeout = execute_probe(
            &client,
            &ProbeSpec {
                url: &format!("{base}/slow"),
                method: "GET",
                headers_json: None,
                body: None,
                timeout_secs: 1,
                expected_status: 200,
            },
        )
        .await;
        assert!(!timeout.is_successful);
        assert!(
            timeout
                .error_message
                .as_deref()
                .unwrap()
                .starts_with("TIMEOUT_EXCEEDED"),
            "{timeout:?}"
        );

        let post = execute_probe(
            &client,
            &ProbeSpec {
                url: &format!("{base}/echo"),
                method: "POST",
                headers_json: Some(
                    "{\"X-Ping\": \"medic\", \"Content-Type\": \"application/json\"}",
                ),
                body: Some("{\"check\":true}"),
                timeout_secs: 5,
                expected_status: 201,
            },
        )
        .await;
        assert!(post.is_successful, "{post:?}");

        let refused = execute_probe(
            &client,
            &ProbeSpec {
                url: "http://127.0.0.1:9/ok",
                method: "GET",
                headers_json: None,
                body: None,
                timeout_secs: 3,
                expected_status: 200,
            },
        )
        .await;
        assert!(!refused.is_successful);
        assert!(
            refused
                .error_message
                .as_deref()
                .unwrap()
                .contains("Connection"),
            "{refused:?}"
        );
    }

    #[tokio::test]
    async fn safe_resolver_blocks_private_hostnames() {
        let port = spawn_target().await;
        let client = crate::infrastructure::http_client::build_http_client(false, true).unwrap();
        let blocked = execute_probe(
            &client,
            &ProbeSpec {
                url: &format!("http://localhost:{port}/ok"),
                method: "GET",
                headers_json: None,
                body: None,
                timeout_secs: 5,
                expected_status: 200,
            },
        )
        .await;
        assert!(!blocked.is_successful);
        assert!(
            blocked
                .error_message
                .as_deref()
                .unwrap()
                .contains("blocked"),
            "{blocked:?}"
        );
    }
}
