//! Domain rules: the incident state machine, entity helpers, serialisation and input validation.

use serde_json::json;
use uptime_medics::domain::incident::{evaluate, Transition, FAILURE_THRESHOLD};
use uptime_medics::domain::monitor::{
    Monitor, MonitorWithLatest, ALLOWED_METHODS, STATUS_DOWN, STATUS_PENDING, STATUS_UP,
};
use uptime_medics::domain::user::{CurrentUser, UserResponse};
use uptime_medics::domain::validation as rules;
use url::Url;

fn sample_monitor() -> Monitor {
    Monitor {
        id: "m1".into(),
        user_id: Some("u1".into()),
        name: "Sample".into(),
        url: "https://example.com/".into(),
        method: "GET".into(),
        headers: Some("{\"Authorization\":\"Bearer x\"}".into()),
        body: Some("{}".into()),
        check_interval_seconds: 60,
        timeout_seconds: 10,
        expected_status_code: 200,
        alert_email: Some("ops@example.com".into()),
        is_anonymous: false,
        is_active: true,
        current_status: STATUS_PENDING.into(),
        consecutive_failures: 0,
        last_checked_at: None,
        created_at: chrono::Utc::now(),
    }
}

#[test]
fn incident_state_machine_covers_every_transition() {
    assert_eq!(FAILURE_THRESHOLD, 2);
    let cases = [
        // (status, failures, success) -> (status, failures, transition)
        (STATUS_PENDING, 0, true, STATUS_UP, 0, None),
        (STATUS_PENDING, 0, false, STATUS_PENDING, 1, None),
        (
            STATUS_PENDING,
            1,
            false,
            STATUS_DOWN,
            2,
            Some(Transition::Down),
        ),
        (STATUS_UP, 0, true, STATUS_UP, 0, None),
        (STATUS_UP, 0, false, STATUS_UP, 1, None),
        (STATUS_UP, 1, false, STATUS_DOWN, 2, Some(Transition::Down)),
        (STATUS_UP, 1, true, STATUS_UP, 0, None),
        (STATUS_DOWN, 2, false, STATUS_DOWN, 3, None),
        (
            STATUS_DOWN,
            5,
            true,
            STATUS_UP,
            0,
            Some(Transition::Recovered),
        ),
        // Streak reset by an edit while DOWN: still down, no duplicate alert.
        (STATUS_DOWN, 0, false, STATUS_DOWN, 1, None),
        (
            STATUS_DOWN,
            0,
            true,
            STATUS_UP,
            0,
            Some(Transition::Recovered),
        ),
    ];
    for (status, failures, ok, want_status, want_failures, want_transition) in cases {
        let e = evaluate(status, failures, ok);
        assert_eq!(
            (e.status.as_str(), e.consecutive_failures, e.transition),
            (want_status, want_failures, want_transition),
            "from ({status}, {failures}, success={ok})"
        );
    }
}

#[test]
fn monitor_ownership_and_redaction() {
    let monitor = sample_monitor();
    assert!(monitor.is_owned_by("u1"));
    assert!(!monitor.is_owned_by("u2"));
    let redacted = monitor.redact();
    assert!(
        redacted.headers.is_none() && redacted.body.is_none() && redacted.alert_email.is_none()
    );
    assert_eq!(
        redacted.name, "Sample",
        "non-sensitive fields survive redaction"
    );

    let anonymous = Monitor {
        user_id: None,
        ..sample_monitor()
    };
    assert!(!anonymous.is_owned_by("u1"));

    let with_latest = MonitorWithLatest {
        monitor: sample_monitor(),
        response_time_ms: Some(42),
        last_error: None,
    }
    .redact();
    assert!(with_latest.monitor.headers.is_none());
    assert_eq!(with_latest.response_time_ms, Some(42));
}

