//! `servicrab run <SERVICE> [--config PATH] [--no-restart]` — run a single
//! service in the foreground.
//!
//! This module only deals with user-facing output and exit-code mapping; all
//! process handling lives in [`servicrab_core::runtime`].

use std::io::Write;
use std::path::Path;

use servicrab_core::runtime::{lookup_service, OutputMode, RunOptions, RunOutcome};
use servicrab_core::{
    event_channel, load, resolve_config_path, EventKind, EventReceiver, ExitReason,
    ForegroundRunner, LogRouter, LogSink, ShutdownReason, Stream,
};

/// Exit code used when a run is cut short by Ctrl+C (`128 + SIGINT`).
const EXIT_SIGINT: i32 = 130;
/// Exit code used when the supervisor itself was terminated (`128 + SIGTERM`).
const EXIT_SIGTERM: i32 = 143;
/// Exit code used when the controlling terminal went away (`128 + SIGHUP`).
const EXIT_SIGHUP: i32 = 129;

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
    let mut router = crate::commands::logs::router_for(&cfg);
    if let Some(router) = router.as_ref() {
        if !router.handles(&service.name) {
            // The service opted out, so there is nothing to capture.
            return foreground(service, no_restart, None);
        }
    } else {
        return foreground(service, no_restart, None);
    }

    foreground(service, no_restart, router.take())
}

/// Run one service in the foreground, optionally teeing its output to a file.
fn foreground(
    service: &servicrab_core::Service,
    no_restart: bool,
    router: Option<LogRouter>,
) -> Result<i32, String> {
    // Without file logging the child inherits our stdio, which keeps colours,
    // TTY detection, and back-pressure exactly as they would be without
    // servicrab in the middle.
    let options = RunOptions {
        no_restart,
        output: if router.is_some() {
            OutputMode::Capture
        } else {
            OutputMode::Inherit
        },
    };
    let mut runner = ForegroundRunner::new(service, options);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("failed to start the async runtime: {e}"))?;

    let outcome = runtime.block_on(async move {
        let Some(router) = router else {
            return runner.run().await;
        };

        let (events_tx, events_rx) = event_channel();
        let tee = tokio::spawn(tee_output(events_rx, router));
        let outcome = runner
            .with_events(servicrab_core::EventSink::new(events_tx))
            .run()
            .await;
        // The runner held the last sender, so the tee finishes on its own.
        let _ = tee.await;
        outcome
    });

    match outcome {
        Ok(outcome) => Ok(exit_code(outcome)),
        Err(err) => Err(err.to_string()),
    }
}

/// Echo captured output verbatim while copying it into the log file.
///
/// The file work runs on a blocking task, so a slow disk cannot stall the
/// runtime that is waiting on the child.
async fn tee_output(mut events: EventReceiver, router: LogRouter) {
    let sink = LogSink::spawn(router);

    while let Some(event) = events.recv().await {
        let EventKind::Log { stream, line } = &event.kind else {
            continue;
        };
        if let Some(problem) = sink.record(&event.service, line).await {
            eprintln!("⚠  {problem}");
        }
        match stream {
            Stream::Stdout => {
                let mut out = std::io::stdout().lock();
                let _ = writeln!(out, "{line}");
                let _ = out.flush();
            }
            Stream::Stderr => {
                let mut err = std::io::stderr().lock();
                let _ = writeln!(err, "{line}");
                let _ = err.flush();
            }
        }
    }

    // The run is over; the queued lines still have to reach the file.
    if let Some(problem) = sink.shutdown().await {
        eprintln!("⚠  {problem}");
    }
}

/// Map a terminal [`RunOutcome`] to a process exit code.
fn exit_code(outcome: RunOutcome) -> i32 {
    match outcome {
        RunOutcome::Exited { reason, .. } => match reason {
            ExitReason::Code(code) => code,
            ExitReason::Signal(sig) => 128 + sig,
            ExitReason::SpawnFailure { .. } | ExitReason::Unhealthy => 1,
        },
        RunOutcome::Stopped { reason } => match reason {
            ShutdownReason::UserInterrupt => EXIT_SIGINT,
            ShutdownReason::Terminated => EXIT_SIGTERM,
            ShutdownReason::HangUp => EXIT_SIGHUP,
            // `run` supervises a single service with no daemon around it, so
            // nobody can ask for a targeted stop; treat it as a clean one.
            ShutdownReason::Requested => 0,
            ShutdownReason::RestartLimit
            | ShutdownReason::StackFailure
            | ShutdownReason::Unhealthy => 1,
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
                reason: ShutdownReason::HangUp
            }),
            129
        );
        assert_eq!(
            exit_code(RunOutcome::Stopped {
                reason: ShutdownReason::RestartLimit
            }),
            1
        );
    }
}
