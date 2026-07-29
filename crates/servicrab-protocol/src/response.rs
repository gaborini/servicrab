//! Response types sent from the daemon back to clients.
//!
//! # Future phases (TODOs)
//!
//! - TODO(phase-2): Add typed success payloads per request variant.
//! - TODO(phase-2): Add structured error codes (`ErrorCode` enum) so that
//!   clients can react programmatically rather than pattern-matching on
//!   error strings.
//! - TODO(phase-2): Add `ServiceInfo` with PID, uptime, restart count, etc.

use serde::{Deserialize, Serialize};

/// A response returned by the servicrab daemon to a client.
///
/// This is a placeholder that will grow substantially in phase 2.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Response {
    /// The requested operation completed successfully.
    Ok {
        /// Optional human-readable message.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },

    /// The requested operation failed.
    Error {
        /// Human-readable description of what went wrong.
        message: String,
    },
}
