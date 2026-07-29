//! Service lifecycle state types.
//!
//! These types model the *observed* state of a supervised service process.
//! They are kept here (in `servicrab-core`) so that they can be shared between
//! the CLI output layer and the future daemon without pulling in the async
//! runtime.
//!
//! ## Future phases (TODOs)
//!
//! - TODO(phase-2): Implement the full state-machine transitions:
//!   `Stopped → Starting → Running → Stopping → Stopped`
//!   and add the `Backoff` / `CrashLoop` states for restart policies.
//! - TODO(phase-2): Store the OS PID and start timestamp inside `Running`.
//! - TODO(phase-3): Add `Degraded` state for services that pass the health
//!   check but with warnings.

use serde::{Deserialize, Serialize};

/// Observable state of a single managed service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    /// The service has not been started yet (or was explicitly stopped and will
    /// not be restarted).
    Stopped,

    /// The service process is being launched (exec in progress).
    ///
    /// TODO(phase-2): Track PID once available.
    Starting,

    /// The service process is alive and (optionally) has passed its health
    /// check.
    ///
    /// TODO(phase-2): Embed `pid: u32` and `started_at: std::time::Instant`.
    Running,

    /// A graceful-shutdown signal has been sent; waiting for the process to
    /// exit.
    Stopping,

    /// The service exited with a non-zero status and is waiting for the backoff
    /// delay before the next restart attempt.
    ///
    /// TODO(phase-2): Add `attempt: u32` and `next_restart_at` timestamp.
    Backoff,

    /// The service failed too many times in a row and the supervisor has given
    /// up restarting it.
    ///
    /// TODO(phase-2): Threshold configurable per service.
    Failed,
}

impl std::fmt::Display for ServiceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceState::Stopped => write!(f, "stopped"),
            ServiceState::Starting => write!(f, "starting"),
            ServiceState::Running => write!(f, "running"),
            ServiceState::Stopping => write!(f, "stopping"),
            ServiceState::Backoff => write!(f, "backoff"),
            ServiceState::Failed => write!(f, "failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display() {
        assert_eq!(ServiceState::Stopped.to_string(), "stopped");
        assert_eq!(ServiceState::Running.to_string(), "running");
        assert_eq!(ServiceState::Failed.to_string(), "failed");
    }
}
