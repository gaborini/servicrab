//! Response types sent from the daemon back to clients.

use serde::{Deserialize, Serialize};

/// The tag every enum here falls back to when the daemon names something this
/// build has never heard of.
///
/// `#[non_exhaustive]` buys nothing on a socket.  It stops a downstream crate
/// from writing an exhaustive `match`, which is a promise about *source*
/// compatibility, and says nothing at all about a line of JSON: serde's
/// internally tagged representation rejects an unrecognised tag outright, so
/// before these fallbacks existed a single new event kind from a 1.1 daemon
/// failed to decode and took the whole event stream down with it — on every
/// client of every earlier release, mid-run.  Every wildcard arm written on the
/// strength of `#[non_exhaustive]` was therefore unreachable from the wire, and
/// the comments explaining them were describing something that could not
/// happen.
///
/// The fallbacks are what make those arms real.  A client decodes what it
/// understands, keeps reading, and hands the rest on untouched — the raw line is
/// still what `--json` prints, so nothing is lost to a consumer that knows more
/// than this build does.
///
/// It is a reserved word in the wire format as a consequence: a future release
/// must not name a real variant `unknown`, or older clients would silently
/// classify it as one of these instead of reporting it.
pub const UNKNOWN: &str = "unknown";

/// The lifecycle state of a service, as reported by the daemon.
///
/// This mirrors `servicrab_core::ServiceState`, but the protocol crate stays
/// independent of the runtime so that clients need not depend on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ServiceState {
    /// Not started yet, or waiting for a dependency.
    Pending,
    /// The process is being spawned.
    Starting,
    /// The process is alive.
    Running,
    /// Waiting before a restart.
    Backoff,
    /// Being shut down.
    Stopping,
    /// Stopped on request, and will not restart.
    Stopped,
    /// The process ended on its own.
    Exited,
    /// The service failed fatally.
    Failed,
    /// A state this build has no name for, because a newer daemon reported it.
    ///
    /// See [`UNKNOWN`] for why every enum on this side of the wire has one.
    #[serde(other)]
    Unknown,
}

impl std::fmt::Display for ServiceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            ServiceState::Pending => "pending",
            ServiceState::Starting => "starting",
            ServiceState::Running => "running",
            ServiceState::Backoff => "backoff",
            ServiceState::Stopping => "stopping",
            ServiceState::Stopped => "stopped",
            ServiceState::Exited => "exited",
            ServiceState::Failed => "failed",
            ServiceState::Unknown => UNKNOWN,
        };
        f.write_str(text)
    }
}

/// What the health checks say about a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Health {
    /// The service has no health check.
    None,
    /// The service has a health check that has not passed yet.
    Starting,
    /// The last probe succeeded.
    Healthy,
    /// The service exhausted its retry budget.
    Unhealthy,
    /// A verdict this build has no name for, because a newer daemon reported it.
    ///
    /// See [`UNKNOWN`].
    #[serde(other)]
    Unknown,
}

impl std::fmt::Display for Health {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Health::None => "-",
            Health::Starting => "starting",
            Health::Healthy => "healthy",
            Health::Unhealthy => "unhealthy",
            Health::Unknown => UNKNOWN,
        };
        f.write_str(text)
    }
}

/// A point-in-time report about one service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceInfo {
    /// Service name as declared in `servicrab.toml`.
    pub name: String,
    /// Current lifecycle state.
    pub state: ServiceState,
    /// Process-group id of the running process, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<i32>,
    /// How long the current process has been running, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_secs: Option<u64>,
    /// How many times the service has been restarted.
    pub restarts: u32,
    /// Health-check verdict.
    pub health: Health,
    /// The most recent noteworthy message (an error, a probe failure, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Which standard stream a captured log line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Stream {
    /// The service's standard output.
    Stdout,
    /// The service's standard error.
    Stderr,
    /// A stream this build has no name for, because a newer daemon named it.
    ///
    /// See [`UNKNOWN`].
    #[serde(other)]
    Unknown,
}

impl std::fmt::Display for Stream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Stream::Stdout => f.write_str("stdout"),
            Stream::Stderr => f.write_str("stderr"),
            Stream::Unknown => f.write_str(UNKNOWN),
        }
    }
}

/// Something that happened to a service, as streamed by the daemon.
///
/// This mirrors the runtime's event enum, but the protocol crate stays
/// independent of the runtime: durations are plain milliseconds and paths are
/// strings, so any client can read them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Event {
    /// The service moved to a new lifecycle state.
    State {
        /// The state it moved to.
        state: ServiceState,
    },
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
        /// Human-readable description of why the process stopped.
        reason: String,
        /// Exit code, when the process exited on its own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<i32>,
        /// Signal number, when the process was killed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signal: Option<i32>,
        /// How long it stayed alive.
        uptime_ms: u64,
    },
    /// The service is waiting before being restarted.
    Backoff {
        /// How long the supervisor will wait.
        delay_ms: u64,
        /// 1-based restart attempt number.
        attempt: u32,
    },
    /// The service was never started because a dependency never came up.
    Skipped {
        /// The dependency that blocked the start.
        dependency: String,
    },
    /// The supervisor is shutting the service down.
    Stopping {
        /// Why the shutdown was requested.
        reason: String,
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
        path: String,
        /// How many paths changed in this batch.
        changed: usize,
    },
    /// A watch-triggered restart was refused by the supervisor.
    WatchFailed {
        /// Why the restart did not happen.
        message: String,
    },
    /// The watched tree is larger than the scan limit.
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
    /// Something a newer daemon reported that this build has no name for.
    ///
    /// See [`UNKNOWN`].  The payload is dropped rather than kept: a client that
    /// wants the detail of an event it does not understand wants the line the
    /// daemon sent, not a half-interpreted copy of it, and that line is what
    /// [`crate::Response`] consumers are handed alongside the decoded value.
    #[serde(other)]
    Unknown,
}

/// A response returned by the servicrab daemon to a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Response {
    /// The requested operation completed successfully.
    Ok {
        /// Optional human-readable message.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },

    /// Answer to [`crate::Request::Ping`].
    Pong {
        /// Project the daemon supervises.
        project: String,
        /// The daemon's own process id.
        pid: u32,
        /// Which revision of this wire format the daemon speaks.
        ///
        /// Optional, and absent means "did not say": a 0.3 daemon has no such
        /// field, and a client that treated silence as a mismatch would refuse
        /// to talk to one.  See [`crate::PROTOCOL_VERSION`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<u32>,
    },

    /// Answer to [`crate::Request::Status`].
    Status {
        /// One entry per service, in start order.
        services: Vec<ServiceInfo>,
    },

    /// One streamed runtime event, sent after [`crate::Request::Subscribe`].
    Event {
        /// The service the event belongs to.
        service: String,
        /// What happened.
        event: Event,
    },

    /// Events were dropped because the client could not keep up.
    Lagged {
        /// How many events were skipped.
        skipped: u64,
    },

    /// The requested operation failed.
    Error {
        /// Human-readable description of what went wrong.
        message: String,
    },

    /// A reply this build has no name for, because a newer daemon sent it.
    ///
    /// See [`UNKNOWN`].  This is the variant that keeps a subscriber alive: an
    /// event stream is read until the daemon goes away, so a line it cannot
    /// name has to be something it can skip rather than something it dies on.
    #[serde(other)]
    Unknown,
}
