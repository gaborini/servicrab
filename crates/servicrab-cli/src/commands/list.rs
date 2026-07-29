//! `servicrab list [config]` — list configured services.

use std::path::Path;

use anyhow::Context;
use servicrab_core::{config::Config, validation::validate};

/// Run the `list` subcommand.
pub fn run(config_path: &Path) -> anyhow::Result<()> {
    let raw = std::fs::read_to_string(config_path)
        .with_context(|| format!("could not read {}", config_path.display()))?;

    let cfg: Config = Config::from_toml_str(&raw)
        .with_context(|| format!("failed to parse {}", config_path.display()))?;

    if let Err(errors) = validate(&cfg) {
        eprintln!("✗ {} has {} error(s):", config_path.display(), errors.len());
        for err in &errors {
            eprintln!("  • {err}");
        }
        anyhow::bail!("configuration validation failed");
    }

    println!("Project: {}", cfg.project.name);
    println!();

    if cfg.services.is_empty() {
        println!("No services defined.");
        return Ok(());
    }

    // Sort by name for deterministic output.
    let mut names: Vec<&String> = cfg.services.keys().collect();
    names.sort();

    println!("{:<20} {:<12} COMMAND", "NAME", "RESTART");
    println!("{}", "-".repeat(60));

    for name in names {
        let svc = &cfg.services[name];
        let cmd_preview = if svc.command.len() > 35 {
            format!("{}…", &svc.command[..34])
        } else {
            svc.command.clone()
        };
        println!(
            "{:<20} {:<12} {}",
            name,
            svc.restart.to_string(),
            cmd_preview
        );
    }

    Ok(())
}
