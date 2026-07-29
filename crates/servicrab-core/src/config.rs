//! Configuration models for `servicrab.toml`.
//!
//! ## Example `servicrab.toml`
//!
//! ```toml
//! [project]
//! name = "my-stack"
//!
//! [services.api]
//! command = "cargo run --bin api"
//! cwd = "./api"
//! restart = "on-failure"
//! depends_on = ["db"]
//!
//! [services.db]
//! command = "postgres -D /usr/local/var/postgres"
//! restart = "always"
//!
//! [services.worker]
//! command = "python worker.py"
//! cwd = "./worker"
//! restart = "never"
//!
//! [services.worker.env]
//! DATABASE_URL = "postgres://localhost/mydb"
//! QUEUE_URL = "redis://localhost"
//! ```

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Top-level configuration parsed from `servicrab.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Project-level metadata.
    pub project: ProjectConfig,

    /// Map of service name → service configuration.
    #[serde(default)]
    pub services: HashMap<String, ServiceConfig>,
}

impl Config {
    /// Parse a [`Config`] from a TOML string.
    ///
    /// Returns a `toml::de::Error` on parse failure; call
    /// [`crate::validation::validate`] afterwards to check semantic rules.
    pub fn from_toml_str(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }
}

/// Project-level metadata (`[project]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectConfig {
    /// Human-readable project name shown in log output.
    pub name: String,
}

/// Configuration for a single managed service (`[services.<name>]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceConfig {
    /// Shell command (or executable path) used to start the service.
    ///
    /// The string is passed to the OS shell (`sh -c` on Unix, `cmd /C` on
    /// Windows) so that shell features such as pipes and redirections work as
    /// expected.
    ///
    /// TODO(phase-2): Add `args` as a separate `Vec<String>` for execvp-style
    /// invocation without a shell.
    pub command: String,

    /// Optional working directory for the process.  Defaults to the directory
    /// that contains `servicrab.toml`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// Environment variables to inject into the process.
    ///
    /// These are *merged* with (and override) the supervisor's own environment.
    ///
    /// TODO(phase-2): Support `.env` file references here.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,

    /// Restart policy for this service.  Defaults to [`RestartPolicy::Never`].
    #[serde(default)]
    pub restart: RestartPolicy,

    /// Services that must be in the *running* state before this service is
    /// started.
    ///
    /// TODO(phase-2): Enforce ordering in the daemon's start sequence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
}

/// Restart policy controlling what happens when a service process exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    /// Never restart the process after it exits.  Useful for one-shot tasks.
    #[default]
    Never,

    /// Restart only when the process exits with a non-zero status code.
    OnFailure,

    /// Always restart the process, regardless of exit status.
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

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_TOML: &str = r#"
[project]
name = "test-project"

[services.hello]
command = "echo hello"
"#;

    const FULL_TOML: &str = r#"
[project]
name = "my-stack"

[services.api]
command = "cargo run --bin api"
cwd = "./api"
restart = "on-failure"
depends_on = ["db"]

[services.db]
command = "postgres -D /tmp/pg"
restart = "always"

[services.worker]
command = "python worker.py"
restart = "never"

[services.worker.env]
DATABASE_URL = "postgres://localhost/mydb"
QUEUE = "redis://localhost"
"#;

    #[test]
    fn parse_minimal_config() {
        let cfg = Config::from_toml_str(MINIMAL_TOML).expect("parse minimal config");
        assert_eq!(cfg.project.name, "test-project");
        assert!(cfg.services.contains_key("hello"));
        let svc = &cfg.services["hello"];
        assert_eq!(svc.command, "echo hello");
        assert_eq!(svc.restart, RestartPolicy::Never);
        assert!(svc.env.is_empty());
        assert!(svc.depends_on.is_empty());
    }

    #[test]
    fn parse_full_config() {
        let cfg = Config::from_toml_str(FULL_TOML).expect("parse full config");
        assert_eq!(cfg.project.name, "my-stack");

        let api = cfg.services.get("api").expect("api service");
        assert_eq!(api.command, "cargo run --bin api");
        assert_eq!(api.cwd.as_deref(), Some("./api"));
        assert_eq!(api.restart, RestartPolicy::OnFailure);
        assert_eq!(api.depends_on, vec!["db"]);

        let db = cfg.services.get("db").expect("db service");
        assert_eq!(db.restart, RestartPolicy::Always);

        let worker = cfg.services.get("worker").expect("worker service");
        assert_eq!(worker.restart, RestartPolicy::Never);
        assert_eq!(
            worker.env.get("DATABASE_URL").map(String::as_str),
            Some("postgres://localhost/mydb")
        );
    }

    #[test]
    fn restart_policy_default_is_never() {
        let toml = r#"
[project]
name = "p"
[services.s]
command = "true"
"#;
        let cfg = Config::from_toml_str(toml).unwrap();
        assert_eq!(cfg.services["s"].restart, RestartPolicy::Never);
    }

    #[test]
    fn parse_invalid_toml_returns_error() {
        let result = Config::from_toml_str("this is not valid toml ][");
        assert!(result.is_err());
    }

    #[test]
    fn missing_project_section_returns_error() {
        let result = Config::from_toml_str("[services.foo]\ncommand = \"echo\"");
        assert!(result.is_err());
    }

    #[test]
    fn restart_policy_display() {
        assert_eq!(RestartPolicy::Never.to_string(), "never");
        assert_eq!(RestartPolicy::OnFailure.to_string(), "on-failure");
        assert_eq!(RestartPolicy::Always.to_string(), "always");
    }
}
