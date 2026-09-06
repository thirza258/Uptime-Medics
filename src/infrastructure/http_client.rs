//! Shared, pooled outbound HTTP client used by the probe engine.

use std::sync::Arc;
use std::time::Duration;

use crate::security::ssrf::{self, SafeResolver};

/// Build a client with rustls TLS, keep-alive pooling, the SSRF-filtering DNS resolver and the
/// SSRF-aware redirect policy (or no redirects at all).
pub fn build_http_client(
    allow_private: bool,
    follow_redirects: bool,
) -> anyhow::Result<reqwest::Client> {
    let resolver = Arc::new(SafeResolver { allow_private });
    let redirect = if follow_redirects {
        ssrf::redirect_policy(allow_private)
    } else {
        reqwest::redirect::Policy::none()
    };
    reqwest::Client::builder()
        .user_agent(format!(
            "UptimeMedics/{} (uptime monitor)",
            env!("CARGO_PKG_VERSION")
        ))
        .dns_resolver(resolver)
        .redirect(redirect)
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(4)
        .tcp_keepalive(Duration::from_secs(60))
        .use_rustls_tls()
        .build()
        .map_err(|e| anyhow::anyhow!("building HTTP client: {e}"))
}
