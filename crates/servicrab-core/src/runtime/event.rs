//! Events emitted by the runtime while supervising services.
//!
//! The runtime never formats output itself: it publishes structured events and
//! lets the CLI decide how to render them.  This keeps process handling and
//! user-facing presentation cleanly separated, and gives the future daemon a
//! ready-made stream to forward over its socket.
//!
//! The channel is unbounded for everything the supervisor says *about* a
//! service — that traffic is bounded by the number of services — but the
//! captured output that flows through it is not, so log lines get an explicit
//! allowance and an explicit drop policy.  See [`MAX_QUEUED_LOG_LINES`].

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
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
    /// A watched path changed and a restart was requested.
    WatchTriggered {
        /// The first changed path, in sort order.
        path: std::path::PathBuf,
        /// How many paths changed in this batch.
        changed: usize,
    },
    /// A watch-triggered restart was refused by the supervisor.
    WatchFailed {
        /// Why the restart did not happen.
        message: String,
    },
    /// The watched tree is larger than the scan limit, so changes beyond it
    /// go unnoticed.
    WatchTruncated {
        /// How many files the watcher is willing to scan.
        limit: usize,
    },
    /// Captured output was dropped because the service produced it faster than
    /// the supervisor could consume it.
    LogLinesDropped {
        /// How many lines were dropped since this was last reported.
        count: u64,
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

/// How many captured log lines may be queued on one event channel.
///
/// The channel itself is unbounded, and stays that way for everything the
/// supervisor sends about its services: the number of lifecycle, health and
/// watch events in flight is bounded by the number of services.  Log lines are
/// not — they are bounded only by what a child chooses to print — and they
/// travel to a consumer that writes files, so a single noisy service could
/// grow the supervisor's heap without limit.
///
/// The stack relays each service's events through a per-service channel into the
/// global one, so the allowance is per channel and the supervisor's total is
/// this many lines per running service plus this many on the global stream.
/// That is a bound, which is the point; it is not a tight one.
pub const MAX_QUEUED_LOG_LINES: usize = 1024;

/// Report dropped lines at least this often while the flood continues.
///
/// A drop is normally reported as soon as a line gets through again; this is the
/// backstop for a service that never stops flooding.
const REPORT_DROPS_EVERY: u64 = 1024;

/// The log-line allowance shared by one channel's senders and its receiver.
#[derive(Debug, Default)]
struct LogBudget {
    /// Log lines handed to the channel but not taken out of it yet.
    queued: AtomicUsize,
    /// Lines dropped since the last [`EventKind::LogLinesDropped`].
    dropped: AtomicU64,
}

/// Sending half of the runtime event stream.
///
/// Cheap to clone, and never blocks: a log line that does not fit in the
/// channel's allowance is dropped rather than queued, and the loss is reported
/// as an [`EventKind::LogLinesDropped`] event so it is visible in `up` and in
/// `servicrab events` instead of being silent.
#[derive(Debug, Clone)]
pub struct EventSender {
    inner: mpsc::UnboundedSender<ServiceEvent>,
    budget: Arc<LogBudget>,
}

impl EventSender {
    /// Publish one event.
    ///
    /// `Err` means the receiver is gone.  A dropped log line is *not* an error:
    /// the channel is alive and the loss is reported through it.
    pub fn send(&self, event: ServiceEvent) -> Result<(), ServiceEvent> {
        if !matches!(event.kind, EventKind::Log { .. }) {
            return self.inner.send(event).map_err(|err| err.0);
        }

        // Dropping the newest line rather than the oldest is what keeps the log
        // in order: the queue is a channel, so the only line the sender can
        // still choose to lose is the one in its hand.
        let queued = self.budget.queued.fetch_add(1, Ordering::AcqRel);
        if queued >= MAX_QUEUED_LOG_LINES {
            self.budget.queued.fetch_sub(1, Ordering::AcqRel);
            let dropped = self.budget.dropped.fetch_add(1, Ordering::AcqRel) + 1;
            if dropped % REPORT_DROPS_EVERY == 0 {
                self.report_drops(&event.service);
            }
            return Ok(());
        }

        // The flood let up, so say how much of it went missing.
        if self.budget.dropped.load(Ordering::Acquire) > 0 {
            self.report_drops(&event.service);
        }

        self.inner.send(event).map_err(|err| {
            self.budget.queued.fetch_sub(1, Ordering::AcqRel);
            err.0
        })
    }

    /// Publish how many lines have been dropped since the last report.
    ///
    /// Sent straight down the channel: the report is one event per burst, not
    /// per line, so it is never itself worth dropping.
    fn report_drops(&self, service: &ServiceName) {
        let count = self.budget.dropped.swap(0, Ordering::AcqRel);
        if count == 0 {
            return;
        }
        let _ = self.inner.send(ServiceEvent::new(
            service.clone(),
            EventKind::LogLinesDropped { count },
        ));
    }
}

/// Receiving half of the runtime event stream.
#[derive(Debug)]
pub struct EventReceiver {
    inner: mpsc::UnboundedReceiver<ServiceEvent>,
    budget: Arc<LogBudget>,
}

impl EventReceiver {
    /// Wait for the next event, or `None` once every sender is gone.
    pub async fn recv(&mut self) -> Option<ServiceEvent> {
        let event = self.inner.recv().await;
        if let Some(event) = &event {
            self.release(event);
        }
        event
    }

    /// Take an event that is already queued.
    pub fn try_recv(&mut self) -> Result<ServiceEvent, mpsc::error::TryRecvError> {
        let event = self.inner.try_recv()?;
        self.release(&event);
        Ok(event)
    }

    /// Give a delivered log line's allowance back to the senders.
    fn release(&self, event: &ServiceEvent) {
        if matches!(event.kind, EventKind::Log { .. }) {
            self.budget.queued.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// Create a new event channel.
pub fn event_channel() -> (EventSender, EventReceiver) {
    let (inner_tx, inner_rx) = mpsc::unbounded_channel();
    let budget = Arc::new(LogBudget::default());
    (
        EventSender {
            inner: inner_tx,
            budget: Arc::clone(&budget),
        },
        EventReceiver {
            inner: inner_rx,
            budget,
        },
    )
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

    fn log(line: &str) -> EventKind {
        EventKind::Log {
            stream: Stream::Stdout,
            line: line.to_string(),
        }
    }

    #[test]
    fn queued_log_lines_are_capped_and_the_loss_is_reported() {
        let (tx, mut rx) = event_channel();
        let api = name("api");

        // Nothing reads while the flood runs, so every line stays queued.
        let flood = MAX_QUEUED_LOG_LINES + REPORT_DROPS_EVERY as usize;
        for i in 0..flood {
            tx.send(ServiceEvent::new(api.clone(), log(&format!("line {i}"))))
                .expect("the channel is alive");
        }

        let mut lines = Vec::new();
        let mut dropped = 0u64;
        while let Ok(event) = rx.try_recv() {
            match event.kind {
                EventKind::Log { line, .. } => lines.push(line),
                EventKind::LogLinesDropped { count } => dropped += count,
                other => panic!("unexpected event {other:?}"),
            }
        }

        // The heap is what this bounds, so the queue length is the assertion.
        assert_eq!(lines.len(), MAX_QUEUED_LOG_LINES);
        // The oldest lines are what survived, so the log stays in order.
        let expected: Vec<String> = (0..MAX_QUEUED_LOG_LINES)
            .map(|i| format!("line {i}"))
            .collect();
        assert_eq!(lines, expected);
        assert_eq!(
            dropped + lines.len() as u64,
            flood as u64,
            "every line is either delivered or counted as dropped"
        );
    }

    #[test]
    fn the_allowance_comes_back_once_a_line_is_delivered() {
        let (tx, mut rx) = event_channel();
        let api = name("api");

        for i in 0..MAX_QUEUED_LOG_LINES {
            tx.send(ServiceEvent::new(api.clone(), log(&format!("first {i}"))))
                .unwrap();
        }
        // Full: one more line has to be dropped.
        tx.send(ServiceEvent::new(api.clone(), log("dropped")))
            .unwrap();

        // Drain, which is what a consumer keeping up looks like, then flood
        // again: the second burst must be delivered in full.
        while rx.try_recv().is_ok() {}
        for i in 0..MAX_QUEUED_LOG_LINES {
            tx.send(ServiceEvent::new(api.clone(), log(&format!("second {i}"))))
                .unwrap();
        }

        let mut lines = 0;
        let mut reported = 0u64;
        while let Ok(event) = rx.try_recv() {
            match event.kind {
                EventKind::Log { .. } => lines += 1,
                EventKind::LogLinesDropped { count } => reported += count,
                other => panic!("unexpected event {other:?}"),
            }
        }
        assert_eq!(lines, MAX_QUEUED_LOG_LINES);
        assert_eq!(reported, 1, "the earlier drop should be reported once");
    }

    #[test]
    fn events_other_than_log_lines_are_never_dropped() {
        let (tx, mut rx) = event_channel();
        let api = name("api");

        for i in 0..MAX_QUEUED_LOG_LINES {
            tx.send(ServiceEvent::new(api.clone(), log(&format!("line {i}"))))
                .unwrap();
        }
        // The queue is full of log lines; a lifecycle event still has to make
        // it, because losing one would corrupt the status registry.
        tx.send(ServiceEvent::new(
            api.clone(),
            EventKind::State(ServiceState::Running),
        ))
        .unwrap();

        let states = std::iter::from_fn(|| rx.try_recv().ok())
            .filter(|event| matches!(event.kind, EventKind::State(ServiceState::Running)))
            .count();
        assert_eq!(states, 1);
    }
}
