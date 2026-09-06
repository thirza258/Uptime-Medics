//! Cross-cutting protection: credential primitives, SSRF guards and rate limiting.

pub mod auth;
pub mod ratelimit;
pub mod ssrf;
