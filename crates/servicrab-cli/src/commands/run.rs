//! `servicrab run <SERVICE> [--config PATH] [--no-restart]` — run a single
//! service in the foreground.
//!
//! This module only deals with user-facing output and exit-code mapping; all
//! process handling lives in [`servicrab_core::runtime`].

use std::path::Path;

use servicrab_core::runtime::{lookup_service, OutputMode, RunOptions, RunOutcome};
use servicrab_core::{load, resolve_config_path, ExitReason, ForegroundRunner, ShutdownReason};

/// Exit code used when a run is cut short by Ctrl+C (`128 + SIGINT`).
const EXIT_SIGINT: i32 = 130;
/// Exit code used when the supervisor itself was terminated (`128 + SIGTERM`).
const EXIT_SIGTERM: i32 = 143;

/// Run the `run` subcommand, returning the process exit code to use.
pub fn run(service: &str, config: Option<&Path>, no_restart: bool) -> Result<i32, String> {
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

    let options = RunOptions {
        no_restart,
        output: OutputMode::Inherit,
    };
    let mut runner = ForegroundRunner::new(service, options);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("failed to start the async runtime: {e}"))?;

    match runtime.block_on(runner.run()) {
        Ok(outcome) => Ok(exit_code(outcome)),
        Err(err) => Err(err.to_string()),
    }
}

/// Map a terminal [`RunOutcome`] to a process exit code.
fn exit_code(outcome: RunOutcome) -> i32 {
    match outcome {
        RunOutcome::Exited { reason, .. } => match reason {
            ExitReason::Code(code) => code,
            ExitReason::Signal(sig) => 128 + sig,
            ExitReason::SpawnFailure { .. } => 1,
        },
        RunOutcome::Stopped { reason } => match reason {
            ShutdownReason::UserInterrupt => EXIT_SIGINT,
            ShutdownReason::Terminated => EXIT_SIGTERM,
            ShutdownReason::RestartLimit | ShutdownReason::StackFailure => 1,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_exit_code_is_propagated() {
        assert_eq!(
            exit_code(RunOutcome::Exited {
                reason: ExitReason::Code(0),
                restarts: 0
            }),
            0
        );
        assert_eq!(
            exit_code(RunOutcome::Exited {
                reason: ExitReason::Code(42),
                restarts: 3
            }),
            42
        );
    }

    #[test]
    fn signal_death_maps_to_128_plus_signal() {
        assert_eq!(
            exit_code(RunOutcome::Exited {
                reason: ExitReason::Signal(9),
                restarts: 0
            }),
            137
        );
    }

    #[test]
    fn shutdown_reasons_map_to_conventional_codes() {
        assert_eq!(
            exit_code(RunOutcome::Stopped {
                reason: ShutdownReason::UserInterrupt
            }),
            130
        );
        assert_eq!(
            exit_code(RunOutcome::Stopped {
                reason: ShutdownReason::Terminated
            }),
            143
        );
        assert_eq!(
            exit_code(RunOutcome::Stopped {
                reason: ShutdownReason::RestartLimit
            }),
            1
        );
    }
}
