//! Request payloads accepted by the API.

use serde::{Deserialize, Deserializer};

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    #[serde(default)]
    pub range: crate::domain::history::HistoryRange,
}

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateMonitorRequest {
    pub url: String,
    pub method: Option<String>,
    pub name: Option<String>,
    /// Either a JSON object or a string containing a JSON object.
    pub headers: Option<serde_json::Value>,
    pub body: Option<String>,
    pub check_interval_seconds: Option<i64>,
    pub timeout_seconds: Option<i64>,
    pub expected_status_code: Option<i64>,
    pub alert_email: Option<String>,
}

/// Partial update. `Option<Option<T>>` fields distinguish "not provided" (outer `None`)
/// from "set to null" (inner `None`).
#[derive(Debug, Default, Deserialize)]
pub struct UpdateMonitorRequest {
    pub name: Option<String>,
    pub url: Option<String>,
    pub method: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    pub headers: Option<Option<serde_json::Value>>,
    #[serde(default, deserialize_with = "double_option")]
    pub body: Option<Option<String>>,
    pub check_interval_seconds: Option<i64>,
    pub timeout_seconds: Option<i64>,
    pub expected_status_code: Option<i64>,
    #[serde(default, deserialize_with = "double_option")]
    pub alert_email: Option<Option<String>>,
    pub is_active: Option<bool>,
}

fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}
