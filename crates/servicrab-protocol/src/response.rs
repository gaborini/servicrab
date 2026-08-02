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
    ///
    /// The same number [`Event::Started`] calls `pgid`, under the name it has
    /// always had here.  Every service runs in its own process group whose
    /// leader is the direct child, so this is a pid as well — but signalling it
    /// as one reaches only the leader, which is the mistake the name invites.
    /// Read [`ServiceInfo::pgid`] instead; this field stays because removing it
    /// would break every existing reader.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pgid: Option<i32>,
    /// Deprecated alias for [`ServiceInfo::pgid`], carrying the same value.
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

/// The version of the machine-readable output contract.
///
/// Carried by every `--json` document the CLI prints, so a script can refuse a
/// stream it was not written for instead of guessing.  It is bumped only when a
/// shape changes in a way that an existing reader would misread; adding an
/// optional field is not such a change.
///
/// The socket protocol's own responses do not carry it: they are tagged by
/// `type` and versioned by the daemon that speaks them, and a subscriber reads
/// thousands of event lines where the number would be pure repetition.  It
/// appears once, in the `ok` line that answers `subscribe`.
pub const SCHEMA_VERSION: u32 = 1;

/// A stable, machine-readable classification of a failed request.
///
/// The point of this existing at all is that [`Response::Error::message`] does
/// not: the message is written for the operator reading it and is free to be
/// reworded, while a script matching on `code` keeps working.  Compared as a
/// string in JSON (`"unknown_service"`), so the set can grow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorCode {
    /// The request named a service this daemon does not supervise.
    UnknownService,
    /// The service is in the middle of another command.
    Busy,
    /// Nothing is listening: no daemon is running for this project.
    NotRunning,
    /// The service, or the daemon, is already running.
    AlreadyRunning,
    /// The configuration file did not load, or did not validate.
    ValidationFailed,
    /// The command is not supported on this platform, or by this daemon.
    Unsupported,
    /// The request failed for a reason with no code of its own.
    ///
    /// Also what a response that predates this field decodes to: such a
    /// response says no more than that the request failed.
    #[default]
    Failed,
}

impl ErrorCode {
    /// The code as it appears on the wire.
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCode::UnknownService => "unknown_service",
            ErrorCode::Busy => "busy",
            ErrorCode::NotRunning => "not_running",
            ErrorCode::AlreadyRunning => "already_running",
            ErrorCode::ValidationFailed => "validation_failed",
            ErrorCode::Unsupported => "unsupported",
            ErrorCode::Failed => "failed",
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How many services a reload added, changed and removed.
///
/// The same three numbers the reload's message spells out in prose, as numbers,
/// because "1 added, 0 changed, 2 removed" is a sentence and a caller that
/// wants to know whether anything happened should not have to parse one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ReloadChanges {
    /// Services that were started because the new configuration declares them.
    pub added: usize,
    /// Services that were restarted because their definition changed.
    pub changed: usize,
    /// Services that were stopped because the new configuration drops them.
    pub removed: usize,
}

impl ReloadChanges {
    /// Whether the reload changed nothing at all.
    pub fn is_empty(&self) -> bool {
        self.added == 0 && self.changed == 0 && self.removed == 0
    }
}

/// A response returned by the servicrab daemon to a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Response {
    /// The requested operation completed successfully.
    Ok {
        /// What happened, in prose, for a person to read.
        ///
        /// Explicitly **not** part of the API: the wording ("api started",
        /// "reloaded demo: 1 added, 0 changed, 0 removed") is free to change
        /// between releases.  Anything a program needs to act on is a field of
        /// its own — see [`Response::Ok::changes`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,

        /// What a reload changed, present only in the answer to
        /// [`crate::Request::Reload`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        changes: Option<ReloadChanges>,

        /// The output contract this daemon speaks, present in the `ok` that
        /// answers [`crate::Request::Subscribe`].
        ///
        /// It is on this one response because that is the handshake: a
        /// subscriber learns the version once and then reads events, rather
        /// than being told again on every line.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        schema_version: Option<u32>,
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
        /// Why it failed, as something to match on.
        ///
        /// This is the field a script should read.  Missing from responses
        /// written before it existed, which decode as [`ErrorCode::Failed`].
        #[serde(default)]
        code: ErrorCode,

        /// Why it failed, in prose, for a person to read.  Not part of the
        /// API; the wording may change between releases.
        message: String,

        /// The individual problems, when there is more than one.
        ///
        /// A configuration that does not validate has one entry per error,
        /// rather than one string with newlines in it that a caller would have
        /// to split.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        errors: Vec<String>,
    },

    /// A reply this build has no name for, because a newer daemon sent it.
    ///
    /// See [`UNKNOWN`].  This is the variant that keeps a subscriber alive: an
    /// event stream is read until the daemon goes away, so a line it cannot
    /// name has to be something it can skip rather than something it dies on.
    #[serde(other)]
    Unknown,
}

