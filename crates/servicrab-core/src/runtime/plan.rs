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

/// Comma-separated list of the profiles the configuration declares.
pub fn known_profiles(config: &Config) -> String {
    let profiles: BTreeSet<&str> = config
        .services
        .values()
        .flat_map(|service| service.profiles.iter().map(String::as_str))
        .collect();

    if profiles.is_empty() {
        return "(none)".to_string();
    }
    profiles.into_iter().collect::<Vec<_>>().join(", ")
}

/// What a stack command was asked to start.
#[derive(Debug, Clone, Copy, Default)]
pub struct Selection<'a> {
    /// Service names given on the command line; empty means "the stack".
    pub services: &'a [String],
    /// Profiles enabled on the command line.
    pub profiles: &'a [String],
}

impl<'a> Selection<'a> {
    /// A selection of nothing but the services named.
    pub fn services(services: &'a [String]) -> Self {
        Self {
            services,
            profiles: &[],
        }
    }

    /// Whether `service` may be started without being named.
    ///
    /// A service without profiles always may; one with profiles is opt-in,
    /// which is the whole point of declaring them.
    fn enabled(&self, service: &crate::config::Service) -> bool {
        service.profiles.is_empty()
            || service
                .profiles
                .iter()
                .any(|profile| self.profiles.contains(profile))
    }
}

/// Build the start plan for a stack command.
///
/// * With no service named, every service with `autostart = true` that the
///   enabled profiles allow — which, with no profile enabled, means every
///   service that declares no profiles.
/// * With services named, exactly those, whatever their profiles say: naming a
///   service is a stronger statement than enabling its group, so the profiles
///   are not consulted.  The CLI rejects the two together rather than let one
///   quietly lose.
///
/// In both cases every transitive dependency is pulled in, profiles included:
/// a service can never be started without the services it declares in
/// `depends_on`, and a dependency that sits in a profile of its own is one
/// somebody chose not to start *on its own*.  The result follows the
/// configuration's deterministic topological start order.
pub fn plan_stack(
    config: &Config,
    selection: Selection<'_>,
) -> Result<Vec<ServiceName>, RuntimeError> {
    for profile in selection.profiles {
        if !config
            .services
            .values()
            .any(|service| service.profiles.contains(profile))
        {
            return Err(RuntimeError::UnknownProfile {
                profile: profile.clone(),
                known: known_profiles(config),
            });
        }
    }

    let mut selected: BTreeSet<ServiceName> = BTreeSet::new();

    if selection.services.is_empty() {
        for (name, service) in &config.services {
            if service.autostart && selection.enabled(service) {
                collect_with_dependencies(config, name, &mut selected);
            }
        }
    } else {
        for name in selection.services {
            let service = lookup_service(config, name)?;
            collect_with_dependencies(config, &service.name, &mut selected);
        }
    }

    if selected.is_empty() {
        return Err(nothing_to_start(config, selection));
    }

    Ok(config
        .start_order
        .iter()
        .filter(|name| selected.contains(*name))
        .cloned()
        .collect())
}

/// Explain an empty plan, which is always a failed command.
///
/// Only the no-names path can produce one: a named service is either found or
/// reported as unknown.
fn nothing_to_start(config: &Config, selection: Selection<'_>) -> RuntimeError {
    let reason = if !selection.profiles.is_empty() {
        format!(
            "no service with autostart = true is in {}",
            list(selection.profiles)
        )
    } else if config
        .services
        .values()
        .any(|service| !service.profiles.is_empty())
    {
        format!(
            "every service with autostart = true is behind a profile; \
             enable one with --profile (declared: {})",
            known_profiles(config)
        )
    } else {
        "none of the configured services have autostart = true".to_string()
    };

    RuntimeError::NothingToStart { reason }
}

