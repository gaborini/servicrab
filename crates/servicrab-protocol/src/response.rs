//! Response types sent from the daemon back to clients.

use serde::{Deserialize, Serialize};

/// The lifecycle state of a service, as reported by the daemon.
///
/// This mirrors `servicrab_core::ServiceState`, but the protocol crate stays
/// independent of the runtime so that clients need not depend on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ServiceState {
    /// Not started yet, or waiting for a dependency.
    Pending,
    /// The process is being spawned.
    Starting,
    /// The process is alive.
    Running,
    /// Waiting before a restart.
    Backoff,
    /// Being shut down.
    Stopping,
    /// Stopped on request, and will not restart.
    Stopped,
    /// The process ended on its own.
    Exited,
    /// The service failed fatally.
    Failed,
}

impl std::fmt::Display for ServiceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            ServiceState::Pending => "pending",
            ServiceState::Starting => "starting",
            ServiceState::Running => "running",
            ServiceState::Backoff => "backoff",
            ServiceState::Stopping => "stopping",
            ServiceState::Stopped => "stopped",
            ServiceState::Exited => "exited",
            ServiceState::Failed => "failed",
        };
        f.write_str(text)
    }
}

/// What the health checks say about a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Health {
    /// The service has no health check.
    None,
    /// The service has a health check that has not passed yet.
    Starting,
    /// The last probe succeeded.
    Healthy,
    /// The service exhausted its retry budget.
    Unhealthy,
}

impl std::fmt::Display for Health {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Health::None => "-",
            Health::Starting => "starting",
            Health::Healthy => "healthy",
            Health::Unhealthy => "unhealthy",
        };
        f.write_str(text)
    }
}

/// A point-in-time report about one service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceInfo {
    /// Service name as declared in `servicrab.toml`.
    pub name: String,
    /// Current lifecycle state.
    pub state: ServiceState,
    /// Process-group id of the running process, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<i32>,
    /// How long the current process has been running, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_secs: Option<u64>,
    /// How many times the service has been restarted.
    pub restarts: u32,
    /// Health-check verdict.
    pub health: Health,
    /// The most recent noteworthy message (an error, a probe failure, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// A response returned by the servicrab daemon to a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Response {
    /// The requested operation completed successfully.
    Ok {
        /// Optional human-readable message.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },

    /// Answer to [`crate::Request::Ping`].
    Pong {
        /// Project the daemon supervises.
        project: String,
        /// The daemon's own process id.
        pid: u32,
    },

    /// Answer to [`crate::Request::Status`].
    Status {
        /// One entry per service, in start order.
        services: Vec<ServiceInfo>,
    },

    /// The requested operation failed.
    Error {
        /// Human-readable description of what went wrong.
        message: String,
    },
}
