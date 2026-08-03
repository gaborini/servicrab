//! Integration tests for `servicrab up`.
//!
//! Every fixture is generated into a [`TempDir`] at test time, so nothing
//! depends on repository-specific paths.  All waits are bounded and each test
//! asserts that no supervised process is left behind.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use tempfile::TempDir;

/// Upper bound for any wait in this file; keeps a hung test from stalling CI.
const CEILING: Duration = Duration::from_secs(10);

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn config(dir: &Path, toml: &str) -> PathBuf {
    let path = dir.join("servicrab.toml");
    fs::write(&path, toml).unwrap();
    path
}

fn binary() -> PathBuf {
    assert_cmd::cargo::cargo_bin("servicrab")
}

/// Run `servicrab up` to completion and return `(exit code, stdout, stderr)`.
fn up(config_path: &Path, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(binary())
        .arg("up")
        .args(args)
        .arg("--config")
        .arg(config_path)
        .env_remove("RUST_LOG")
        .env("NO_COLOR", "1")
        .output()
        .expect("failed to run servicrab up");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Spawn `servicrab up` in the background with stdout captured.
fn spawn_up(config_path: &Path, args: &[&str]) -> Child {
    Command::new(binary())
        .arg("up")
        .args(args)
        .arg("--config")
        .arg(config_path)
        .env_remove("RUST_LOG")
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn servicrab up")
}

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + CEILING;
    while Instant::now() < deadline {
        if path.exists() && fs::metadata(path).map(|m| m.len() > 0).unwrap_or(false) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {}", path.display());
}

fn wait_bounded(child: &mut Child) -> i32 {
    let deadline = Instant::now() + CEILING;
    loop {
        match child.try_wait().unwrap() {
            Some(status) => return status.code().unwrap_or(-1),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("supervisor did not exit within {CEILING:?}");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn is_alive(pid: i32) -> bool {
    kill(Pid::from_raw(pid), None).is_ok()
}

fn read_pid(path: &Path) -> i32 {
    fs::read_to_string(path)
        .expect("pid file")
        .trim()
        .parse()
        .expect("numeric pid")
}

// ── planning ───────────────────────────────────────────────────────────────

#[test]
fn only_autostart_services_run_by_default() {
    let dir = TempDir::new().unwrap();
    script(dir.path(), "hello.sh", "echo hello-from-$1");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.auto]
command = ["{0}", "auto"]

[services.manual]
command = ["{0}", "manual"]
autostart = false
"#,
            dir.path().join("hello.sh").display()
        ),
    );

    let (code, stdout, _stderr) = up(&cfg, &[]);
    assert_eq!(code, 0, "stdout: {stdout}");
    assert!(stdout.contains("hello-from-auto"), "stdout: {stdout}");
    assert!(!stdout.contains("hello-from-manual"), "stdout: {stdout}");
}

#[test]
fn named_services_pull_in_their_dependencies() {
    let dir = TempDir::new().unwrap();
    script(dir.path(), "hello.sh", "echo hello-from-$1");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.db]
command = ["{0}", "db"]
autostart = false

[services.api]
command = ["{0}", "api"]
autostart = false
depends_on = ["db"]
"#,
            dir.path().join("hello.sh").display()
        ),
    );

    let (code, stdout, _stderr) = up(&cfg, &["api"]);
    assert_eq!(code, 0, "stdout: {stdout}");
    assert!(stdout.contains("hello-from-db"), "stdout: {stdout}");
    assert!(stdout.contains("hello-from-api"), "stdout: {stdout}");
}

