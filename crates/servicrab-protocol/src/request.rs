//! Request types sent from the CLI (or other clients) to the daemon.
//!
//! # Future phases (TODOs)
//!
//! - TODO(phase-2): Expand with `Start { service: String }`,
//!   `Stop { service: String }`, `Restart { service: String }`,
//!   `Reload {}` (hot-reload config), and `Logs { service: String,
//!   follow: bool }`.
//! - TODO(phase-2): Add a `request_id: uuid::Uuid` field for request/response
//!   correlation.

use serde::{Deserialize, Serialize};

/// A request that a client sends to the servicrab daemon.
///
/// This is a placeholder that will grow substantially in phase 2.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Request {
    /// Ask the daemon to report its own status (health check).
    Ping,

    /// Ask the daemon to return a list of all known services and their states.
    ListServices,

    /// Ask the daemon for the current state of a single service.
    GetService {
        /// Name of the service as declared in `servicrab.toml`.
        name: String,
    },
}
