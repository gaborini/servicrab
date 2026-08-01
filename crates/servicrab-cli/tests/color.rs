//! Integration tests for the colour decision.
//!
//! Colour is a per-stream question — is *this* stream a terminal — and the only
//! way to hold that to account is to give the process a real terminal on one
//! stream and a pipe on the other, which is what the pty here is for.

#![cfg(unix)]

use std::fs;
use std::io::Read;
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tempfile::TempDir;

/// Any escape sequence at all; the tests only care whether there is colour.
const ESCAPE: &str = "\x1b[";

fn binary() -> PathBuf {
    assert_cmd::cargo::cargo_bin("servicrab")
}

/// A stack of one service that prints one line and exits.
fn stack(dir: &Path) -> PathBuf {
    let script = dir.join("hello.sh");
    fs::write(&script, "#!/bin/sh\necho hello-on-stdout\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

    let path = dir.join("servicrab.toml");
    fs::write(
        &path,
        format!(
            "version = 1\n[project]\nname = \"demo\"\n[services.api]\ncommand = [\"{}\"]\nrestart = \"never\"\n",
            script.display()
        ),
    )
    .unwrap();
    path
}

/// Which stream gets the terminal.
#[derive(Clone, Copy)]
enum Terminal {
    Stdout,
    Stderr,
    Neither,
}

/// What one run wrote, per stream.
struct Written {
    stdout: String,
    stderr: String,
}

/// Run `servicrab up` with `terminal` on a pty and the other stream on a pipe.
///
/// The pty's output is read back as that stream's text, so a caller can ask the
/// same question of either stream without caring which one was the terminal.
fn run(config: &Path, terminal: Terminal, args: &[&str], env: &[(&str, &str)]) -> Written {
    let mut command = Command::new(binary());
    command
        .arg("up")
        .args(args)
        .arg("--config")
        .arg(config)
        .env_remove("RUST_LOG")
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env("TERM", "xterm-256color")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }

    let pty = match terminal {
        Terminal::Neither => None,
        Terminal::Stdout | Terminal::Stderr => Some(nix::pty::openpty(None, None).expect("a pty")),
    };
    if let Some(pty) = &pty {
        let slave = pty.slave.try_clone().expect("clone the pty");
        match terminal {
            Terminal::Stdout => command.stdout(Stdio::from(slave)),
            Terminal::Stderr => command.stderr(Stdio::from(slave)),
            Terminal::Neither => unreachable!(),
        };
        // Without a session of its own the child shares this process's
        // controlling terminal, and the pty's terminal-ness is the only thing
        // under test.
        unsafe {
            command.pre_exec(|| {
                nix::unistd::setsid().map_err(std::io::Error::from)?;
                Ok(())
            });
        }
    }

    let mut child = command.spawn().expect("failed to run servicrab");
    // The parent's copy of the slave has to go, or the master would never see
    // the far end close.  Reading it has to happen *while* the child runs:
    // whatever the slave wrote and nobody read is discarded when the slave is
    // closed, so draining afterwards would find an empty terminal.
    let master = pty.map(|pty| {
        drop(pty.slave);
        pty.master
    });
    let finished = Arc::new(AtomicBool::new(false));
    let terminal_reader = master.map(|master| {
        let finished = Arc::clone(&finished);
        std::thread::spawn(move || read_terminal(master, &finished))
    });

    // Whichever stream got the pty is `None` here; the other is a pipe, and
    // both are drained concurrently so neither can fill and stall the child.
    let out = child.stdout.take();
    let out_reader = std::thread::spawn(move || drain(out));
    let err = child.stderr.take();
    let err_reader = std::thread::spawn(move || drain(err));

    child.wait().expect("servicrab exited");
    finished.store(true, Ordering::SeqCst);
    let piped_out = out_reader.join().expect("read stdout");
    let piped_err = err_reader.join().expect("read stderr");
    let from_terminal = terminal_reader
        .map(|reader| reader.join().expect("read the terminal"))
        .unwrap_or_default();

    match terminal {
        Terminal::Stdout => Written {
            stdout: from_terminal,
            stderr: piped_err,
        },
        Terminal::Stderr => Written {
            stdout: piped_out,
            stderr: from_terminal,
        },
        Terminal::Neither => Written {
            stdout: piped_out,
            stderr: piped_err,
        },
    }
}

/// Read one of the child's pipes to the end, if it has one.
fn drain(stream: Option<impl Read>) -> String {
    let mut text = String::new();
    if let Some(mut stream) = stream {
        let _ = stream.read_to_string(&mut text);
    }
    text
}

