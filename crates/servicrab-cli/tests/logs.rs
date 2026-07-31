//! Integration tests for file logging and `servicrab logs`.

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader};
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

/// Run any subcommand to completion and return `(exit code, stdout, stderr)`.
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

fn spawn(args: &[&str], config_path: &Path) -> Child {
    Command::new(binary())
        .args(args)
        .arg("--config")
        .arg(config_path)
        .env_remove("RUST_LOG")
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn servicrab")
}

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + CEILING;
    while Instant::now() < deadline {
        if fs::metadata(path).map(|m| m.len() > 0).unwrap_or(false) {
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
                panic!("servicrab did not exit within {CEILING:?}");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn interrupt(child: &Child) {
    kill(Pid::from_raw(child.id() as i32), Signal::SIGINT).unwrap();
}

#[test]
fn up_writes_one_log_file_per_service() {
    let dir = TempDir::new().unwrap();
    let hello = script(dir.path(), "hello.sh", "echo hello from api");
    let bye = script(dir.path(), "bye.sh", "echo hello from web");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[project.logs]
[services.api]
command = ["{}"]
restart = "never"
[services.web]
command = ["{}"]
restart = "never"
"#,
            hello.display(),
            bye.display()
        ),
    );

    let (code, _, _) = cli(&["up"], &cfg);
    assert_eq!(code, 0);

    let logs = dir.path().join(".servicrab/logs");
    let api = fs::read_to_string(logs.join("api.log")).unwrap();
    let web = fs::read_to_string(logs.join("web.log")).unwrap();

    assert!(api.contains("hello from api"), "{api}");
    assert!(web.contains("hello from web"), "{web}");
    // Each service owns its own file; nothing leaks across.
    assert!(!api.contains("hello from web"), "{api}");
}

#[test]
fn a_service_can_opt_out_of_file_logging() {
    let dir = TempDir::new().unwrap();
    let noisy = script(dir.path(), "noisy.sh", "echo secret");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[project.logs]
dir = "logs"
[services.api]
command = ["{}"]
restart = "never"
[services.api.logs]
enabled = false
"#,
            noisy.display()
        ),
    );

    let (code, stdout, _) = cli(&["up"], &cfg);
    assert_eq!(code, 0);
    assert!(stdout.contains("secret"), "{stdout}");
    assert!(!dir.path().join("logs/api.log").exists());
}

#[test]
fn without_a_logs_table_nothing_is_written() {
    let dir = TempDir::new().unwrap();
    let hello = script(dir.path(), "hello.sh", "echo hi");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
restart = "never"
"#,
            hello.display()
        ),
    );

    let (code, _, _) = cli(&["up"], &cfg);
    assert_eq!(code, 0);
    assert!(!dir.path().join(".servicrab").exists());
}

#[test]
fn oversized_logs_are_rotated() {
    let dir = TempDir::new().unwrap();
    // Each iteration writes ~100 bytes, so 40 of them cross the 1KB threshold
    // several times over.
    let chatty = script(
        dir.path(),
        "chatty.sh",
        "i=0\nwhile [ $i -lt 40 ]; do echo \"line $i aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"; i=$((i+1)); done",
    );
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[project.logs]
dir = "logs"
max_size = "1KB"
max_files = 2
[services.api]
command = ["{}"]
restart = "never"
"#,
            chatty.display()
        ),
    );

    let (code, _, _) = cli(&["up"], &cfg);
    assert_eq!(code, 0);

    let logs = dir.path().join("logs");
    assert!(logs.join("api.log").exists());
    assert!(logs.join("api.log.1").exists(), "rotation did not happen");
    assert!(logs.join("api.log.2").exists());
    // max_files = 2 means the third generation is dropped.
    assert!(!logs.join("api.log.3").exists());
}

#[test]
fn logs_shows_the_last_lines() {
    let dir = TempDir::new().unwrap();
    let counter = script(
        dir.path(),
        "counter.sh",
        "i=1\nwhile [ $i -le 10 ]; do echo \"line $i\"; i=$((i+1)); done",
    );
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[project.logs]
dir = "logs"
[services.api]
command = ["{}"]
restart = "never"
"#,
            counter.display()
        ),
    );

    assert_eq!(cli(&["up"], &cfg).0, 0);

    let (code, stdout, _) = cli(&["logs", "-n", "3"], &cfg);
    assert_eq!(code, 0);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines, vec!["line 8", "line 9", "line 10"], "{stdout}");
}

#[test]
fn logs_prefixes_lines_when_several_services_are_shown() {
    let dir = TempDir::new().unwrap();
    let api = script(dir.path(), "api.sh", "echo from-api");
    let web = script(dir.path(), "web.sh", "echo from-web");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[project.logs]
dir = "logs"
[services.api]
command = ["{}"]
restart = "never"
[services.web]
command = ["{}"]
restart = "never"
"#,
            api.display(),
            web.display()
        ),
    );

    assert_eq!(cli(&["up"], &cfg).0, 0);

    let (code, stdout, _) = cli(&["logs"], &cfg);
    assert_eq!(code, 0);
    assert!(stdout.contains("api | from-api"), "{stdout}");
    assert!(stdout.contains("web | from-web"), "{stdout}");

    // Selecting a single service drops the prefix again.
    let (_, stdout, _) = cli(&["logs", "api"], &cfg);
    assert_eq!(stdout.trim(), "from-api");
}