#[test]
fn unknown_service_is_reported_clearly() {
    let dir = TempDir::new().unwrap();
    let cfg = config(
        dir.path(),
        r#"
version = 1

[project]
name = "demo"

[services.api]
command = ["true"]
"#,
    );

    let (code, _stdout, stderr) = up(&cfg, &["nope"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("unknown service"), "stderr: {stderr}");
    assert!(stderr.contains("api"), "stderr: {stderr}");
}

#[test]
fn a_stack_without_autostart_services_is_an_error() {
    let dir = TempDir::new().unwrap();
    let cfg = config(
        dir.path(),
        r#"
version = 1

[project]
name = "demo"

[services.api]
command = ["true"]
autostart = false
"#,
    );

    let (code, _stdout, stderr) = up(&cfg, &[]);
    assert_eq!(code, 1);
    assert!(stderr.contains("no services to start"), "stderr: {stderr}");
}

// ── profiles ───────────────────────────────────────────────────────────────

/// A stack whose `seeder` only runs when a profile asks for it.
fn profiled_config(dir: &Path) -> PathBuf {
    config(
        dir,
        r#"
version = 1

[project]
name = "demo"

[services.api]
command = ["echo", "api-line"]

[services.seeder]
command = ["echo", "seeder-line"]
profiles = ["dev"]
"#,
    )
}

#[test]
fn a_profiled_service_is_left_out_by_default() {
    let dir = TempDir::new().unwrap();
    let cfg = profiled_config(dir.path());

    let (code, stdout, _stderr) = up(&cfg, &[]);
    assert_eq!(code, 0, "stdout: {stdout}");
    assert!(stdout.contains("api-line"), "stdout: {stdout}");
    assert!(!stdout.contains("seeder-line"), "stdout: {stdout}");
}

#[test]
fn enabling_the_profile_brings_the_service_in() {
    let dir = TempDir::new().unwrap();
    let cfg = profiled_config(dir.path());

    let (code, stdout, _stderr) = up(&cfg, &["--profile", "dev"]);
    assert_eq!(code, 0, "stdout: {stdout}");
    assert!(stdout.contains("api-line"), "stdout: {stdout}");
    assert!(stdout.contains("seeder-line"), "stdout: {stdout}");
}

#[test]
fn a_profile_no_service_declares_is_rejected() {
    let dir = TempDir::new().unwrap();
    let cfg = profiled_config(dir.path());

    let (code, _stdout, stderr) = up(&cfg, &["--profile", "prod"]);
    assert_eq!(code, 1);
    assert!(stderr.contains("no service is in profile"), "{stderr}");
    assert!(
        stderr.contains("dev"),
        "the message should list what exists: {stderr}"
    );
}

#[test]
fn naming_services_and_enabling_profiles_at_once_is_refused() {
    // Two ways of saying what to start, with no unsurprising combination —
    // better to ask than to let one of them quietly lose.
    let dir = TempDir::new().unwrap();
    let cfg = profiled_config(dir.path());

    let (code, _stdout, stderr) = up(&cfg, &["api", "--profile", "dev"]);
    assert_eq!(code, 2, "clap rejects the combination: {stderr}");
    assert!(stderr.contains("--profile"), "{stderr}");
}

#[test]
fn a_stack_that_is_all_behind_profiles_says_so() {
    let dir = TempDir::new().unwrap();
    let cfg = config(
        dir.path(),
        r#"
version = 1

[project]
name = "demo"

[services.seeder]
command = ["true"]
profiles = ["dev"]
"#,
    );

    let (code, _stdout, stderr) = up(&cfg, &[]);
    assert_eq!(code, 1);
    assert!(stderr.contains("no services to start"), "stderr: {stderr}");
    assert!(
        stderr.contains("--profile") && stderr.contains("dev"),
        "the message should point at the way out: {stderr}"
    );
}

// ── output ─────────────────────────────────────────────────────────────────

#[test]
fn output_is_prefixed_with_the_service_name() {
    let dir = TempDir::new().unwrap();
    script(dir.path(), "hello.sh", "echo line-from-$1");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.api]
command = ["{0}", "api"]
"#,
            dir.path().join("hello.sh").display()
        ),
    );

    let (code, stdout, _stderr) = up(&cfg, &[]);
    assert_eq!(code, 0);
    assert!(
        stdout.lines().any(|l| l.starts_with("api | ")),
        "stdout: {stdout}"
    );
}

#[test]
fn prefixes_can_be_disabled() {
    let dir = TempDir::new().unwrap();
    script(dir.path(), "hello.sh", "echo bare-line");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.api]
command = ["{0}"]
"#,
            dir.path().join("hello.sh").display()
        ),
    );

    let (code, stdout, _stderr) = up(&cfg, &["--no-prefix"]);
    assert_eq!(code, 0);
    assert!(stdout.lines().any(|l| l == "bare-line"), "stdout: {stdout}");
}

#[test]
fn stderr_of_a_service_is_forwarded_to_stderr() {
    let dir = TempDir::new().unwrap();
    script(dir.path(), "noisy.sh", "echo to-stdout; echo to-stderr >&2");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.api]
