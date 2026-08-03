//! `servicrab-core` — configuration types, loading, validation, dependency-graph
//! utilities, and the process runtime.
//!
//! # Architecture
//!
//! This crate does the work; the CLI renders it. It never formats output for a
//! terminal: it returns typed values and publishes structured events, and
//! whoever calls it decides how those look.
//!
//! It is not I/O-free, and it is not sync. [`mod@load`] reads files,
//! [`validation`] walks `PATH` to check that a command exists, the TCP health
//! probe in [`runtime::health`] opens connections, and [`runtime`] is built on
//! `tokio`, which is a full dependency. What the crate avoids is a *second*
//! runtime and the CLI's own dependencies — no `clap`, no styling, no terminal
//! detection — so a different front end can link it without inheriting ours.
//!
//! The process runtime is Linux and macOS only. On other platforms the runtime
//! entry points return [`RuntimeError::UnsupportedPlatform`] rather than failing
//! to compile, so the whole workspace still builds and its tests still run
//! there.
//!
//! There is no semver guarantee on this Rust API. The stable surface of the
//! project is the `servicrab` CLI, its socket protocol and its JSON output; this
//! crate is an implementation detail of those and may change in any release.
//!
//! ## Usage
//!
//! 1. Discover or specify the path to `servicrab.toml`.
//! 2. Call [`load::load`] to read, parse, and validate the file — including
//!    any files it pulls in with `include`.
//! 3. Use the resulting [`config::Config`] as the runtime configuration.

pub mod config;
pub mod envfile;
pub mod error;
pub mod graph;
mod include;
pub mod lifecycle;
pub mod load;
pub mod raw;
pub mod runtime;
mod subst;
pub mod validation;

// Convenience re-exports.
pub use config::{
    Config, Dependency, DependencyCondition, HealthCheck, HealthProbe, LogSettings, Project,
    ProjectName, RestartPolicy, Service, ServiceName, ShutdownSignal, UnhealthyAction,
    WatchSettings,
};
pub use envfile::EnvFileError;
pub use error::{ConfigError, ConfigWarning, RuntimeError};
pub use lifecycle::{
    ExitReason, InvalidTransition, ProcessOutcome, RestartDecision, RestartTracker, ServiceState,
    ShutdownReason, StateMachine,
};
pub use load::{discover_config, load, resolve_config_path};
pub use runtime::filewatch::{spawn_watchers, watched_services};
pub use runtime::{
    control_channel, event_channel, plan_stack, with_dependents, Control, ControlTx, EventKind,
    EventReceiver, EventSender, EventSink, ForegroundRunner, Health, LogRouter, LogSink, LogWriter,
    OutputMode, RunOptions, RunOutcome, Selection, ServiceEvent, ServiceRunner, ServiceStatus,
    SignalWatcher, StackOptions, StackOutcome, StackSupervisor, StatusRegistry, Stream,
};
