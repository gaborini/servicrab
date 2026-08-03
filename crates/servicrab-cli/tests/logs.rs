//! Integration tests for file logging and `servicrab logs`.

#![cfg(unix)]

use std::collections::BTreeSet;
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
fn a_sustained_append_is_never_followed_into_a_duplicate_line() {
    // The bug this pins: `follow` sampled the file length, read to whatever the
    // current end was — which by then could be past that length — and then set
    // the offset back to the stale sample.  Everything appended in between came
    // out again on the next pass.  A writer that never pauses is what makes that
    // window land on every pass, so the assertion is simply that no line is ever
    // printed twice.
    const LINES: usize = 6_000;
    /// Paced so the writer is still appending during every follow pass: at 200µs
    /// a line, the flood outlasts several of the 200ms polls, which is exactly
    /// when a read can run past a length sampled before it started.
    const PACE: Duration = Duration::from_micros(200);

    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join("logs")).unwrap();
    let log = dir.path().join("logs/api.log");
    // A tail to start from, so the follower has an offset that is not zero.
    fs::write(&log, "line 0\n").unwrap();

    let cfg = config(
        dir.path(),
        r#"
version = 1
[project]
name = "demo"
[project.logs]
dir = "logs"
[services.api]
command = ["true"]
restart = "never"
"#,
    );

    let mut follower = spawn(&["logs", "-f", "-n", "1"], &cfg);
    let reader = BufReader::new(follower.stdout.take().unwrap());

    // Nothing supervises this file: the writer here is the test, so the flood is
    // under its control rather than a service's.
    let appender = std::thread::spawn({
        let log = log.clone();
        move || {
            use std::io::Write;
            let mut file = fs::OpenOptions::new().append(true).open(&log).unwrap();
            for i in 1..=LINES {
                writeln!(file, "line {i}").unwrap();
                file.flush().unwrap();
                std::thread::sleep(PACE);
            }
        }
    });

    let mut seen: Vec<String> = Vec::new();
    let last = format!("line {LINES}");
    // Draining stdout on its own thread keeps two things true at once: the
    // follower never stalls on a full pipe (it has to keep polling a growing
    // file, or the duplicate this test hunts for could not arise), and the wait
    // below can be bounded by something other than a stopwatch.
    let (lines_tx, lines_rx) = std::sync::mpsc::channel();
    let drain = std::thread::spawn(move || {
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if lines_tx.send(line.trim().to_string()).is_err() {
                break;
            }
        }
    });

    // The condition is silence, not elapsed time. `CEILING` used to have to
    // cover the paced flood *and* the follow of it, and the flood alone is
    // `LINES * PACE` of sleeping — a floor rather than a duration, since every
    // sleep overruns and on a loaded runner it overruns by enough that the
    // deadline expired while the writer was still mid-file. That failed a
    // follower that was working perfectly, blaming whichever line the clock cut
    // it off at. A follow that has genuinely stopped is instead recognisable by
    // what it does: it goes quiet and stays quiet, however slow the machine.
    while let Ok(line) = lines_rx.recv_timeout(CEILING) {
        let done = line == last;
        seen.push(line);
        if done {
            break;
        }
    }

    appender.join().unwrap();
    let _ = follower.kill();
    let _ = follower.wait();
    drop(lines_rx);
    let _ = drain.join();

    let mut once: BTreeSet<&String> = BTreeSet::new();
    let mut twice: Vec<&String> = Vec::new();
    for line in &seen {
        if !once.insert(line) {
            twice.push(line);
        }
    }
    assert!(
        twice.is_empty(),
        "{} line(s) came out more than once, e.g. {:?}",
        twice.len(),
        &twice[..twice.len().min(5)]
    );
    assert!(
        seen.iter().any(|l| l == &format!("line {LINES}")),
        "the follow stopped early: last was {:?}",
        seen.last()
    );
}