command = ["{0}"]
"#,
            dir.path().join("noisy.sh").display()
        ),
    );

    let (code, stdout, stderr) = up(&cfg, &[]);
    assert_eq!(code, 0);
    assert!(stdout.contains("to-stdout"), "stdout: {stdout}");
    assert!(stderr.contains("to-stderr"), "stderr: {stderr}");
    assert!(!stdout.contains("to-stderr"), "stdout: {stdout}");
}

#[test]
fn timestamps_can_be_enabled() {
    let dir = TempDir::new().unwrap();
    script(dir.path(), "hello.sh", "echo stamped");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.api]
command = ["{0}"]
"#,
            dir.path().join("hello.sh").display()
        ),
    );

    let (code, stdout, _stderr) = up(&cfg, &["--timestamps"]);
    assert_eq!(code, 0);
    let line = stdout
        .lines()
        .find(|l| l.contains("stamped"))
        .unwrap_or_else(|| panic!("no output line: {stdout}"));
    let stamp = &line[..8];
    assert!(
        stamp.len() == 8 && stamp.chars().filter(|c| *c == ':').count() == 2,
        "expected a HH:MM:SS prefix, got {line:?}"
    );
}

/// A service that prints faster than the supervisor can render must not grow
/// the supervisor's heap without limit.  The bound comes with a drop policy, and
/// the policy has to be *visible*: the operator sees a `LogLinesDropped` event
/// rather than silently missing output.
#[test]
fn a_flooding_service_has_its_output_dropped_and_says_so() {
    // Far more than the channel's log-line allowance, and more than fits in the
    // pipe the renderer writes to while it is blocked below.
    const LINES: usize = 20_000;

    let dir = TempDir::new().unwrap();
    let printed = dir.path().join("printed");
    script(
        dir.path(),
        "flood.sh",
        &format!(
            "awk 'BEGIN{{for (i = 1; i <= {LINES}; i++) print \"line \" i}}'\necho done > {}\nsleep 30",
            printed.display()
        ),
    );
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.api]
command = ["{0}"]
restart = "never"
"#,
            dir.path().join("flood.sh").display()
        ),
    );

    // Both streams are piped and neither is read yet, so the renderer blocks on
    // its first full stdout write and the queue behind it has to absorb the
    // flood — or refuse to.
    let mut child = Command::new(binary())
        .arg("up")
        .arg("--config")
        .arg(&cfg)
        .env_remove("RUST_LOG")
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn servicrab up");

    // The script is done printing, so every line has been read off the pipe and
    // either queued or dropped.
    wait_for_file(&printed);

    // Now let the renderer catch up, which is when it gets to the report.
    let mut stdout = child.stdout.take().expect("stdout");
    let drain = std::thread::spawn(move || {
        let mut sink = String::new();
        use std::io::Read;
        let _ = stdout.read_to_string(&mut sink);
        sink.lines().filter(|l| l.contains("line ")).count()
    });

    let mut stderr = std::io::BufReader::new(child.stderr.take().expect("stderr"));
    let mut reported = None;
    let deadline = Instant::now() + CEILING;
    let mut seen = String::new();
    while Instant::now() < deadline {
        let mut line = String::new();
        use std::io::BufRead;
        if stderr.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        seen.push_str(&line);
        if line.contains("dropped") {
            reported = Some(line);
            break;
        }
    }

    kill(Pid::from_raw(child.id() as i32), Signal::SIGINT).unwrap();
    wait_bounded(&mut child);
    let rendered = drain.join().expect("drain thread");

    let reported = reported.unwrap_or_else(|| {
        panic!("the drop was never reported; stderr so far:\n{seen}\nrendered {rendered} line(s)")
    });
    assert!(
        reported.contains("output line(s)"),
        "unexpected report: {reported}"
    );
    // The verdict is the observable one: output really was dropped, so the
    // supervisor never had to hold all of it.
    assert!(
        rendered < LINES,
        "nothing was dropped, so the channel was not bounded ({rendered} of {LINES} rendered)"
    );
}

// ── ordering, dependencies, shutdown ───────────────────────────────────────

