//! Integration tests for `servicrab exec`.
//!
//! None of these start a daemon or a service: reproducing an environment is
//! meant to work on a stack that is not running.

use std::fs;

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use tempfile::TempDir;

/// Write `servicrab.toml` and any extra files into a fresh temp dir.
fn project(toml: &str, files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("servicrab.toml"), toml).unwrap();
    for (name, contents) in files {
        let path = dir.path().join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }
    dir
}

fn exec(dir: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("servicrab").unwrap();
    cmd.current_dir(dir.path()).arg("exec");
    cmd
}

#[test]
fn the_command_sees_the_services_layered_environment() {
    let dir = project(
        "version = 1\n\
         [project]\n\
         name = \"demo\"\n\
         [project.env]\n\
         FROM_PROJECT = \"project\"\n\
         [services.api]\n\
         command = [\"true\"]\n\
         env_file = \".env\"\n\
         [services.api.env]\n\
         SHARED = \"inline\"\n",
        &[(".env", "FROM_FILE=file\nSHARED=file\n")],
    );

    exec(&dir)
        .arg("api")
        .arg("--")
        .arg("sh")
        .arg("-c")
        .arg("echo $FROM_FILE $FROM_PROJECT $SHARED")
        // The service's own `env` wins over its `env_file`, exactly as it does
        // when the supervisor starts it.
        .assert()
        .success()
        .stdout("file project inline\n");
}

#[test]
fn the_command_runs_in_the_services_working_directory() {
    let dir = project(
        "version = 1\n\
         [project]\n\
         name = \"demo\"\n\
         [services.api]\n\
         command = [\"true\"]\n\
         cwd = \"sub\"\n",
        &[("sub/keep", "")],
    );
    let expected = dir.path().join("sub").canonicalize().unwrap();

    exec(&dir)
        .arg("api")
        .arg("--")
        .arg("pwd")
        .assert()
        .success()
        .stdout(format!("{}\n", expected.display()));
}

#[test]
fn the_environment_servicrab_was_started_in_is_inherited() {
    let dir = project(
        "version = 1\n[project]\nname = \"demo\"\n[services.api]\ncommand = [\"true\"]\n",
        &[],
    );

    exec(&dir)
        .env("OUTER", "from-the-caller")
        .arg("api")
        .arg("--")
        .arg("sh")
        .arg("-c")
        .arg("echo $OUTER")
        .assert()
        .success()
        .stdout("from-the-caller\n");
}

#[test]
fn the_commands_exit_code_becomes_ours() {
    let dir = project(
        "version = 1\n[project]\nname = \"demo\"\n[services.api]\ncommand = [\"true\"]\n",
        &[],
    );

    exec(&dir)
        .arg("api")
        .arg("--")
        .arg("sh")
        .arg("-c")
        .arg("exit 42")
        .assert()
        .code(42);
}

#[test]
fn output_is_passed_through_undecorated() {
    let dir = project(
        "version = 1\n[project]\nname = \"demo\"\n[services.api]\ncommand = [\"true\"]\n",
        &[],
    );

    // Nothing of ours may end up in stdout: `exec` is meant to sit in a pipe.
    exec(&dir)
        .arg("api")
        .arg("--")
        .arg("echo")
        .arg("plain")
        .assert()
        .success()
        .stdout("plain\n");
}

#[test]
fn everything_after_the_service_belongs_to_the_command() {
    let dir = project(
        "version = 1\n[project]\nname = \"demo\"\n[services.api]\ncommand = [\"true\"]\n",
        &[],
    );

    // `--config` here is an argument of `echo`, not of servicrab.
    exec(&dir)
        .arg("api")
        .arg("--")
        .arg("echo")
        .arg("--config")
        .assert()
        .success()
        .stdout("--config\n");
}

#[test]
fn a_command_that_does_not_exist_reports_127() {
    let dir = project(
        "version = 1\n[project]\nname = \"demo\"\n[services.api]\ncommand = [\"true\"]\n",
        &[],
    );

    // 127 is what a shell returns for "command not found", so a script can
    // tell a missing command from one that ran and failed.
    exec(&dir)
        .arg("api")
        .arg("--")
        .arg("definitely-not-on-this-machine")
        .assert()
        .code(127)
        .stderr(contains("could not run"));
}

#[test]
fn an_unknown_service_names_the_ones_that_exist() {
    let dir = project(
        "version = 1\n[project]\nname = \"demo\"\n[services.api]\ncommand = [\"true\"]\n",
        &[],
    );

    exec(&dir)
        .arg("nope")
        .arg("--")
        .arg("true")
        .assert()
        .failure()
        .stderr(contains("unknown service").and(contains("api")));
}

#[test]
fn a_broken_config_is_refused_before_anything_runs() {
    let dir = project(
        "version = 1\n[project]\nname = \"demo\"\n[services.api]\ncommand = []\n",
        &[],
    );

    exec(&dir)
        .arg("api")
        .arg("--")
        .arg("echo")
        .arg("should-not-run")
        .assert()
        .failure()
        .stdout("");
}

#[test]
fn exec_needs_a_command() {
    let dir = project(
        "version = 1\n[project]\nname = \"demo\"\n[services.api]\ncommand = [\"true\"]\n",
        &[],
    );

    exec(&dir)
        .arg("api")
        .assert()
        .failure()
        .stderr(contains("COMMAND"));
}
