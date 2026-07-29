//! Deciding *which* services a stack command should start.
//!
//! This is pure graph work: it takes the validated configuration and the
//! service names the user asked for, and produces a deterministic start order
//! that always contains every transitive dependency.

use std::collections::BTreeSet;

use crate::config::{Config, ServiceName};
use crate::error::RuntimeError;

/// Resolve a single service name against the configuration.
pub fn lookup_service<'a>(
    config: &'a Config,
    requested: &str,
) -> Result<&'a crate::config::Service, RuntimeError> {
    config
        .services
        .iter()
        .find(|(name, _)| name.as_str() == requested)
        .map(|(_, service)| service)
        .ok_or_else(|| RuntimeError::UnknownService {
            service: requested.to_string(),
            known: known_services(config),
        })
}

/// Comma-separated list of configured service names, for error messages.
pub fn known_services(config: &Config) -> String {
    if config.services.is_empty() {
        return "(none)".to_string();
    }
    config
        .services
        .keys()
        .map(|name| name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Build the start plan for a stack command.
///
/// * With no requested names, every service with `autostart = true` is
///   selected.
/// * With explicit names, only those services are selected.
///
/// In both cases every transitive dependency is pulled in — a service can
/// never be started without the services it declares in `depends_on`.  The
/// result is ordered according to the configuration's deterministic
/// topological start order.
pub fn plan_stack(config: &Config, requested: &[String]) -> Result<Vec<ServiceName>, RuntimeError> {
    let mut selected: BTreeSet<ServiceName> = BTreeSet::new();

    if requested.is_empty() {
        for (name, service) in &config.services {
            if service.autostart {
                collect_with_dependencies(config, name, &mut selected);
            }
        }
    } else {
        for name in requested {
            let service = lookup_service(config, name)?;
            collect_with_dependencies(config, &service.name, &mut selected);
        }
    }

    Ok(config
        .start_order
        .iter()
        .filter(|name| selected.contains(*name))
        .cloned()
        .collect())
}

fn collect_with_dependencies(
    config: &Config,
    name: &ServiceName,
    selected: &mut BTreeSet<ServiceName>,
) {
    if !selected.insert(name.clone()) {
        return;
    }
    // The configuration layer already rejected cycles and unknown
    // dependencies, so this recursion always terminates.
    if let Some(service) = config.services.get(name) {
        for dep in &service.depends_on {
            collect_with_dependencies(config, dep, selected);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn config_with(body: &str) -> (TempDir, Config) {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("servicrab.toml");
        let mut file = std::fs::File::create(&path).expect("create config");
        file.write_all(body.as_bytes()).expect("write config");
        let (config, _warnings) = crate::load::load(&path).expect("valid config");
        (dir, config)
    }

    const STACK: &str = r#"
version = 1

[project]
name = "demo"

[services.db]
command = ["true"]

[services.cache]
command = ["true"]
autostart = false

[services.api]
command = ["true"]
depends_on = ["db"]

[services.worker]
command = ["true"]
autostart = false
depends_on = ["cache"]
"#;

    #[test]
    fn empty_request_selects_autostart_services_and_their_dependencies() {
        let (_dir, config) = config_with(STACK);
        let plan = plan_stack(&config, &[]).expect("plan");
        let names: Vec<&str> = plan.iter().map(|n| n.as_str()).collect();
        assert_eq!(names, vec!["db", "api"]);
    }

    #[test]
    fn explicit_request_pulls_in_dependencies() {
        let (_dir, config) = config_with(STACK);
        let plan = plan_stack(&config, &["worker".to_string()]).expect("plan");
        let names: Vec<&str> = plan.iter().map(|n| n.as_str()).collect();
        assert_eq!(names, vec!["cache", "worker"]);
    }

    #[test]
    fn plan_follows_the_topological_start_order() {
        let (_dir, config) = config_with(STACK);
        let plan = plan_stack(&config, &["api".to_string(), "db".to_string()]).expect("plan");
        let names: Vec<&str> = plan.iter().map(|n| n.as_str()).collect();
        assert_eq!(names, vec!["db", "api"]);
    }

    #[test]
    fn unknown_service_is_rejected() {
        let (_dir, config) = config_with(STACK);
        let err = plan_stack(&config, &["nope".to_string()]).expect_err("unknown service");
        assert!(matches!(err, RuntimeError::UnknownService { .. }));
        assert!(err.to_string().contains("api"));
    }

    #[test]
    fn duplicate_requests_are_deduplicated() {
        let (_dir, config) = config_with(STACK);
        let plan = plan_stack(&config, &["db".to_string(), "db".to_string()]).expect("plan");
        assert_eq!(plan.len(), 1);
    }
}
