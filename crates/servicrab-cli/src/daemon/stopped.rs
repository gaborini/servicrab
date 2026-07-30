//! Which services the operator stopped by hand, remembered between daemon
//! runs.
//!
//! Only `restart = "unless-stopped"` consults this, and only when a stack is
//! started: a hand-stopped service is never restarted under any policy, but
//! only this one survives `servicrab down` followed by `servicrab start`.
//!
//! The file is a plain list of names, one per line, next to the daemon's socket
//! and log — small enough to read and edit when a stack ends up in a state
//! nobody wanted.  The daemon is its only writer.

use std::collections::BTreeSet;
use std::path::Path;

use servicrab_core::{with_dependents, Config, RestartPolicy, ServiceName};

/// Read the remembered set.
///
/// A missing or unreadable file simply means nothing is remembered: the memory
/// of a stopped service is a convenience, and refusing to start a stack over it
/// would be worse than starting one service too many.
pub fn read(path: &Path) -> BTreeSet<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeSet::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect()
}

/// Record whether `service` is currently stopped by hand.
///
/// Read-modify-write on every change rather than a set held in memory: the file
/// is the state, so a hand edit is picked up and two daemons for one project —
/// which the socket already prevents — could not silently disagree.
pub fn record(path: &Path, service: &str, stopped: bool) -> Result<(), String> {
    let mut names = read(path);
    let changed = if stopped {
        names.insert(service.to_string())
    } else {
        names.remove(service)
    };
    if !changed {
        return Ok(());
    }

    let mut text = names.into_iter().collect::<Vec<_>>().join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    std::fs::write(path, text).map_err(|e| format!("could not write {}: {e}", path.display()))
}

/// The planned services that must not be started, given what is remembered.
///
/// A remembered name only counts while the service asks for it with
/// `restart = "unless-stopped"`; every other policy starts as it always has, so
/// adopting this file cannot change an existing stack.  Dependents of a held
/// back service are held back too — see [`with_dependents`].
pub fn held_back(
    config: &Config,
    plan: &[ServiceName],
    remembered: &BTreeSet<String>,
) -> BTreeSet<ServiceName> {
    let seeds: BTreeSet<ServiceName> = plan
        .iter()
        .filter(|name| remembered.contains(name.as_str()))
        .filter(|name| {
            config
                .services
                .get(*name)
                .is_some_and(|service| service.restart == RestartPolicy::UnlessStopped)
        })
        .cloned()
        .collect();

    if seeds.is_empty() {
        return BTreeSet::new();
    }
    with_dependents(config, plan, &seeds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(ToString::to_string).collect()
    }

    fn config_with(body: &str) -> (TempDir, Config, Vec<ServiceName>) {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("servicrab.toml");
        let mut file = std::fs::File::create(&path).expect("create config");
        file.write_all(body.as_bytes()).expect("write config");
        let (config, _) = servicrab_core::load(&path).expect("valid config");
        let plan = config.start_order.clone();
        (dir, config, plan)
    }

    const STACK: &str = r#"
version = 1
[project]
name = "demo"

[services.db]
command = ["true"]
restart = "unless-stopped"

[services.api]
command = ["true"]
depends_on = ["db"]
restart = "always"

[services.cache]
command = ["true"]
restart = "always"
"#;

    #[test]
    fn an_absent_file_remembers_nothing() {
        let dir = TempDir::new().expect("temp dir");
        assert!(read(&dir.path().join("stopped")).is_empty());
    }

    #[test]
    fn a_recorded_name_survives_a_reread() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("stopped");

        record(&path, "api", true).expect("record");
        record(&path, "db", true).expect("record");
        assert_eq!(read(&path), set(&["api", "db"]));

        record(&path, "api", false).expect("record");
        assert_eq!(read(&path), set(&["db"]));
    }

    #[test]
    fn recording_the_same_state_twice_is_a_no_op() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("stopped");

        record(&path, "api", true).expect("record");
        record(&path, "api", true).expect("record");
        assert_eq!(read(&path), set(&["api"]));

        // Forgetting a name that was never there must not create the file.
        let empty = dir.path().join("none");
        record(&empty, "api", false).expect("record");
        assert!(!empty.exists());
    }

    #[test]
    fn blank_lines_and_stray_whitespace_are_ignored() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("stopped");
        std::fs::write(&path, "api\n\n  db  \n\n").expect("write");
        assert_eq!(read(&path), set(&["api", "db"]));
    }

    #[test]
    fn only_unless_stopped_services_are_held_back() {
        let (_dir, config, plan) = config_with(STACK);

        // `cache` is remembered, but its policy does not ask to be.
        assert!(held_back(&config, &plan, &set(&["cache"])).is_empty());
    }

    #[test]
    fn a_held_back_service_takes_its_dependents_with_it() {
        let (_dir, config, plan) = config_with(STACK);
        let held = held_back(&config, &plan, &set(&["db"]));

        let held: Vec<&str> = held.iter().map(ServiceName::as_str).collect();
        assert_eq!(held, vec!["api", "db"]);
    }

    #[test]
    fn a_name_outside_the_plan_is_ignored() {
        let (_dir, config, plan) = config_with(STACK);
        let plan: Vec<ServiceName> = plan
            .into_iter()
            .filter(|name| name.as_str() != "db")
            .collect();

        assert!(held_back(&config, &plan, &set(&["db", "gone"])).is_empty());
    }
}
