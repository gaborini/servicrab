//! Translating runtime types into their wire form.
//!
//! `servicrab-protocol` deliberately knows nothing about the runtime, so the
//! mapping lives here — shared by the daemon (which streams events over its
//! socket) and by `up --json` (which prints the very same lines).

use servicrab_core::{EventKind, ServiceState};
use servicrab_protocol::Event;

/// Translate a runtime event into its wire form.
pub fn to_wire_event(kind: &EventKind) -> Event {
    use servicrab_core::ExitReason;

    match kind {
        EventKind::State(state) => Event::State {
            state: to_wire_state(*state),
        },
        EventKind::Started { pgid } => Event::Started { pgid: *pgid },
        EventKind::Log { stream, line } => Event::Log {
            stream: match stream {
                servicrab_core::Stream::Stdout => servicrab_protocol::Stream::Stdout,
                servicrab_core::Stream::Stderr => servicrab_protocol::Stream::Stderr,
            },
            line: line.clone(),
        },
        EventKind::Exited { reason, uptime } => Event::Exited {
            reason: reason.to_string(),
            code: match reason {
                ExitReason::Code(code) => Some(*code),
                _ => None,
            },
            signal: match reason {
                ExitReason::Signal(signal) => Some(*signal),
                _ => None,
            },
            uptime_ms: uptime.as_millis() as u64,
        },
        EventKind::Backoff { delay, attempt } => Event::Backoff {
            delay_ms: delay.as_millis() as u64,
            attempt: *attempt,
        },
        EventKind::Skipped { dependency } => Event::Skipped {
            dependency: dependency.to_string(),
        },
        EventKind::Stopping { reason } => Event::Stopping {
            reason: reason.to_string(),
        },
        EventKind::Finished { summary } => Event::Finished {
            summary: summary.clone(),
        },
        EventKind::Healthy => Event::Healthy,
        EventKind::HealthProbeFailed {
            message,
            consecutive,
            retries,
        } => Event::HealthProbeFailed {
            message: message.clone(),
            consecutive: *consecutive,
            retries: *retries,
        },
        EventKind::Unhealthy { message } => Event::Unhealthy {
            message: message.clone(),
        },
        EventKind::WatchTriggered { path, changed } => Event::WatchTriggered {
            path: path.display().to_string(),
            changed: *changed,
        },
        EventKind::WatchFailed { message } => Event::WatchFailed {
            message: message.clone(),
        },
        EventKind::WatchTruncated { limit } => Event::WatchTruncated { limit: *limit },
        EventKind::Failed { message } => Event::Failed {
            message: message.clone(),
        },
    }
}

/// Convert a runtime lifecycle state into its wire representation.
pub fn to_wire_state(state: ServiceState) -> servicrab_protocol::ServiceState {
    use servicrab_protocol::ServiceState as Wire;

    match state {
        ServiceState::Pending => Wire::Pending,
        ServiceState::Starting => Wire::Starting,
        ServiceState::Running => Wire::Running,
        ServiceState::Backoff => Wire::Backoff,
        ServiceState::Stopping => Wire::Stopping,
        ServiceState::Stopped => Wire::Stopped,
        ServiceState::Exited => Wire::Exited,
        ServiceState::Failed => Wire::Failed,
    }
}

/// Convert a runtime status into its wire representation.
pub fn to_wire_status(status: &servicrab_core::ServiceStatus) -> servicrab_protocol::ServiceInfo {
    servicrab_protocol::ServiceInfo {
        name: status.name.to_string(),
        state: to_wire_state(status.state),
        pid: status.pid,
        uptime_secs: status.uptime.map(|d| d.as_secs()),
        restarts: status.restarts,
        health: match status.health {
            servicrab_core::Health::None => servicrab_protocol::Health::None,
            servicrab_core::Health::Starting => servicrab_protocol::Health::Starting,
            servicrab_core::Health::Healthy => servicrab_protocol::Health::Healthy,
            servicrab_core::Health::Unhealthy => servicrab_protocol::Health::Unhealthy,
        },
        message: status.message.clone(),
    }
}
