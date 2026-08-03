//! `servicrab list [--config PATH] [--json]` — list configured services.

use std::path::Path;

use serde::Serialize;
use servicrab_core::{load, resolve_config_path, Service, ServiceName};
use servicrab_protocol::ErrorCode;

use crate::output::{self, CliError};

/// Run the `list` subcommand.
pub fn run(config: Option<&Path>, json: bool) -> Result<(), CliError> {
    let path = resolve_config_path(config)
        .map_err(|e| CliError::from(format!("could not find config: {e}")).in_json(json))?;

    let (cfg, warnings) = load(&path).map_err(|errors| {
        CliError::new(
            ErrorCode::ValidationFailed,
            format!("{} has {} error(s)", path.display(), errors.len()),
        )
        .with_errors(errors.iter().map(ToString::to_string).collect())
        .in_json(json)
    })?;

    for warning in &warnings {
        eprintln!("⚠  {warning}");
    }

    if json {
        return print_json(&cfg.project.name.to_string(), &cfg.services);
    }
    print_table(&cfg.services, &cfg.project.name.to_string());

    Ok(())
}

// ── JSON output ────────────────────────────────────────────────────────────

/// The `list --json` document.
///
/// An envelope rather than the bare array this used to print: every `--json`
/// document carries a `schema_version`, and an array has nowhere to put one.
/// The services themselves are still an array, under `services`.
#[derive(Serialize)]
struct ListJson<'a> {
    project: &'a str,
    services: Vec<ServiceJson<'a>>,
}

#[derive(Serialize)]
struct ServiceJson<'a> {
    name: &'a str,
    command: Vec<&'a str>,
    cwd: String,
    depends_on: Vec<DependencyJson<'a>>,
    /// Empty for a service that is part of every run.
    profiles: Vec<&'a str>,
    autostart: bool,
    restart: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    health: Option<String>,
}

/// One dependency, with the condition resolved.
///
/// Resolved rather than as written, because a consumer of `--json` wants to
/// know what will be waited for without having to reimplement the rule for an
/// omitted condition.
#[derive(Serialize)]
struct DependencyJson<'a> {
    service: &'a str,
    condition: String,
}

fn print_json(
    project: &str,
    services: &std::collections::BTreeMap<ServiceName, Service>,
) -> Result<(), CliError> {
    let list: Vec<ServiceJson<'_>> = services
        .values()
        .map(|svc| {
            let mut cmd = vec![svc.executable.as_str()];
            cmd.extend(svc.args.iter().map(String::as_str));
            ServiceJson {
                name: svc.name.as_str(),
                command: cmd,
                cwd: svc.cwd.display().to_string(),
                depends_on: dependencies(svc, services),
                profiles: svc.profiles.iter().map(String::as_str).collect(),
                autostart: svc.autostart,
                restart: match svc.restart {
                    servicrab_core::RestartPolicy::Never => "never",
                    servicrab_core::RestartPolicy::OnFailure => "on-failure",
                    servicrab_core::RestartPolicy::Always => "always",
                    servicrab_core::RestartPolicy::UnlessStopped => "unless-stopped",
                },
                health: svc.health.as_ref().map(|h| h.probe.to_string()),
            }
        })
        .collect();

    output::print_document(ListJson {
        project,
        services: list,
    })
}

/// The dependencies of one service, each with its effective condition.
fn dependencies<'a>(
    service: &'a Service,
    services: &'a std::collections::BTreeMap<ServiceName, Service>,
) -> Vec<DependencyJson<'a>> {
    service
        .depends_on
        .iter()
        .map(|dep| DependencyJson {
            service: dep.service.as_str(),
            condition: match services.get(&dep.service) {
                Some(target) => dep.condition_for(target).to_string(),
                // Unreachable for a loaded config: validation rejects a
                // dependency on a service that does not exist.
                None => "unknown".to_string(),
            },
        })
        .collect()
}

// ── Human-readable table ───────────────────────────────────────────────────

/// Shorten a command line to at most `max` characters, marking the cut with
/// an ellipsis.
///
/// The width is counted in characters and the slice is taken at a character
/// boundary: `len()` is in bytes, so slicing on a byte offset panics as soon as
/// a command contains an accented path, an emoji or any other multi-byte
/// character.  Grapheme clusters can still be split — this is a column preview,
/// not a faithful rendering — but the output is always valid UTF-8.
fn truncate(cmd: &str, max: usize) -> String {
    debug_assert!(max > 0, "a preview needs room for at least the ellipsis");
    // The character *after* the last one that fits, plus whatever follows it.
    let mut rest = cmd.char_indices().skip(max - 1);
    match (rest.next(), rest.next()) {
        // Only `max` characters at most: the ellipsis would cost more than the
        // one character it would hide.
        (Some((cut, _)), Some(_)) => format!("{}…", &cmd[..cut]),
        _ => cmd.to_string(),
    }
}

fn print_table(services: &std::collections::BTreeMap<ServiceName, Service>, project: &str) {
    println!("Project: {project}");
    println!();

    if services.is_empty() {
        println!("No services defined.");
        return;
    }

    println!("{:<24} {:<12} {:<8} COMMAND", "NAME", "RESTART", "AUTO");
    println!("{}", "─".repeat(72));

    for svc in services.values() {
        let mut cmd_parts = vec![svc.executable.as_str()];
        cmd_parts.extend(svc.args.iter().map(String::as_str));
        let cmd_str = cmd_parts.join(" ");
        let cmd_preview = truncate(&cmd_str, 30);
        println!(
            "{:<24} {:<12} {:<8} {}",
            svc.name.as_str(),
            svc.restart.to_string(),
            if svc.autostart { "yes" } else { "no" },
            cmd_preview
        );
        if !svc.depends_on.is_empty() {
            let deps: Vec<String> = dependencies(svc, services)
                .into_iter()
                .map(|dep| format!("{} ({})", dep.service, dep.condition))
                .collect();
            println!("  depends on: {}", deps.join(", "));
        }
        if !svc.profiles.is_empty() {
            println!("  profiles: {}", svc.profiles.join(", "));
        }
        if let Some(health) = &svc.health {
            println!(
                "  health: {} every {}",
                health.probe,
                humantime::format_duration(health.interval)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_command_is_left_alone() {
        assert_eq!(truncate("echo hi", 30), "echo hi");
        // Exactly at the limit, so nothing is hidden and no ellipsis is needed.
        assert_eq!(truncate(&"a".repeat(30), 30), "a".repeat(30));
    }

    #[test]
    fn a_long_command_is_cut_with_an_ellipsis() {
        assert_eq!(
            truncate(&"a".repeat(31), 30),
            format!("{}…", "a".repeat(29))
        );
    }

    #[test]
    fn a_multi_byte_command_is_cut_on_a_character_boundary() {
        // Byte 29 lands inside the last '€', which is what used to panic.
        let cmd = "echo x€€€€€€€€€";
        assert_eq!(truncate(cmd, 30), cmd, "15 characters is under the limit");

        // Long enough to be cut, with the cut point inside a multi-byte
        // character: the result must stay valid UTF-8 and count characters.
        let long = format!("echo {}", "€".repeat(40));
        let cut = truncate(&long, 30);
        assert_eq!(cut.chars().count(), 30, "29 characters plus the ellipsis");
        assert!(cut.ends_with('…'), "{cut}");
    }
}
