//! Where the daemon keeps its socket, pidfile and log.
//!
//! Everything lives next to the config file so that two projects never fight
//! over one daemon, and so that removing the project directory removes its
//! runtime state with it.
//!
//! The socket is the exception, because it has a hard path length limit that a
//! deeply nested project can exceed.  When that happens it moves to
//! `$XDG_RUNTIME_DIR`, which the system creates 0700 and per-user.  It never
//! moves to the shared temp directory: a name there is predictable to every
//! local user, and `/tmp` being sticky means a squatted path can be neither
//! unlinked nor bound, so the project could never start a daemon at all.

use std::path::{Path, PathBuf};

/// Unix sockets have a hard path limit (108 bytes on Linux, 104 on macOS);
/// staying well below it leaves room for the file names themselves.
const MAX_SOCKET_PATH: usize = 90;

/// Runtime file locations for one project's daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonPaths {
    /// Directory holding all of the below.
    pub dir: PathBuf,
    /// The Unix socket clients connect to.
    pub socket: PathBuf,
    /// File holding the daemon's process id while it runs.
    ///
    /// Nothing reads the number — every command decides liveness by connecting
    /// to the socket — but the daemon holds an exclusive `flock` on this file
    /// for its entire life, and that is what keeps two daemons off one project.
    pub pid: PathBuf,
    /// Where a detached daemon's own output is appended.
    pub log: PathBuf,
    /// The services an operator stopped by hand, one name per line.
    pub stopped: PathBuf,
}

impl DaemonPaths {
    /// Derive the paths for a project from its config file.
    ///
    /// The socket lives next to the config like everything else.  Only when
    /// that path would overflow the socket length limit does it move to
    /// `$XDG_RUNTIME_DIR`, under a name derived from the project directory.
    pub fn for_config(config: &Path) -> Self {
        let dir = project_dir(config).join(".servicrab");
        let socket = dir.join("daemon.sock");

        Self {
            socket: relocate_if_too_long(socket, &dir),
            pid: dir.join("daemon.pid"),
            log: dir.join("daemon.log"),
            stopped: dir.join("stopped"),
            dir,
        }
    }

    /// Create the state directory if it does not exist yet.
    pub fn ensure_dir(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| format!("could not create {}: {e}", self.dir.display()))
    }
}

/// The directory holding `config`, absolute wherever we can make it so.
///
/// `--config` is taken verbatim, so it may well be relative.  Two spellings of
/// one project (`a/servicrab.toml` and `./a/servicrab.toml`) have to reach the
/// same daemon, and the length limit has to be measured against the path the
/// kernel will actually see.
fn project_dir(config: &Path) -> PathBuf {
    let parent = config.parent().unwrap_or_else(|| Path::new("."));
    // The project directory exists in every real use; `canonicalize` resolves
    // symlinks too, so the same directory reached two ways is one project.
    if let Ok(resolved) = parent.canonicalize() {
        return resolved;
    }
    // It does not exist yet — `generate` and the error paths both hit this.
    // Absolute is still better than relative, and no worse than before.
    match std::env::current_dir() {
        Ok(cwd) if parent.is_relative() => normalize(&cwd.join(parent)),
        _ => parent.to_path_buf(),
    }
}

/// Drop `.` and `..` components lexically, for a path we cannot ask the
/// filesystem about.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Move the socket out of the project when its path is too long for `bind`.
///
/// `$XDG_RUNTIME_DIR` is created by the system as 0700 and per-user, so a name
/// in it is no more reachable than one inside the project.  The shared temp
/// directory is not an option: `$TMPDIR` puts the location under an attacker's
/// influence, the name would be predictable to every local user, and `/tmp`
/// being sticky means a pre-created path can be neither unlinked nor bound.
///
/// With nowhere private to go we keep the long path.  `bind` then fails with
/// `ENAMETOOLONG`, which names the real problem, where a fallback into a shared
/// directory would silently trade a startup failure for a spoofable socket.
fn relocate_if_too_long(socket: PathBuf, dir: &Path) -> PathBuf {
    if socket.as_os_str().len() <= MAX_SOCKET_PATH {
        return socket;
    }
    let Some(runtime) = runtime_dir() else {
        return socket;
    };

    let moved = runtime.join(format!("servicrab-{}.sock", project_slug(dir)));
    if moved.as_os_str().len() <= MAX_SOCKET_PATH {
        moved
    } else {
        socket
    }
}

