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
pub use response::{Event, Health, Response, ServiceInfo, ServiceState, Stream, UNKNOWN};

/// Which revision of this wire format this build speaks.
///
/// It is not the crate version: the two move independently, and a release that
/// changes nothing on the socket must not make every daemon look mismatched.
/// It travels in the `ping`/`pong` exchange as an `Option`, because 0.3 spoke
/// this format without naming it and "did not say" has to stay distinguishable
/// from "said 0".
///
/// Nothing refuses to talk on the strength of this number.  Both sides decode
/// leniently — see [`UNKNOWN`] — so a mismatch is a thing to report, not a thing
/// to fail on, and the report is what turns "my command silently did nothing
/// useful" into "this daemon is older than this client".
pub const PROTOCOL_VERSION: u32 = 1;
