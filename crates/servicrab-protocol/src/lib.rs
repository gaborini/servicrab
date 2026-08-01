//! `servicrab-protocol` — the wire format spoken between the `servicrab` CLI
//! and the background daemon.
//!
//! The transport is a Unix domain socket carrying newline-delimited JSON: the
//! client writes one [`Request`] per line and reads one [`Response`] per line.
//! Both sides use [`frame::encode`] / [`frame::decode`], so the framing rules
//! live in exactly one place.
//!
//! The crate deliberately depends on neither the runtime nor Tokio: it is just
//! types plus (de)serialization, which keeps it usable from scripts, tests and
//! any future non-Rust client.

#![deny(missing_docs)]

pub mod frame;
pub mod request;
pub mod response;

pub use frame::{decode, encode, FrameError};
pub use request::Request;
pub use response::{
    Event, Health, ReloadChanges, Response, ServiceInfo, ServiceState, Stream, ErrorCode,
    SCHEMA_VERSION,
};