/// `$XDG_RUNTIME_DIR`, if it is set to an absolute path.
///
/// A relative value would resolve against whatever directory the command
/// happened to start in, so two invocations for one project could disagree
/// about where the socket is.
fn runtime_dir() -> Option<PathBuf> {
    let value = std::env::var_os("XDG_RUNTIME_DIR")?;
    let path = PathBuf::from(value);
    (path.is_absolute() && path.is_dir()).then_some(path)
}

/// A short, stable, filesystem-safe name for the project at `dir`.
///
/// FNV-1a: tiny and dependency-free.  It is not a security boundary — the
/// directory it names is per-user and 0700, and the peer check is what keeps
/// strangers out — so all it has to do is separate two projects.  A collision
/// costs one confusing "already running" for two projects with the same hash.
fn project_slug(dir: &Path) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in dir.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// The socket location reads `$XDG_RUNTIME_DIR`, which is process-global,
    /// so the tests that set it must not run beside each other.
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Set `$XDG_RUNTIME_DIR` for the duration of `body`.
    fn with_runtime_dir<T>(value: Option<&Path>, body: impl FnOnce() -> T) -> T {
        let _guard = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::env::var_os("XDG_RUNTIME_DIR");
        match value {
            // Safety: the mutex above is what keeps this off other threads.
            Some(path) => unsafe { std::env::set_var("XDG_RUNTIME_DIR", path) },
            None => unsafe { std::env::remove_var("XDG_RUNTIME_DIR") },
        }
        let out = body();
        match previous {
            Some(old) => unsafe { std::env::set_var("XDG_RUNTIME_DIR", old) },
            None => unsafe { std::env::remove_var("XDG_RUNTIME_DIR") },
        }
        out
    }

    /// A directory nested deeply enough that the socket path inside it cannot
    /// fit, returned with the config file inside it.
    fn a_project_too_deep_for_a_socket() -> (TempDir, PathBuf) {
        let root = TempDir::new().expect("temp dir");
        let deep = root
            .path()
            .join("nested".repeat(12))
            .join("more".repeat(12));
        std::fs::create_dir_all(&deep).expect("create the deep project");
        let config = deep.join("servicrab.toml");
        (root, config)
    }

    /// A stand-in for `$XDG_RUNTIME_DIR` that is short enough to hold a socket.
    ///
    /// A real one is `/run/user/1000`; macOS's per-user temp directory, which
    /// is what `TempDir` uses by default, is 60 bytes on its own and would
    /// leave no room for the socket name.
    fn a_runtime_dir() -> TempDir {
        let short = PathBuf::from("/tmp");
        let root = if short.is_dir() {
            short
        } else {
            std::env::temp_dir()
        };
        TempDir::new_in(root).expect("temp dir")
    }

    #[test]
    fn state_lives_next_to_the_config() {
        let dir = TempDir::new().expect("temp dir");
        let root = dir.path().canonicalize().expect("canonicalize");
        // Pinned: macOS temp paths are long enough that whether the socket
        // relocates depends on `$XDG_RUNTIME_DIR`, and that is process-global.
        let runtime = a_runtime_dir();
        let paths = with_runtime_dir(Some(runtime.path()), || {
            DaemonPaths::for_config(&root.join("servicrab.toml"))
        });

        assert_eq!(paths.dir, root.join(".servicrab"));
        assert_eq!(paths.pid, root.join(".servicrab/daemon.pid"));
        assert_eq!(paths.log, root.join(".servicrab/daemon.log"));
        assert_eq!(paths.stopped, root.join(".servicrab/stopped"));
    }

    /// Two spellings of one project are one project.
    ///
    /// The length check used to run on the path as typed, so a short relative
    /// path whose absolute form was too long slipped through and failed `bind`
    /// with ENAMETOOLONG — and `-c a/servicrab.toml` and `-c ./a/servicrab.toml`
    /// hashed differently, giving one project two sockets.
    #[test]
    fn the_same_project_spelled_two_ways_gets_one_socket() {
        let dir = TempDir::new().expect("temp dir");
        std::fs::create_dir_all(dir.path().join("app")).expect("create");
        let runtime = a_runtime_dir();

        let (plain, dotted, detoured) = with_runtime_dir(Some(runtime.path()), || {
            (
                DaemonPaths::for_config(&dir.path().join("app/servicrab.toml")),
                DaemonPaths::for_config(&dir.path().join("./app/./servicrab.toml")),
                DaemonPaths::for_config(&dir.path().join("app/../app/servicrab.toml")),
            )
        });

        assert_eq!(plain.socket, dotted.socket);
        assert_eq!(plain.socket, detoured.socket);
        assert_eq!(plain.dir, detoured.dir);
        assert!(plain.dir.is_absolute(), "{}", plain.dir.display());
    }

    /// A project whose path leaves room for the socket keeps it next to the
    /// config, which is where the documentation and every third-party client
    /// look for it.
    #[test]
    fn a_short_project_keeps_its_socket_next_to_the_config() {
        let dir = a_runtime_dir();
        let root = dir.path().canonicalize().expect("canonicalize");
        let runtime = a_runtime_dir();

        let paths = with_runtime_dir(Some(runtime.path()), || {
            DaemonPaths::for_config(&root.join("servicrab.toml"))
        });

        assert_eq!(paths.socket, root.join(".servicrab/daemon.sock"));
    }

    #[test]
    fn a_relative_config_becomes_an_absolute_socket() {
        let paths = DaemonPaths::for_config(Path::new("servicrab.toml"));
        // The length limit has to be measured against this, not against the
        // fourteen characters the operator typed.
        assert!(paths.dir.is_absolute(), "{}", paths.dir.display());
        assert!(paths.dir.ends_with(".servicrab"));
    }

    #[test]
    fn a_long_path_moves_the_socket_to_the_runtime_dir() {
        let runtime = a_runtime_dir();
        let (_root, config) = a_project_too_deep_for_a_socket();

        let paths = with_runtime_dir(Some(runtime.path()), || DaemonPaths::for_config(&config));

        assert!(
            paths.socket.starts_with(runtime.path()),
            "{} is not in the runtime dir",
            paths.socket.display()
        );
        // Only the socket moves; the rest of the state stays with the project.
        assert!(paths.dir.ends_with(".servicrab"));
        assert!(paths.pid.starts_with(&paths.dir));
    }

    /// With no private directory to move to, the long path stays put and `bind`
    /// reports it.  The shared temp directory is never an answer: a name there
    /// is predictable to every local user, and `/tmp` being sticky means a
    /// squatted path can be neither unlinked nor bound.
    #[test]
    fn without_a_runtime_dir_the_socket_stays_in_the_project() {
        let (_root, config) = a_project_too_deep_for_a_socket();

        let paths = with_runtime_dir(None, || DaemonPaths::for_config(&config));

        assert!(paths.socket.starts_with(&paths.dir), "{paths:?}");
        assert!(
            !paths.socket.starts_with(std::env::temp_dir()),
            "the socket must never land in the shared temp directory"
        );
    }

    /// A relative `$XDG_RUNTIME_DIR` would resolve against whatever directory
    /// each command started in, so one project would get several sockets.
    #[test]
    fn a_relative_runtime_dir_is_ignored() {
        let (_root, config) = a_project_too_deep_for_a_socket();

        let paths = with_runtime_dir(Some(Path::new("relative/run")), || {
            DaemonPaths::for_config(&config)
        });

        assert!(paths.socket.starts_with(&paths.dir), "{paths:?}");
    }

    #[test]
    fn different_projects_get_different_sockets() {
        let runtime = a_runtime_dir();
        let (_root, one) = a_project_too_deep_for_a_socket();
        let (_other_root, two) = a_project_too_deep_for_a_socket();

        let (first, second) = with_runtime_dir(Some(runtime.path()), || {
            (DaemonPaths::for_config(&one), DaemonPaths::for_config(&two))
        });

        assert!(first.socket.starts_with(runtime.path()));
        assert_ne!(first.socket, second.socket);
    }

    #[test]
    fn a_relocated_socket_is_short_enough_to_bind() {
        let runtime = a_runtime_dir();
        let (_root, config) = a_project_too_deep_for_a_socket();

        let paths = with_runtime_dir(Some(runtime.path()), || DaemonPaths::for_config(&config));

        assert!(
            paths.socket.as_os_str().len() <= MAX_SOCKET_PATH,
            "{} is {} bytes",
            paths.socket.display(),
            paths.socket.as_os_str().len()
        );
    }
}
