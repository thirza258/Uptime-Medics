//! Individual probe results.

use chrono::{DateTime, Utc};
use serde::Serialize;

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct CheckLog {
    pub id: i64,
    pub monitor_id: String,
    pub status_code: Option<i64>,
    pub response_time_ms: i64,
    pub is_successful: bool,
    pub error_message: Option<String>,
    pub checked_at: DateTime<Utc>,
}

/// A probe result waiting to be persisted by the batched log writer.
#[derive(Debug, Clone)]
pub struct CheckLogInsert {
    pub monitor_id: String,
    pub status_code: Option<i64>,
    pub response_time_ms: i64,
    pub is_successful: bool,
    pub error_message: Option<String>,
    pub checked_at: String,
}
