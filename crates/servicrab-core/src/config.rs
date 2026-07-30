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

/// What a dependent waits for before it starts.
///
/// The names are the ones Docker Compose uses, because that is where anyone
/// writing this field has met the idea before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyCondition {
    /// The dependency's process is up.  Its exit status is not consulted: a
    /// one-shot that has already run counts as started.
    ServiceStarted,
    /// A health probe of the dependency has passed.
    ServiceHealthy,
    /// The dependency has exited with status 0.
    ServiceCompletedSuccessfully,
}

impl std::fmt::Display for DependencyCondition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DependencyCondition::ServiceStarted => write!(f, "service_started"),
            DependencyCondition::ServiceHealthy => write!(f, "service_healthy"),
            DependencyCondition::ServiceCompletedSuccessfully => {
                write!(f, "service_completed_successfully")
            }
        }
    }
}

/// One validated `depends_on` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Dependency {
    /// The service to wait for.
    pub service: ServiceName,
    /// What to wait for, when the config spelled it out.
    ///
    /// Left as declared rather than resolved to a concrete condition, because
    /// `PartialEq` on [`Service`] is the hot-reload restart trigger: resolving
    /// it here would restart a dependent whenever its *dependency* gained a
    /// health check, which changes nothing about the dependent's own process.
    /// Use [`Dependency::condition_for`] to get the effective condition.
    pub condition: Option<DependencyCondition>,
}

impl Dependency {
    /// The condition to actually wait for, given the dependency it points at.
    ///
    /// An unspecified condition means "healthy if it promised a health check,
    /// up otherwise" — the rule servicrab used before conditions existed, kept
    /// so that adding a health check to a service still gates its dependents.
    pub fn condition_for(&self, dependency: &Service) -> DependencyCondition {
        self.condition.unwrap_or(if dependency.health.is_some() {
            DependencyCondition::ServiceHealthy
        } else {
            DependencyCondition::ServiceStarted
        })
    }
}

// ── Project ────────────────────────────────────────────────────────────────

/// Validated file-logging settings.
///
/// File logging is opt-in: it is only active when the config declares a
/// `[project.logs]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LogSettings {
    /// Absolute directory the log files are written to.
    pub dir: PathBuf,
    /// Rotate a service's log file once it grows past this many bytes.
    pub max_size: u64,
    /// How many rotated files to keep per service.
    pub max_files: u32,
}

impl LogSettings {
    /// Path of the active log file for `service`.
    pub fn file_for(&self, service: &ServiceName) -> PathBuf {
        self.dir.join(format!("{service}.log"))
    }

    /// Path of the `n`-th rotated file for `service` (1 is the most recent).
    pub fn rotated_file_for(&self, service: &ServiceName, n: u32) -> PathBuf {
        self.dir.join(format!("{service}.log.{n}"))
    }
}

/// Validated project metadata.
#[derive(Debug, Clone, Serialize)]
pub struct Project {
    /// Validated project name.
    pub name: ProjectName,
    /// Project-level environment variables (as declared in `[project.env]`).
    pub env: BTreeMap<String, String>,
    /// Absolute paths of the project-level `env_file` entries, in declaration
    /// order.
    pub env_files: Vec<PathBuf>,
    /// File-logging settings, when `[project.logs]` was declared.
    pub logs: Option<LogSettings>,
}

// ── Health checks ──────────────────────────────────────────────────────────

/// How a service's health is probed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum HealthProbe {
    /// Run a command; exit status `0` means healthy.
    Command {
        /// Executable to run.
        executable: String,
        /// Arguments passed to the executable.
        args: Vec<String>,
    },
    /// Issue a plain `HTTP/1.1` `GET` and require a `2xx`/`3xx` response.
    Http {
        /// The original URL, as written in the config.
        url: String,
        /// Host part of the URL.
        host: String,
        /// Port (defaults to `80` when the URL omits it).
        port: u16,
        /// Request target, including any query string.
        path: String,
    },
    /// Open a TCP connection to `host:port`.
    Tcp {
        /// Host part of the address.
        host: String,
        /// Port part of the address.
        port: u16,
    },
}

