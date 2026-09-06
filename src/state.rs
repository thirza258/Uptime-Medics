//! Shared application state: the composition root wires configuration, the database pool,
//! outbound HTTP clients, the alert manager and in-memory guards together at start-up.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use sqlx::SqlitePool;
use tokio::sync::{mpsc, Semaphore};

use crate::config::Config;
use crate::domain::check_log::CheckLogInsert;
use crate::domain::stats::StatsResponse;
use crate::infrastructure::http_client::build_http_client;
use crate::infrastructure::mailer::Mailer;
use crate::security::ratelimit::RateLimiter;
use crate::services::alerts::AlertManager;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: SqlitePool,
    /// Follows redirects (up to 5 hops, SSRF-checked). Used unless a 3xx status is expected.
    pub http_follow: reqwest::Client,
    /// Never follows redirects. Used when the expected status is itself a 3xx.
    pub http_no_follow: reqwest::Client,
    pub rate_limiter: Arc<RateLimiter>,
    pub alerts: Arc<AlertManager>,
    pub log_tx: mpsc::Sender<CheckLogInsert>,
    pub in_flight: Arc<Mutex<HashSet<String>>>,
    pub probe_semaphore: Arc<Semaphore>,
    pub stats_cache: Arc<Mutex<Option<(Instant, StatsResponse)>>>,
    pub started_at: Instant,
}

impl AppState {
    pub fn new(
        config: Arc<Config>,
        pool: SqlitePool,
    ) -> anyhow::Result<(Self, mpsc::Receiver<CheckLogInsert>)> {
        let allow_private = config.allow_private_targets;
        let http_follow = build_http_client(allow_private, true)?;
        let http_no_follow = build_http_client(allow_private, false)?;

        let mailer = Mailer::from_config(&config)?;
        match &mailer {
            Some(m) => tracing::info!(smtp = m.description(), "SMTP alerting enabled"),
            None => tracing::warn!("SMTP_HOST not set: email alerts are disabled"),
        }
        let alerts = Arc::new(AlertManager::new(mailer, config.public_base_url.clone()));
        let (log_tx, log_rx) = mpsc::channel(10_000);

        let state = Self {
            rate_limiter: Arc::new(RateLimiter::new(config.rate_limit_per_hour)),
            probe_semaphore: Arc::new(Semaphore::new(config.probe_concurrency)),
            config,
            pool,
            http_follow,
            http_no_follow,
            alerts,
            log_tx,
            in_flight: Arc::new(Mutex::new(HashSet::new())),
            stats_cache: Arc::new(Mutex::new(None)),
            started_at: Instant::now(),
        };
        Ok((state, log_rx))
    }

    pub fn http_client_for(&self, expected_status: i64) -> &reqwest::Client {
        if (300..400).contains(&expected_status) {
            &self.http_no_follow
        } else {
            &self.http_follow
        }
    }
}
