//! Incident state machine: when a monitor becomes DOWN, when it recovers, and which
//! transitions deserve a notification.

use super::monitor::{STATUS_DOWN, STATUS_UP};

/// Incidents open after this many consecutive failed probes (protects against single blips).
pub const FAILURE_THRESHOLD: i64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Down,
    Recovered,
}

/// The monitor state after applying one probe result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    pub status: String,
    pub consecutive_failures: i64,
    pub transition: Option<Transition>,
}

/// Apply a probe outcome to the current state.
///
/// * success            -> `UP`, streak reset; `Recovered` when the monitor was `DOWN`
/// * failure, streak < 2 -> status unchanged (a single blip is not an incident)
/// * failure, streak >= 2 -> `DOWN`; `Down` transition the first time
pub fn evaluate(current_status: &str, consecutive_failures: i64, success: bool) -> Evaluation {
    let was_down = current_status == STATUS_DOWN;
    if success {
        return Evaluation {
            status: STATUS_UP.to_string(),
            consecutive_failures: 0,
            transition: if was_down {
                Some(Transition::Recovered)
            } else {
                None
            },
        };
    }
    let failures = consecutive_failures + 1;
    if failures >= FAILURE_THRESHOLD {
        Evaluation {
            status: STATUS_DOWN.to_string(),
            consecutive_failures: failures,
            transition: if was_down {
                None
            } else {
                Some(Transition::Down)
            },
        }
    } else {
        Evaluation {
            status: current_status.to_string(),
            consecutive_failures: failures,
            transition: None,
        }
    }
}
