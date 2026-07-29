//! Validated runtime configuration model.
//!
//! These types are produced by the validation pipeline in
//! [`crate::validation`] and are the *only* types that the CLI and the future
//! daemon should use after the config file has been loaded.
//!
//! No public constructor accepts the raw TOML model directly; all config
//! must flow through [`crate::load::load`].

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;

// ── Name newtypes ──────────────────────────────────────────────────────────

/// A validated project name.
///
/// Rules: 1–64 ASCII bytes, starts with an ASCII alphanumeric character,
/// remaining characters are ASCII alphanumerics, `.`, `_`, or `-`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ProjectName(pub(crate) String);

impl ProjectName {
    /// Return the name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProjectName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A validated service name.
///
/// Rules: 1–48 ASCII bytes, starts with an ASCII alphanumeric character,
/// remaining characters are ASCII alphanumerics, `.`, `_`, or `-`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ServiceName(pub(crate) String);

impl ServiceName {
    /// Return the name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ServiceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

// ── Policy enums ───────────────────────────────────────────────────────────

/// Restart policy for a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    /// Never restart the process (default).
    #[default]
    Never,
    /// Restart only when the process exits with a non-zero status.
    OnFailure,
    /// Always restart, regardless of exit status.
    Always,
}

impl std::fmt::Display for RestartPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RestartPolicy::Never => write!(f, "never"),
            RestartPolicy::OnFailure => write!(f, "on-failure"),
            RestartPolicy::Always => write!(f, "always"),
        }
    }
}

/// Signal sent to a service when requesting graceful shutdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ShutdownSignal {
    /// `SIGTERM` (default).
    #[default]
    Term,
    /// `SIGINT`.
    Int,
    /// `SIGQUIT`.
    Quit,
    /// `SIGHUP`.
    Hup,
}

impl std::fmt::Display for ShutdownSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShutdownSignal::Term => write!(f, "term"),
            ShutdownSignal::Int => write!(f, "int"),
            ShutdownSignal::Quit => write!(f, "quit"),
            ShutdownSignal::Hup => write!(f, "hup"),
        }
    }
}

// ── Project ────────────────────────────────────────────────────────────────

/// Validated project metadata.
#[derive(Debug, Clone, Serialize)]
pub struct Project {
    /// Validated project name.
    pub name: ProjectName,
    /// Project-level environment variables (as declared in `[project.env]`).
    pub env: BTreeMap<String, String>,
}

// ── Service ────────────────────────────────────────────────────────────────

/// Validated configuration for a single managed service.
#[derive(Debug, Clone, Serialize)]
pub struct Service {
    /// Validated service name.
    pub name: ServiceName,
    /// The executable to run (first element of the raw `command` list).
    pub executable: String,
    /// Arguments to pass to the executable (remaining elements of `command`).
    pub args: Vec<String>,
    /// Absolute, canonicalized working directory.
    pub cwd: PathBuf,
    /// Effective environment: process env + project env + service env (later
    /// entries override earlier ones).
    pub env: BTreeMap<String, String>,
    /// Validated list of service names that must start before this one.
    pub depends_on: Vec<ServiceName>,
    /// Whether the supervisor should start this service automatically.
    pub autostart: bool,
    /// Restart policy.
    pub restart: RestartPolicy,
    /// Minimum delay before the first restart attempt.
    pub restart_delay: Duration,
    /// Maximum delay between restart attempts (exponential-backoff ceiling).
    pub restart_max_delay: Duration,
    /// Maximum number of restart attempts before giving up.
    pub max_restarts: u32,
    /// How long the process must run before it is considered stable.
    pub stable_after: Duration,
    /// Signal used to request graceful shutdown.
    pub shutdown_signal: ShutdownSignal,
    /// How long to wait for the process to exit after the shutdown signal.
    pub shutdown_timeout: Duration,
}

// ── Config ─────────────────────────────────────────────────────────────────

/// Fully validated runtime configuration.
///
/// Obtain this via [`crate::load::load`]; do not construct it directly.
#[derive(Debug, Clone, Serialize)]
pub struct Config {
    /// Absolute path to the `servicrab.toml` file that was loaded.
    pub source_path: PathBuf,
    /// Absolute path to the directory containing the config file.
    pub source_dir: PathBuf,
    /// Validated project metadata.
    pub project: Project,
    /// Validated services, keyed by service name (deterministic BTreeMap).
    pub services: BTreeMap<ServiceName, Service>,
    /// Deterministic topological start order (dependencies before dependents).
    pub start_order: Vec<ServiceName>,
}
