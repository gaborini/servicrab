//! `servicrab list [--config PATH] [--json]` — list configured services.

use std::path::Path;

use serde::Serialize;
use servicrab_core::{load, resolve_config_path, Service, ServiceName};

/// Run the `list` subcommand.
pub fn run(config: Option<&Path>, json: bool) -> Result<(), String> {
    let path = resolve_config_path(config).map_err(|e| format!("could not find config: {e}"))?;

    let (cfg, _) = load(&path).map_err(|errors| {
        let msgs: Vec<String> = errors.iter().map(|e| format!("  • {e}")).collect();
        format!(
            "✗ {} has {} error(s):\n{}",
            path.display(),
            errors.len(),
            msgs.join("\n")
        )
    })?;

    if json {
        print_json(&cfg.services);
    } else {
        print_table(&cfg.services, &cfg.project.name.to_string());
    }

    Ok(())
}

// ── JSON output ────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ServiceJson<'a> {
    name: &'a str,
    command: Vec<&'a str>,
    cwd: String,
    depends_on: Vec<&'a str>,
    autostart: bool,
    restart: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    health: Option<String>,
}

fn print_json(services: &std::collections::BTreeMap<ServiceName, Service>) {
    let list: Vec<ServiceJson<'_>> = services
        .values()
        .map(|svc| {
            let mut cmd = vec![svc.executable.as_str()];
            cmd.extend(svc.args.iter().map(String::as_str));
            ServiceJson {
                name: svc.name.as_str(),
                command: cmd,
                cwd: svc.cwd.display().to_string(),
                depends_on: svc.depends_on.iter().map(|n| n.as_str()).collect(),
                autostart: svc.autostart,
                restart: match svc.restart {
                    servicrab_core::RestartPolicy::Never => "never",
                    servicrab_core::RestartPolicy::OnFailure => "on-failure",
                    servicrab_core::RestartPolicy::Always => "always",
                },
                health: svc.health.as_ref().map(|h| h.probe.to_string()),
            }
        })
        .collect();

    println!(
        "{}",
        serde_json::to_string_pretty(&list).expect("JSON serialization")
    );
}

// ── Human-readable table ───────────────────────────────────────────────────

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
        let cmd_preview = if cmd_str.len() > 30 {
            format!("{}…", &cmd_str[..29])
        } else {
            cmd_str
        };
        println!(
            "{:<24} {:<12} {:<8} {}",
            svc.name.as_str(),
            svc.restart.to_string(),
            if svc.autostart { "yes" } else { "no" },
            cmd_preview
        );
        if !svc.depends_on.is_empty() {
            let deps: Vec<&str> = svc.depends_on.iter().map(|n| n.as_str()).collect();
            println!("  depends on: {}", deps.join(", "));
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
