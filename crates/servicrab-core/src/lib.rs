//! `servicrab-core` — configuration types, loading, validation, and
//! dependency-graph utilities.
//!
//! # Architecture
//!
//! This crate is intentionally dependency-light so that it can be shared by
//! the CLI, its daemon, and any other client without pulling in the full async
//! runtime.  All I/O is delegated to callers.
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
    control_channel, event_channel, plan_stack, Control, ControlTx, EventKind, EventReceiver,
    EventSender, EventSink, ForegroundRunner, Health, LogRouter, LogWriter, OutputMode, RunOptions,
    RunOutcome, Selection, ServiceEvent, ServiceRunner, ServiceStatus, SignalWatcher, StackOptions,
    StackOutcome, StackSupervisor, StatusRegistry, Stream,
};
