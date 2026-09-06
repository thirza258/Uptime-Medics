//! Incident notifications: DOWN alerts and recovery emails with flap suppression.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub use crate::domain::incident::Transition;
use crate::domain::monitor::Monitor;
use crate::infrastructure::mailer::Mailer;

/// A monitor that changes state more than `FLAP_THRESHOLD` times within `FLAP_WINDOW` is
/// considered flapping; its notifications are suppressed until it calms down.
const FLAP_WINDOW: Duration = Duration::from_secs(30 * 60);
const FLAP_THRESHOLD: usize = 4;

pub struct AlertManager {
    mailer: Option<Mailer>,
    public_base_url: Option<String>,
    transitions: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl AlertManager {
    pub fn new(mailer: Option<Mailer>, public_base_url: Option<String>) -> Self {
        Self {
            mailer,
            public_base_url,
            transitions: Mutex::new(HashMap::new()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.mailer.is_some()
    }

    /// Record a state transition and report whether the monitor is flapping.
    pub fn record_transition(&self, monitor_id: &str, now: Instant) -> bool {
        let mut map = self.transitions.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() > 20_000 {
            map.retain(|_, q| {
                q.back()
                    .is_some_and(|t| now.duration_since(*t) <= FLAP_WINDOW)
            });
        }
        let queue = map.entry(monitor_id.to_string()).or_default();
        while let Some(front) = queue.front() {
            if now.duration_since(*front) > FLAP_WINDOW {
                queue.pop_front();
            } else {
                break;
            }
        }
        queue.push_back(now);
        queue.len() > FLAP_THRESHOLD
    }

    /// Dispatch a notification for `monitor` (already updated with its new state) in the
    /// background.
    pub fn notify(self: &Arc<Self>, monitor: Monitor, transition: Transition, reason: String) {
        let flapping = self.record_transition(&monitor.id, Instant::now());
        let Some(to) = monitor.alert_email.clone().filter(|e| !e.trim().is_empty()) else {
            return;
        };
        if self.mailer.is_none() {
            tracing::info!(
                monitor_id = %monitor.id,
                ?transition,
                "alert email not sent: SMTP is not configured"
            );
            return;
        }
        if flapping {
            tracing::warn!(
                monitor_id = %monitor.id,
                ?transition,
                "monitor is flapping; notification suppressed"
            );
            return;
        }

        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let (subject, body) = manager.compose(&monitor, transition, &reason);
            let mailer = manager.mailer.as_ref().expect("mailer checked above");
            match mailer.send_with_backoff(&to, &subject, body).await {
                Ok(()) => {
                    tracing::info!(monitor_id = %monitor.id, ?transition, to = %to, "notification sent")
                }
                Err(e) => {
                    tracing::error!(monitor_id = %monitor.id, ?transition, to = %to, error = %e, "notification failed")
                }
            }
        });
    }

    fn compose(&self, m: &Monitor, transition: Transition, reason: &str) -> (String, String) {
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
        let link = self
            .public_base_url
            .as_ref()
            .map(|base| format!("Details: {base}/#monitor={}\n", m.id))
            .unwrap_or_default();
        match transition {
            Transition::Down => (
                format!("[ALERT] Monitor DOWN: {}", m.name),
                format!(
                    "Your monitor '{name}' is DOWN.\n\n\
                     Target URL: {url}\n\
                     Method: {method}\n\
                     Failure reason: {reason}\n\
                     Consecutive failures: {failures}\n\
                     Time: {now}\n\
                     {link}\n\
                     You will receive a recovery notification when the endpoint responds with status {expected} again.\n\n\
                     -- Uptime Medics",
                    name = m.name,
                    url = m.url,
                    method = m.method,
                    failures = m.consecutive_failures,
                    expected = m.expected_status_code,
                ),
            ),
            Transition::Recovered => (
                format!("[RESOLVED] Monitor UP: {}", m.name),
                format!(
                    "Good news: your monitor '{name}' is back UP.\n\n\
                     Target URL: {url}\n\
                     Method: {method}\n\
                     Latest result: {reason}\n\
                     Time: {now}\n\
                     {link}\n\
                     -- Uptime Medics",
                    name = m.name,
                    url = m.url,
                    method = m.method,
                ),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_flapping() {
        let manager = AlertManager::new(None, None);
        let start = Instant::now();
        for i in 0..FLAP_THRESHOLD {
            assert!(!manager.record_transition("m1", start + Duration::from_secs(i as u64)));
        }
        assert!(manager.record_transition("m1", start + Duration::from_secs(10)));
        // Transitions outside the window are forgotten.
        assert!(!manager.record_transition("m1", start + FLAP_WINDOW + Duration::from_secs(60)));
        assert!(!manager.record_transition("m2", start));
    }
}
