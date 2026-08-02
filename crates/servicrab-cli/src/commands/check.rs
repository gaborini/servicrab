//! `servicrab check [--config PATH]` — load and validate a `servicrab.toml`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use servicrab_core::{load, resolve_config_path, Config};
use servicrab_protocol::ErrorCode;

use crate::output::{self, CliError};

/// What `check --json` reports about a configuration that loaded.
///
/// The start order and the profile membership are the two things a script
/// actually asks `check` for — "will this config work, and what will run" — so
/// they are fields rather than the prose the human report renders them as.
#[derive(Serialize)]
struct CheckJson<'a> {
    ok: bool,
    config: String,
    project: &'a str,
    services: usize,
    start_order: Vec<&'a str>,
    profiles: BTreeMap<&'a str, Vec<&'a str>>,
    warnings: Vec<String>,
}

/// Run the `check` subcommand.
pub fn run(config: Option<&Path>, json: bool) -> Result<(), CliError> {
    let path = resolve_config_path(config)
        .map_err(|e| CliError::from(format!("could not find config: {e}")).in_json(json))?;

    let (cfg, warnings) = match load(&path) {
        Ok(loaded) => loaded,
        Err(errors) => {
            // Reported once: the errors used to be printed here *and* summarized
            // again by the `error:` line main prints for a failed command, which
            // told the operator the same thing twice.
            return Err(CliError::new(
                ErrorCode::ValidationFailed,
                format!("{} has {} error(s)", path.display(), errors.len()),
            )
            .with_errors(errors.iter().map(ToString::to_string).collect())
            .in_json(json));
        }
    };

    if json {
        return output::print_document(CheckJson {
            ok: true,
            config: path.display().to_string(),
            project: cfg.project.name.as_str(),
            services: cfg.services.len(),
            start_order: cfg.start_order.iter().map(|n| n.as_str()).collect(),
            profiles: profile_members(&cfg),
            warnings: warnings.iter().map(ToString::to_string).collect(),
        });
    }

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

/// Which services each profile holds. Empty when the config uses no profiles.
fn profile_members(cfg: &Config) -> BTreeMap<&str, Vec<&str>> {
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
}

/// One line per profile, naming the services it holds. Empty when the config
/// uses no profiles.
fn profile_lines(cfg: &Config) -> Vec<String> {
    profile_members(cfg)
        .into_iter()
        .map(|(profile, services)| format!("profile {profile}: {}", services.join(", ")))
        .collect()
}
