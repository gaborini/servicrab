//! What happens when the two ends of the socket are not the same release.
//!
//! After 1.0 the wire format is frozen, which means every line either side
//! sends has to be readable by a build that predates it.  These tests stand a
//! *newer* daemon up by hand — a plain Unix-socket server writing lines this
//! build has no variants for — because there is no other way to get one: the
//! real daemon can only send what this crate can already name.

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tempfile::TempDir;

/// Upper bound for any wait here; keeps a hung test from stalling CI.
const CEILING: Duration = Duration::from_secs(20);

fn binary() -> PathBuf {
    assert_cmd::cargo::cargo_bin("servicrab")
}

/// A project the CLI will load, whose one service exits at once — nothing here
/// is about supervision.
fn a_project(dir: &Path) -> PathBuf {
    let path = dir.join("servicrab.toml");
    fs::write(
        &path,
        "version = 1\n[project]\nname = \"demo\"\n[services.api]\ncommand = [\"true\"]\n",
    )
    .unwrap();
    path
}

/// Where the CLI will look for this project's daemon.
fn socket_path(config: &Path) -> PathBuf {
    config.parent().unwrap().join(".servicrab/daemon.sock")
}

/// A daemon from the future: it answers by request type, so a test says what a
/// 1.1 daemon would send without having to know how many connections a command
/// opens or in what order.
///
/// The thread is left running rather than joined.  It ends when the listener is
/// dropped with the temporary directory, and a test that waited on it would hang
/// on exactly the failure it is trying to report: a client that gave up early
/// leaves the next `accept` waiting forever.
struct StandIn {
    _dir: TempDir,
    config: PathBuf,
}

