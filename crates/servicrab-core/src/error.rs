//! Structured error and warning types for configuration loading and validation.

use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

/// A single configuration error.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The configuration file could not be read.
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The configuration file could not be parsed as TOML.
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    /// No `servicrab.toml` file found in the directory tree.
    #[error("could not discover servicrab.toml starting from {dir}")]
    ConfigNotFound { dir: PathBuf },

    /// The `version` field is not `1`.
    #[error("unsupported schema version {version}; only version 1 is supported")]
    UnsupportedVersion { version: u32 },

    /// The project name violates naming rules.
    #[error("invalid project name {name:?}: {reason}")]
    InvalidProjectName { name: String, reason: String },

    /// A service name violates naming rules.
    #[error("invalid service name {name:?}: {reason}")]
    InvalidServiceName { name: String, reason: String },

    /// `[services]` is present but contains no entries.
    #[error("no services defined; at least one [services.*] entry is required")]
    NoServices,

    /// A service's `command` list is empty.
    #[error("service {service:?}: command must not be empty")]
    EmptyCommand { service: String },

    /// The first element of `command` (the executable) is empty or contains NUL.
    #[error("service {service:?}: executable must not be empty or contain a NUL byte")]
    InvalidExecutable { service: String },

    /// A command argument contains a NUL byte.
    #[error("service {service:?}: command argument contains a NUL byte")]
    NulInCommandArg { service: String },

    /// The `cwd` path does not exist, is not a directory, or could not be
    /// resolved.
    #[error("service {service:?}: cwd {cwd:?} does not exist or is not a directory")]
    InvalidCwd { service: String, cwd: PathBuf },

    /// A service environment key is invalid (empty, or contains `=` or NUL).
    #[error("service {service:?}: environment key {key:?} is invalid (must be non-empty and must not contain '=' or NUL)")]
    InvalidEnvKey { service: String, key: String },

    /// A project environment key is invalid.
    #[error("project: environment key {key:?} is invalid (must be non-empty and must not contain '=' or NUL)")]
    InvalidProjectEnvKey { key: String },

    /// An environment value contains a NUL byte.
    #[error("service {service:?}: environment value for key {key:?} contains a NUL byte")]
    NulInEnvValue { service: String, key: String },

    /// A project environment value contains a NUL byte.
    #[error("project: environment value for key {key:?} contains a NUL byte")]
    NulInProjectEnvValue { key: String },

    /// A service depends on a service that does not exist.
    #[error("service {service:?}: depends on unknown service {dep:?}")]
    UnknownDependency { service: String, dep: String },

    /// A service depends on itself.
    #[error("service {service:?}: depends on itself")]
    SelfDependency { service: String },

    /// A service lists the same dependency more than once.
    #[error("service {service:?}: duplicate dependency {dep:?}")]
    DuplicateDependency { service: String, dep: String },

    /// A dependency cycle was detected.
    #[error("dependency cycle detected: {cycle}")]
    DependencyCycle { cycle: String },

    /// A duration string could not be parsed.
    #[error("service {service:?}: invalid duration {value:?} for field `{field}`: {reason}")]
    InvalidDuration {
        service: String,
        field: &'static str,
        value: String,
        reason: String,
    },

    /// A duration is outside the allowed range.
    #[error("service {service:?}: field `{field}` is out of range: {reason}")]
    DurationOutOfRange {
        service: String,
        field: &'static str,
        reason: String,
    },

    /// `restart_max_delay` is less than `restart_delay`.
    #[error(
        "service {service:?}: restart_max_delay ({max_delay:?}) must be >= restart_delay ({delay:?})"
    )]
    RestartMaxDelayTooSmall {
        service: String,
        delay: Duration,
        max_delay: Duration,
    },

    /// An unrecognised shutdown signal string.
    #[error("service {service:?}: unknown shutdown_signal {value:?}; expected one of: term, int, quit, hup")]
    InvalidShutdownSignal { service: String, value: String },

    /// The `[health]` table declared no probe at all.
    #[error(
        "service {service:?}: [health] must declare exactly one of `command`, `http` or `tcp`"
    )]
    MissingHealthProbe { service: String },

    /// The `[health]` table declared more than one probe.
    #[error(
        "service {service:?}: [health] declares multiple probes ({probes}); exactly one of `command`, `http` or `tcp` is allowed"
    )]
    ConflictingHealthProbes { service: String, probes: String },

    /// A health probe was declared but is not usable.
    #[error("service {service:?}: invalid [health] {field} probe {value:?}: {reason}")]
    InvalidHealthProbe {
        service: String,
        field: &'static str,
        value: String,
        reason: String,
    },

    /// A malformed byte size in `[project.logs]`.
    #[error("project.logs: invalid size {value:?} for field `{field}`: {reason}")]
    InvalidSize {
        field: &'static str,
        value: String,
        reason: String,
    },

    /// `[project.logs] max_files` is outside the supported range.
    #[error("project.logs: max_files must be between 0 and 100, got {value}")]
    InvalidMaxFiles { value: u32 },

    /// The log directory could not be created.
    #[error("project.logs: log directory {dir} could not be created: {reason}")]
    InvalidLogDir { dir: PathBuf, reason: String },

    /// An unrecognised `on_unhealthy` action.
    #[error(
        "service {service:?}: unknown [health] on_unhealthy {value:?}; expected one of: restart, ignore"
    )]
    InvalidUnhealthyAction { service: String, value: String },
}

