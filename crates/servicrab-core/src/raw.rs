//! Raw TOML deserialization model for `servicrab.toml`.
//!
//! These types are used only for parsing.  After deserializing, callers must
//! run the validation pipeline in [`crate::validation`] to obtain the
//! [`crate::config::Config`] runtime model.
//!
//! All structs use `#[serde(deny_unknown_fields)]` so that typos in field
//! names produce a hard error rather than being silently ignored.

use std::collections::BTreeMap;

use serde::Deserialize;

/// Top-level raw configuration, as parsed directly from `servicrab.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawConfig {
    /// Schema version; must be `1`.
    pub version: u32,

    /// Project-level metadata.
    pub project: RawProject,

    /// Service definitions.  The outer `BTreeMap` key is the service name.
    #[serde(default)]
    pub services: BTreeMap<String, RawService>,
}

/// Raw project metadata (`[project]`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawProject {
    /// Project name (validated by the validation layer).
    pub name: String,

    /// Project-level environment variables.
    #[serde(default)]
    pub env: BTreeMap<String, String>,

    /// Optional file-logging settings (`[project.logs]`).
    #[serde(default)]
    pub logs: Option<RawLogs>,
}

/// Raw file-logging settings (`[project.logs]`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawLogs {
    /// Directory for the log files (relative paths resolve against the config
    /// file's directory).  Defaults to `".servicrab/logs"`.
    #[serde(default)]
    pub dir: Option<String>,

    /// Rotate a log file once it grows past this size, e.g. `"10MB"`.
    /// Defaults to `"10MB"`.
    #[serde(default)]
    pub max_size: Option<String>,

    /// How many rotated files to keep per service.  Defaults to `3`.
    #[serde(default)]
    pub max_files: Option<u32>,
}

/// Raw per-service logging settings (`[services.<name>.logs]`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawServiceLogs {
    /// Whether this service's output is written to a log file.  Defaults to
    /// `true` when `[project.logs]` is present.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// Raw service configuration (`[services.<name>]`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawService {
    /// Command to execute: first element is the executable, rest are arguments.
    pub command: Vec<String>,

    /// Working directory (relative paths are resolved against the config
    /// file's parent directory by the validation layer).
    #[serde(default)]
    pub cwd: Option<String>,

    /// Service-level environment variables.
    #[serde(default)]
    pub env: BTreeMap<String, String>,

    /// Services that must be started before this one.
    #[serde(default)]
    pub depends_on: Vec<String>,

    /// Whether to start this service automatically.  Defaults to `true`.
    #[serde(default = "default_true")]
    pub autostart: bool,

    /// Restart policy for this service.
    #[serde(default)]
    pub restart: RawRestartPolicy,

    /// Minimum delay before the first restart.  Defaults to `"1s"`.
    #[serde(default)]
    pub restart_delay: Option<String>,

    /// Maximum delay between restarts (exponential-backoff ceiling).
    /// Defaults to `"30s"`.
    #[serde(default)]
    pub restart_max_delay: Option<String>,

    /// Maximum number of restarts before giving up.  Defaults to `10`.
    #[serde(default)]
    pub max_restarts: Option<u32>,

    /// How long the process must run before it is considered stable.
    /// Defaults to `"60s"`.
    #[serde(default)]
    pub stable_after: Option<String>,

    /// Signal used to request graceful shutdown.  One of `term`, `int`,
    /// `quit`, `hup`.  Defaults to `"term"`.
    #[serde(default)]
    pub shutdown_signal: Option<String>,

    /// How long to wait for the service to exit after sending the shutdown
    /// signal before forcibly killing it.  Defaults to `"10s"`.
    #[serde(default)]
    pub shutdown_timeout: Option<String>,

    /// Optional health check (`[services.<name>.health]`).
    #[serde(default)]
    pub health: Option<RawHealthCheck>,

    /// Optional per-service logging settings (`[services.<name>.logs]`).
    #[serde(default)]
    pub logs: Option<RawServiceLogs>,
}

/// Raw health-check configuration (`[services.<name>.health]`).
///
/// Exactly one of `command`, `http` or `tcp` must be set.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawHealthCheck {
    /// Command probe: first element is the executable, rest are arguments.
    #[serde(default)]
    pub command: Option<Vec<String>>,

    /// HTTP probe: an `http://host[:port][/path]` URL.
    #[serde(default)]
    pub http: Option<String>,

    /// TCP probe: a `host:port` address.
    #[serde(default)]
    pub tcp: Option<String>,

    /// Delay between probes.  Defaults to `"2s"`.
    #[serde(default)]
    pub interval: Option<String>,

    /// Per-probe timeout.  Defaults to `"5s"`.
    #[serde(default)]
    pub timeout: Option<String>,

    /// Consecutive failures tolerated before the service is unhealthy.
    /// Defaults to `3`.
    #[serde(default)]
    pub retries: Option<u32>,

    /// Grace period after start during which failures do not count.
    /// Defaults to `"0s"`.
    #[serde(default)]
    pub start_period: Option<String>,

    /// What to do when the service becomes unhealthy: `restart` (default) or
    /// `ignore`.
    #[serde(default)]
    pub on_unhealthy: Option<String>,
}

/// Raw restart-policy string token.
#[derive(Debug, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RawRestartPolicy {
    /// Never restart (default).
    #[default]
    Never,
    /// Restart only on non-zero exit.
    OnFailure,
    /// Always restart.
    Always,
}

fn default_true() -> bool {
    true
}
