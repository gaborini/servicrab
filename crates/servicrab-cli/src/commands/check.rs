//! `servicrab check [--config PATH]` — load and validate a `servicrab.toml`.

use std::path::Path;

use servicrab_core::{load, resolve_config_path};

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
