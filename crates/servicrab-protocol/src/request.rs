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

    /// Re-read the configuration file and apply the difference.
    Reload,

    /// Follow the daemon's event stream.
    ///
    /// The daemon answers `ok` and then keeps writing
    /// [`crate::Response::Event`] lines until the client disconnects, so this
    /// is the only request that turns a connection one-way.
    Subscribe {
        /// Only report these services; empty means all of them.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        services: Vec<String>,

        /// Whether captured stdout/stderr lines are part of the stream.
        #[serde(default = "yes")]
        logs: bool,
    },
}

/// Serde default for flags that are on unless a client says otherwise.
fn yes() -> bool {
    true
}
