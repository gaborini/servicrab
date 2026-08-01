//! Integration tests for the `servicrab` CLI binary.

use std::fs;

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use tempfile::TempDir;

// ── helpers ────────────────────────────────────────────────────────────────

/// Create a temp dir with a minimal valid `servicrab.toml`.
fn temp_dir_with_config(toml: &str) -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("servicrab.toml");
    fs::write(&path, toml).unwrap();
    (dir, path)
}

fn minimal_config() -> &'static str {
    "version = 1\n[project]\nname = \"test-project\"\n[services.web]\ncommand = [\"echo\", \"hello\"]\n"
}

fn cmd() -> Command {
    Command::cargo_bin("servicrab").unwrap()
}

// ── init ───────────────────────────────────────────────────────────────────

#[test]
fn init_creates_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("servicrab.toml");

    cmd()
        .arg("init")
        .arg("--path")
        .arg(&path)
        .assert()
        .success()
        .stdout(contains("Created"));

    assert!(path.exists(), "config file should have been created");
    let contents = fs::read_to_string(&path).unwrap();
    assert!(
        contents.contains("version = 1"),
        "file should contain version"
    );
}

#[test]
fn init_does_not_overwrite_without_force() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("servicrab.toml");
    fs::write(&path, "existing content").unwrap();

    cmd()
        .arg("init")
        .arg("--path")
        .arg(&path)
        .assert()
        .failure()
        .stderr(contains("already exists"));

    // File should be untouched.
    assert_eq!(fs::read_to_string(&path).unwrap(), "existing content");
}

#[test]
fn init_overwrites_with_force() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("servicrab.toml");
    fs::write(&path, "old content").unwrap();

    cmd()
        .arg("init")
        .arg("--path")
        .arg(&path)
        .arg("--force")
        .assert()
        .success();

    let contents = fs::read_to_string(&path).unwrap();
    assert!(
        contents.contains("version = 1"),
        "file should be overwritten"
    );
}

// ── check ──────────────────────────────────────────────────────────────────

#[test]
fn check_success() {
    let (_dir, path) = temp_dir_with_config(minimal_config());

    cmd()
        .arg("check")
        .arg("--config")
        .arg(&path)
        .assert()
        .success()
        .stdout(contains("test-project"));
}

#[test]
fn check_failure_invalid_config() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("servicrab.toml");
    fs::write(&path, "version = 1\n[project]\nname = \"p\"\n").unwrap();

    cmd()
        .arg("check")
        .arg("--config")
        .arg(&path)
        .assert()
        .failure()
        .stderr(contains("error"));
}

#[test]
fn check_failure_missing_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("does-not-exist.toml");

    cmd()
        .arg("check")
        .arg("--config")
        .arg(&path)
        .assert()
        .failure();
}

#[test]
fn check_shows_start_order() {
    let toml = r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["echo", "api"]
depends_on = ["db"]
[services.db]
command = ["echo", "db"]
"#;
    let (_dir, path) = temp_dir_with_config(toml);

    cmd()
        .arg("check")
        .arg("--config")
        .arg(&path)
        .assert()
        .success()
        .stdout(contains("db"))
        .stdout(contains("api"));
}

#[test]
fn check_says_which_services_wait_for_a_profile() {
    let toml = r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["echo", "api"]
[services.mailhog]
command = ["echo", "mail"]
profiles = ["dev"]
[services.seeder]
command = ["echo", "seed"]
profiles = ["dev", "test"]
"#;
    let (_dir, path) = temp_dir_with_config(toml);

    cmd()
        .arg("check")
        .arg("--config")
        .arg(&path)
        .assert()
        .success()
        .stdout(contains("profile dev: mailhog, seeder"))
        .stdout(contains("profile test: seeder"));
}

#[test]
fn check_stays_quiet_about_profiles_when_there_are_none() {
    let toml = r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["echo", "api"]
"#;
    let (_dir, path) = temp_dir_with_config(toml);

    cmd()
        .arg("check")
        .arg("--config")
        .arg(&path)
        .assert()
        .success()
        .stdout(contains("profile").not());
}

// ── list ───────────────────────────────────────────────────────────────────

