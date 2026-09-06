//! Input rules for monitors and accounts: methods, intervals, timeouts, status codes, names,
//! custom headers, emails and passwords. Pure functions; the HTTP layer maps `ValidationError`
//! to a 400 response.

use std::fmt;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use url::Url;

use super::monitor::ALLOWED_METHODS;

pub const MAX_NAME_LEN: usize = 120;
pub const MAX_BODY_BYTES: usize = 64 * 1024;
pub const MAX_HEADERS_BYTES: usize = 8 * 1024;
pub const MAX_HEADER_COUNT: usize = 25;
pub const MIN_INTERVAL_SECS: i64 = 30;
pub const MAX_INTERVAL_SECS: i64 = 86_400;
pub const MIN_TIMEOUT_SECS: i64 = 1;
pub const MAX_TIMEOUT_SECS: i64 = 60;
pub const MIN_PASSWORD_CHARS: usize = 8;
pub const MAX_PASSWORD_CHARS: usize = 256;
pub const MAX_EMAIL_LEN: usize = 254;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError(pub String);

impl ValidationError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ValidationError {}

pub type ValidationResult<T> = Result<T, ValidationError>;

pub fn normalize_method(method: Option<&str>) -> ValidationResult<String> {
    let method = method
        .map(|m| m.trim().to_ascii_uppercase())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "GET".to_string());
    if ALLOWED_METHODS.contains(&method.as_str()) {
        Ok(method)
    } else {
        Err(ValidationError::new(format!(
            "Unsupported method '{method}'. Allowed methods: {}",
            ALLOWED_METHODS.join(", ")
        )))
    }
}