/// A non-fatal configuration warning.
#[derive(Debug, Clone)]
pub enum ConfigWarning {
    /// The executable was not found in `PATH` at config-load time.
    ExecutableNotInPath { service: String, executable: String },

    /// A restart-related field was set but `restart = "never"` means it has
    /// no effect.
    RestartSettingsIgnored {
        service: String,
        field: &'static str,
    },
}

impl std::fmt::Display for ConfigWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigWarning::ExecutableNotInPath {
                service,
                executable,
            } => {
                write!(
                    f,
                    "service {service:?}: executable {executable:?} not found in PATH"
                )
            }
            ConfigWarning::RestartSettingsIgnored { service, field } => {
                write!(
                    f,
                    "service {service:?}: field `{field}` has no effect when restart = \"never\""
                )
            }
        }
    }
}

// ── Runtime errors ─────────────────────────────────────────────────────────

/// An error raised while supervising a service process.
///
/// Every variant carries the affected service name so that the CLI can report
/// failures without needing extra context.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// The requested service is not defined in the configuration.
    #[error("unknown service {service:?}; known services: {known}")]
    UnknownService {
        /// The service name that was requested.
        service: String,
        /// Comma-separated list of configured service names.
        known: String,
    },

    /// The configured executable could not be spawned.
    #[error("service {service:?}: failed to spawn {executable:?}: {source}")]
    SpawnFailed {
        /// Affected service.
        service: String,
        /// The executable that could not be started.
        executable: String,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// A signal handler could not be installed.
    #[error("service {service:?}: failed to register handler for {signal}: {source}")]
    SignalRegistrationFailed {
        /// Affected service.
        service: String,
        /// The signal whose handler could not be installed.
        signal: &'static str,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// A signal could not be delivered to the service's process group.
    #[error("service {service:?}: failed to send {signal} to process group {pgid}: {source}")]
    SignalDeliveryFailed {
        /// Affected service.
        service: String,
        /// The signal that could not be delivered.
        signal: String,
        /// Target process-group id.
        pgid: i32,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// Waiting for the child process failed.
    #[error("service {service:?}: failed to wait for child process: {source}")]
    WaitFailed {
        /// Affected service.
        service: String,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// The child could not be placed into its own process group.
    #[error("service {service:?}: failed to set up a process group: {source}")]
    ProcessGroupSetupFailed {
        /// Affected service.
        service: String,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// The forced `SIGKILL` escalation after the shutdown timeout failed.
    #[error("service {service:?}: failed to force-kill process group {pgid} after {timeout:?}: {source}")]
    ForceKillFailed {
        /// Affected service.
        service: String,
        /// Target process-group id.
        pgid: i32,
        /// The shutdown timeout that elapsed before escalation.
        timeout: Duration,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// The service exhausted its restart budget.
    #[error("service {service:?}: giving up after {attempts} restart attempt(s)")]
    RestartLimitExhausted {
        /// Affected service.
        service: String,
        /// Number of restarts that were performed.
        attempts: u32,
    },

    /// The lifecycle state machine rejected a transition.
    #[error("service {service:?}: {source}")]
    InvalidTransition {
        /// Affected service.
        service: String,
        /// The rejected transition.
        #[source]
        source: crate::lifecycle::InvalidTransition,
    },

    /// The foreground runner is not available on this platform.
    #[error("service {service:?}: the foreground runner is only supported on Linux and macOS")]
    UnsupportedPlatform {
        /// Affected service.
        service: String,
    },
}