#[test]
fn list_human_output() {
    let (_dir, path) = temp_dir_with_config(minimal_config());

    cmd()
        .arg("list")
        .arg("--config")
        .arg(&path)
        .assert()
        .success()
        .stdout(contains("test-project"))
        .stdout(contains("web"));
}

#[test]
fn list_json_output() {
    let (_dir, path) = temp_dir_with_config(minimal_config());

    let output = cmd()
        .arg("list")
        .arg("--config")
        .arg(&path)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let text = String::from_utf8(output).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");

    // An envelope with the services inside it, so there is somewhere to put
    // the schema version.
    assert_eq!(json["schema_version"], 1, "{json:#}");
    assert_eq!(json["project"], "test-project", "{json:#}");
    let arr = json["services"]
        .as_array()
        .expect("the services are still an array");
    assert!(!arr.is_empty());

    let first = &arr[0];
    assert_eq!(first["name"].as_str().unwrap(), "web");
    assert!(first["command"].is_array());
    assert!(first["autostart"].as_bool().unwrap());
    assert_eq!(first["restart"].as_str().unwrap(), "never");
}

#[test]
fn list_handles_a_command_with_multi_byte_characters() {
    // The preview used to be sliced at byte 29, which falls inside the last
    // '€' here: `list` died with "not a char boundary" and exit 101.
    let toml = "version = 1\n[project]\nname = \"demo\"\n[services.api]\ncommand = [\"echo\", \"x€€€€€€€€€\"]\n";
    let (_dir, path) = temp_dir_with_config(toml);

    cmd()
        .arg("list")
        .arg("--config")
        .arg(&path)
        .assert()
        .success()
        .stdout(contains("x€€€€€€€€€"));

    let output = cmd()
        .arg("list")
        .arg("--config")
        .arg(&path)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8(output).unwrap()).expect("valid JSON");
    assert_eq!(
        json["services"][0]["command"],
        serde_json::json!(["echo", "x€€€€€€€€€"])
    );
}

#[test]
fn list_truncates_a_long_multi_byte_command_without_panicking() {
    // Long enough that the preview has to cut, with the cut point inside a
    // multi-byte character.
    let arg = "€".repeat(40);
    let toml = format!(
        "version = 1\n[project]\nname = \"demo\"\n[services.api]\ncommand = [\"echo\", \"{arg}\"]\n"
    );
    let (_dir, path) = temp_dir_with_config(&toml);

    // 29 characters of the command line, then the ellipsis.
    let expected = format!("echo {}…", "€".repeat(24));
    cmd()
        .arg("list")
        .arg("--config")
        .arg(&path)
        .assert()
        .success()
        // Cut at a character boundary, so the ellipsis follows whole '€'s.
        .stdout(contains(expected));
}

#[test]
fn list_reports_config_warnings() {
    // `max_restarts` alongside `restart = "never"` is inert, so loading warns.
    // `list` used to throw those warnings away, unlike the other commands.
    let toml = "version = 1\n[project]\nname = \"demo\"\n[services.api]\ncommand = [\"echo\"]\nrestart = \"never\"\nmax_restarts = 3\n";
    let (_dir, path) = temp_dir_with_config(toml);

    cmd()
        .arg("list")
        .arg("--config")
        .arg(&path)
        .assert()
        .success()
        .stderr(contains("max_restarts"));

    // On stderr, so `--json` output stays parseable on stdout.
    let assert = cmd()
        .arg("list")
        .arg("--config")
        .arg(&path)
        .arg("--json")
        .assert()
        .success()
        .stderr(contains("max_restarts"));
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    serde_json::from_str::<serde_json::Value>(&stdout).expect("valid JSON");
}

// ── profiles ───────────────────────────────────────────────────────────────

#[test]
fn list_reports_the_profiles_a_service_belongs_to() {
    let toml = r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["echo"]
[services.seeder]
command = ["echo"]
profiles = ["dev", "test"]
"#;
    let (_dir, path) = temp_dir_with_config(toml);

    cmd()
        .arg("list")
        .arg("--config")
        .arg(&path)
        .assert()
        .success()
        .stdout(contains("profiles: dev, test"));

    let output = cmd()
        .arg("list")
        .arg("--config")
        .arg(&path)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8(output).unwrap()).expect("valid JSON");
    let services = &json["services"];
    assert_eq!(services[0]["name"], "api");
    assert_eq!(services[0]["profiles"], serde_json::json!([]), "{json:#}");
    assert_eq!(
        services[1]["profiles"],
        serde_json::json!(["dev", "test"]),
        "{json:#}"
    );
}