#[test]
fn monitor_serialisation_hides_owner_and_skips_absent_secrets() {
    let full = serde_json::to_value(sample_monitor()).unwrap();
    assert!(
        full.get("user_id").is_none(),
        "owner id is never serialised"
    );
    assert_eq!(full["headers"], "{\"Authorization\":\"Bearer x\"}");
    assert_eq!(full["alert_email"], "ops@example.com");
    assert!(full["last_checked_at"].is_null());

    let redacted = serde_json::to_value(sample_monitor().redact()).unwrap();
    assert!(
        redacted.get("headers").is_none()
            && redacted.get("body").is_none()
            && redacted.get("alert_email").is_none()
    );

    let flattened = serde_json::to_value(MonitorWithLatest {
        monitor: sample_monitor(),
        response_time_ms: Some(42),
        last_error: Some("Status 502 Bad Gateway (expected 200)".into()),
    })
    .unwrap();
    assert_eq!(flattened["name"], "Sample", "monitor fields are flattened");
    assert_eq!(flattened["response_time_ms"], 42);
    assert_eq!(
        flattened["last_error"],
        "Status 502 Bad Gateway (expected 200)"
    );

    let user: UserResponse = CurrentUser {
        id: "u1".into(),
        email: "a@b.c".into(),
    }
    .into();
    assert_eq!(
        serde_json::to_value(user).unwrap(),
        json!({ "id": "u1", "email": "a@b.c" })
    );
}

#[test]
fn method_rules() {
    assert_eq!(
        ALLOWED_METHODS,
        &["HEAD", "GET", "POST", "PATCH", "PUT", "DELETE", "OPTIONS"]
    );
    for method in ALLOWED_METHODS {
        assert_eq!(
            rules::normalize_method(Some(&method.to_lowercase())).unwrap(),
            *method
        );
    }
    assert_eq!(rules::normalize_method(None).unwrap(), "GET");
    assert_eq!(rules::normalize_method(Some("  ")).unwrap(), "GET");
    assert!(rules::normalize_method(Some("TRACE")).is_err());
    assert!(rules::normalize_method(Some("CONNECT"))
        .unwrap_err()
        .to_string()
        .contains("Allowed methods"));
}

#[test]
fn name_interval_timeout_and_status_rules() {
    let url = Url::parse("https://api.example.com/v1/health?x=1").unwrap();
    assert_eq!(
        rules::normalize_name(None, &url).unwrap(),
        "api.example.com/v1/health"
    );
    assert_eq!(
        rules::normalize_name(Some(""), &Url::parse("https://example.com").unwrap()).unwrap(),
        "example.com"
    );
    assert_eq!(
        rules::normalize_name(Some("  My API "), &url).unwrap(),
        "My API"
    );
    let long_path = Url::parse(&format!("https://example.com/{}", "p".repeat(300))).unwrap();
    assert_eq!(
        rules::normalize_name(None, &long_path)
            .unwrap()
            .chars()
            .count(),
        rules::MAX_NAME_LEN
    );
    assert!(rules::normalize_name(Some(&"n".repeat(rules::MAX_NAME_LEN + 1)), &url).is_err());

    assert!(rules::validate_interval(rules::MIN_INTERVAL_SECS - 1).is_err());
    assert!(rules::validate_interval(rules::MIN_INTERVAL_SECS).is_ok());
    assert!(rules::validate_interval(rules::MAX_INTERVAL_SECS).is_ok());
    assert!(rules::validate_interval(rules::MAX_INTERVAL_SECS + 1).is_err());
    assert!(rules::validate_timeout(0).is_err());
    assert!(rules::validate_timeout(1).is_ok() && rules::validate_timeout(60).is_ok());
    assert!(rules::validate_timeout(61).is_err());
    assert!(rules::validate_status(99).is_err());
    assert!(rules::validate_status(100).is_ok() && rules::validate_status(599).is_ok());
    assert!(rules::validate_status(600).is_err());
}

