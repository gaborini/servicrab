//! `servicrab run <service> [config]` — run a service in the foreground.
//!
//! Spawns the service's command via the OS shell, inherits stdout/stderr, and
//! waits for it to exit.  The exit code of the child is propagated to the
//! supervisor process.
//!
//! ## Future phases (TODOs)
//!
//! - TODO(phase-2): Honour the `restart` policy: loop on non-zero exit with
//!   exponential backoff.
//! - TODO(phase-2): Start `depends_on` services before the requested one, or
//!   check that they are already running via the daemon.
//! - TODO(phase-2): Set up structured logging per-service (timestamps, log
//!   rotation) rather than raw stdio passthrough.
//! - TODO(phase-3): Support Windows via `cmd /C` instead of `sh -c`.

use std::path::Path;
use std::process::Stdio;

use anyhow::Context;
use servicrab_core::{config::Config, validation::validate};
use tokio::process::Command;
use tracing::{debug, info, warn};

/// Run the `run` subcommand asynchronously.
pub async fn run(service_name: &str, config_path: &Path) -> anyhow::Result<()> {
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

    let svc = cfg.services.get(service_name).with_context(|| {
        format!(
            "service '{}' not found in {}",
            service_name,
            config_path.display()
        )
    })?;

    info!(service = service_name, command = %svc.command, "Starting service");

    if !svc.depends_on.is_empty() {
        // TODO(phase-2): Start or check dependencies before proceeding.
        warn!(
            service = service_name,
            deps = ?svc.depends_on,
            "This service has dependencies; dependency management is not yet implemented"
        );
    }

    let mut cmd = build_command(svc, config_path)?;

    debug!(service = service_name, "Spawning process");

    let status = cmd
        .status()
        .await
        .with_context(|| format!("failed to spawn service '{service_name}'"))?;

    if status.success() {
        info!(service = service_name, "Service exited successfully");
    } else {
        let code = status.code().unwrap_or(-1);
        warn!(
            service = service_name,
            exit_code = code,
            "Service exited with non-zero status"
        );
        // Propagate the exit code so that shell scripts can react.
        std::process::exit(code);
    }

    Ok(())
}

/// Build a [`tokio::process::Command`] from a [`ServiceConfig`].
fn build_command(
    svc: &servicrab_core::config::ServiceConfig,
    config_path: &Path,
) -> anyhow::Result<Command> {
    // Resolve the working directory: use the explicitly configured `cwd` if
    // given, otherwise fall back to the directory that contains the config
    // file, and finally the current working directory.
    let cwd = if let Some(ref cwd) = svc.cwd {
        std::path::PathBuf::from(cwd)
    } else if let Some(parent) = config_path.parent() {
        if parent.as_os_str().is_empty() {
            std::env::current_dir()?
        } else {
            parent.to_path_buf()
        }
    } else {
        std::env::current_dir()?
    };

    // Use the platform shell to execute the command string so that shell
    // features (pipes, redirections, etc.) work as expected.
    #[cfg(unix)]
    let (shell, flag) = ("sh", "-c");
    #[cfg(windows)]
    let (shell, flag) = ("cmd", "/C");

    let mut cmd = Command::new(shell);
    cmd.arg(flag)
        .arg(&svc.command)
        .current_dir(&cwd)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .envs(&svc.env);

    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use servicrab_core::config::ServiceConfig;

    fn make_svc(command: &str, cwd: Option<&str>) -> ServiceConfig {
        ServiceConfig {
            command: command.to_string(),
            cwd: cwd.map(str::to_string),
            env: HashMap::new(),
            restart: Default::default(),
            depends_on: vec![],
        }
    }

    #[test]
    fn build_command_uses_shell() {
        let svc = make_svc("echo hello", None);
        let config_path = Path::new("servicrab.toml");
        let cmd = build_command(&svc, config_path).expect("build command");
        // The program should be the shell binary.
        #[cfg(unix)]
        assert_eq!(cmd.as_std().get_program(), "sh");
        #[cfg(windows)]
        assert_eq!(cmd.as_std().get_program(), "cmd");
    }

    #[test]
    fn build_command_with_explicit_cwd() {
        let svc = make_svc("echo hello", Some("/tmp"));
        let config_path = Path::new("servicrab.toml");
        let cmd = build_command(&svc, config_path).expect("build command");
        let actual_cwd = cmd.as_std().get_current_dir().expect("cwd set");
        assert_eq!(actual_cwd, Path::new("/tmp"));
    }
}