/// Drain a pty master until the child is gone and it has stopped producing.
///
/// A master has no end of file to wait for: the kernel keeps it open whether or
/// not any slave is left, so a blocking read would park here forever.  Reading
/// without blocking, and stopping once the process has exited and the terminal
/// has gone quiet, is what ends this.
fn read_terminal(master: OwnedFd, finished: &AtomicBool) -> String {
    use nix::fcntl::{fcntl, FcntlArg, OFlag};

    fcntl(&master, FcntlArg::F_SETFL(OFlag::O_NONBLOCK)).expect("a non-blocking master");

    let mut collected: Vec<u8> = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        match nix::unistd::read(&master, &mut buffer) {
            Ok(0) => return String::from_utf8_lossy(&collected).into_owned(),
            Ok(read) => collected.extend_from_slice(&buffer[..read]),
            Err(nix::errno::Errno::EAGAIN) => {
                if finished.load(Ordering::SeqCst) {
                    return String::from_utf8_lossy(&collected).into_owned();
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(_) => return String::from_utf8_lossy(&collected).into_owned(),
        }
    }
}

#[test]
fn a_terminal_on_stderr_is_coloured_even_when_stdout_is_a_pipe() {
    let dir = TempDir::new().unwrap();
    let config = stack(dir.path());

    let written = run(&config, Terminal::Stderr, &[], &[]);

    // The banner and the status lines are stderr's, and stderr is the terminal.
    assert!(
        written.stderr.contains(ESCAPE),
        "stderr had no colour: {:?}",
        written.stderr
    );
    // The service's own output went into a pipe, which gets none.
    assert!(
        !written.stdout.contains(ESCAPE),
        "stdout was coloured into a pipe: {:?}",
        written.stdout
    );
}

#[test]
fn a_terminal_on_stdout_leaves_a_redirected_stderr_uncoloured() {
    let dir = TempDir::new().unwrap();
    let config = stack(dir.path());

    let written = run(&config, Terminal::Stdout, &[], &[]);

    assert!(
        written.stdout.contains(ESCAPE),
        "stdout had no colour: {:?}",
        written.stdout
    );
    // `servicrab up 2> stack.err` should not put escapes in the file.
    assert!(
        !written.stderr.contains(ESCAPE),
        "stderr was coloured into a pipe: {:?}",
        written.stderr
    );
}

#[test]
fn two_pipes_are_left_alone() {
    let dir = TempDir::new().unwrap();
    let config = stack(dir.path());

    let written = run(&config, Terminal::Neither, &[], &[]);
    assert!(!written.stdout.contains(ESCAPE), "{:?}", written.stdout);
    assert!(!written.stderr.contains(ESCAPE), "{:?}", written.stderr);
}

#[test]
fn color_always_colours_both_pipes() {
    let dir = TempDir::new().unwrap();
    let config = stack(dir.path());

    let written = run(&config, Terminal::Neither, &["--color=always"], &[]);
    assert!(written.stdout.contains(ESCAPE), "{:?}", written.stdout);
    assert!(written.stderr.contains(ESCAPE), "{:?}", written.stderr);
}

#[test]
fn color_never_leaves_a_terminal_alone() {
    let dir = TempDir::new().unwrap();
    let config = stack(dir.path());

    let written = run(&config, Terminal::Stderr, &["--color=never"], &[]);
    assert!(!written.stderr.contains(ESCAPE), "{:?}", written.stderr);

    let written = run(&config, Terminal::Stderr, &["--no-color"], &[]);
    assert!(!written.stderr.contains(ESCAPE), "{:?}", written.stderr);
}

#[test]
fn clicolor_force_colours_a_pipe() {
    let dir = TempDir::new().unwrap();
    let config = stack(dir.path());

    let written = run(&config, Terminal::Neither, &[], &[("CLICOLOR_FORCE", "1")]);
    assert!(written.stderr.contains(ESCAPE), "{:?}", written.stderr);

    // Turned off the way the convention says it is turned off.
    let written = run(&config, Terminal::Neither, &[], &[("CLICOLOR_FORCE", "0")]);
    assert!(!written.stderr.contains(ESCAPE), "{:?}", written.stderr);
}

#[test]
fn no_color_wins_over_clicolor_force_and_the_flag_wins_over_both() {
    let dir = TempDir::new().unwrap();
    let config = stack(dir.path());

    let env = [("CLICOLOR_FORCE", "1"), ("NO_COLOR", "1")];
    let written = run(&config, Terminal::Neither, &[], &env);
    assert!(!written.stderr.contains(ESCAPE), "{:?}", written.stderr);

    let written = run(&config, Terminal::Neither, &["--color=always"], &env);
    assert!(written.stderr.contains(ESCAPE), "{:?}", written.stderr);
}

#[test]
fn color_and_no_color_cannot_be_asked_for_at_once() {
    let dir = TempDir::new().unwrap();
    let config = stack(dir.path());

    let output = Command::new(binary())
        .arg("up")
        .arg("--color=always")
        .arg("--no-color")
        .arg("--config")
        .arg(&config)
        .output()
        .expect("failed to run servicrab");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--no-color"),
        "{:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}