/// Ctrl+C has to work even when the operator's own output has backed up.
///
/// The renderer writes to stdout and stderr synchronously, and those writes
/// block while the far end is not keeping up — which is the right thing to do
/// to a slow terminal.  But a consumer that has stopped reading altogether (a
/// parent that captured both pipes and reads them only after the child exits is
/// the classic way to arrange this) leaves the renderer parked in a write that
/// never returns.  The supervisor used to wait for that renderer before exiting,
/// so `up` never exited at all: not in ten seconds, not in two minutes.
///
/// The verdict is the observable one an operator cares about: the service is
/// gone and the process is gone, with the Ctrl+C exit code.
#[test]
fn a_ctrl_c_is_honoured_while_the_output_nobody_reads_is_backed_up() {
    // Comfortably more than a pipe buffer holds once the renderer has prefixed
    // it, so the renderer is certain to be parked in a write by the time the
    // signal arrives.
    const LINES: usize = 1_500;
    const WIDTH: usize = 200;

    let dir = TempDir::new().unwrap();
    let printed = dir.path().join("printed");
    let pidfile = dir.path().join("service.pid");
    script(
        dir.path(),
        "noisy.sh",
        &format!(
            "echo $$ > {}\n\
             awk 'BEGIN{{for (i = 1; i <= {LINES}; i++) printf \"warn %d {}\\n\", i}}' 1>&2\n\
             echo done > {}\n\
             sleep 30",
            pidfile.display(),
            "-".repeat(WIDTH),
            printed.display()
        ),
    );
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.api]
command = ["{0}"]
restart = "never"
"#,
            dir.path().join("noisy.sh").display()
        ),
    );

    // Both pipes are captured and neither is ever read, so every write the
    // renderer makes past the first pipeful blocks for good.
    let mut child = Command::new(binary())
        .arg("up")
        .arg("--config")
        .arg(&cfg)
        .env_remove("RUST_LOG")
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn servicrab up");

    wait_for_file(&pidfile);
    let service = read_pid(&pidfile);
    // Every line has been handed over, so the renderer has as much to write as
    // it is ever going to get.
    wait_for_file(&printed);

    kill(Pid::from_raw(child.id() as i32), Signal::SIGINT).unwrap();
    let code = wait_bounded(&mut child);

    assert_eq!(
        code, 130,
        "a Ctrl+C that is honoured exits 130, whatever the output is doing"
    );
    // Nothing may be left behind, which is the promise the process groups exist
    // for; the pid is the service's shell, which leads its own group.
    assert!(
        !is_alive(service),
        "the service ({service}) outlived the supervisor"
    );
}

/// The verdict comes from the supervisor's own event stream, not from what the
/// two service scripts manage to write to a shared file.
///
/// An earlier version of this test had the dependent sleep 300ms and then look
/// for a marker file the dependency writes.  That asserts the scheduling of two
/// shells rather than the start order: on a loaded machine the dependency's
/// first `echo` had not run yet after 300ms, and the test failed while the
/// supervisor had done exactly the right thing.  `started` events are emitted
/// by the supervisor as it acts, through one channel, so their order *is* the
/// start order.
#[test]
fn a_dependent_starts_only_after_its_dependency_is_up() {
    let dir = TempDir::new().unwrap();
    // The dependency outlives the dependent, so a supervisor that ignored
    // depends_on would have no reason to produce this order by accident.
    script(dir.path(), "db.sh", "sleep 0.5");
    script(dir.path(), "api.sh", "true");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.api]
command = ["{}"]
depends_on = ["db"]

[services.db]
command = ["{}"]
"#,
            dir.path().join("api.sh").display(),
            dir.path().join("db.sh").display()
        ),
    );

    let (code, stdout, stderr) = up(&cfg, &["--json"]);
    assert_eq!(code, 0, "{stdout}{stderr}");

    let started: Vec<String> = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["event"]["kind"] == "started")
        .filter_map(|event| event["service"].as_str().map(str::to_string))
        .collect();
    assert_eq!(started, vec!["db", "api"], "{stdout}");
}

#[test]
fn a_dependent_is_skipped_when_its_dependency_fails_to_start() {
    let dir = TempDir::new().unwrap();
    script(dir.path(), "hello.sh", "echo api-started");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.db]
command = ["{}/definitely-not-here"]

