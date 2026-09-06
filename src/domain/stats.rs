//! Landing-page health metrics.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct StatsResponse {
    pub total_monitors: i64,
    pub public_monitors: i64,
    pub up: i64,
    pub down: i64,
    pub pending: i64,
    pub total_checks: i64,
    pub alerting_enabled: bool,
}
