//! SSRF guards, the filtering DNS resolver, the rate limiter and credential primitives.

use std::net::IpAddr;
use std::time::{Duration, Instant};

use jsonwebtoken::{EncodingKey, Header};
use serde::Serialize;
use tokio::net::TcpListener;
use uptime_medics::config::Config;
use uptime_medics::infrastructure::http_client::build_http_client;
use uptime_medics::security::ratelimit::RateLimiter;
use uptime_medics::security::{auth, ssrf};
use uptime_medics::services::probe;
use url::Url;

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

#[test]
fn ssrf_blocks_private_reserved_and_mapped_addresses() {
    for blocked in [
        "127.0.0.1",
        "127.255.255.254",
        "10.0.0.1",
        "172.16.0.1",
        "172.31.255.255",
        "192.168.1.1",
        "169.254.169.254",
        "0.0.0.0",
        "100.64.0.1",
        "198.18.0.1",
        "192.0.2.1",
        "224.0.0.1",
        "240.0.0.1",
        "255.255.255.255",
        "::1",
        "::",
        "fc00::1",
        "fd12:3456::1",
        "fe80::1",
        "::ffff:127.0.0.1",
        "::ffff:10.1.2.3",
        "2001:db8::1",
        "ff02::1",
        "2002:7f00:0001::1",
    ] {
        assert!(
            ssrf::is_ip_blocked(ip(blocked)),
            "{blocked} should be blocked"
        );
    }
    for allowed in [
        "1.1.1.1",
        "8.8.8.8",
        "93.184.216.34",
        "172.32.0.1",
        "100.128.0.1",
        "2606:4700:4700::1111",
    ] {
        assert!(
            !ssrf::is_ip_blocked(ip(allowed)),
            "{allowed} should be allowed"
        );
    }
    for host in [
        "localhost",
        "LOCALHOST.",
        "db.localhost",
        "printer.local",
        "vault.internal",
        "pc.localdomain",
    ] {
        assert!(ssrf::is_blocked_hostname(host), "{host} should be blocked");
    }
    assert!(!ssrf::is_blocked_hostname("example.com"));
    assert!(!ssrf::is_blocked_hostname("internal.example.com"));
}

#[test]
fn ssrf_validates_target_urls() {
    assert!(ssrf::validate_target_url(" https://example.com/health ", false).is_ok());
    assert!(ssrf::validate_target_url("http://93.184.216.34:8080/", false).is_ok());
    for bad in [
        "",
        "not a url",
        "ftp://example.com/",
        "file:///etc/passwd",
        "https://",
        "http://localhost/",
        "http://127.0.0.1/",
        "http://[::1]/",
        "http://169.254.169.254/latest/meta-data",
        "http://router.local/",
    ] {
        assert!(
            ssrf::validate_target_url(bad, false).is_err(),
            "{bad:?} should be rejected"
        );
    }
    let too_long = format!("https://example.com/{}", "a".repeat(ssrf::MAX_URL_LEN));
    assert!(ssrf::validate_target_url(&too_long, false).is_err());
    // The escape hatch only relaxes the address rules, not scheme or syntax.
    assert!(ssrf::validate_target_url("http://127.0.0.1/", true).is_ok());
    assert!(ssrf::validate_target_url("http://localhost:3000/", true).is_ok());
    assert!(ssrf::validate_target_url("ftp://127.0.0.1/", true).is_err());
}

