//! The machine-readable output contract: exit codes, JSON envelopes, and the
//! one format every error is reported in.
//!
//! These are the shapes v1.0 freezes, so each one gets a test that fails if it
//! moves — not because the behaviour is subtle, but because it is now a promise.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Upper bound for any wait in this file.
const CEILING: Duration = Duration::from_secs(20);

fn binary() -> PathBuf {
    assert_cmd::cargo::cargo_bin("servicrab")
}

fn config(dir: &Path, toml: &str) -> PathBuf {
    let path = dir.join("servicrab.toml");
    fs::write(&path, toml).unwrap();
    path
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A long-lived service that exits cleanly on SIGTERM.
fn resident(dir: &Path, name: &str) -> PathBuf {
    script(
        dir,
        name,
        "trap 'exit 0' TERM INT\necho up\nwhile true; do sleep 0.2; done",
    )
}

fn cli(args: &[&str], config_path: &Path) -> (i32, String, String) {
    let output = Command::new(binary())
        .args(args)
        .arg("--config")
        .arg(config_path)
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

/// A config with one resident service, and nothing else going on.
fn one_service(dir: &Path) -> PathBuf {
    let svc = resident(dir, "api.sh");
    config(
        dir,
        &format!(
            r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
restart = "always"
"#,
            svc.display()
        ),
    )
}

/// A config that does not validate, for the error paths.
fn broken(dir: &Path) -> PathBuf {
    config(
        dir,
        r#"
version = 1
[project]
name = "demo"
[services.api]
command = []
[services.web]
command = ["true"]
depends_on = ["nowhere"]
"#,
    )
}

/// Stops the daemon when the test ends, however it ends.
struct Daemon {
    config: PathBuf,
}

impl Daemon {
    fn start(config: &Path) -> Self {
        let (code, stdout, stderr) = cli(&["start"], config);
        assert_eq!(code, 0, "start failed: {stdout}{stderr}");
        Self {
            config: config.to_path_buf(),
        }
    }

    fn wait_for_status(&self, what: &str, ready: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + CEILING;
        loop {
            let (_, stdout, _) = cli(&["status"], &self.config);
            if ready(&stdout) {
                return stdout;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = cli(&["down"], &self.config);
    }
}

fn json_of(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("not JSON: {e}\n{text}"))
}

// ── Item 3: every --json document carries a schema version ─────────────────

#[test]
fn every_json_document_carries_a_schema_version() {
    let dir = TempDir::new().unwrap();
    let cfg = one_service(dir.path());

    for args in [
        vec!["check", "--json"],
        vec!["list", "--json"],
        vec!["status", "--json"],
    ] {
        let (_, stdout, stderr) = cli(&args, &cfg);
        let json = json_of(&stdout);
        assert_eq!(
            json["schema_version"], 1,
            "{args:?} printed {stdout}{stderr}"
        );
    }
}

/// The absent-daemon branch used to be a hand-written compact string that never
/// went through serde, so it could drift from the running case it is supposed
/// to mirror.
#[test]
fn status_json_reports_an_absent_daemon_in_the_same_shape_as_a_running_one() {
    let dir = TempDir::new().unwrap();
    let cfg = one_service(dir.path());

    let (code, stopped, stderr) = cli(&["status", "--json"], &cfg);
    assert_eq!(code, 3, "{stopped}{stderr}");
    let stopped = json_of(&stopped);
    assert_eq!(stopped["running"], false);
    assert_eq!(stopped["services"], serde_json::json!([]));
    assert_eq!(stopped["schema_version"], 1);

    let daemon = Daemon::start(&cfg);
    daemon.wait_for_status("api to run", |s| s.contains("running"));

    let (code, running, stderr) = cli(&["status", "--json"], &cfg);
    assert_eq!(code, 0, "{running}{stderr}");
    let running = json_of(&running);
    assert_eq!(running["running"], true);
    assert_eq!(running["schema_version"], 1);

    // The same keys in both, so a reader does not have to special-case one.
    let mut stopped_keys: Vec<&String> = stopped.as_object().unwrap().keys().collect();
    let mut running_keys: Vec<&String> = running.as_object().unwrap().keys().collect();
    stopped_keys.sort();
    running_keys.sort();
    assert_eq!(stopped_keys, running_keys);
}

/// `check` is the most scripted command, and its whole job is reporting
/// problems, so those have to arrive as a list rather than as a paragraph.
#[test]
fn check_json_reports_validation_errors_as_a_list() {
    let dir = TempDir::new().unwrap();
    let cfg = broken(dir.path());

    let (code, stdout, stderr) = cli(&["check", "--json"], &cfg);
    assert_eq!(code, 1, "{stdout}{stderr}");
    // Nothing on stdout: a caller parsing it sees only the documents it asked
    // for, never an error.
    assert!(stdout.is_empty(), "{stdout}");

    let json = json_of(&stderr);
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["error"]["code"], "validation_failed");
    let errors = json["error"]["errors"].as_array().expect("a list");
    assert_eq!(errors.len(), 2, "{json:#}");
    assert!(
        errors.iter().all(|e| e.is_string()),
        "each error is its own string: {json:#}"
    );
}

#[test]
fn check_json_describes_a_config_that_loads() {
    let dir = TempDir::new().unwrap();
    let svc = script(dir.path(), "api.sh", "true");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
[services.tools]
command = ["{}"]
profiles = ["dev"]
"#,
            svc.display(),
            svc.display()
        ),
    );

    let (code, stdout, stderr) = cli(&["check", "--json"], &cfg);
    assert_eq!(code, 0, "{stdout}{stderr}");

    let json = json_of(&stdout);
    assert_eq!(json["ok"], true);
    assert_eq!(json["project"], "demo");
    assert_eq!(json["services"], 2);
    assert_eq!(json["start_order"], serde_json::json!(["api", "tools"]));
    assert_eq!(json["profiles"]["dev"], serde_json::json!(["tools"]));
}

