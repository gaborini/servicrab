//! Request types sent from the CLI (or other clients) to the daemon.

use serde::{Deserialize, Serialize};

/// A request that a client sends to the servicrab daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Request {
    /// Ask the daemon whether it is alive.
    Ping,

    /// Ask the daemon for the current state of every service.
    Status,

    /// Ask the daemon to stop the whole stack and exit.
    Shutdown,

    /// Start one service that is currently stopped.
    StartService {
        /// Service name as declared in `servicrab.toml`.
        name: String,
    },

    /// Stop one service, leaving the rest of the stack alone.
    StopService {
        /// Service name as declared in `servicrab.toml`.
        name: String,
    },

    /// Stop one service and start it again.
    RestartService {
        /// Service name as declared in `servicrab.toml`.
        name: String,
    },
}
