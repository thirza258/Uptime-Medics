//! Uptime Medics library crate. Everything except the binary entry point lives here so the
//! integration tests in `tests/` can drive the application in-process.
//!
//! Layered layout (each layer depends only on the ones below it):
//! - `api`            HTTP delivery: router, handlers, extractors, middleware, errors
//! - `services`       application logic: probes, scheduler, alerts, sessions
//! - `security`       argon2/JWT primitives, SSRF guards, rate limiting
//! - `infrastructure` adapters: SQLite, SMTP, outbound HTTP client
//! - `domain`         entities, statuses, incident rules and input validation (no I/O)
//! - `config`/`state` configuration and the shared state wired together by the binary

pub mod api;
pub mod config;
pub mod domain;
pub mod infrastructure;
pub mod security;
pub mod services;
pub mod state;