/// Errors never emitted JSON at all, even under `--json`: a script asking for
/// machine-readable output got a text `error: …` line the moment anything went
/// wrong.
#[test]
fn an_error_under_json_is_json() {
    let dir = TempDir::new().unwrap();
    let cfg = broken(dir.path());

    for args in [vec!["list", "--json"], vec!["check", "--json"]] {
        let (code, stdout, stderr) = cli(&args, &cfg);
        assert_eq!(code, 1, "{args:?}: {stdout}{stderr}");
        assert!(stdout.is_empty(), "{args:?} wrote to stdout: {stdout}");
        let json = json_of(&stderr);
        assert_eq!(json["schema_version"], 1, "{args:?}");
        assert_eq!(json["error"]["code"], "validation_failed", "{args:?}");
    }
}

/// Without `--json` the same failure is one `error: ` line with the problems as
/// bullets — never a bare message, and never a `✗` used as an error marker.
#[test]
fn an_error_without_json_is_one_prefixed_line_on_stderr() {
    let dir = TempDir::new().unwrap();
    let cfg = broken(dir.path());

    let (code, stdout, stderr) = cli(&["check"], &cfg);
    assert_eq!(code, 1, "{stdout}{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.starts_with("error: "), "{stderr}");
    assert!(!stderr.contains('✗'), "✗ is not an error marker: {stderr}");

    // Reported once: the errors used to be printed here and then summarized
    // again by main's own `error:` line.
    assert_eq!(
        stderr.matches("error(s)").count(),
        1,
        "said twice: {stderr}"
    );
}

// ── Item 4: exit codes ─────────────────────────────────────────────────────

/// The dedicated code is what makes "nothing is running" scriptable, and it has
/// to be the same everywhere — it used to be `1` for five commands and `0` for
/// `down`.
#[test]
fn no_daemon_is_exit_three_for_every_command_that_needs_one() {
    let dir = TempDir::new().unwrap();
    let cfg = one_service(dir.path());

    for args in [
        vec!["status"],
        vec!["stop", "api"],
        vec!["restart", "api"],
        vec!["start", "api"],
        vec!["reload"],
        vec!["events"],
        vec!["down"],
    ] {
        let (code, stdout, stderr) = cli(&args, &cfg);
        assert_eq!(code, 3, "{args:?} exited {code}: {stdout}{stderr}");
    }
}

/// `down` is the one that used to exit `0`.  Idempotence is the point and it is
/// preserved — the command still does not *fail* — but the code now says
/// whether there was anything to do.
#[test]
fn down_says_there_was_nothing_to_stop_without_calling_it_a_failure() {
    let dir = TempDir::new().unwrap();
    let cfg = one_service(dir.path());

    let (code, stdout, stderr) = cli(&["down"], &cfg);
    assert_eq!(code, 3);
    // Not an error: no `error: ` prefix.  It is a diagnostic all the same —
    // nothing was stopped, so stdout has nothing to report.
    assert!(stderr.contains("no daemon is running"), "{stderr}");
    assert!(!stderr.contains("error: "), "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");

    let daemon = Daemon::start(&cfg);
    daemon.wait_for_status("api to run", |s| s.contains("running"));
    let (code, stdout, stderr) = cli(&["down"], &cfg);
    assert_eq!(code, 0, "{stdout}{stderr}");

    // And again, now that it really is gone.
    let (code, _, _) = cli(&["down"], &cfg);
    assert_eq!(code, 3);
}

/// A per-service rejection used to print a bare `✗ …` with no `error: ` prefix,
/// which is a third format for the same thing.
#[test]
fn a_rejected_per_service_command_is_reported_like_every_other_error() {
    let dir = TempDir::new().unwrap();
    let cfg = one_service(dir.path());
    let _daemon = Daemon::start(&cfg);

    let (code, stdout, stderr) = cli(&["stop", "nowhere"], &cfg);
    assert_eq!(code, 1, "{stdout}{stderr}");
    assert!(stderr.contains("error: "), "{stderr}");
    assert!(!stderr.contains('✗'), "{stderr}");
    assert!(stderr.contains("unknown service"), "{stderr}");
}

// ── Item 1: structured fields beside the prose ─────────────────────────────

/// The reload's three counts were only ever available as a sentence, so a
/// caller deciding whether anything happened had to parse English.
#[test]
fn a_reload_reports_its_counts_as_numbers_over_the_socket() {
    let dir = TempDir::new().unwrap();
    let api = resident(dir.path(), "api.sh");
    let cache = resident(dir.path(), "cache.sh");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
restart = "always"
"#,
            api.display()
        ),
    );

    let daemon = Daemon::start(&cfg);
    daemon.wait_for_status("api to run", |s| s.contains("running"));

    let socket = dir.path().join(".servicrab/daemon.sock");

    // Nothing changed yet.
    let reply = json_of(&ask(&socket, r#"{"type":"reload"}"#));
    assert_eq!(reply["type"], "ok", "{reply:#}");
    assert_eq!(reply["changes"]["added"], 0, "{reply:#}");
    assert_eq!(reply["changes"]["changed"], 0, "{reply:#}");
    assert_eq!(reply["changes"]["removed"], 0, "{reply:#}");

    fs::write(
        &cfg,
        format!(
            r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
restart = "always"
[services.cache]
command = ["{}"]
restart = "always"
"#,
            api.display(),
            cache.display()
        ),
    )
    .unwrap();

    let reply = json_of(&ask(&socket, r#"{"type":"reload"}"#));
    assert_eq!(reply["changes"]["added"], 1, "{reply:#}");
    assert_eq!(reply["changes"]["changed"], 0, "{reply:#}");
    assert_eq!(reply["changes"]["removed"], 0, "{reply:#}");
    // The prose is still there, for a person.
    assert!(
        reply["message"]
            .as_str()
            .expect("a message")
            .contains("1 added"),
        "{reply:#}"
    );
}

/// A refused config used to arrive as one string with `\n`s and `•`s in it,
/// which every caller had to take apart again.
#[test]
fn a_refused_reload_lists_its_errors_and_carries_a_code() {
    let dir = TempDir::new().unwrap();
    let cfg = one_service(dir.path());
    let _daemon = Daemon::start(&cfg);
    let socket = dir.path().join(".servicrab/daemon.sock");

    fs::write(
        &cfg,
        r#"
version = 1
[project]
name = "demo"
[services.api]
command = []
[services.web]
command = ["true"]
depends_on = ["nowhere"]
"#,
    )
    .unwrap();

    let reply = json_of(&ask(&socket, r#"{"type":"reload"}"#));
    assert_eq!(reply["type"], "error", "{reply:#}");
    assert_eq!(reply["code"], "validation_failed", "{reply:#}");
    assert_eq!(reply["errors"].as_array().expect("a list").len(), 2);
    assert!(
        !reply["message"].as_str().expect("a message").contains('\n'),
        "the message is one line now: {reply:#}"
    );
}

#[test]
fn an_unknown_service_over_the_socket_is_coded_as_such() {
    let dir = TempDir::new().unwrap();
    let cfg = one_service(dir.path());
    let _daemon = Daemon::start(&cfg);
    let socket = dir.path().join(".servicrab/daemon.sock");

    let reply = json_of(&ask(&socket, r#"{"type":"stop_service","name":"nowhere"}"#));
    assert_eq!(reply["type"], "error");
    assert_eq!(reply["code"], "unknown_service", "{reply:#}");
}

// ── Item 2: pid is really a pgid ───────────────────────────────────────────

/// `ServiceInfo.pid` always was a process-group id — its own doc comment said
/// so, and `Event::Started` calls the same number `pgid`.  Both names now carry
/// it, so nothing that reads `pid` breaks while new readers get the right one.
#[test]
fn status_reports_the_process_group_under_both_pid_and_pgid() {
    let dir = TempDir::new().unwrap();
    let cfg = one_service(dir.path());
    let daemon = Daemon::start(&cfg);
    daemon.wait_for_status("api to run", |s| s.contains("running"));

    let (code, stdout, stderr) = cli(&["status", "--json"], &cfg);
    assert_eq!(code, 0, "{stdout}{stderr}");

    let json = json_of(&stdout);
    let api = &json["services"][0];
    let pgid = api["pgid"].as_i64().expect("a pgid: {json:#}");
    assert!(pgid > 0, "{json:#}");
    assert_eq!(api["pid"], pgid, "the alias carries the same value");
}

/// Send one line to the daemon's socket and read one line back.
fn ask(socket: &Path, request: &str) -> String {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket).expect("connect to the daemon");
    stream
        .write_all(format!("{request}\n").as_bytes())
        .expect("write the request");
    stream.flush().expect("flush");

    let mut reply = String::new();
    BufReader::new(&stream)
        .read_line(&mut reply)
        .expect("read the reply");
    reply
}