/// Trimmed name, or a name derived from the URL when none was given.
pub fn normalize_name(name: Option<&str>, url: &Url) -> ValidationResult<String> {
    let trimmed = name.map(str::trim).unwrap_or("");
    if trimmed.is_empty() {
        let host = url.host_str().unwrap_or("monitor");
        let path = url.path();
        let derived = if path.is_empty() || path == "/" {
            host.to_string()
        } else {
            format!("{host}{path}")
        };
        return Ok(truncate_chars(&derived, MAX_NAME_LEN));
    }
    if trimmed.chars().count() > MAX_NAME_LEN {
        return Err(ValidationError::new(format!(
            "Name must be at most {MAX_NAME_LEN} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        s.chars().take(max - 1).collect::<String>() + "…"
    } else {
        s.to_string()
    }
}

/// Parse a JSON header object into a `HeaderMap`, validating names and values.
pub fn parse_headers(json: &str) -> ValidationResult<HeaderMap> {
    let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(json)
        .map_err(|e| ValidationError::new(format!("headers must be a JSON object: {e}")))?;
    let mut out = HeaderMap::new();
    for (key, value) in map {
        let name = HeaderName::from_bytes(key.trim().as_bytes())
            .map_err(|_| ValidationError::new(format!("invalid header name '{key}'")))?;
        let raw = match value {
            serde_json::Value::String(s) => s,
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            _ => {
                return Err(ValidationError::new(format!(
                    "header '{key}' must have a string value"
                )))
            }
        };
        let value = HeaderValue::from_str(raw.trim())
            .map_err(|_| ValidationError::new(format!("invalid value for header '{key}'")))?;
        out.append(name, value);
    }
    Ok(out)
}

/// Accepts a JSON object or a string containing one; returns a canonical JSON string.
pub fn normalize_headers(value: Option<&serde_json::Value>) -> ValidationResult<Option<String>> {
    use serde_json::Value;
    let Some(value) = value else {
        return Ok(None);
    };
    let map: serde_json::Map<String, Value> = match value {
        Value::Null => return Ok(None),
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                return Ok(None);
            }
            if s.len() > MAX_HEADERS_BYTES {
                return Err(ValidationError::new("Headers are too large (max 8 KB)"));
            }
            serde_json::from_str(s).map_err(|e| {
                ValidationError::new(format!(
                    "Headers must be a JSON object of header names to values: {e}"
                ))
            })?
        }
        Value::Object(map) => map.clone(),
        _ => {
            return Err(ValidationError::new(
                "Headers must be a JSON object (or a string containing a JSON object)",
            ))
        }
    };
    if map.is_empty() {
        return Ok(None);
    }
    if map.len() > MAX_HEADER_COUNT {
        return Err(ValidationError::new(format!(
            "At most {MAX_HEADER_COUNT} custom headers are allowed"
        )));
    }
    let mut normalized = serde_json::Map::new();
    for (key, value) in map {
        let text = match value {
            Value::String(s) => s,
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            _ => {
                return Err(ValidationError::new(format!(
                    "Header '{key}' must have a string value"
                )))
            }
        };
        normalized.insert(key.trim().to_string(), Value::String(text));
    }
    let json = Value::Object(normalized).to_string();
    if json.len() > MAX_HEADERS_BYTES {
        return Err(ValidationError::new("Headers are too large (max 8 KB)"));
    }
    parse_headers(&json).map_err(|e| ValidationError::new(format!("Invalid headers: {e}")))?;
    Ok(Some(json))
}

pub fn normalize_body(body: Option<String>) -> ValidationResult<Option<String>> {
    match body {
        None => Ok(None),
        Some(b) if b.is_empty() => Ok(None),
        Some(b) if b.len() > MAX_BODY_BYTES => Err(ValidationError::new(
            "Request body is too large (max 64 KB)",
        )),
        Some(b) => Ok(Some(b)),
    }
}

pub fn validate_interval(seconds: i64) -> ValidationResult<i64> {
    if (MIN_INTERVAL_SECS..=MAX_INTERVAL_SECS).contains(&seconds) {
        Ok(seconds)
    } else {
        Err(ValidationError::new(format!(
            "check_interval_seconds must be between {MIN_INTERVAL_SECS} and {MAX_INTERVAL_SECS}"
        )))
    }
}

pub fn validate_timeout(seconds: i64) -> ValidationResult<i64> {
    if (MIN_TIMEOUT_SECS..=MAX_TIMEOUT_SECS).contains(&seconds) {
        Ok(seconds)
    } else {
        Err(ValidationError::new(format!(
            "timeout_seconds must be between {MIN_TIMEOUT_SECS} and {MAX_TIMEOUT_SECS}"
        )))
    }
}

pub fn validate_status(code: i64) -> ValidationResult<i64> {
    if (100..=599).contains(&code) {
        Ok(code)
    } else {
        Err(ValidationError::new(
            "expected_status_code must be an HTTP status code between 100 and 599",
        ))
    }
}

/// Lower-cased, RFC-validated email; `None` when empty.
pub fn normalize_email_opt(email: Option<&str>) -> ValidationResult<Option<String>> {
    let Some(email) = email.map(str::trim).filter(|e| !e.is_empty()) else {
        return Ok(None);
    };
    let lower = email.to_ascii_lowercase();
    if lower.len() > MAX_EMAIL_LEN || lower.parse::<lettre::Address>().is_err() {
        return Err(ValidationError::new(
            "alert_email is not a valid email address",
        ));
    }
    Ok(Some(lower))
}

/// Lower-cased, RFC-validated account email.
pub fn normalize_email(raw: &str) -> ValidationResult<String> {
    let email = raw.trim().to_ascii_lowercase();
    if email.is_empty() || email.len() > MAX_EMAIL_LEN || email.parse::<lettre::Address>().is_err()
    {
        return Err(ValidationError::new("Please provide a valid email address"));
    }
    Ok(email)
}

pub fn validate_password(password: &str) -> ValidationResult<()> {
    let len = password.chars().count();
    if len < MIN_PASSWORD_CHARS {
        return Err(ValidationError::new(format!(
            "Password must be at least {MIN_PASSWORD_CHARS} characters"
        )));
    }
    if len > MAX_PASSWORD_CHARS {
        return Err(ValidationError::new(format!(
            "Password must be at most {MAX_PASSWORD_CHARS} characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_methods() {
        assert_eq!(normalize_method(None).unwrap(), "GET");
        assert_eq!(normalize_method(Some(" post ")).unwrap(), "POST");
        assert!(normalize_method(Some("TRACE")).is_err());
    }

    #[test]
    fn parses_headers() {
        let map = parse_headers("{\"Authorization\": \"Bearer x\", \"X-Num\": 5}").unwrap();
        assert_eq!(map.get("authorization").unwrap(), "Bearer x");
        assert_eq!(map.get("x-num").unwrap(), "5");
        assert!(parse_headers("[1,2]").is_err());
        assert!(parse_headers("{\"bad name\": \"v\"}").is_err());
        assert!(parse_headers("{\"X\": \"line\\nbreak\"}").is_err());
    }

    #[test]
    fn normalizes_headers_from_string_or_object() {
        let as_string = serde_json::Value::String("{\"X-Ping\": \"Uptime Medics\"}".into());
        assert_eq!(
            normalize_headers(Some(&as_string)).unwrap().unwrap(),
            "{\"X-Ping\":\"Uptime Medics\"}"
        );
        let as_object = serde_json::json!({"Authorization": "Bearer t", "X-Num": 7});
        assert_eq!(
            normalize_headers(Some(&as_object)).unwrap().unwrap(),
            "{\"Authorization\":\"Bearer t\",\"X-Num\":\"7\"}"
        );
        assert_eq!(
            normalize_headers(Some(&serde_json::Value::String("  ".into()))).unwrap(),
            None
        );
        assert!(normalize_headers(Some(&serde_json::json!([1]))).is_err());
        assert!(normalize_headers(Some(&serde_json::json!({"bad header": "x"}))).is_err());
    }

    #[test]
    fn derives_names_and_validates_ranges() {
        let url = Url::parse("https://api.example.com/v1/health").unwrap();
        assert_eq!(
            normalize_name(None, &url).unwrap(),
            "api.example.com/v1/health"
        );
        assert_eq!(normalize_name(Some("  My API "), &url).unwrap(), "My API");
        assert!(validate_interval(29).is_err());
        assert!(validate_interval(30).is_ok());
        assert!(validate_timeout(61).is_err());
        assert!(validate_status(99).is_err());
        assert!(validate_status(301).is_ok());
        assert_eq!(
            normalize_email_opt(Some(" Ops@Example.com "))
                .unwrap()
                .unwrap(),
            "ops@example.com"
        );
        assert!(normalize_email_opt(Some("nope")).is_err());
        assert_eq!(normalize_email_opt(Some("")).unwrap(), None);
    }

    #[test]
    fn validates_accounts() {
        assert_eq!(
            normalize_email(" Someone@Example.COM ").unwrap(),
            "someone@example.com"
        );
        assert!(normalize_email("not-an-email").is_err());
        assert!(validate_password("short").is_err());
        assert!(validate_password("long enough password").is_ok());
    }
}
