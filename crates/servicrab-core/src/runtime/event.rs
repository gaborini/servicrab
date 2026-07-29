//! Events emitted by the runtime while supervising services.
//!
//! The runtime never formats output itself: it publishes structured events and
//! lets the CLI decide how to render them.  This keeps process handling and
//! user-facing presentation cleanly separated, and gives the future daemon a
//! ready-made stream to forward over its socket.

use std::time::Duration;

use tokio::sync::mpsc;

use crate::config::ServiceName;
use crate::lifecycle::{ExitReason, ServiceState, ShutdownReason};

/// Which standard stream a captured log line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// The child's standard output.
    Stdout,
    /// The child's standard error.
    Stderr,
}

impl std::fmt::Display for Stream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Stream::Stdout => f.write_str("stdout"),
            Stream::Stderr => f.write_str("stderr"),
        }
    }
}

/// What happened to a supervised service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventKind {
    /// The service moved to a new lifecycle state.
    State(ServiceState),
    /// The service's process was spawned.
    Started {
        /// Process-group id (equal to the direct child's pid).
        pgid: i32,
    },
    /// A line was read from the service's captured output.
    Log {
        /// Which stream the line came from.
        stream: Stream,
        /// The line, without its trailing newline.
        line: String,
    },
    /// The service's process stopped.
    Exited {
        /// Why the process stopped.
        reason: ExitReason,
        /// How long it stayed alive.
        uptime: Duration,
    },
    /// The service is waiting before being restarted.
    Backoff {
        /// How long the supervisor will wait.
        delay: Duration,
        /// 1-based restart attempt number.
        attempt: u32,
    },
    /// The service will not be started because a dependency never became
    /// available.
    Skipped {
        /// The dependency that blocked the start.
        dependency: ServiceName,
    },
    /// The supervisor is shutting the service down.
    Stopping {
        /// Why the shutdown was requested.
        reason: ShutdownReason,
    },
    /// The service stopped for good, and no restart will happen.
    Finished {
        /// A human-readable description of the final outcome.
        summary: String,
    },
    /// The service passed its health check and is considered ready.
    Healthy,
    /// A health probe failed.
    HealthProbeFailed {
        /// Why the probe failed.
        message: String,
        /// How many consecutive failures have been seen so far.
        consecutive: u32,
        /// How many consecutive failures are tolerated.
        retries: u32,
    },
    /// The service exhausted its health-check retry budget.
    Unhealthy {
        /// Why the last probe failed.
        message: String,
    },
    /// The service failed fatally.
    Failed {
        /// A human-readable description of the failure.
        message: String,
    },
}

/// A single runtime event, tagged with the service it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceEvent {
    /// The service the event refers to.
    pub service: ServiceName,
    /// What happened.
    pub kind: EventKind,
}

impl ServiceEvent {
    /// Build an event.
    pub fn new(service: ServiceName, kind: EventKind) -> Self {
        Self { service, kind }
    }
}

/// Sending half of the runtime event stream.
pub type EventSender = mpsc::UnboundedSender<ServiceEvent>;
/// Receiving half of the runtime event stream.
pub type EventReceiver = mpsc::UnboundedReceiver<ServiceEvent>;

/// Create a new event channel.
pub fn event_channel() -> (EventSender, EventReceiver) {
    mpsc::unbounded_channel()
}

/// An optional event sink.
///
/// `run` uses no sink (the child inherits the terminal), while `up` collects
/// every event so it can interleave and prefix the output of many services.
#[derive(Debug, Clone, Default)]
pub struct EventSink {
    sender: Option<EventSender>,
}

impl EventSink {
    /// A sink that drops everything.
    pub fn none() -> Self {
        Self { sender: None }
    }

    /// A sink that forwards to `sender`.
    pub fn new(sender: EventSender) -> Self {
        Self {
            sender: Some(sender),
        }
    }

    /// Whether anything is listening.
    pub fn is_active(&self) -> bool {
        self.sender.is_some()
    }

    /// Publish an event, ignoring a closed receiver.
    pub fn emit(&self, service: &ServiceName, kind: EventKind) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(ServiceEvent::new(service.clone(), kind));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(raw: &str) -> ServiceName {
        crate::validation::validate_service_name(raw).expect("valid test name")
    }

    #[test]
    fn inactive_sink_drops_events() {
        let sink = EventSink::none();
        assert!(!sink.is_active());
        sink.emit(&name("api"), EventKind::State(ServiceState::Running));
    }

    #[test]
    fn active_sink_forwards_events() {
        let (tx, mut rx) = event_channel();
        let sink = EventSink::new(tx);
        assert!(sink.is_active());
        sink.emit(&name("api"), EventKind::State(ServiceState::Running));

        let event = rx.try_recv().expect("event delivered");
        assert_eq!(event.service.as_str(), "api");
        assert_eq!(event.kind, EventKind::State(ServiceState::Running));
    }

    #[test]
    fn emitting_into_a_closed_channel_is_not_an_error() {
        let (tx, rx) = event_channel();
        drop(rx);
        let sink = EventSink::new(tx);
        sink.emit(&name("api"), EventKind::State(ServiceState::Failed));
    }

    #[test]
    fn streams_display_as_lowercase_names() {
        assert_eq!(Stream::Stdout.to_string(), "stdout");
        assert_eq!(Stream::Stderr.to_string(), "stderr");
    }
}
