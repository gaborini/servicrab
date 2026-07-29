//! Semantic validation of a parsed [`Config`].
//!
//! After successfully deserializing the TOML, callers should run
//! [`validate`] to catch logical errors such as unknown dependency names,
//! duplicate entries, or empty commands.

use thiserror::Error;

use crate::config::Config;

/// A semantic validation error found in a [`Config`].
#[derive(Debug, Error, PartialEq)]
pub enum ValidationError {
    /// A service name is empty or contains only whitespace.
    #[error("service name must not be empty or blank")]
    EmptyServiceName,

    /// The `command` field of a service is empty or contains only whitespace.
    #[error("service '{0}': command must not be empty")]
    EmptyCommand(String),

    /// A service lists a dependency that does not exist in `[services]`.
    #[error("service '{service}': unknown dependency '{dep}'")]
    UnknownDependency { service: String, dep: String },

    /// The project name is empty or contains only whitespace.
    #[error("project name must not be empty")]
    EmptyProjectName,

    /// A service lists itself as a dependency (direct self-loop).
    #[error("service '{0}': depends on itself")]
    SelfDependency(String),
}

/// Validate a [`Config`] for semantic correctness.
///
/// Returns `Ok(())` if the config is valid, or a `Vec` of all validation
/// errors found (so that the user can fix all problems in one pass).
///
/// # Example
///
/// ```
/// use servicrab_core::{Config, validation::validate};
///
/// let cfg = Config::from_toml_str(r#"
/// [project]
/// name = "demo"
/// [services.web]
/// command = "python -m http.server"
/// "#).unwrap();
///
/// assert!(validate(&cfg).is_ok());
/// ```
pub fn validate(cfg: &Config) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();

    // Project-level checks
    if cfg.project.name.trim().is_empty() {
        errors.push(ValidationError::EmptyProjectName);
    }

    for (name, svc) in &cfg.services {
        // Service name check
        if name.trim().is_empty() {
            errors.push(ValidationError::EmptyServiceName);
        }

        // Command check
        if svc.command.trim().is_empty() {
            errors.push(ValidationError::EmptyCommand(name.clone()));
        }

        // Dependency checks
        for dep in &svc.depends_on {
            if dep == name {
                errors.push(ValidationError::SelfDependency(name.clone()));
            } else if !cfg.services.contains_key(dep) {
                errors.push(ValidationError::UnknownDependency {
                    service: name.clone(),
                    dep: dep.clone(),
                });
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::config::{ProjectConfig, ServiceConfig};

    fn make_config(services: Vec<(&str, ServiceConfig)>) -> Config {
        Config {
            project: ProjectConfig {
                name: "test".to_string(),
            },
            services: services
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        }
    }

    fn simple_service(cmd: &str) -> ServiceConfig {
        ServiceConfig {
            command: cmd.to_string(),
            cwd: None,
            env: HashMap::new(),
            restart: Default::default(),
            depends_on: vec![],
        }
    }

    #[test]
    fn valid_config_passes() {
        let cfg = make_config(vec![("web", simple_service("python -m http.server"))]);
        assert!(validate(&cfg).is_ok());
    }

    #[test]
    fn empty_command_fails() {
        let cfg = make_config(vec![("web", simple_service("  "))]);
        let errs = validate(&cfg).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::EmptyCommand(n) if n == "web")));
    }

    #[test]
    fn unknown_dependency_fails() {
        let mut svc = simple_service("echo");
        svc.depends_on = vec!["nonexistent".to_string()];
        let cfg = make_config(vec![("web", svc)]);
        let errs = validate(&cfg).unwrap_err();
        assert!(errs.iter().any(|e| matches!(
            e,
            ValidationError::UnknownDependency { service, dep }
            if service == "web" && dep == "nonexistent"
        )));
    }

    #[test]
    fn self_dependency_fails() {
        let mut svc = simple_service("echo");
        svc.depends_on = vec!["web".to_string()];
        let cfg = make_config(vec![("web", svc)]);
        let errs = validate(&cfg).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::SelfDependency(n) if n == "web")));
    }

    #[test]
    fn empty_project_name_fails() {
        let mut cfg = make_config(vec![]);
        cfg.project.name = "  ".to_string();
        let errs = validate(&cfg).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::EmptyProjectName)));
    }

    #[test]
    fn multiple_errors_reported() {
        let svc1 = simple_service("  "); // empty command
        let mut svc2 = simple_service("echo");
        svc2.depends_on = vec!["ghost".to_string()]; // unknown dep
        let cfg = make_config(vec![("s1", svc1), ("s2", svc2)]);
        let errs = validate(&cfg).unwrap_err();
        assert!(errs.len() >= 2);
    }

    #[test]
    fn valid_dependency_passes() {
        let mut svc_api = simple_service("cargo run");
        svc_api.depends_on = vec!["db".to_string()];
        let svc_db = simple_service("postgres");
        let cfg = make_config(vec![("api", svc_api), ("db", svc_db)]);
        assert!(validate(&cfg).is_ok());
    }
}
