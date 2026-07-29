//! `servicrab-protocol` — shared request/response types for the future local
//! daemon API.
//!
//! # Architecture notes
//!
//! In a future phase the `servicrab` CLI will communicate with a background
//! daemon process over a Unix domain socket (or a named pipe on Windows).
//! This crate contains the wire types that will be serialized/deserialized by
//! both sides.
//!
//! ## Future phases (TODOs)
//!
//! - TODO(phase-2): Define a framing protocol (length-prefixed JSON or
//!   MessagePack) for the Unix socket transport.
//! - TODO(phase-2): Add request/response pairs for all daemon commands:
//!   `Start`, `Stop`, `Restart`, `Status`, `Logs`, `Reload`.
//! - TODO(phase-2): Add streaming response types for log tailing.
//! - TODO(phase-3): Add authentication/capability tokens so that multiple
//!   projects can share a single host daemon.

pub mod request;
pub mod response;

pub use request::Request;
pub use response::Response;