#[test]
fn logs_follows_new_output() {
    let dir = TempDir::new().unwrap();
    let ready = dir.path().join("ready");
    let ticker = script(
        dir.path(),
        "ticker.sh",
        &format!(
            "echo first\necho started > {}\nsleep 0.4\necho second\nsleep 30",
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
[project.logs]
dir = "logs"
[services.api]
command = ["{}"]
restart = "never"
"#,
            ticker.display()
        ),
    );

    let mut stack = spawn(&["up"], &cfg);
    wait_for_file(&ready);
    wait_for_file(&dir.path().join("logs/api.log"));

    let mut follower = spawn(&["logs", "-f", "-n", "1"], &cfg);
    let mut reader = BufReader::new(follower.stdout.take().unwrap());
    let mut seen = Vec::new();
    let deadline = Instant::now() + CEILING;
    while Instant::now() < deadline {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        seen.push(line.trim().to_string());
        if seen.iter().any(|l| l == "second") {
            break;
        }
    }

    let _ = follower.kill();
    let _ = follower.wait();
    interrupt(&stack);
    wait_bounded(&mut stack);

    assert!(
        seen.iter().any(|l| l == "first"),
        "tail was not shown: {seen:?}"
    );
    assert!(
        seen.iter().any(|l| l == "second"),
        "new output was not followed: {seen:?}"
    );
}

#[test]
fn logs_explains_that_file_logging_is_off() {
    let dir = TempDir::new().unwrap();
    let hello = script(dir.path(), "hello.sh", "echo hi");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["{}"]
"#,
            hello.display()
        ),
    );

    let (code, _, stderr) = cli(&["logs"], &cfg);
    assert_eq!(code, 1);
    assert!(stderr.contains("[project.logs]"), "{stderr}");
}

#[test]
fn logs_rejects_an_unknown_service() {
    let dir = TempDir::new().unwrap();
    let hello = script(dir.path(), "hello.sh", "echo hi");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[project.logs]
[services.api]
command = ["{}"]
"#,
            hello.display()
        ),
    );

    let (code, _, stderr) = cli(&["logs", "nope"], &cfg);
    assert_eq!(code, 1);
    assert!(stderr.contains("unknown service"), "{stderr}");
    assert!(stderr.contains("api"), "{stderr}");
}

#[test]
fn logs_reports_when_nothing_has_been_captured_yet() {
    let dir = TempDir::new().unwrap();
    let hello = script(dir.path(), "hello.sh", "echo hi");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[project.logs]
[services.api]
command = ["{}"]
"#,
            hello.display()
        ),
    );

    let (code, _, stderr) = cli(&["logs"], &cfg);
    assert_eq!(code, 1);
    assert!(stderr.contains("no log output yet"), "{stderr}");
}

#[test]
fn every_line_reaches_the_file_across_a_graceful_stop() {
    // The writer batches its flushes, so the interesting case is the tail: the
    // lines that were still buffered when the stack was asked to stop.  They
    // have to be on disk by the time the process exits, in order, with nothing
    // missing in between.
    const LINES: usize = 2_000;

    let dir = TempDir::new().unwrap();
    let done = dir.path().join("printed");
    let chatty = script(
        dir.path(),
        "chatty.sh",
        &format!(
            "i=1\nwhile [ $i -le {LINES} ]; do echo \"line $i\"; i=$((i+1)); done\necho printed > {}\nsleep 30",
            done.display()
        ),
    );
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[project.logs]
dir = "logs"
max_size = "1MB"
max_files = 1
[services.api]
command = ["{}"]
restart = "never"
"#,
            chatty.display()
        ),
    );

    let mut stack = spawn(&["up"], &cfg);
    // The script is done printing, so every line is either in the pipe, in the
    // queue or already written — a stop from here must lose none of them.
    wait_for_file(&done);
    interrupt(&stack);
    wait_bounded(&mut stack);

    let written = fs::read_to_string(dir.path().join("logs/api.log")).unwrap();
    let seen: Vec<&str> = written.lines().collect();
    let expected: Vec<String> = (1..=LINES).map(|i| format!("line {i}")).collect();
    assert_eq!(
        seen.len(),
        LINES,
        "lost {} line(s) of output; last written was {:?}",
        LINES - seen.len().min(LINES),
        seen.last()
    );
    assert_eq!(seen, expected, "output was reordered");
}

#[test]
fn run_also_writes_a_log_file_while_still_printing_output() {
    let dir = TempDir::new().unwrap();
    let hello = script(dir.path(), "hello.sh", "echo hello-from-run");
    let cfg = config(
        dir.path(),
        &format!(
            r#"
version = 1
[project]
name = "demo"
[project.logs]
dir = "logs"
[services.api]
command = ["{}"]
restart = "never"
"#,
            hello.display()
        ),
    );

    let (code, stdout, _) = cli(&["run", "api"], &cfg);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "hello-from-run");

    let written = fs::read_to_string(dir.path().join("logs/api.log")).unwrap();
    assert!(written.contains("hello-from-run"), "{written}");
}