[services.api]
command = ["{}"]
depends_on = ["db"]
"#,
            dir.path().display(),
            dir.path().join("hello.sh").display()
        ),
    );

    let (code, stdout, stderr) = up(&cfg, &[]);
    assert_eq!(code, 1, "stderr: {stderr}");
    assert!(!stdout.contains("api-started"), "stdout: {stdout}");
    assert!(
        stderr.contains("never became available"),
        "stderr: {stderr}"
    );
}

/// The condition a migration or seed step needs: the dependent starts only
/// after the one-shot has exited, and only if it exited cleanly.
#[test]
fn service_completed_successfully_waits_for_the_one_shot_to_exit() {
    let dir = TempDir::new().unwrap();
    // The migration stays alive for a moment, so a supervisor that waited only
    // for the process to *start* would run api while this is still going.
    script(dir.path(), "migrate.sh", "sleep 0.5");
    script(dir.path(), "api.sh", "true");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.api]
command = ["{}"]
depends_on = {{ migrate = {{ condition = "service_completed_successfully" }} }}

[services.migrate]
command = ["{}"]
"#,
            dir.path().join("api.sh").display(),
            dir.path().join("migrate.sh").display()
        ),
    );

    let (code, stdout, stderr) = up(&cfg, &["--json"]);
    assert_eq!(code, 0, "{stdout}{stderr}");

    // As in the start-order test above, the verdict comes from the supervisor's
    // own event stream: the migration's exit must precede api's start.
    let order: Vec<String> = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|event| {
            let service = event["service"].as_str()?;
            let kind = event["event"]["kind"].as_str()?;
            Some(format!("{service}/{kind}"))
        })
        .filter(|event| event == "migrate/exited" || event == "api/started")
        .collect();
    assert_eq!(order, vec!["migrate/exited", "api/started"], "{stdout}");
}

#[test]
fn service_completed_successfully_skips_the_dependent_when_the_one_shot_fails() {
    let dir = TempDir::new().unwrap();
    script(dir.path(), "migrate.sh", "exit 1");
    script(dir.path(), "api.sh", "echo api-started");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.api]
command = ["{}"]
depends_on = {{ migrate = {{ condition = "service_completed_successfully" }} }}

[services.migrate]
command = ["{}"]
"#,
            dir.path().join("api.sh").display(),
            dir.path().join("migrate.sh").display()
        ),
    );

    let (code, stdout, stderr) = up(&cfg, &[]);
    assert_eq!(code, 1, "stderr: {stderr}");
    assert!(!stdout.contains("api-started"), "stdout: {stdout}");
    assert!(
        stderr.contains("never became available"),
        "stderr: {stderr}"
    );
}

/// The counterpart of the test above, and the reason the condition exists: the
/// short `depends_on` form waits for the dependency to have *started* and never
/// looks at how it ended, so the same failing migration lets api run.
#[test]
fn the_short_dependency_form_does_not_look_at_the_exit_status() {
    let dir = TempDir::new().unwrap();
    script(dir.path(), "migrate.sh", "exit 1");
    script(dir.path(), "api.sh", "echo api-started");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.api]
command = ["{}"]
depends_on = ["migrate"]

[services.migrate]
command = ["{}"]
"#,
            dir.path().join("api.sh").display(),
            dir.path().join("migrate.sh").display()
        ),
    );

    let (code, stdout, stderr) = up(&cfg, &[]);
    assert!(stdout.contains("api-started"), "stdout: {stdout}");
    // And the run as a whole passes: a service that exits on its own is not a
    // stack failure, which is precisely why the exit status of a migration goes
    // unnoticed unless something waits for it.
    assert_eq!(code, 0, "{stdout}{stderr}");
}

#[test]
fn ctrl_c_stops_the_whole_stack() {
    let dir = TempDir::new().unwrap();
    let ready_a = dir.path().join("a.pid");
    let ready_b = dir.path().join("b.pid");
    script(
        dir.path(),
        "sleeper.sh",
        "echo $$ > $1\ntrap 'exit 0' TERM\nwhile true; do sleep 0.1; done",
    );
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.alpha]
command = ["{0}", "{1}"]
shutdown_timeout = "2s"

