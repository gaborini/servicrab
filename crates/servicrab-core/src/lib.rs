//! `servicrab-core` — configuration models, validation, and service lifecycle
//! state types.
//!
//! # Architecture notes
//!
//! This crate is intentionally dependency-light so that it can be shared by
//! both the CLI and (in a future phase) the daemon without pulling in the full
//! async runtime.  All I/O is delegated to callers.
//!
//! ## Future phases (TODOs)
//!
//! - TODO(phase-2): Add `HealthCheck` configuration (HTTP probe, command probe,
//!   interval, retries).
//! - TODO(phase-2): Add `ServiceState` machine transitions and a process-table
//!   abstraction for the background daemon.
//! - TODO(phase-3): Support `include` directives to split large configs across
//!   multiple files.
//! - TODO(phase-3): Schema versioning (`config_version`) with migration helpers.

pub mod config;
pub mod lifecycle;
pub mod validation;

pub use config::{Config, ProjectConfig, RestartPolicy, ServiceConfig};
pub use lifecycle::ServiceState;
pub use validation::ValidationError;