/// `profile "dev"` or `profiles dev, test`, for a message that reads.
fn list(profiles: &[String]) -> String {
    match profiles {
        [one] => format!("profile {one:?}"),
        many => format!("profiles {}", many.join(", ")),
    }
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
            collect_with_dependencies(config, &dep.service, selected);
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

    /// Plan the whole stack with the given profiles enabled.
    fn plan_with(config: &Config, profiles: &[&str]) -> Vec<String> {
        let profiles: Vec<String> = profiles.iter().map(ToString::to_string).collect();
        let plan = plan_stack(
            config,
            Selection {
                services: &[],
                profiles: &profiles,
            },
        )
        .expect("plan");
        plan.iter().map(|n| n.to_string()).collect()
    }

    /// Plan the services named.
    fn plan_named(config: &Config, services: &[&str]) -> Vec<String> {
        let services: Vec<String> = services.iter().map(ToString::to_string).collect();
        let plan = plan_stack(config, Selection::services(&services)).expect("plan");
        plan.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn empty_request_selects_autostart_services_and_their_dependencies() {
        let (_dir, config) = config_with(STACK);
        assert_eq!(plan_with(&config, &[]), ["db", "api"]);
    }

    #[test]
    fn explicit_request_pulls_in_dependencies() {
        let (_dir, config) = config_with(STACK);
        assert_eq!(plan_named(&config, &["worker"]), ["cache", "worker"]);
    }

    #[test]
    fn plan_follows_the_topological_start_order() {
        let (_dir, config) = config_with(STACK);
        assert_eq!(plan_named(&config, &["api", "db"]), ["db", "api"]);
    }

    #[test]
    fn unknown_service_is_rejected() {
        let (_dir, config) = config_with(STACK);
        let named = ["nope".to_string()];
        let err = plan_stack(&config, Selection::services(&named)).expect_err("unknown service");
        assert!(matches!(err, RuntimeError::UnknownService { .. }));
        assert!(err.to_string().contains("api"));
    }

    #[test]
    fn duplicate_requests_are_deduplicated() {
        let (_dir, config) = config_with(STACK);
        assert_eq!(plan_named(&config, &["db", "db"]), ["db"]);
    }

    // ── profiles ───────────────────────────────────────────────────────────

    const PROFILED: &str = r#"
version = 1

[project]
name = "demo"

[services.db]
command = ["true"]

[services.api]
command = ["true"]
depends_on = ["db"]

[services.mailhog]
command = ["true"]
profiles = ["dev"]

[services.seeder]
command = ["true"]
profiles = ["dev", "test"]
"#;

    #[test]
    fn a_profiled_service_stays_out_until_its_profile_is_enabled() {
        let (_dir, config) = config_with(PROFILED);

        assert_eq!(plan_with(&config, &[]), ["db", "api"]);
        assert_eq!(
            plan_with(&config, &["dev"]),
            ["db", "api", "mailhog", "seeder"]
        );
    }

    #[test]
    fn a_service_joins_when_any_of_its_profiles_is_enabled() {
        let (_dir, config) = config_with(PROFILED);

        // `seeder` is in both, `mailhog` only in `dev`.
        assert_eq!(plan_with(&config, &["test"]), ["db", "api", "seeder"]);
    }

    #[test]
    fn several_profiles_add_up() {
        let (_dir, config) = config_with(PROFILED);

        assert_eq!(
            plan_with(&config, &["test", "dev"]),
            ["db", "api", "mailhog", "seeder"]
        );
    }

    #[test]
    fn a_profiled_service_can_still_be_named() {
        let (_dir, config) = config_with(PROFILED);

        assert_eq!(plan_named(&config, &["mailhog"]), ["mailhog"]);
    }

    #[test]
    fn a_dependency_comes_along_whatever_its_profile() {
        // `worker` is unprofiled but depends on a service that is not: asking
        // for the plain stack has to bring `tools` with it, because a service
        // cannot run without what it depends on.
        let (_dir, config) = config_with(
            r#"
version = 1
[project]
name = "demo"
[services.tools]
command = ["true"]
profiles = ["dev"]
[services.worker]
command = ["true"]
depends_on = ["tools"]
"#,
        );

        assert_eq!(plan_with(&config, &[]), ["tools", "worker"]);
    }

    #[test]
    fn a_profile_nothing_declares_is_a_typo() {
        let (_dir, config) = config_with(PROFILED);
        let profiles = ["prod".to_string()];

        let err = plan_stack(
            &config,
            Selection {
                services: &[],
                profiles: &profiles,
            },
        )
        .expect_err("unknown profile");

        assert!(
            matches!(&err, RuntimeError::UnknownProfile { profile, known }
                if profile == "prod" && known == "dev, test"),
            "{err}"
        );
    }
}