impl StandIn {
    /// `pong` names a revision far ahead of ours; `status` and the subscribe
    /// stream are whatever the test hands in.
    fn new(status: &'static str, stream: Vec<String>) -> Self {
        let dir = TempDir::new().unwrap();
        let config = a_project(dir.path());
        fs::create_dir_all(config.parent().unwrap().join(".servicrab")).unwrap();
        let listener = UnixListener::bind(socket_path(&config)).expect("bind the stand-in daemon");

        std::thread::spawn(move || {
            while let Ok((stream_socket, _)) = listener.accept() {
                let mut writer = stream_socket.try_clone().expect("clone");
                let mut request = String::new();
                let mut reader = BufReader::new(stream_socket);
                if reader.read_line(&mut request).unwrap_or(0) == 0 {
                    continue;
                }

                let mut lines = Vec::new();
                if request.contains("\"ping\"") {
                    lines.push(
                        r#"{"type":"pong","project":"demo","pid":1,"version":99}"#.to_string(),
                    );
                } else if request.contains("\"status\"") {
                    lines.push(status.to_string());
                } else if request.contains("\"subscribe\"") {
                    lines.push(r#"{"type":"ok"}"#.to_string());
                    lines.extend(stream.iter().cloned());
                } else {
                    lines.push(r#"{"type":"error","message":"not in this test"}"#.to_string());
                }

                for line in lines {
                    if writer.write_all(line.as_bytes()).is_err() {
                        break;
                    }
                    let _ = writer.write_all(b"\n");
                    let _ = writer.flush();
                }
                // Closing is how a daemon shutting down looks, which is what
                // ends a subscriber cleanly.
            }
        });

        Self { _dir: dir, config }
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        run_cli(&self.config, args)
    }
}

fn run_cli(config: &Path, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(binary())
        .args(args)
        .arg("--config")
        .arg(config)
        .env_remove("RUST_LOG")
        .env("NO_COLOR", "1")
        .output()
        .expect("failed to run servicrab");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// One `log` line, as a 1.1 daemon would still send it.
fn a_log_line(text: &str) -> String {
    format!(
        r#"{{"type":"event","service":"api","event":{{"kind":"log","stream":"stdout","line":"{text}"}}}}"#
    )
}

/// The defect this file exists for.
///
/// One event kind a 1.0 client has never heard of used to fail to decode, and
/// the subscribe loop turned that into an error and a non-zero exit — so a
/// single new event type in 1.1 would have killed `servicrab events` on every
/// 1.0 client, mid-run, and taken every event behind it with it.  What has to
/// survive is not the unknown line but the ones after it.
#[test]
fn an_unknown_event_kind_does_not_end_the_stream() {
    let daemon = StandIn::new(
        "",
        vec![
            a_log_line("before"),
            // The line from the future, in the middle where it does the damage.
            r#"{"type":"event","service":"api","event":{"kind":"teleported","destination":"mars"}}"#
                .to_string(),
            a_log_line("after"),
        ],
    );

    let (code, stdout, stderr) = daemon.run(&["events", "--json"]);

    assert_eq!(code, 0, "the stream ended badly: {stdout}{stderr}");
    assert!(stdout.contains("\"line\":\"before\""), "{stdout}");
    assert!(
        stdout.contains("\"line\":\"after\""),
        "everything after the unknown event was lost:\n{stdout}{stderr}"
    );
    // `--json` is a passthrough, so the unknown line survives verbatim for a
    // consumer that knows what it means even though this build does not.
    assert!(stdout.contains("\"kind\":\"teleported\""), "{stdout}");
}

/// The same for a reply type rather than an event kind: a 1.1 daemon may
/// announce something mid-stream that has nothing to do with any service, and a
/// client that cannot name it still has to keep reading.
#[test]
fn an_unknown_response_type_does_not_end_the_stream() {
    let daemon = StandIn::new(
        "",
        vec![
            r#"{"type":"reconfigured","by":"someone"}"#.to_string(),
            a_log_line("after"),
        ],
    );

    let (code, stdout, stderr) = daemon.run(&["events", "--json"]);

    assert_eq!(code, 0, "the stream ended badly: {stdout}{stderr}");
    assert!(stdout.contains("\"line\":\"after\""), "{stdout}{stderr}");
}

/// A state or health verdict from a newer daemon used to fail the whole status
/// snapshot, so the reply was reported as malformed rather than as the services
/// it could perfectly well have named.
#[test]
fn an_unknown_state_still_leaves_a_status_readable() {
    let daemon = StandIn::new(
        r#"{"type":"status","services":[{"name":"api","state":"hibernating","restarts":0,"health":"degraded"},{"name":"db","state":"running","restarts":0,"health":"healthy"}]}"#,
        Vec::new(),
    );

    let (_, stdout, stderr) = daemon.run(&["status"]);

    assert!(
        stdout.contains("db") && stdout.contains("running"),
        "the service this build understands was lost:\n{stdout}{stderr}"
    );
    assert!(stdout.contains("api"), "{stdout}{stderr}");
    assert!(
        !stderr.contains("malformed message"),
        "a state from the future was reported as a broken message:\n{stderr}"
    );
}

/// Write one line to a daemon and read the answer.
fn ask(socket: &Path, line: &str) -> String {
    let stream = UnixStream::connect(socket).expect("connect to the daemon");
    stream
        .set_read_timeout(Some(CEILING))
        .expect("read timeout");
    let mut writer = stream.try_clone().expect("clone");
    writer.write_all(line.as_bytes()).expect("write");
    writer.flush().expect("flush");

    let mut reply = String::new();
    BufReader::new(stream)
        .read_line(&mut reply)
        .expect("read the reply");
    reply
}

/// Runs a real daemon for the tests where the daemon is the side under test,
/// and stops it however the test ends.
struct RealDaemon {
    _dir: TempDir,
    config: PathBuf,
}

impl RealDaemon {
    fn start() -> Self {
        let dir = TempDir::new().unwrap();
        let config = a_project(dir.path());
        let (code, stdout, stderr) = run_cli(&config, &["start"]);
        assert_eq!(code, 0, "start failed: {stdout}{stderr}");
        Self { _dir: dir, config }
    }

    fn socket(&self) -> PathBuf {
        socket_path(&self.config)
    }

    fn log(&self) -> String {
        fs::read_to_string(self.config.parent().unwrap().join(".servicrab/daemon.log"))
            .expect("the daemon keeps a log")
    }
}

impl Drop for RealDaemon {
    fn drop(&mut self) {
        let _ = run_cli(&self.config, &["down"]);
    }
}

/// The daemon's own end of the same problem: a request it has no name for used
/// to come back as `malformed message: unknown variant …`, which reads as "your
/// client is broken" when the truth is "this daemon is older than your client".
///
/// The refusal has to name the request, and that is not a nicety.  Deciding an
/// unknown request is no longer a decode error is also deciding to throw away
/// what serde said about it — `unknown variant "strat", expected one of …`,
/// which told a client author their typo *and* the complete valid set.  A
/// genuinely newer client knows what it asked for; a typo or a half-written
/// client is the common case and does not.
#[test]
fn a_request_from_the_future_is_refused_by_name() {
    let daemon = RealDaemon::start();

    let reply = ask(&daemon.socket(), "{\"type\":\"drain\",\"grace_ms\":500}\n");

    assert!(reply.contains("does not support"), "{reply}");
    assert!(
        reply.contains("drain"),
        "the refusal did not say which request it was refusing: {reply}"
    );
    // And what to write instead, which is the other half of what serde used to
    // give a client author for free.
    for supported in ["ping", "status", "restart_service", "subscribe"] {
        assert!(reply.contains(supported), "{supported} unlisted: {reply}");
    }
    // Being unable to act on it is not a reason to stop serving.
    assert!(
        ask(&daemon.socket(), "{\"type\":\"ping\"}\n").contains("pong"),
        "the daemon stopped answering"
    );
}

/// A typo is the case this wording is really for, so it is worth pinning
/// separately from the request-from-the-future one: `strat` is a plausible slip
/// for `status`, and the reply has to be enough to spot it without reading the
/// source.
#[test]
fn a_misspelled_request_is_quoted_back_with_the_alternatives() {
    let daemon = RealDaemon::start();

    let reply = ask(&daemon.socket(), "{\"type\":\"strat\"}\n");

    assert!(reply.contains("strat"), "{reply}");
    assert!(reply.contains("status"), "{reply}");
    // The reply is JSON, so a quoted tag has to survive being embedded in it.
    let parsed: serde_json::Value = serde_json::from_str(reply.trim()).expect("a JSON reply");
    assert_eq!(parsed["type"], "error");
    assert!(
        parsed["message"]
            .as_str()
            .expect("a message")
            .contains("strat"),
        "{reply}"
    );
}

/// A refusal is written back down the socket the request arrived on, so the tag
/// it quotes cannot be unbounded: that would make the refusal a way to have the
/// daemon echo a payload of the peer's choosing.
#[test]
fn an_absurd_request_tag_does_not_come_back_in_full() {
    let daemon = RealDaemon::start();
    let huge = "z".repeat(4096);

    let reply = ask(&daemon.socket(), &format!("{{\"type\":\"{huge}\"}}\n"));

    assert!(reply.contains("does not support"), "{reply}");
    assert!(
        reply.len() < 500,
        "the refusal carried the tag back at length ({} bytes)",
        reply.len()
    );
}

/// A request nobody can act on still costs the connection a strike, or the
/// leniency that makes the refusal possible would also make a connection free to
/// talk to forever.
#[test]
fn requests_from_the_future_still_run_out_of_patience() {
    let daemon = RealDaemon::start();

    let stream = UnixStream::connect(daemon.socket()).expect("connect");
    stream
        .set_read_timeout(Some(CEILING))
        .expect("read timeout");
    let mut writer = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(stream);

    let mut answers = 0;
    let mut closed = false;
    for _ in 0..64 {
        if writer.write_all(b"{\"type\":\"drain\"}\n").is_err() || writer.flush().is_err() {
            closed = true;
            break;
        }
        let mut reply = String::new();
        match reader.read_line(&mut reply) {
            Ok(0) | Err(_) => {
                closed = true;
                break;
            }
            Ok(_) => answers += 1,
        }
    }

    assert!(closed, "the daemon answered {answers} of them forever");
    // Per connection, not per daemon.
    assert!(ask(&daemon.socket(), "{\"type\":\"ping\"}\n").contains("pong"));
}

/// A `ping` says which revision of the wire format the client speaks, and a
/// daemon that hears an older one says so.  That log line is the whole point of
/// the field: it is the only place an operator chasing version skew will look.
#[test]
fn the_daemon_reports_a_client_that_is_behind() {
    let daemon = RealDaemon::start();

    let old = ask(&daemon.socket(), "{\"type\":\"ping\",\"version\":0}\n");
    // A 0.3 client sends no version at all, and has to stay unremarked rather
    // than be reported as ancient — otherwise every one of them is noise.
    let silent = ask(&daemon.socket(), "{\"type\":\"ping\"}\n");

    assert!(old.contains("\"type\":\"pong\""), "{old}");
    assert!(
        old.contains("\"version\":1"),
        "the daemon has to name its own revision back: {old}"
    );
    assert!(silent.contains("\"type\":\"pong\""), "{silent}");

    let log = daemon.log();
    assert!(
        log.contains("older revision of the protocol"),
        "the skew went unreported:\n{log}"
    );
    assert_eq!(
        log.matches("older revision of the protocol").count(),
        1,
        "a client that said nothing must not be reported as behind:\n{log}"
    );
}