#[test]
fn header_and_body_rules() {
    let as_string = json!("{\"X-Ping\": \"Uptime Medics\", \"X-Num\": 5}");
    assert_eq!(
        rules::normalize_headers(Some(&as_string)).unwrap().unwrap(),
        "{\"X-Num\":\"5\",\"X-Ping\":\"Uptime Medics\"}"
    );
    let as_object = json!({ "Authorization": "Bearer t", "Enabled": true });
    assert_eq!(
        rules::normalize_headers(Some(&as_object)).unwrap().unwrap(),
        "{\"Authorization\":\"Bearer t\",\"Enabled\":\"true\"}"
    );
    assert_eq!(rules::normalize_headers(None).unwrap(), None);
    assert_eq!(rules::normalize_headers(Some(&json!(null))).unwrap(), None);
    assert_eq!(rules::normalize_headers(Some(&json!("  "))).unwrap(), None);
    assert_eq!(rules::normalize_headers(Some(&json!({}))).unwrap(), None);
    assert!(rules::normalize_headers(Some(&json!([1]))).is_err());
    assert!(rules::normalize_headers(Some(&json!("not json"))).is_err());
    assert!(rules::normalize_headers(Some(&json!({ "bad header": "x" }))).is_err());
    assert!(rules::normalize_headers(Some(&json!({ "X": { "nested": true } }))).is_err());
    assert!(rules::normalize_headers(Some(&json!({ "X": "line\nbreak" }))).is_err());
    let too_many: serde_json::Map<String, serde_json::Value> = (0..rules::MAX_HEADER_COUNT + 1)
        .map(|i| (format!("X-{i}"), json!("v")))
        .collect();
    assert!(rules::normalize_headers(Some(&serde_json::Value::Object(too_many))).is_err());
    let huge = json!({ "X-Big": "v".repeat(rules::MAX_HEADERS_BYTES) });
    assert!(rules::normalize_headers(Some(&huge)).is_err());

    let map = rules::parse_headers("{\"Content-Type\": \"application/json\"}").unwrap();
    assert_eq!(map.get("content-type").unwrap(), "application/json");

    assert_eq!(rules::normalize_body(None).unwrap(), None);
    assert_eq!(rules::normalize_body(Some(String::new())).unwrap(), None);
    assert_eq!(
        rules::normalize_body(Some("{}".into())).unwrap(),
        Some("{}".into())
    );
    assert!(rules::normalize_body(Some("x".repeat(rules::MAX_BODY_BYTES + 1))).is_err());
}

#[test]
fn email_and_password_rules() {
    assert_eq!(
        rules::normalize_email_opt(Some(" Ops@Example.com ")).unwrap(),
        Some("ops@example.com".into())
    );
    assert_eq!(rules::normalize_email_opt(Some("")).unwrap(), None);
    assert_eq!(rules::normalize_email_opt(None).unwrap(), None);
    assert!(rules::normalize_email_opt(Some("nope")).is_err());
    assert!(rules::normalize_email_opt(Some("two@@example.com")).is_err());

    assert_eq!(
        rules::normalize_email(" Someone@Example.COM ").unwrap(),
        "someone@example.com"
    );
    assert!(rules::normalize_email("").is_err());
    assert!(rules::normalize_email("not-an-email").is_err());
    assert!(
        rules::normalize_email(&format!("{}@example.com", "a".repeat(rules::MAX_EMAIL_LEN)))
            .is_err()
    );

    assert!(rules::validate_password("1234567").is_err());
    assert!(rules::validate_password("12345678").is_ok());
    assert!(
        rules::validate_password("ünïcödé!").is_ok(),
        "length is counted in characters"
    );
    assert!(rules::validate_password(&"p".repeat(rules::MAX_PASSWORD_CHARS + 1)).is_err());
    let err = rules::validate_password("x").unwrap_err();
    assert!(err.to_string().contains("at least 8"));
}
