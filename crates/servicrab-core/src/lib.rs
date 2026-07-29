//! `servicrab-core` — configuration types, loading, validation, and
//! dependency-graph utilities.
//!
//! # Architecture
//!
//! This crate is intentionally dependency-light so that it can be shared by
//! both the CLI and (in a future phase) the daemon without pulling in the full
//! async runtime.  All I/O is delegated to callers.
//!
//! ## Usage
//!
//! 1. Discover or specify the path to `servicrab.toml`.
//! 2. Call [`load::load`] to read, parse, and validate the file.
//! 3. Use the resulting [`config::Config`] as the runtime configuration.
//!
//! ## Future phases (TODOs)
//!
//! - TODO(phase-2): Add `HealthCheck` configuration (HTTP probe, command
//!   probe, interval, retries).
//! - TODO(phase-2): Add a process-table abstraction for the background daemon.
//! - TODO(phase-3): Support `include` directives to split large configs
//!   across multiple files.

pub mod config;
pub mod error;
pub mod graph;
pub mod lifecycle;
pub mod load;
pub mod raw;
pub mod runtime;
pub mod validation;

// Convenience re-exports.
pub use config::{
    Config, Project, ProjectName, RestartPolicy, Service, ServiceName, ShutdownSignal,
};
pub use error::{ConfigError, ConfigWarning, RuntimeError};
pub use lifecycle::{
    ExitReason, InvalidTransition, ProcessOutcome, RestartDecision, RestartTracker, ServiceState,
    ShutdownReason, StateMachine,
};
pub use load::{discover_config, load, resolve_config_path};
pub use runtime::{
    event_channel, plan_stack, EventKind, EventReceiver, EventSender, EventSink, ForegroundRunner,
    OutputMode, RunOptions, RunOutcome, ServiceEvent, ServiceRunner, SignalWatcher, StackOptions,
    StackOutcome, StackSupervisor, Stream,
};