#[test]
fn a_line_still_being_written_is_printed_once_and_whole() {
    // A log file ends mid-line whenever a service is in the middle of writing
    // one.  Treating that fragment as a finished line printed it immediately and
    // then again, in full, once its newline arrived.
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join("logs")).unwrap();
    let log = dir.path().join("logs/api.log");
    fs::write(&log, "complete\n").unwrap();

    let cfg = config(
        dir.path(),
        r#"
version = 1
[project]
name = "demo"
[project.logs]
dir = "logs"
[services.api]
command = ["true"]
restart = "never"
"#,
    );

    let mut follower = spawn(&["logs", "-f", "-n", "1"], &cfg);
    let mut reader = BufReader::new(follower.stdout.take().unwrap());

    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    assert_eq!(first.trim(), "complete");

    {
        use std::io::Write;
        let mut file = fs::OpenOptions::new().append(true).open(&log).unwrap();
        write!(file, "half a").unwrap();
        file.flush().unwrap();
        // Long enough that a follow pass has certainly seen the fragment.
        std::thread::sleep(Duration::from_millis(500));
        writeln!(file, " line").unwrap();
        file.flush().unwrap();
    }

    let mut second = String::new();
    reader.read_line(&mut second).unwrap();
    let _ = follower.kill();
    let _ = follower.wait();

    assert_eq!(
        second.trim(),
        "half a line",
        "the fragment was printed before it was finished"
    );
}

#[test]
fn a_log_line_that_is_not_utf8_does_not_stop_the_command() {
    // A service is free to print a stray byte — a binary blob, output in another
    // encoding, a multi-byte character caught mid-write.  That used to fail the
    // whole command, and silently cut a follow short at the offending line.
    let dir = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join("logs")).unwrap();
    fs::write(
        dir.path().join("logs/api.log"),
        b"before\n\xff\xfe not text\nafter\n",
    )
    .unwrap();

    let cfg = config(
        dir.path(),
        r#"
version = 1
[project]
name = "demo"
[project.logs]
dir = "logs"
[services.api]
command = ["true"]
restart = "never"
"#,
    );

    let (code, stdout, stderr) = cli(&["logs"], &cfg);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("before"), "{stdout}");
    assert!(stdout.contains("after"), "{stdout}");
    assert!(stdout.contains('\u{fffd}'), "{stdout}");
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
fn logs_reports_config_warnings() {
    let dir = TempDir::new().unwrap();
    let hello = script(dir.path(), "hello.sh", "echo hi");
    // `max_restarts` alongside `restart = "never"` is inert, so loading warns.
    // `logs` used to throw those warnings away, unlike the other commands.
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
max_restarts = 3
"#,
            hello.display()
        ),
    );

    // The command itself still fails (nothing has been captured), which is
    // exactly the case where a warning is easiest to lose.
    let (_, _, stderr) = cli(&["logs"], &cfg);
    assert!(stderr.contains("max_restarts"), "{stderr}");
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

    // An empty log directory is what a stack that has not run yet looks like,
    // so this says so and succeeds; `servicrab logs && …` is not a failure path.
    let (code, stdout, stderr) = cli(&["logs"], &cfg);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("no log output yet"), "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
}

#[test]
fn every_line_reaches_the_file_across_a_graceful_stop() {
    // The writer batches its flushes, so the interesting case is the tail: the
    // lines that were still buffered when the stack was asked to stop.  They
    // have to be on disk by the time the process exits, in order, with nothing
    // missing in between.
    // Fewer than the event channel's log-line allowance, so nothing is dropped
    // and the batched flush is what the test is about; far more than one flush
    // batch, so the tail really is still buffered when the stop arrives.
    const LINES: usize = 900;

    let dir = TempDir::new().unwrap();
    let done = dir.path().join("printed");
    let chatty = script(
        dir.path(),
        "chatty.sh",
        &format!(
            "awk 'BEGIN{{for (i = 1; i <= {LINES}; i++) print \"line \" i}}'\necho printed > {}\nsleep 30",
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
    // Keep reading the terminal output, so the renderer never stalls on a full
    // pipe: what is being tested here is the batched flush, not the drop policy
    // that a stalled consumer would trigger.
    let terminal = stack.stdout.take().expect("stdout");
    let drain = std::thread::spawn(move || {
        use std::io::Read;
        let mut sink = Vec::new();
        let _ = std::io::BufReader::new(terminal).read_to_end(&mut sink);
    });

    // The script is done printing, so every line is either in the pipe, in the
    // queue or already written — a stop from here must lose none of them.
    wait_for_file(&done);
    interrupt(&stack);
    wait_bounded(&mut stack);
    drain.join().expect("drain thread");

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