// ── include ────────────────────────────────────────────────────────────────

/// Write a root config that includes `services/db.toml`, plus that fragment.
fn temp_dir_with_include(root_extra: &str, fragment: &str) -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join("services")).unwrap();
    fs::write(dir.path().join("services/db.toml"), fragment).unwrap();

    let path = dir.path().join("servicrab.toml");
    let root = format!(
        "version = 1\ninclude = [\"services/db.toml\"]\n[project]\nname = \"demo\"\n{root_extra}"
    );
    fs::write(&path, root).unwrap();
    (dir, path)
}

#[test]
fn an_included_service_is_part_of_the_stack() {
    let (_dir, path) = temp_dir_with_include(
        "[services.api]\ncommand = [\"echo\", \"api\"]\ndepends_on = [\"db\"]\n",
        "[services.db]\ncommand = [\"echo\", \"db\"]\n",
    );

    cmd()
        .arg("check")
        .arg("--config")
        .arg(&path)
        .assert()
        .success()
        .stdout(contains("db"))
        .stdout(contains("api"));
}

#[test]
fn a_relative_path_in_a_fragment_belongs_to_the_fragment() {
    // `cwd = "."` in services/db.toml means the services directory, so that a
    // fragment can be moved together with what it describes.
    let (dir, path) =
        temp_dir_with_include("", "[services.db]\ncommand = [\"pwd\"]\ncwd = \".\"\n");
    let services = dir.path().join("services").canonicalize().unwrap();

    cmd()
        .arg("run")
        .arg("db")
        .arg("--config")
        .arg(&path)
        .assert()
        .success()
        .stdout(contains(services.to_str().unwrap()));
}

#[test]
fn a_missing_include_is_reported_with_both_files() {
    let (_dir, path) = temp_dir_with_config(
        "version = 1\ninclude = [\"services/db.toml\"]\n[project]\nname = \"demo\"\n",
    );

    cmd()
        .arg("check")
        .arg("--config")
        .arg(&path)
        .assert()
        .failure()
        .stderr(contains("servicrab.toml"))
        .stderr(contains("services/db.toml"));
}

// ── variable substitution ──────────────────────────────────────────────────

/// The config a substitution test loads: one value from the environment, one
/// with a default.
fn config_with_variables() -> &'static str {
    r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["${SERVICRAB_TEST_EXE}", "--port=${SERVICRAB_TEST_PORT:-3000}"]
"#
}

#[test]
fn values_are_taken_from_the_environment() {
    let (_dir, path) = temp_dir_with_config(config_with_variables());

    let output = cmd()
        .arg("list")
        .arg("--config")
        .arg(&path)
        .arg("--json")
        .env("SERVICRAB_TEST_EXE", "echo")
        .env_remove("SERVICRAB_TEST_PORT")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8(output).unwrap()).expect("valid JSON");
    assert_eq!(
        json["services"][0]["command"],
        serde_json::json!(["echo", "--port=3000"]),
        "{json:#}"
    );
}

#[test]
fn an_unset_variable_is_reported_rather_than_emptied() {
    let (_dir, path) = temp_dir_with_config(config_with_variables());

    cmd()
        .arg("check")
        .arg("--config")
        .arg(&path)
        .env_remove("SERVICRAB_TEST_EXE")
        .assert()
        .failure()
        .stderr(contains("SERVICRAB_TEST_EXE"))
        .stderr(contains("command[0]"));
}

