//! SMTP delivery via lettre with exponential-backoff retries.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use lettre::message::{header::ContentType, Mailbox};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use crate::config::{Config, SmtpTls};

const MAX_ATTEMPTS: u32 = 4;
const INITIAL_BACKOFF: Duration = Duration::from_secs(2);

pub struct Mailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
    description: String,
    initial_backoff: Duration,
}

impl Mailer {
    /// Build a mailer from configuration; `Ok(None)` when SMTP is not configured.
    pub fn from_config(config: &Config) -> Result<Option<Mailer>> {
        let Some(smtp) = &config.smtp else {
            return Ok(None);
        };
        let from: Mailbox = smtp
            .from_address
            .parse()
            .map_err(|e| anyhow!("invalid SMTP_FROM_ADDRESS {:?}: {e}", smtp.from_address))?;

        let mut builder = match smtp.tls {
            SmtpTls::StartTls => {
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&smtp.host)
                    .with_context(|| format!("configuring STARTTLS relay {}", smtp.host))?
            }
            SmtpTls::Implicit => AsyncSmtpTransport::<Tokio1Executor>::relay(&smtp.host)
                .with_context(|| format!("configuring TLS relay {}", smtp.host))?,
            SmtpTls::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&smtp.host),
        };
        builder = builder
            .port(smtp.port)
            .timeout(Some(Duration::from_secs(30)));
        if let (Some(user), Some(pass)) = (&smtp.username, &smtp.password) {
            builder = builder.credentials(Credentials::new(user.clone(), pass.clone()));
        }

        Ok(Some(Mailer {
            transport: builder.build(),
            from,
            description: format!("{}:{} ({:?})", smtp.host, smtp.port, smtp.tls),
            initial_backoff: INITIAL_BACKOFF,
        }))
    }

    /// Override the first retry delay (doubles on every further attempt).
    pub fn with_initial_backoff(mut self, delay: Duration) -> Self {
        self.initial_backoff = delay;
        self
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub async fn send(&self, to: &str, subject: &str, body: String) -> Result<()> {
        let to: Mailbox = to
            .parse()
            .map_err(|e| anyhow!("invalid recipient address {to:?}: {e}"))?;
        let email = Message::builder()
            .from(self.from.clone())
            .to(to)
            .subject(subject)
            .header(ContentType::TEXT_PLAIN)
            .body(body)
            .context("building email")?;
        self.transport
            .send(email)
            .await
            .context("SMTP delivery failed")?;
        Ok(())
    }

    /// Send with exponential backoff (by default 2s, 4s, 8s between the four attempts).
    pub async fn send_with_backoff(&self, to: &str, subject: &str, body: String) -> Result<()> {
        let mut delay = self.initial_backoff;
        let mut attempt = 1;
        loop {
            match self.send(to, subject, body.clone()).await {
                Ok(()) => return Ok(()),
                Err(e) if attempt < MAX_ATTEMPTS => {
                    tracing::warn!(
                        attempt,
                        retry_in_secs = delay.as_secs(),
                        error = %e,
                        "email delivery failed; retrying"
                    );
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                    attempt += 1;
                }
                Err(e) => return Err(e.context(format!("giving up after {attempt} attempts"))),
            }
        }
    }
}
