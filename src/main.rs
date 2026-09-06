//! Uptime Medics binary: loads configuration, wires the application together and serves it.
//! All modules live in the library crate (`src/lib.rs`).

use uptime_medics::{api, config, infrastructure, services, state};

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let mut args = std::env::args().skip(1);
    let command = args.next();

    // `healthcheck` is used by the Docker HEALTHCHECK; keep it quiet and dependency-free.
    if command.as_deref() == Some("healthcheck") {
        let port: u16 = std::env::var("PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(8080);
        let url = format!("http://127.0.0.1:{port}/api/health");
        let response = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()?
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        if response.status().is_success() {
            println!("ok");
            return Ok(());
        }
        anyhow::bail!("unhealthy: {} returned {}", url, response.status());
    }

    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn,hyper=warn,tower_http=info")),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = Arc::new(config::Config::from_env().context("loading configuration")?);

    match command.as_deref() {
        None => {}
        Some("smtp-test") => {
            let to = args
                .next()
                .context("usage: uptime-medics smtp-test <recipient@example.com>")?;
            let mailer = infrastructure::mailer::Mailer::from_config(&config)?.context(
                "SMTP is not configured; set SMTP_HOST, SMTP_PORT, SMTP_USERNAME, SMTP_PASSWORD and SMTP_FROM_ADDRESS",
            )?;
            mailer
                .send_with_backoff(
                    &to,
                    "[TEST] Uptime Medics SMTP check",
                    "If you can read this, Uptime Medics can deliver alert emails.\n".to_string(),
                )
                .await?;
            println!("Test email sent to {to} via {}", mailer.description());
            return Ok(());
        }
        Some(other) => anyhow::bail!(
            "unknown command {other:?} (available: smtp-test <recipient>, healthcheck)"
        ),
    }

    let pool = infrastructure::db::init(&config.database_url).await?;
    let (state, log_rx) = state::AppState::new(config.clone(), pool)?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let scheduler = tokio::spawn(services::scheduler::run(state.clone(), log_rx, shutdown_rx));

    let app = api::router(state.clone());
    let addr: SocketAddr = format!("{}:{}", config.host, config.port)
        .parse()
        .with_context(|| format!("invalid HOST/PORT: {}:{}", config.host, config.port))?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, alerting = state.alerts.enabled(), "Uptime Medics is listening");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("HTTP server error")?;

    tracing::info!("HTTP server stopped; flushing pending check logs");
    let _ = shutdown_tx.send(true);
    if tokio::time::timeout(Duration::from_secs(15), scheduler)
        .await
        .is_err()
    {
        tracing::warn!("background scheduler did not stop within 15s");
    }
    tracing::info!("bye");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
