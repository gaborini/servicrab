//! `servicrab check [config]` — parse and validate a `servicrab.toml`.

use std::path::Path;

use anyhow::Context;
use servicrab_core::{config::Config, validation::validate};
use tracing::info;

/// Run the `check` subcommand.
pub fn run(config_path: &Path) -> anyhow::Result<()> {
    let raw = std::fs::read_to_string(config_path)
        .with_context(|| format!("could not read {}", config_path.display()))?;

    let cfg: Config = Config::from_toml_str(&raw)
        .with_context(|| format!("failed to parse {}", config_path.display()))?;

    match validate(&cfg) {
        Ok(()) => {
            info!(
                config = %config_path.display(),
                services = cfg.services.len(),
                "Configuration is valid"
            );
            println!(
                "✓ {} is valid ({} service{}).",
                config_path.display(),
                cfg.services.len(),
                if cfg.services.len() == 1 { "" } else { "s" }
            );
            Ok(())
        }
        Err(errors) => {
            eprintln!("✗ {} has {} error(s):", config_path.display(), errors.len());
            for err in &errors {
                eprintln!("  • {err}");
            }
            anyhow::bail!("configuration validation failed");
        }
    }
}
