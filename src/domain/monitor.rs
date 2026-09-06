//! Monitor entity and its read models.

use chrono::{DateTime, Utc};
use serde::Serialize;

pub const ALLOWED_METHODS: &[&str] = &["HEAD", "GET", "POST", "PATCH", "PUT", "DELETE", "OPTIONS"];
pub const STATUS_UP: &str = "UP";
pub const STATUS_DOWN: &str = "DOWN";
pub const STATUS_PENDING: &str = "PENDING";

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Monitor {
    pub id: String,
    #[serde(skip_serializing)]
    pub user_id: Option<String>,
    pub name: String,
    pub url: String,
    pub method: String,
    /// JSON object string (header name -> value). Hidden from non-owners.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<String>,
    /// Raw request payload. Hidden from non-owners.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub check_interval_seconds: i64,
    pub timeout_seconds: i64,
    pub expected_status_code: i64,
    /// Hidden from non-owners.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alert_email: Option<String>,
    pub is_anonymous: bool,
    pub is_active: bool,
    pub current_status: String,
    pub consecutive_failures: i64,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl Monitor {
    pub fn is_owned_by(&self, user_id: &str) -> bool {
        self.user_id.as_deref() == Some(user_id)
    }

    /// Strip fields that may contain secrets before showing a monitor to the public.
    pub fn redact(mut self) -> Self {
        self.headers = None;
        self.body = None;
        self.alert_email = None;
        self
    }
}

/// A monitor joined with the latest check result.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct MonitorWithLatest {
    #[sqlx(flatten)]
    #[serde(flatten)]
    pub monitor: Monitor,
    pub response_time_ms: Option<i64>,
    pub last_error: Option<String>,
}

impl MonitorWithLatest {
    pub fn redact(mut self) -> Self {
        self.monitor = self.monitor.redact();
        self
    }
}

/// Row shape for `GET /api/public/random-monitors`.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct RandomMonitor {
    pub id: String,
    pub name: String,
    pub url: String,
    pub method: String,
    pub current_status: String,
    pub response_time_ms: Option<i64>,
    pub last_error: Option<String>,
    pub last_checked_at: Option<DateTime<Utc>>,
}