#[tokio::test]
async fn ssrf_dns_check_rejects_domains_resolving_to_private_ranges() {
    let local = Url::parse("http://localhost:1234/").unwrap();
    assert!(ssrf::check_resolves_public(&local).await.is_err());
    // IP literals are validated elsewhere and unknown domains are tolerated (the probe fails later).
    assert!(
        ssrf::check_resolves_public(&Url::parse("http://93.184.216.34/").unwrap())
            .await
            .is_ok()
    );
    assert!(
        ssrf::check_resolves_public(&Url::parse("http://does-not-exist.invalid/").unwrap())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn safe_resolver_filters_private_answers_at_probe_time() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let strict = build_http_client(false, true).unwrap();
    let err = strict
        .get(format!("http://localhost:{port}/"))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect_err("localhost must not resolve");
    let message = probe::describe(&err);
    assert!(message.contains("blocked"), "{message}");

    // With the escape hatch the name resolves; the connection then just times out because the
    // listener never answers.
    let relaxed = build_http_client(true, true).unwrap();
    let err = relaxed
        .get(format!("http://localhost:{port}/"))
        .timeout(Duration::from_millis(500))
        .send()
        .await
        .expect_err("nobody answers");
    assert!(err.is_timeout(), "{err}");
}

#[test]
fn rate_limiter_allows_bursts_then_refills() {
    let limiter = RateLimiter::new(3);
    let client = ip("203.0.113.9");
    let start = Instant::now();
    for _ in 0..3 {
        assert!(limiter.try_acquire_at(client, start).is_ok());
    }
    let wait = limiter.try_acquire_at(client, start).unwrap_err();
    assert!(
        (1000..=1300).contains(&wait),
        "one token refills every 20 minutes, got {wait}s"
    );
    assert!(
        limiter.try_acquire_at(ip("203.0.113.10"), start).is_ok(),
        "buckets are per address"
    );
    assert!(limiter
        .try_acquire_at(client, start + Duration::from_secs(20 * 60 + 1))
        .is_ok());
    assert!(limiter
        .try_acquire_at(client, start + Duration::from_secs(20 * 60 + 1))
        .is_err());
    // Refill is continuous: an hour after the last use the bucket is full again.
    let much_later = start + Duration::from_secs(2 * 3600);
    for _ in 0..3 {
        assert!(limiter.try_acquire_at(client, much_later).is_ok());
    }
    assert!(limiter.try_acquire_at(client, much_later).is_err());
    limiter.prune();
    assert!(limiter.try_acquire(ip("203.0.113.11")).is_ok());
}

#[test]
fn password_hashes_are_salted_and_verifiable() {
    let first = auth::hash_password("correct horse battery staple").unwrap();
    let second = auth::hash_password("correct horse battery staple").unwrap();
    assert_ne!(first, second, "each hash uses a fresh salt");
    assert!(first.starts_with("$argon2id$"));
    assert!(auth::verify_password("correct horse battery staple", &first).unwrap());
    assert!(!auth::verify_password("Correct horse battery staple", &first).unwrap());
    assert!(auth::verify_password("anything", "not-a-phc-string").is_err());
}

#[derive(Serialize)]
struct RawClaims<'a> {
    sub: &'a str,
    sid: &'a str,
    iat: i64,
    exp: i64,
}

#[test]
fn session_tokens_roundtrip_and_reject_tampering() {
    let config = Config {
        jwt_secret: "unit-test-secret-that-is-long-enough-0123456789".to_string(),
        ..Config::default()
    };
    let token = auth::issue_token(&config, "user-1", "session-1").unwrap();
    assert_eq!(
        auth::decode_token(&config, &token),
        Some(("user-1".to_string(), "session-1".to_string()))
    );

    let other = Config {
        jwt_secret: "another-secret-entirely-abcdefghijklmnopqrstuvwxyz".to_string(),
        ..Config::default()
    };
    assert_eq!(auth::decode_token(&other, &token), None, "wrong secret");
    assert_eq!(auth::decode_token(&config, "garbage"), None);

    let mut parts: Vec<String> = token.split('.').map(str::to_string).collect();
    parts[1].push('x');
    assert_eq!(
        auth::decode_token(&config, &parts.join(".")),
        None,
        "tampered payload"
    );

    let now = chrono::Utc::now().timestamp();
    let expired = jsonwebtoken::encode(
        &Header::default(),
        &RawClaims {
            sub: "user-1",
            sid: "session-1",
            iat: now - 7200,
            exp: now - 3600,
        },
        &EncodingKey::from_secret(config.jwt_secret.as_bytes()),
    )
    .unwrap();
    assert_eq!(auth::decode_token(&config, &expired), None, "expired token");
}

#[test]
fn cookie_attributes_follow_configuration() {
    let plain = Config {
        cookie_secure: false,
        session_ttl: Duration::from_secs(3600),
        ..Config::default()
    };
    let cookie = auth::session_cookie(&plain, "tok");
    assert!(cookie.starts_with("um_session=tok;"));
    assert!(
        cookie.contains("Max-Age=3600")
            && cookie.contains("HttpOnly")
            && cookie.contains("SameSite=Lax")
            && cookie.contains("Path=/")
    );
    assert!(!cookie.contains("Secure"));
    assert!(auth::clear_cookie(&plain).contains("Max-Age=0"));

    let secure = Config {
        cookie_secure: true,
        ..Config::default()
    };
    assert!(auth::session_cookie(&secure, "tok").ends_with("; Secure"));
    assert!(auth::clear_cookie(&secure).ends_with("; Secure"));
}
