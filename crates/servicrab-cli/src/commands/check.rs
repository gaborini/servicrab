//! `servicrab check [--config PATH]` — load and validate a `servicrab.toml`.

use std::collections::BTreeMap;
use std::path::Path;

use servicrab_core::{load, resolve_config_path, Config};

/// Run the `check` subcommand.
pub fn run(config: Option<&Path>) -> Result<(), String> {
    let path = resolve_config_path(config).map_err(|e| format!("could not find config: {e}"))?;

    match load(&path) {
        Ok((cfg, warnings)) => {
            let svc_count = cfg.services.len();
            println!("✓ {} — project: {}", path.display(), cfg.project.name);
            println!(
                "  {} service{}",
                svc_count,
                if svc_count == 1 { "" } else { "s" }
            );

            let order: Vec<&str> = cfg.start_order.iter().map(|n| n.as_str()).collect();
            println!("  start order: {}", order.join(" → "));

            // The order above lists every service, profiled ones included, so
            // say which of them wait to be asked for.
            for line in profile_lines(&cfg) {
                println!("  {line}");
            }

            if !warnings.is_empty() {
                println!("  {} warning(s):", warnings.len());
                for w in &warnings {
                    println!("    ⚠  {w}");
                }
            }
            Ok(())
        }
        Err(errors) => {
            eprintln!("✗ {} has {} error(s):", path.display(), errors.len());
            for err in &errors {
                eprintln!("  • {err}");
            }
            Err(format!(
                "configuration validation failed ({} error(s))",
                errors.len()
            ))
        }
    }
}

/// One line per profile, naming the services it holds. Empty when the config
/// uses no profiles.
fn profile_lines(cfg: &Config) -> Vec<String> {
    let mut members: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for name in &cfg.start_order {
        let service = &cfg.services[name];
        for profile in &service.profiles {
            members
                .entry(profile.as_str())
                .or_default()
                .push(name.as_str());
        }
    }

    members
        .into_iter()
        .map(|(profile, services)| format!("profile {profile}: {}", services.join(", ")))
        .collect()
}