[services.beta]
command = ["{0}", "{2}"]
depends_on = ["alpha"]
shutdown_timeout = "2s"
"#,
            dir.path().join("sleeper.sh").display(),
            ready_a.display(),
            ready_b.display()
        ),
    );

    let mut child = spawn_up(&cfg, &[]);
    wait_for_file(&ready_a);
    wait_for_file(&ready_b);
    let pid_a = read_pid(&ready_a);
    let pid_b = read_pid(&ready_b);

    kill(Pid::from_raw(child.id() as i32), Signal::SIGINT).expect("send SIGINT");
    let code = wait_bounded(&mut child);
    assert_eq!(code, 130, "expected 128+SIGINT");

    std::thread::sleep(Duration::from_millis(200));
    assert!(!is_alive(pid_a), "alpha survived the shutdown");
    assert!(!is_alive(pid_b), "beta survived the shutdown");
}

#[test]
fn shutdown_runs_in_reverse_dependency_order() {
    let dir = TempDir::new().unwrap();
    let log = dir.path().join("stops.txt");
    let ready = dir.path().join("ready");
    script(
        dir.path(),
        "sleeper.sh",
        &format!(
            "trap 'echo $1 >> {}; exit 0' TERM\necho up > {}.$1\nwhile true; do sleep 0.1; done",
            log.display(),
            ready.display()
        ),
    );
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.db]
command = ["{0}", "db"]
shutdown_timeout = "2s"

[services.api]
command = ["{0}", "api"]
depends_on = ["db"]
shutdown_timeout = "2s"
"#,
            dir.path().join("sleeper.sh").display()
        ),
    );

    let mut child = spawn_up(&cfg, &[]);
    wait_for_file(&PathBuf::from(format!("{}.db", ready.display())));
    wait_for_file(&PathBuf::from(format!("{}.api", ready.display())));

    kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM).expect("send SIGTERM");
    let code = wait_bounded(&mut child);
    assert_eq!(code, 143, "expected 128+SIGTERM");

    let stops = fs::read_to_string(&log).expect("stop log");
    let order: Vec<&str> = stops.lines().collect();
    assert_eq!(order, vec!["api", "db"], "stop order: {stops}");
}

// ── restart policies and failures ──────────────────────────────────────────

#[test]
fn no_restart_disables_restarting_for_every_service() {
    let dir = TempDir::new().unwrap();
    let counter = dir.path().join("runs.txt");
    script(
        dir.path(),
        "flaky.sh",
        &format!("echo run >> {}\nexit 1", counter.display()),
    );
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.flaky]
command = ["{0}"]
restart = "on-failure"
restart_delay = "100ms"
max_restarts = 3
"#,
            dir.path().join("flaky.sh").display()
        ),
    );

    let (code, _stdout, _stderr) = up(&cfg, &["--no-restart"]);
    assert_eq!(code, 0, "a clean stop without restarts is not a failure");
    let runs = fs::read_to_string(&counter).unwrap().lines().count();
    assert_eq!(runs, 1, "the service should have run exactly once");
}

#[test]
fn a_service_that_exhausts_its_restart_budget_fails_the_stack() {
    let dir = TempDir::new().unwrap();
    let counter = dir.path().join("runs.txt");
    script(
        dir.path(),
        "flaky.sh",
        &format!("echo run >> {}\nexit 1", counter.display()),
    );
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.flaky]
command = ["{0}"]
restart = "on-failure"
restart_delay = "100ms"
restart_max_delay = "200ms"
max_restarts = 2
"#,
            dir.path().join("flaky.sh").display()
        ),
    );

    let (code, _stdout, stderr) = up(&cfg, &[]);
    assert_eq!(code, 1, "stderr: {stderr}");
    assert!(stderr.contains("giving up"), "stderr: {stderr}");
    let runs = fs::read_to_string(&counter).unwrap().lines().count();
    assert_eq!(runs, 3, "one initial run plus two restarts");
}

#[test]
fn abort_on_failure_tears_down_the_rest_of_the_stack() {
    let dir = TempDir::new().unwrap();
    let ready = dir.path().join("long.pid");
    script(
        dir.path(),
        "sleeper.sh",
        "echo $$ > $1\ntrap 'exit 0' TERM\nwhile true; do sleep 0.1; done",
    );
    script(dir.path(), "boom.sh", "exit 7");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.long]
command = ["{0}", "{1}"]
shutdown_timeout = "2s"

