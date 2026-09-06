//! Credential primitives: argon2id password hashing, JWT session tokens and cookie attributes.

use anyhow::{anyhow, Result};
use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

use crate::config::Config;

pub const COOKIE_NAME: &str = "um_session";

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    /// User id.
    sub: String,
    /// Session id (row in `sessions`).
    sid: String,
    iat: i64,
    exp: i64,
}

pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| anyhow!("password hashing failed: {e}"))
}

pub fn verify_password(password: &str, hash: &str) -> Result<bool> {
    let parsed =
        PasswordHash::new(hash).map_err(|e| anyhow!("stored password hash is invalid: {e}"))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

pub fn issue_token(config: &Config, user_id: &str, session_id: &str) -> Result<String> {
    let now = chrono::Utc::now().timestamp();
    let claims = Claims {
        sub: user_id.to_string(),
        sid: session_id.to_string(),
        iat: now,
        exp: now + config.session_ttl.as_secs() as i64,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(config.jwt_secret.as_bytes()),
    )
    .map_err(|e| anyhow!("token encoding failed: {e}"))
}

/// Returns `(user_id, session_id)` for a valid, unexpired token.
pub fn decode_token(config: &Config, token: &str) -> Option<(String, String)> {
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(config.jwt_secret.as_bytes()),
        &Validation::default(),
    )
    .ok()?;
    Some((data.claims.sub, data.claims.sid))
}

pub fn session_cookie(config: &Config, token: &str) -> String {
    format!(
        "{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{}",
        config.session_ttl.as_secs(),
        if config.cookie_secure { "; Secure" } else { "" }
    )
}

pub fn clear_cookie(config: &Config) -> String {
    format!(
        "{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{}",
        if config.cookie_secure { "; Secure" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_roundtrip() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery staple", &hash).unwrap());
        assert!(!verify_password("wrong", &hash).unwrap());
    }

    #[test]
    fn token_roundtrip() {
        std::env::set_var(
            "JWT_SECRET",
            "unit-test-secret-that-is-long-enough-0123456789",
        );
        let config = Config::from_env().unwrap();
        let token = issue_token(&config, "user-1", "session-1").unwrap();
        assert_eq!(
            decode_token(&config, &token),
            Some(("user-1".to_string(), "session-1".to_string()))
        );
        assert_eq!(decode_token(&config, "garbage"), None);
        let cookie = session_cookie(&config, &token);
        assert!(cookie.starts_with("um_session=") && cookie.contains("HttpOnly"));
        assert!(clear_cookie(&config).contains("Max-Age=0"));
    }
}
