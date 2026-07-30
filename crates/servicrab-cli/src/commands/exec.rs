//! `servicrab exec <SERVICE> [--config PATH] -- <COMMAND>...` — run a command
//! in a service's environment.
//!
//! This reproduces the environment a service *would* get — its merged `env`,
//! its `env_file` layers and its `cwd` — and runs something else in it.  It
//! does not talk to the daemon and does not enter a running process, so unlike
//! `docker exec` it works whether or not the service is up, and it cannot see
//! anything the process changed after it started.
//!
//! The command inherits our stdio, so interactive tools and pipes behave as if
//! servicrab were not in the middle, and its exit status becomes ours.

use std::path::Path;
use std::process::Command;

use servicrab_core::runtime::lookup_service;
use servicrab_core::{load, resolve_config_path};

/// Exit code for "command not found", following the shell convention.
const EXIT_NOT_FOUND: i32 = 127;
/// Exit code for "found but not executable", following the shell convention.
const EXIT_NOT_EXECUTABLE: i32 = 126;

/// Run the `exec` subcommand, returning the process exit code to use.
pub fn run(service: &str, command: &[String], config: Option<&Path>) -> Result<i32, String> {
    let (executable, args) = command
        .split_first()
        .ok_or_else(|| "no command given; try: servicrab exec <SERVICE> -- ls".to_string())?;

    let path = resolve_config_path(config).map_err(|e| format!("could not find config: {e}"))?;

    let (cfg, warnings) = load(&path).map_err(|errors| {
        let msgs: Vec<String> = errors.iter().map(|e| format!("  • {e}")).collect();
        format!(
            "✗ {} has {} error(s):\n{}",
            path.display(),
            errors.len(),
            msgs.join("\n")
        )
    })?;

    for warning in &warnings {
        eprintln!("⚠  {warning}");
    }

    let service = lookup_service(&cfg, service).map_err(|e| e.to_string())?;

    // `service.env` is already the whole layered environment, and `env_clear`
    // is what the supervisor does before applying it, so the command sees
    // neither more nor less than the service would.
    let status = Command::new(executable)
        .args(args)
        .current_dir(&service.cwd)
        .env_clear()
        .envs(&service.env)
        .status();

    match status {
        Ok(status) => Ok(exit_code(status)),
        Err(err) => {
            // A script that runs a command for its exit status needs to tell
            // "the command is missing" from "the command said no", which is why
            // shells reserve 126 and 127 for this instead of a plain failure.
            let code = match err.kind() {
                std::io::ErrorKind::NotFound => EXIT_NOT_FOUND,
                std::io::ErrorKind::PermissionDenied => EXIT_NOT_EXECUTABLE,
                _ => 1,
            };
            eprintln!("error: could not run {executable:?}: {err}");
            Ok(code)
        }
    }
}

/// Map the command's exit status to a process exit code.
fn exit_code(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_command_is_refused_before_the_config_is_read() {
        let err = run("api", &[], None).unwrap_err();
        assert!(err.contains("no command given"), "got: {err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_clean_exit_is_propagated() {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(exit_code(std::process::ExitStatus::from_raw(0)), 0);
    }

    #[cfg(unix)]
    #[test]
    fn signal_death_maps_to_128_plus_signal() {
        use std::os::unix::process::ExitStatusExt;
        // Raw wait status for "killed by SIGKILL" is the signal number itself.
        assert_eq!(exit_code(std::process::ExitStatus::from_raw(9)), 137);
    }
}