impl std::fmt::Display for HealthProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HealthProbe::Command { executable, args } => {
                write!(f, "command {executable}")?;
                for arg in args {
                    write!(f, " {arg}")?;
                }
                Ok(())
            }
            HealthProbe::Http { url, .. } => write!(f, "http {url}"),
            HealthProbe::Tcp { host, port } => write!(f, "tcp {host}:{port}"),
        }
    }
}

/// What the supervisor does when a service is declared unhealthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnhealthyAction {
    /// Stop the process and let the restart policy decide what happens next
    /// (default).
    #[default]
    Restart,
    /// Only report the failure and keep the process running.
    Ignore,
}

impl std::fmt::Display for UnhealthyAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnhealthyAction::Restart => write!(f, "restart"),
            UnhealthyAction::Ignore => write!(f, "ignore"),
        }
    }
}

/// Validated health-check configuration for a service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthCheck {
    /// How health is probed.
    pub probe: HealthProbe,
    /// How long to wait between probes.
    pub interval: Duration,
    /// How long a single probe may take before it counts as a failure.
    pub timeout: Duration,
    /// Consecutive failures tolerated before the service is unhealthy.
    pub retries: u32,
    /// Grace period after start during which failures do not count.
    pub start_period: Duration,
    /// What to do once the service is declared unhealthy.
    pub on_unhealthy: UnhealthyAction,
}

// ── Service ────────────────────────────────────────────────────────────────

/// Validated file-watching settings for a service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WatchSettings {
    /// Absolute paths (files or directories) to watch.
    pub paths: Vec<PathBuf>,
    /// Names, path prefixes or `*.ext` patterns that are never watched.
    pub ignore: Vec<String>,
    /// How often the watched paths are scanned.
    pub interval: Duration,
    /// How long the tree must stay unchanged before the restart fires.
    pub debounce: Duration,
}

impl WatchSettings {
    /// Whether `relative` — a path relative to one of the watched roots —
    /// matches any ignore entry.
    ///
    /// An entry matches when it equals a path component, when it is a prefix
    /// of the relative path, or when it is a `*.ext` pattern matching the file
    /// extension.
    pub fn is_ignored(&self, relative: &std::path::Path) -> bool {
        self.ignore.iter().any(|entry| {
            if let Some(ext) = entry.strip_prefix("*.") {
                return relative
                    .extension()
                    .is_some_and(|actual| actual.to_string_lossy() == ext);
            }
            if entry.contains('/') {
                return relative.starts_with(entry);
            }
            relative
                .components()
                .any(|c| c.as_os_str().to_string_lossy() == *entry)
        })
    }
}

/// Validated configuration for a single managed service.
///
/// `PartialEq` is what config hot-reload uses to decide whether a service has
/// to be restarted, so every field that affects the running process must take
/// part in the comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Service {
    /// Validated service name.
    pub name: ServiceName,
    /// The executable to run (first element of the raw `command` list).
    pub executable: String,
    /// Arguments to pass to the executable (remaining elements of `command`).
    pub args: Vec<String>,
    /// Absolute, canonicalized working directory.
    pub cwd: PathBuf,
    /// Effective environment: process env + project env files + project env +
    /// service env files + service env (later entries override earlier ones).
    pub env: BTreeMap<String, String>,
    /// Absolute paths of the service-level `env_file` entries, in declaration
    /// order.
    pub env_files: Vec<PathBuf>,
    /// Validated dependencies that must be ready before this one starts.
    pub depends_on: Vec<Dependency>,
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
    /// Optional health check used for readiness gating and liveness.
    pub health: Option<HealthCheck>,
    /// Whether this service's output is written to a log file (only relevant
    /// when the project declares `[project.logs]`).
    pub log_to_file: bool,
    /// Optional file-watching settings; when set, the supervisor restarts the
    /// service after a change under the watched paths.
    pub watch: Option<WatchSettings>,
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
