//! Background daemon: state paths, the socket server, and the client used by
//! `start`, `status` and `down`.
//!
//! Only Unix is supported; on other platforms the commands fail with a clear
//! message instead of pretending to work.

pub mod paths;
pub mod stopped;

#[cfg(unix)]
pub mod client;
#[cfg(unix)]
pub mod server;

pub use paths::DaemonPaths;
