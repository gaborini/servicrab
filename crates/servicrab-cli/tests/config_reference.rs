//! The configuration reference in `README.md` promises specific ranges, and a
//! range is the kind of claim that rots quietly: someone widens a bound in
//! `validation.rs`, every test still passes, and the documented number is
//! wrong until a user hits it.
//!
//! So this file reads the table out of `README.md` and, for each documented
//! bound, feeds the binary a config that sits **just outside** it and one that
//! sits **on** it. If a bound moves, the pair disagrees with the README and
//! this test fails naming the field.
//!
//! It deliberately checks the documentation against the binary rather than
//! against the constants: a test that imported `DUR_1H` would agree with the
//! code by construction and would never catch the README drifting.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

fn binary() -> PathBuf {
    assert_cmd::cargo::cargo_bin("servicrab")
}

fn readme() -> String {
    // CARGO_MANIFEST_DIR is crates/servicrab-cli.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .join("README.md");
    fs::read_to_string(&root).unwrap_or_else(|e| panic!("cannot read {}: {e}", root.display()))
}

/// Run `check` on a config and return `(exit code, stderr)`.
fn check(toml: &str) -> (i32, String) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("servicrab.toml");
    fs::write(&path, toml).unwrap();
    let out = Command::new(binary())
        .arg("check")
        .arg("--config")
        .arg(&path)
        .env_remove("RUST_LOG")
        .env("NO_COLOR", "1")
        .output()
        .expect("failed to run servicrab");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The README row for `field`, as the text after the last `|` pair.
///
/// Rows look like `| `field` | duration | `1s` | **100ms … 1h.** |`.
fn documented_row(field: &str) -> String {
    let needle = format!("| `{field}` |");
    let readme = readme();
    let row = readme
        .lines()
        .find(|line| line.starts_with(&needle))
        .unwrap_or_else(|| panic!("README has no configuration-reference row for `{field}`"));
    row.to_string()
}

/// Assert the README row for `field` mentions `bound`, so the prose and the
/// behaviour cannot drift apart silently.
fn assert_documents(field: &str, bound: &str) {
    let row = documented_row(field);
    assert!(
        row.contains(bound),
        "README row for `{field}` no longer documents the bound {bound}:\n{row}"
    );
}

fn service_with(body: &str) -> String {
    format!("version = 1\n[project]\nname = \"p\"\n[services.api]\ncommand = [\"echo\", \"hi\"]\nrestart = \"always\"\n{body}\n")
}

fn health_with(body: &str) -> String {
    format!("version = 1\n[project]\nname = \"p\"\n[services.api]\ncommand = [\"echo\", \"hi\"]\n[services.api.health]\ntcp = \"127.0.0.1:5432\"\n{body}\n")
}

fn watch_with(body: &str) -> String {
    let dir = "."; // the config's own directory always exists
    format!("version = 1\n[project]\nname = \"p\"\n[services.api]\ncommand = [\"echo\", \"hi\"]\n[services.api.watch]\npaths = [\"{dir}\"]\n{body}\n")
}

fn logs_with(body: &str) -> String {
    format!("version = 1\n[project]\nname = \"p\"\n[project.logs]\n{body}\n[services.api]\ncommand = [\"echo\", \"hi\"]\n")
}

/// A value on the documented bound is accepted; one past it is refused, and the
/// refusal names the field so an operator can find it.
#[track_caller]
fn bound_holds(field: &str, accepted: &str, refused: &str) {
    let (code, _) = check(accepted);
    assert_eq!(
        code, 0,
        "`{field}`: the value the README documents as the edge of the range was rejected"
    );

    let (code, stderr) = check(refused);
    assert_eq!(
        code, 1,
        "`{field}`: a value outside the documented range was accepted"
    );
    assert!(
        stderr.contains(field),
        "`{field}`: the error does not name the field:\n{stderr}"
    );
}

#[test]
fn shutdown_timeout_range_is_as_documented() {
    assert_documents("shutdown_timeout", "100ms … 1h");
    bound_holds(
        "shutdown_timeout",
        &service_with("shutdown_timeout = \"1h\""),
        &service_with("shutdown_timeout = \"1h1s\""),
    );
    bound_holds(
        "shutdown_timeout",
        &service_with("shutdown_timeout = \"100ms\""),
        &service_with("shutdown_timeout = \"99ms\""),
    );
}

#[test]
fn restart_delay_range_is_as_documented() {
    assert_documents("restart_delay", "100ms … 1h");
    // `restart_max_delay` must stay >= `restart_delay`, which the reference also
    // documents, so the ceiling has to be raised to probe this bound at all.
    bound_holds(
        "restart_delay",
        &service_with("restart_delay = \"1h\"\nrestart_max_delay = \"2h\""),
        &service_with("restart_delay = \"61m\"\nrestart_max_delay = \"2h\""),
    );
    bound_holds(
        "restart_delay",
        &service_with("restart_delay = \"100ms\""),
        &service_with("restart_delay = \"50ms\""),
    );
}

