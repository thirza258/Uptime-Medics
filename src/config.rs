//! Runtime configuration loaded from environment variables (and `.env` when present).

use std::time::Duration;

use anyhow::{bail, Context, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmtpTls {
    /// Plain connection upgraded with STARTTLS (typical for port 587).
    StartTls,
    /// Implicit TLS from the first byte (typical for port 465).
    Implicit,
    /// No encryption (local relays / test sinks such as Mailpit only).
    None,
}

#[derive(Debug, Clone)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub from_address: String,
    pub tls: SmtpTls,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub database_url: String,
    pub jwt_secret: String,
    pub session_ttl: Duration,
    pub cookie_secure: bool,
    pub public_base_url: Option<String>,
    pub smtp: Option<SmtpConfig>,
    pub probe_concurrency: usize,
    pub scheduler_tick: Duration,
    pub log_flush_interval: Duration,
    pub log_batch_size: usize,
    pub anon_retention_hours: i64,
    pub registered_retention_days: i64,
    pub rate_limit_per_hour: u32,
    pub trust_proxy_headers: bool,
    pub allow_private_targets: bool,
    pub cors_allowed_origins: Vec<String>,
}

impl Default for Config {
    /// The documented defaults, with a random JWT secret. `from_env` layers environment
    /// variables on top of these; tests construct a `Config` from them directly.
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 8080,
            database_url: "sqlite://data/uptime_medics.db".to_string(),
            jwt_secret: random_secret(),
            session_ttl: Duration::from_secs(168 * 3600),
            cookie_secure: false,
            public_base_url: None,
            smtp: None,
            probe_concurrency: 100,
            scheduler_tick: Duration::from_millis(1000),
            log_flush_interval: Duration::from_secs(5),
            log_batch_size: 100,
            anon_retention_hours: 30 * 24,
            registered_retention_days: 30,
            rate_limit_per_hour: 10,
            trust_proxy_headers: false,
            allow_private_targets: false,
            cors_allowed_origins: Vec::new(),
        }
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let defaults = Self::default();

        let host: String = env_or("HOST", defaults.host)?;
        let port: u16 = env_or("PORT", defaults.port)?;
        let database_url: String = env_or("DATABASE_URL", defaults.database_url)?;

        let jwt_secret = match env_opt("JWT_SECRET") {
            Some(secret) => {
                if secret.len() < 32 {
                    tracing::warn!(
                        "JWT_SECRET is shorter than 32 characters; use a long random value in production"
                    );
                }
                secret
            }
            None => {
                tracing::warn!(
                    "JWT_SECRET is not set; using a random secret for this process (all sessions are invalidated on restart)"
                );
                defaults.jwt_secret
            }
        };

        let session_ttl_hours: u64 =
            env_or("SESSION_TTL_HOURS", defaults.session_ttl.as_secs() / 3600)?;
        let public_base_url =
            env_opt("PUBLIC_BASE_URL").map(|u| u.trim_end_matches('/').to_string());
        let cookie_secure = match env_opt("COOKIE_SECURE") {
            Some(v) => parse_bool("COOKIE_SECURE", &v)?,
            None => public_base_url
                .as_deref()
                .map(|u| u.starts_with("https://"))
                .unwrap_or(false),
        };

        let smtp = match env_opt("SMTP_HOST") {
            None => None,
            Some(smtp_host) => {
                let smtp_port: u16 = env_or("SMTP_PORT", 587)?;
                let username = env_opt("SMTP_USERNAME").or_else(|| env_opt("SMTP_USER"));
                let password = env_opt("SMTP_PASSWORD").or_else(|| env_opt("SMTP_PASS"));
                let from_address = env_opt("SMTP_FROM_ADDRESS")
                    .or_else(|| env_opt("SMTP_FROM"))
                    .context("SMTP_FROM_ADDRESS must be set when SMTP_HOST is configured")?;
                let tls = match env_opt("SMTP_TLS").map(|v| v.to_ascii_lowercase()) {
                    None => {
                        if smtp_port == 465 {
                            SmtpTls::Implicit
                        } else {
                            SmtpTls::StartTls
                        }
                    }
                    Some(v) => match v.as_str() {
                        "starttls" | "start_tls" => SmtpTls::StartTls,
                        "tls" | "implicit" | "smtps" | "wrapper" => SmtpTls::Implicit,
                        "none" | "plain" | "off" => SmtpTls::None,
                        other => {
                            bail!("invalid SMTP_TLS={other:?} (expected starttls, tls or none)")
                        }
                    },
                };
                Some(SmtpConfig {
                    host: smtp_host,
                    port: smtp_port,
                    username,
                    password,
                    from_address,
                    tls,
                })
            }
        };

        let probe_concurrency: usize = env_or("PROBE_CONCURRENCY", defaults.probe_concurrency)?;
        let scheduler_tick_ms: u64 = env_or(
            "SCHEDULER_TICK_MS",
            defaults.scheduler_tick.as_millis() as u64,
        )?;
        let log_flush_interval_secs: u64 = env_or(
            "LOG_FLUSH_INTERVAL_SECONDS",
            defaults.log_flush_interval.as_secs(),
        )?;
        let log_batch_size: usize = env_or("LOG_BATCH_SIZE", defaults.log_batch_size)?;
        let anon_retention_hours: i64 =
            env_or("ANON_LOG_RETENTION_HOURS", defaults.anon_retention_hours)?;
        let registered_retention_days: i64 = env_or(
            "REGISTERED_LOG_RETENTION_DAYS",
            defaults.registered_retention_days,
        )?;
        let rate_limit_per_hour: u32 =
            env_or("ANON_RATE_LIMIT_PER_HOUR", defaults.rate_limit_per_hour)?;
        let trust_proxy_headers = env_bool("TRUST_PROXY_HEADERS", defaults.trust_proxy_headers)?;
        let allow_private_targets =
            env_bool("ALLOW_PRIVATE_TARGETS", defaults.allow_private_targets)?;
        let cors_allowed_origins: Vec<String> = env_opt("CORS_ALLOWED_ORIGINS")
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        if probe_concurrency == 0 {
            bail!("PROBE_CONCURRENCY must be at least 1");
        }
        if log_batch_size == 0 {
            bail!("LOG_BATCH_SIZE must be at least 1");
        }
        if scheduler_tick_ms < 100 {
            bail!("SCHEDULER_TICK_MS must be at least 100");
        }
        if rate_limit_per_hour == 0 {
            bail!("ANON_RATE_LIMIT_PER_HOUR must be at least 1");
        }
        if allow_private_targets {
            tracing::warn!(
                "ALLOW_PRIVATE_TARGETS=true: SSRF protection is disabled; monitors may target private/loopback addresses"
            );
        }

        Ok(Self {
            host,
            port,
            database_url,
            jwt_secret,
            session_ttl: Duration::from_secs(session_ttl_hours.max(1) * 3600),
            cookie_secure,
            public_base_url,
            smtp,
            probe_concurrency,
            scheduler_tick: Duration::from_millis(scheduler_tick_ms),
            log_flush_interval: Duration::from_secs(log_flush_interval_secs.max(1)),
            log_batch_size,
            anon_retention_hours,
            registered_retention_days,
            rate_limit_per_hour,
            trust_proxy_headers,
            allow_private_targets,
            cors_allowed_origins,
        })
    }
}

fn random_secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn env_opt(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn env_or<T>(key: &str, default: T) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match env_opt(key) {
        Some(v) => v
            .parse::<T>()
            .map_err(|e| anyhow::anyhow!("invalid value for {key} ({v:?}): {e}")),
        None => Ok(default),
    }
}

fn parse_bool(key: &str, v: &str) -> Result<bool> {
    match v.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => bail!("invalid boolean for {key}: {other:?}"),
    }
}

fn env_bool(key: &str, default: bool) -> Result<bool> {
    match env_opt(key) {
        Some(v) => parse_bool(key, &v),
        None => Ok(default),
    }
}