[services.boom]
command = ["{2}"]
restart = "on-failure"
restart_delay = "500ms"
max_restarts = 2
"#,
            dir.path().join("sleeper.sh").display(),
            ready.display(),
            dir.path().join("boom.sh").display()
        ),
    );

    let mut child = spawn_up(&cfg, &["--abort-on-failure"]);
    wait_for_file(&ready);
    let pid = read_pid(&ready);

    let code = wait_bounded(&mut child);
    assert_eq!(code, 1);

    std::thread::sleep(Duration::from_millis(200));
    assert!(!is_alive(pid), "the long-running service was not torn down");
}

#[test]
fn descendants_do_not_survive_a_stack_shutdown() {
    let dir = TempDir::new().unwrap();
    let parent_pid = dir.path().join("parent.pid");
    let child_pid = dir.path().join("child.pid");
    script(
        dir.path(),
        "tree.sh",
        &format!(
            "sh -c 'echo $$ > {}; trap \"\" TERM; while true; do sleep 0.1; done' &\n\
             echo $$ > {}\n\
             trap 'exit 0' TERM\n\
             while true; do sleep 0.1; done",
            child_pid.display(),
            parent_pid.display()
        ),
    );
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1

[project]
name = "demo"

[services.tree]
command = ["{0}"]
shutdown_timeout = "1s"
"#,
            dir.path().join("tree.sh").display()
        ),
    );

    let mut child = spawn_up(&cfg, &[]);
    wait_for_file(&parent_pid);
    wait_for_file(&child_pid);
    let descendant = read_pid(&child_pid);

    kill(Pid::from_raw(child.id() as i32), Signal::SIGINT).expect("send SIGINT");
    wait_bounded(&mut child);

    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !is_alive(descendant),
        "a descendant outlived the stack shutdown"
    );
}

#[test]
fn json_mode_emits_one_protocol_event_per_line() {
    let dir = TempDir::new().unwrap();
    let hello = script(dir.path(), "hello.sh", "echo hi\necho oops >&2");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[services.hello]
command = ["{}"]
"#,
            hello.display()
        ),
    );

    let (code, stdout, stderr) = up(&cfg, &["--json"]);
    assert_eq!(code, 0, "{stdout}{stderr}");

    let lines: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{line:?}: {e}")))
        .collect();

    // The stream opens with the same handshake line the daemon answers a
    // `subscribe` with, so a reader can treat all three streams identically.
    let (header, events) = lines.split_first().expect("a handshake line");
    assert_eq!(header["type"], "ok", "{stdout}");
    assert_eq!(header["schema_version"], 1, "{stdout}");

    assert!(!events.is_empty(), "no events on stdout");
    assert!(events.iter().all(|e| e["type"] == "event"));
    assert!(events.iter().all(|e| e["service"] == "hello"));

    // Captured output keeps its stream, and both streams end up on stdout.
    // The two readers race, so only membership is guaranteed, not order.
    let mut logs: Vec<(String, String)> = events
        .iter()
        .filter(|e| e["event"]["kind"] == "log")
        .map(|e| {
            (
                e["event"]["stream"].as_str().unwrap().to_string(),
                e["event"]["line"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    logs.sort();
    assert_eq!(
        logs,
        vec![
            ("stderr".to_string(), "oops".to_string()),
            ("stdout".to_string(), "hi".to_string()),
        ],
        "{stdout}"
    );

    // The lifecycle is there too, in order.
    let kinds: Vec<&str> = events
        .iter()
        .filter_map(|e| e["event"]["kind"].as_str())
        .collect();
    assert!(kinds.contains(&"started"), "{kinds:?}");
    assert!(kinds.contains(&"exited"), "{kinds:?}");
    assert!(kinds.contains(&"finished"), "{kinds:?}");
}

#[test]
fn json_mode_keeps_the_banner_off_stdout() {
    let dir = TempDir::new().unwrap();
    let hello = script(dir.path(), "hello.sh", "echo hi");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[services.hello]
command = ["{}"]
"#,
            hello.display()
        ),
    );

    let (_, stdout, _) = up(&cfg, &["--json"]);
    assert!(!stdout.contains("servicrab up"), "{stdout}");
    for line in stdout.lines() {
        assert!(
            serde_json::from_str::<serde_json::Value>(line).is_ok(),
            "not JSON: {line:?}"
        );
    }
}