#[test]
fn restart_max_delay_range_is_as_documented() {
    assert_documents("restart_max_delay", "100ms … 24h");
    bound_holds(
        "restart_max_delay",
        &service_with("restart_max_delay = \"24h\""),
        &service_with("restart_max_delay = \"25h\""),
    );
}

#[test]
fn stable_after_range_is_as_documented() {
    assert_documents("stable_after", "1s … 24h");
    bound_holds(
        "stable_after",
        &service_with("stable_after = \"1s\""),
        &service_with("stable_after = \"999ms\""),
    );
    bound_holds(
        "stable_after",
        &service_with("stable_after = \"24h\""),
        &service_with("stable_after = \"24h1s\""),
    );
}

#[test]
fn health_interval_and_timeout_ranges_are_as_documented() {
    assert_documents("interval", "100ms … 1h");
    bound_holds(
        "health.interval",
        &health_with("interval = \"100ms\""),
        &health_with("interval = \"99ms\""),
    );
    bound_holds(
        "health.timeout",
        &health_with("timeout = \"1h\""),
        &health_with("timeout = \"2h\""),
    );
}

#[test]
fn health_start_period_range_is_as_documented() {
    assert_documents("start_period", "0s … 24h");
    bound_holds(
        "health.start_period",
        &health_with("start_period = \"24h\""),
        &health_with("start_period = \"48h\""),
    );
    // Zero is inside the range, and is the default.
    let (code, _) = check(&health_with("start_period = \"0s\""));
    assert_eq!(code, 0, "start_period = 0s should be accepted");
}

#[test]
fn health_retries_minimum_is_as_documented() {
    assert_documents("retries", "≥ 1");
    let (code, _) = check(&health_with("retries = 1"));
    assert_eq!(code, 0, "retries = 1 should be accepted");

    let (code, stderr) = check(&health_with("retries = 0"));
    assert_eq!(code, 1, "retries = 0 should be refused");
    assert!(stderr.contains("retries"), "{stderr}");
}

#[test]
fn watch_debounce_minimum_is_as_documented() {
    assert_documents("debounce", "50ms … 1h");
    bound_holds(
        "watch.debounce",
        &watch_with("debounce = \"50ms\""),
        &watch_with("debounce = \"49ms\""),
    );
}

#[test]
fn watch_interval_range_is_as_documented() {
    bound_holds(
        "watch.interval",
        &watch_with("interval = \"100ms\""),
        &watch_with("interval = \"99ms\""),
    );
}

#[test]
fn log_max_size_range_is_as_documented() {
    assert_documents("max_size", "1 KiB … 1 TiB");
    // The smallest and largest accepted thresholds, spelled both ways round.
    let (code, _) = check(&logs_with("max_size = \"1024\""));
    assert_eq!(code, 0, "1 KiB should be accepted");
    let (code, _) = check(&logs_with("max_size = \"1TiB\""));
    assert_eq!(code, 0, "1 TiB should be accepted");

    let (code, stderr) = check(&logs_with("max_size = \"1023\""));
    assert_eq!(code, 1, "below 1 KiB should be refused");
    assert!(stderr.contains("max_size"), "{stderr}");

    let (code, stderr) = check(&logs_with("max_size = \"2TiB\""));
    assert_eq!(code, 1, "above 1 TiB should be refused");
    assert!(stderr.contains("max_size"), "{stderr}");
}

#[test]
fn log_max_files_range_is_as_documented() {
    assert_documents("max_files", "0 … 100");
    let (code, _) = check(&logs_with("max_files = 100"));
    assert_eq!(code, 0, "100 rotated files should be accepted");
    let (code, _) = check(&logs_with("max_files = 0"));
    assert_eq!(code, 0, "0 rotated files should be accepted");

    let (code, stderr) = check(&logs_with("max_files = 101"));
    assert_eq!(code, 1, "101 rotated files should be refused");
    assert!(stderr.contains("max_files"), "{stderr}");
}

/// The name-length limits the reference quotes, checked by overshooting them.
#[test]
fn name_length_limits_are_as_documented() {
    let readme = readme();
    assert!(
        readme.contains("64 bytes"),
        "README no longer documents the 64-byte project-name limit"
    );
    assert!(
        readme.contains("48-byte"),
        "README no longer documents the 48-byte service-name limit"
    );

    let long_project = "p".repeat(65);
    let (code, stderr) = check(&format!(
        "version = 1\n[project]\nname = \"{long_project}\"\n[services.api]\ncommand = [\"echo\"]\n"
    ));
    assert_eq!(code, 1, "a 65-byte project name should be refused");
    assert!(stderr.contains("64"), "{stderr}");

    let long_service = "s".repeat(49);
    let (code, stderr) = check(&format!(
        "version = 1\n[project]\nname = \"p\"\n[services.{long_service}]\ncommand = [\"echo\"]\n"
    ));
    assert_eq!(code, 1, "a 49-byte service name should be refused");
    assert!(stderr.contains("48"), "{stderr}");
}