#[test]
fn list_reports_the_effective_dependency_condition() {
    let toml = r#"
version = 1
[project]
name = "demo"
[services.api]
command = ["echo", "api"]
depends_on = ["db", "migrate"]
[services.migrate]
command = ["echo", "migrate"]
[services.db]
command = ["echo", "db"]
[services.db.health]
tcp = "127.0.0.1:1"
"#;
    let (_dir, path) = temp_dir_with_config(toml);

    let output = cmd()
        .arg("list")
        .arg("--config")
        .arg(&path)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8(output).unwrap()).expect("valid JSON");
    let api = json["services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|svc| svc["name"] == "api")
        .expect("api in the listing");

    // Neither entry spells a condition out, so both are resolved from the
    // dependency: the health-checked one gates on a probe, the other does not.
    assert_eq!(
        api["depends_on"],
        serde_json::json!([
            { "service": "db", "condition": "service_healthy" },
            { "service": "migrate", "condition": "service_started" },
        ]),
        "{json:#}"
    );
}

#[test]
fn list_failure_on_invalid_config() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("servicrab.toml");
    // Missing services section
    fs::write(&path, "version = 1\n[project]\nname = \"p\"\n").unwrap();

    cmd()
        .arg("list")
        .arg("--config")
        .arg(&path)
        .assert()
        .failure();
}

// ── completions ────────────────────────────────────────────────────────────

#[test]
fn completions_are_generated_for_every_supported_shell() {
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let output = cmd()
            .arg("completions")
            .arg(shell)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let text = String::from_utf8(output).unwrap();
        assert!(
            text.contains("servicrab"),
            "{shell} completions should mention the binary name"
        );
        assert!(
            text.len() > 200,
            "{shell} completions look suspiciously short"
        );
    }
}

#[test]
fn completions_mention_the_subcommands() {
    let output = cmd()
        .arg("completions")
        .arg("bash")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(output).unwrap();
    for sub in ["up", "down", "logs", "status", "restart"] {
        assert!(text.contains(sub), "bash completions should list {sub}");
    }
}

#[test]
fn completions_reject_an_unknown_shell() {
    cmd().arg("completions").arg("tcsh").assert().failure();
}

// ── man ────────────────────────────────────────────────────────────────────

#[test]
fn the_man_page_documents_every_subcommand() {
    let output = cmd()
        .arg("man")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(output).unwrap();

    assert!(text.starts_with(".ie"), "should start with roff, not text");
    assert!(text.contains(".TH servicrab 1"), "missing the man title");

    // The page is generated from the clap definitions, so a new subcommand
    // shows up here without anyone remembering to add it.
    for sub in [
        "init",
        "check",
        "list",
        "run",
        "up",
        "watch",
        "logs",
        "start",
        "stop",
        "restart",
        "reload",
        "events",
        "status",
        "down",
        "generate",
        "completions",
        "man",
        "daemon",
    ] {
        assert!(text.contains(sub), "the man page should mention {sub}");
    }
}

#[test]
fn the_man_page_has_the_hand_written_sections() {
    let output = cmd()
        .arg("man")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(output).unwrap();

    for section in [".SH FILES", ".SH ENVIRONMENT", ".SH EXIT STATUS"] {
        assert!(text.contains(section), "the man page should have {section}");
    }
    assert!(text.contains("daemon.sock"), "FILES should list the socket");
    assert!(
        text.contains("RUST_LOG"),
        "ENVIRONMENT should list RUST_LOG"
    );
}

// ── env_file ───────────────────────────────────────────────────────────────

#[test]
fn env_file_values_reach_the_service() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join(".env"), "GREETING=from-file\n").unwrap();
    let path = dir.path().join("servicrab.toml");
    fs::write(
        &path,
        "version = 1\n[project]\nname = \"p\"\n[services.web]\ncommand = [\"sh\", \"-c\", \"echo $GREETING\"]\nenv_file = \".env\"\n",
    )
    .unwrap();

    cmd()
        .arg("run")
        .arg("web")
        .arg("--config")
        .arg(&path)
        .assert()
        .success()
        .stdout(contains("from-file"));
}

#[test]
fn a_missing_env_file_fails_the_config() {
    let (_dir, path) = temp_dir_with_config(
        "version = 1\n[project]\nname = \"p\"\n[services.web]\ncommand = [\"echo\"]\nenv_file = \"nope.env\"\n",
    );

    cmd()
        .arg("check")
        .arg("--config")
        .arg(&path)
        .assert()
        .failure()
        .stderr(contains("env_file"));
}
