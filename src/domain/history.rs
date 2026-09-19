//! Uptime measured from recorded checks. Missing checks are never treated as successes.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::check_log::CheckLog;

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
pub enum HistoryRange {
    #[default]
    #[serde(rename = "24h")]
    Day,
    #[serde(rename = "7d")]
    Week,
    #[serde(rename = "30d")]
    Month,
}

impl HistoryRange {
    pub fn seconds(self) -> i64 {
        match self {
            Self::Day => 24 * 3600,
            Self::Week => 7 * 24 * 3600,
            Self::Month => 30 * 24 * 3600,
        }
    }

    pub fn bucket_seconds(self) -> i64 {
        match self {
            Self::Day => 3600,
            Self::Week => 6 * 3600,
            Self::Month => 24 * 3600,
        }
    }
}

#[derive(Debug, Default, Serialize)]
pub struct HistorySummary {
    pub total_checks: i64,
    pub successful_checks: i64,
    pub failed_checks: i64,
    pub uptime_percent: Option<f64>,
    pub avg_response_time_ms: Option<f64>,
    pub first_checked_at: Option<DateTime<Utc>>,
    pub last_checked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct HistoryBucket {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    #[serde(flatten)]
    pub summary: HistorySummary,
}

#[derive(Debug, Serialize)]
pub struct HistoryResponse {
    pub range: HistoryRange,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub bucket_seconds: i64,
    /// None means that registered check history is retained indefinitely.
    pub retention_hours: Option<i64>,
    pub summary: HistorySummary,
    pub buckets: Vec<HistoryBucket>,
    pub recent_checks: Vec<CheckLog>,
}
