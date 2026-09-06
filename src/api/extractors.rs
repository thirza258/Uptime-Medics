//! Request extractors: the authenticated user (required or optional) and client IP resolution.

use std::net::{IpAddr, SocketAddr};
use std::ops::Deref;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{header, HeaderMap};

use crate::api::error::ApiError;
use crate::domain::user::CurrentUser;
use crate::security::auth::COOKIE_NAME;
use crate::services::session;
use crate::state::AppState;

/// Rejects the request with 401 when no valid session cookie is present.
pub struct AuthUser(pub CurrentUser);

impl Deref for AuthUser {
    type Target = CurrentUser;

    fn deref(&self) -> &CurrentUser {
        &self.0
    }
}

/// `Some(user)` for a valid session, `None` for anonymous visitors.
pub struct OptionalAuthUser(pub Option<CurrentUser>);

pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    for value in headers.get_all(header::COOKIE) {
        let Ok(raw) = value.to_str() else { continue };
        for pair in raw.split(';') {
            if let Some((key, val)) = pair.trim().split_once('=') {
                if key.trim() == name {
                    return Some(val.trim().to_string());
                }
            }
        }
    }
    None
}

/// Client address for rate limiting; proxy headers are honoured only when configured.
pub fn client_ip(headers: &HeaderMap, addr: SocketAddr, trust_proxy: bool) -> IpAddr {
    if trust_proxy {
        if let Some(forwarded) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            if let Some(first) = forwarded.split(',').next() {
                if let Ok(ip) = first.trim().parse::<IpAddr>() {
                    return ip;
                }
            }
        }
        if let Some(real) = headers.get("x-real-ip").and_then(|v| v.to_str().ok()) {
            if let Ok(ip) = real.trim().parse::<IpAddr>() {
                return ip;
            }
        }
    }
    addr.ip()
}

async fn current_user(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<CurrentUser>, ApiError> {
    let Some(token) = cookie_value(headers, COOKIE_NAME) else {
        return Ok(None);
    };
    Ok(session::resolve_session(&state.pool, &state.config, &token).await?)
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        current_user(state, &parts.headers)
            .await?
            .map(AuthUser)
            .ok_or_else(ApiError::unauthorized)
    }
}

impl FromRequestParts<AppState> for OptionalAuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(OptionalAuthUser(current_user(state, &parts.headers).await?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cookie_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            "a=1; um_session=tok.en.value; b=2".parse().unwrap(),
        );
        assert_eq!(
            cookie_value(&headers, COOKIE_NAME).as_deref(),
            Some("tok.en.value")
        );
        assert_eq!(cookie_value(&headers, "missing"), None);
    }

    #[test]
    fn resolves_client_ip() {
        let addr: SocketAddr = "10.0.0.9:4321".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.5, 10.0.0.1".parse().unwrap());
        assert_eq!(client_ip(&headers, addr, false).to_string(), "10.0.0.9");
        assert_eq!(client_ip(&headers, addr, true).to_string(), "203.0.113.5");
        headers.clear();
        headers.insert("x-real-ip", "198.51.100.7".parse().unwrap());
        assert_eq!(client_ip(&headers, addr, true).to_string(), "198.51.100.7");
        headers.clear();
        assert_eq!(client_ip(&headers, addr, true).to_string(), "10.0.0.9");
    }
}