impl Response {
    /// A bare success, with nothing to say about it.
    pub fn ok() -> Self {
        Response::Ok {
            message: None,
            changes: None,
            schema_version: None,
        }
    }

    /// A success with a message for the operator.
    pub fn message(message: impl Into<String>) -> Self {
        Response::Ok {
            message: Some(message.into()),
            changes: None,
            schema_version: None,
        }
    }

    /// A failure with a code to match on and a message to read.
    pub fn error(code: ErrorCode, message: impl Into<String>) -> Self {
        Response::Error {
            code,
            message: message.into(),
            errors: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole reason `pid` survives: a client written against 0.x reads that
    /// field and must keep finding the number there.
    #[test]
    fn a_status_entry_reports_the_process_group_under_both_names() {
        let info = ServiceInfo {
            name: "api".to_string(),
            state: ServiceState::Running,
            pgid: Some(4242),
            pid: Some(4242),
            uptime_secs: Some(3),
            restarts: 0,
            health: Health::None,
            message: None,
        };

        let json = serde_json::to_value(&info).expect("serialize");
        assert_eq!(json["pgid"], 4242);
        assert_eq!(json["pid"], 4242, "the deprecated alias carries the value");
    }

    /// A reader that only knows `pid` is exactly the reader this alias is for,
    /// and one that only knows `pgid` is the reader it is being replaced by.
    #[test]
    fn a_status_entry_decodes_from_either_field_alone() {
        let old: ServiceInfo = serde_json::from_str(
            r#"{"name":"api","state":"running","pid":7,"restarts":0,"health":"none"}"#,
        )
        .expect("a 0.x status entry");
        assert_eq!(old.pid, Some(7));

        let new: ServiceInfo = serde_json::from_str(
            r#"{"name":"api","state":"running","pgid":7,"restarts":0,"health":"none"}"#,
        )
        .expect("a status entry with only the new name");
        assert_eq!(new.pgid, Some(7));
    }

    /// The point of the field: `1 added, 0 changed, 2 removed` is a sentence,
    /// and a caller deciding whether to act should read numbers instead.
    #[test]
    fn a_reload_reports_its_counts_as_numbers() {
        let response = Response::Ok {
            message: Some("reloaded demo: 1 added, 0 changed, 2 removed".to_string()),
            changes: Some(ReloadChanges {
                added: 1,
                changed: 0,
                removed: 2,
            }),
            schema_version: None,
        };

        let json = serde_json::to_value(&response).expect("serialize");
        assert_eq!(json["changes"]["added"], 1);
        assert_eq!(json["changes"]["changed"], 0);
        assert_eq!(json["changes"]["removed"], 2);
    }

    /// Every other `ok` keeps the shape it had, so a reader of those lines sees
    /// no new keys at all.
    #[test]
    fn an_ok_with_nothing_to_add_carries_no_extra_fields() {
        let json = serde_json::to_value(Response::message("api started")).expect("serialize");

        assert_eq!(json["type"], "ok");
        assert_eq!(json["message"], "api started");
        assert!(json.get("changes").is_none(), "{json}");
        assert!(json.get("schema_version").is_none(), "{json}");
    }

    #[test]
    fn an_error_carries_a_code_beside_its_message() {
        let json = serde_json::to_value(Response::error(
            ErrorCode::UnknownService,
            "unknown service \"web\"",
        ))
        .expect("serialize");

        assert_eq!(json["code"], "unknown_service");
        assert_eq!(json["message"], "unknown service \"web\"");
        assert!(json.get("errors").is_none(), "no list to report: {json}");
    }

    /// A validation failure used to be one string with newlines and bullets in
    /// it, which every caller had to take apart again.
    #[test]
    fn a_validation_failure_lists_its_errors_separately() {
        let response = Response::Error {
            code: ErrorCode::ValidationFailed,
            message: "servicrab.toml has 2 error(s); the stack was left untouched".to_string(),
            errors: vec![
                "services.api: command must not be empty".to_string(),
                "services.db: unknown dependency \"cache\"".to_string(),
            ],
        };

        let json = serde_json::to_value(&response).expect("serialize");
        assert_eq!(json["code"], "validation_failed");
        assert_eq!(json["errors"].as_array().expect("a list").len(), 2);
        assert!(
            !json["message"].as_str().expect("a message").contains('\n'),
            "the message is one line now: {json}"
        );
    }

    /// An error line from a daemon that predates the field still decodes, and
    /// says no more than that the request failed.
    #[test]
    fn an_error_without_a_code_decodes_as_a_plain_failure() {
        let response: Response =
            serde_json::from_str(r#"{"type":"error","message":"it went wrong"}"#)
                .expect("a 0.x error line");

        let Response::Error { code, errors, .. } = response else {
            panic!("expected an error");
        };
        assert_eq!(code, ErrorCode::Failed);
        assert!(errors.is_empty());
    }
}
