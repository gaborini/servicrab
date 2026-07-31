//! Where the daemon keeps its socket, pidfile and log.
//!
//! Everything lives next to the config file so that two projects never fight
//! over one daemon, and so that removing the project directory removes its
//! runtime state with it.
//!
//! The socket is the exception, because it has a hard path length limit that a
//! deeply nested project can exceed.  When that happens it moves to the first
//! directory that is genuinely private to this user: `$XDG_RUNTIME_DIR`, then
//! `$TMPDIR`.  Neither is trusted on the strength of its name — both are
//! resolved and then inspected, and only a directory this user owns with no
//! group or other bits at all is accepted.  That is what keeps the socket out
//! of `/tmp`, where a name is predictable to every local user and the sticky
//! bit means a squatted path can be neither unlinked nor bound.
//!
//! With no private directory to move to the long path stays put and `bind`
//! fails, saying which candidates were rejected and why.

use std::path::{Path, PathBuf};

/// How much of a `sockaddr_un` the kernel gives us for a path.
///
/// A socket path has to fit in `sun_path` *including* its terminating NUL, so
/// the longest bindable path is one byte shorter than this — which is what
/// `std` enforces too, rejecting anything longer before it reaches the kernel.
#[cfg(any(target_os = "linux", target_os = "android"))]
const SUN_PATH: usize = 108;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
const SUN_PATH: usize = 104;

/// The longest socket path `bind` will accept.
///
/// This is the real limit rather than a margin below it.  The check runs on the
/// finished, canonicalised path, so there is nothing left to append and nothing
/// for headroom to protect: every byte held back only relocates a project that
/// would have been happy with its socket next to its config.
const MAX_SOCKET_PATH: usize = SUN_PATH - 1;

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
    /// Why the socket could not be moved somewhere shorter, when it had to stay
    /// on a path too long to bind.
    ///
    /// Empty in every ordinary case.  `bind` failing with `ENAMETOOLONG` is
    /// otherwise indistinguishable from servicrab being broken, when the real
    /// answer is usually one line long: `$TMPDIR` is group-readable.
    pub socket_rejections: Vec<String>,
}

impl DaemonPaths {
    /// Derive the paths for a project from its config file.
    ///
    /// The socket lives next to the config like everything else.  Only when
    /// that path would overflow the socket length limit does it move to a
    /// private directory, under a name derived from the project directory.
    pub fn for_config(config: &Path) -> Self {
        let dir = project_dir(config).join(".servicrab");
        let (socket, socket_rejections) = choose_socket(dir.join("daemon.sock"), &dir);

        Self {
            socket,
            socket_rejections,
            pid: dir.join("daemon.pid"),
            log: dir.join("daemon.log"),
            stopped: dir.join("stopped"),
            dir,
        }
    }

    /// Whether the socket sits next to the rest of the project's state.
    ///
    /// When it does not, the path is worth telling the user about: nothing else
    /// would lead them to it.
    pub fn socket_is_in_place(&self) -> bool {
        self.socket.starts_with(&self.dir)
    }

    /// Everything we know about why the socket is where it is, as one line per
    /// rejected candidate, for an error that would otherwise say only
    /// `ENAMETOOLONG`.
    pub fn socket_advice(&self) -> String {
        if self.socket_rejections.is_empty() {
            return String::new();
        }
        format!(
            "\nthe path is {} bytes and the limit is {MAX_SOCKET_PATH}; \
             nowhere private was available to move it to:\n{}",
            self.socket.as_os_str().len(),
            self.socket_rejections
                .iter()
                .map(|why| format!("  • {why}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
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
/// Returns the path to use and, when nothing worked, one line per rejected
/// candidate.
///
/// The candidates are `$XDG_RUNTIME_DIR` and then `$TMPDIR`, and each has to
/// earn it: [`is_private_dir`] resolves the value and then checks that this
/// user owns it and that it has no group or other bits at all.  Plain `/tmp`
/// fails that last check, which is the whole point — it is the shared directory
/// the old FNV-named socket lived in, where the name is predictable to every
/// local user and the sticky bit means a squatted path can be neither unlinked
/// nor bound.
///
/// With nowhere private to go we keep the long path.  `bind` then fails, and
/// the rejections explain what to fix, which is the difference between
/// "servicrab is broken" and "your `$TMPDIR` is group-readable".
fn choose_socket(socket: PathBuf, dir: &Path) -> (PathBuf, Vec<String>) {
    if socket.as_os_str().len() <= MAX_SOCKET_PATH {
        return (socket, Vec::new());
    }

    let name = format!("servicrab-{}.sock", project_slug(dir));
    let mut rejections = Vec::new();
    for (label, value) in candidates() {
        let Some(value) = value else {
            rejections.push(format!("{label} is not set"));
            continue;
        };
        let candidate = match is_private_dir(&value) {
            Ok(resolved) => resolved,
            Err(why) => {
                rejections.push(format!("{label} ({}) {why}", value.display()));
                continue;
            }
        };

        let moved = candidate.join(&name);
        if moved.as_os_str().len() <= MAX_SOCKET_PATH {
            return (moved, Vec::new());
        }
        rejections.push(format!(
            "{label} ({}) is itself too long to hold a socket",
            candidate.display()
        ));
    }

    (socket, rejections)
}

/// Where the socket may move to, best first, each labelled the way the user
/// would recognise it.
///
/// `$XDG_RUNTIME_DIR` first because a system that sets it means it: it is
/// created 0700 per-user and cleaned up at logout.  It is unset on macOS,
/// though, which is where the length limit bites hardest, so the per-user
/// `$TMPDIR` there is the one that makes a deeply nested project work at all.
///
/// [`std::env::temp_dir`] rather than `$TMPDIR` directly, so that a system
/// without the variable still gets a named candidate and therefore a named
/// rejection — on Linux that is `/tmp`, and hearing why it was refused is more
/// use than hearing that a variable is unset.
fn candidates() -> Vec<(String, Option<PathBuf>)> {
    vec![
        (
            "$XDG_RUNTIME_DIR".to_string(),
            std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
        ),
        ("the temp directory".to_string(), Some(std::env::temp_dir())),
    ]
}

/// Judge a directory we are considering putting the socket in, returning it
/// resolved.
///
/// Anyone who can create a name in the directory holding the socket can
/// pre-create it, so "private to this user" has to be checked rather than
/// assumed from the variable's name — both of these are attacker-settable
/// environment variables.
///
/// The checks, in the order a caller would want to hear about a failure:
///
/// * absolute, because a relative value would resolve against whatever
///   directory each command happened to start in, so one project would get
///   several sockets;
/// * a directory, after `canonicalize`, so a symlinked `$TMPDIR` cannot smuggle
///   in a world-writable target;
/// * owned by this uid, so nobody else can rename or replace what is in it;
/// * `mode & 0o077 == 0` — not merely "not world-writable": read or execute for
///   a group is enough to enumerate the sockets, and connecting is the only
///   capability an attacker needs.
fn is_private_dir(path: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::MetadataExt;

    if !path.is_absolute() {
        return Err("is not an absolute path".to_string());
    }
    let resolved = path
        .canonicalize()
        .map_err(|e| format!("cannot be resolved: {e}"))?;
    let meta = std::fs::metadata(&resolved).map_err(|e| format!("cannot be read: {e}"))?;
    if !meta.is_dir() {
        return Err("is not a directory".to_string());
    }

    let us = nix::unistd::getuid().as_raw();
    if meta.uid() != us {
        return Err(format!(
            "is owned by uid {} and not by you (uid {us})",
            meta.uid()
        ));
    }
    let mode = meta.mode() & 0o7777;
    if mode & 0o077 != 0 {
        return Err(format!(
            "is mode {mode:04o}, which lets other users in; \
             it must have no group or other permissions at all"
        ));
    }
    Ok(resolved)
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

    /// The socket location reads the environment, which is process-global, so
    /// the tests that set it must not run beside each other.
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Set `$XDG_RUNTIME_DIR` and `$TMPDIR` for the duration of `body`.
    ///
    /// `$TMPDIR` too, because [`std::env::temp_dir`] reads it: leaving the real
    /// one in place would make the second candidate whatever the machine
    /// running the tests happens to have, and these tests are about the
    /// predicate, not about this machine.
    fn with_env<T>(runtime: Option<&Path>, temp: Option<&Path>, body: impl FnOnce() -> T) -> T {
        let _guard = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = [
            ("XDG_RUNTIME_DIR", std::env::var_os("XDG_RUNTIME_DIR")),
            ("TMPDIR", std::env::var_os("TMPDIR")),
        ];
        // Safety: the mutex above is what keeps this off other threads.
        for (name, value) in [("XDG_RUNTIME_DIR", runtime), ("TMPDIR", temp)] {
            match value {
                Some(path) => unsafe { std::env::set_var(name, path) },
                None => unsafe { std::env::remove_var(name) },
            }
        }
        let out = body();
        for (name, value) in previous {
            match value {
                Some(old) => unsafe { std::env::set_var(name, old) },
                None => unsafe { std::env::remove_var(name) },
            }
        }
        out
    }

    /// A short, already-canonical directory to build test directories under.
    ///
    /// Not `TempDir::new()`, which reads `$TMPDIR` — these tests set `$TMPDIR`,
    /// and it is process-global, so a directory created while another test held
    /// it would land somewhere that test is about to delete.  Canonical because
    /// the code under test canonicalises its candidates, and `/tmp` is a symlink
    /// to `/private/tmp` on macOS.  Short because a runtime directory is
    /// `/run/user/1000`-sized in real life, and macOS's per-user temp directory
    /// is 56 bytes before the socket name.
    static TEMP_ROOT: std::sync::LazyLock<PathBuf> = std::sync::LazyLock::new(|| {
        let short = Path::new("/tmp");
        let root = if short.is_dir() {
            short.to_path_buf()
        } else {
            std::env::temp_dir()
        };
        root.canonicalize().unwrap_or(root)
    });

    /// A directory to put a project or a socket in.
    fn a_dir() -> TempDir {
        TempDir::new_in(&*TEMP_ROOT).expect("temp dir")
    }

    /// A stand-in for `$XDG_RUNTIME_DIR`: short, ours, and 0700.
    fn a_runtime_dir() -> TempDir {
        a_dir_with_mode(0o700)
    }

    /// A directory with exactly `mode`, to feed the predicate.
    fn a_dir_with_mode(mode: u32) -> TempDir {
        use std::os::unix::fs::PermissionsExt;

        let dir = a_dir();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(mode))
            .expect("set the mode");
        dir
    }

    /// Set `$XDG_RUNTIME_DIR` for the duration of `body`, with a private temp
    /// directory behind it so it is never the reason a test passes.
    fn with_runtime_dir<T>(value: Option<&Path>, body: impl FnOnce() -> T) -> T {
        let private = a_dir_with_mode(0o700);
        with_env(value, Some(private.path()), body)
    }

    /// A directory nested deeply enough that the socket path inside it cannot
    /// fit, returned with the config file inside it.
    fn a_project_too_deep_for_a_socket() -> (TempDir, PathBuf) {
        let root = a_dir();
        let deep = root
            .path()
            .join("nested".repeat(12))
            .join("more".repeat(12));
        std::fs::create_dir_all(&deep).expect("create the deep project");
        let config = deep.join("servicrab.toml");
        (root, config)
    }

    /// A stand-in for `$XDG_RUNTIME_DIR` that is short enough to hold a socket.
    #[test]
    fn state_lives_next_to_the_config() {
        let dir = a_dir();
        let root = dir.path().canonicalize().expect("canonicalize");
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
        let dir = a_dir();
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
        let dir = a_dir();
        let root = dir.path().canonicalize().expect("canonicalize");
        let runtime = a_runtime_dir();

        let paths = with_runtime_dir(Some(runtime.path()), || {
            DaemonPaths::for_config(&root.join("servicrab.toml"))
        });

        assert_eq!(paths.socket, root.join(".servicrab/daemon.sock"));
        assert!(paths.socket_is_in_place());
        assert_eq!(paths.socket_advice(), "");
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
    /// reports it, with the rejections to explain why.  A shared directory is
    /// never an answer: a name there is predictable to every local user, and
    /// `/tmp` being sticky means a squatted path can be neither unlinked nor
    /// bound.
    #[test]
    fn with_nowhere_private_to_go_the_socket_stays_in_the_project() {
        let shared = a_dir_with_mode(0o777);
        let (_root, config) = a_project_too_deep_for_a_socket();

        let paths = with_env(None, Some(shared.path()), || {
            DaemonPaths::for_config(&config)
        });

        assert!(paths.socket.starts_with(&paths.dir), "{paths:?}");
        assert!(
            !paths.socket.starts_with(shared.path()),
            "the socket must never land in a directory other users can reach"
        );
        // The operator has to be able to act on this, so the message names the
        // candidate and the reason, not just the length.
        let advice = paths.socket_advice();
        assert!(advice.contains("$XDG_RUNTIME_DIR is not set"), "{advice}");
        assert!(advice.contains("0777"), "{advice}");
        assert!(advice.contains("no group or other permissions"), "{advice}");
    }

    /// The point of the predicate: the directory the vulnerable version used is
    /// exactly the kind the new fallback refuses.
    #[test]
    fn the_shared_temp_directory_is_rejected() {
        let shared = Path::new("/tmp");
        if !shared.is_dir() {
            return;
        }
        // Only meaningful where /tmp is the usual world-writable, root-owned
        // directory; a hermetic builder may hand us a private one.
        let meta = std::fs::metadata(shared.canonicalize().expect("resolve /tmp")).expect("stat");
        use std::os::unix::fs::MetadataExt;
        if meta.uid() == nix::unistd::getuid().as_raw() && meta.mode() & 0o077 == 0 {
            return;
        }

        let why = is_private_dir(shared).expect_err("/tmp must not be accepted");
        assert!(
            why.contains("lets other users in") || why.contains("not by you"),
            "{why}"
        );
    }

    #[test]
    fn a_private_directory_is_accepted() {
        let dir = a_dir_with_mode(0o700);

        let resolved = is_private_dir(dir.path()).expect("a 0700 directory we own");

        assert_eq!(resolved, dir.path().canonicalize().expect("canonicalize"));
    }

    /// A relative value would resolve against whatever directory each command
    /// started in, so one project would get several sockets.
    #[test]
    fn a_relative_candidate_is_rejected() {
        let why = is_private_dir(Path::new("relative/run")).expect_err("relative");

        assert!(why.contains("absolute"), "{why}");
    }

    #[test]
    fn a_candidate_that_is_not_a_directory_is_rejected() {
        let dir = a_dir_with_mode(0o700);
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, b"").expect("write");

        let why = is_private_dir(&file).expect_err("a file");

        assert!(why.contains("not a directory"), "{why}");
    }

    #[test]
    fn a_candidate_that_does_not_exist_is_rejected() {
        let dir = a_dir_with_mode(0o700);

        let why = is_private_dir(&dir.path().join("absent")).expect_err("absent");

        assert!(why.contains("cannot be resolved"), "{why}");
    }

    /// Every bit outside the owner's is a way in: group read is enough to
    /// enumerate the sockets, and connecting is all an attacker needs.
    #[test]
    fn any_group_or_other_bit_is_rejected() {
        for mode in [0o777, 0o750, 0o740, 0o701, 0o704, 0o710] {
            let dir = a_dir_with_mode(mode);

            let why = is_private_dir(dir.path())
                .expect_err(&format!("mode {mode:04o} must not be accepted"));

            assert!(why.contains("lets other users in"), "{mode:04o}: {why}");
        }
    }

    /// A symlinked candidate is judged by its target, so pointing `$TMPDIR` at
    /// a world-writable directory does not smuggle one past the check.
    #[test]
    fn a_symlink_to_a_shared_directory_is_rejected() {
        let shared = a_dir_with_mode(0o777);
        let home = a_dir_with_mode(0o700);
        let link = home.path().join("link");
        std::os::unix::fs::symlink(shared.path(), &link).expect("symlink");

        let why = is_private_dir(&link).expect_err("a symlink to a shared directory");

        assert!(why.contains("lets other users in"), "{why}");
    }

    /// The check has to be able to fail on ownership, not only on mode, or a
    /// directory somebody else owns and keeps 0700 would be accepted.
    #[test]
    fn a_directory_owned_by_someone_else_is_rejected() {
        // Root's home is the one directory we can rely on not being ours, and
        // only when we are not root ourselves.
        if nix::unistd::getuid().is_root() {
            return;
        }
        let candidates = ["/var/root", "/root", "/var/db", "/usr"];
        let Some(theirs) = candidates.iter().map(Path::new).find(|path| {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(path)
                .map(|meta| meta.uid() != nix::unistd::getuid().as_raw())
                .unwrap_or(false)
        }) else {
            return;
        };

        let why = is_private_dir(theirs).expect_err("a directory we do not own");

        assert!(
            why.contains("not by you") || why.contains("lets other users in"),
            "{why}"
        );
    }

    /// On a system with no `$XDG_RUNTIME_DIR` but a private per-user temp
    /// directory — macOS as shipped — a deeply nested project still gets a
    /// socket.  Without this it had nowhere to go and its daemon would not
    /// start at all.
    #[test]
    fn a_private_temp_directory_carries_the_socket_when_the_runtime_dir_is_unset() {
        let temp = a_dir_with_mode(0o700);
        let (_root, config) = a_project_too_deep_for_a_socket();

        let paths = with_env(None, Some(temp.path()), || DaemonPaths::for_config(&config));

        assert!(
            paths
                .socket
                .starts_with(temp.path().canonicalize().expect("canonicalize")),
            "{} is not in the temp directory",
            paths.socket.display()
        );
        assert!(paths.socket_rejections.is_empty(), "{paths:?}");
        assert!(!paths.socket_is_in_place());
        // Only the socket moves.
        assert!(paths.pid.starts_with(&paths.dir));
        assert!(paths.log.starts_with(&paths.dir));
        assert!(paths.stopped.starts_with(&paths.dir));
    }

    /// `$XDG_RUNTIME_DIR` is tried first, so a system that provides one keeps
    /// its socket there even though the temp directory would also do.
    #[test]
    fn the_runtime_dir_is_preferred_over_the_temp_directory() {
        let runtime = a_runtime_dir();
        let temp = a_dir_with_mode(0o700);
        let (_root, config) = a_project_too_deep_for_a_socket();

        let paths = with_env(Some(runtime.path()), Some(temp.path()), || {
            DaemonPaths::for_config(&config)
        });

        assert!(paths.socket.starts_with(runtime.path()), "{paths:?}");
    }

    /// A relative `$XDG_RUNTIME_DIR` would resolve against whatever directory
    /// each command started in, so one project would get several sockets.
    #[test]
    fn a_relative_runtime_dir_is_ignored() {
        let temp = a_dir_with_mode(0o700);
        let (_root, config) = a_project_too_deep_for_a_socket();

        let paths = with_env(Some(Path::new("relative/run")), Some(temp.path()), || {
            DaemonPaths::for_config(&config)
        });

        assert!(paths
            .socket
            .starts_with(temp.path().canonicalize().expect("canonicalize")));
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

    /// The constant is a kernel ABI limit, not a preference, so it is worth
    /// pinning against the thing that enforces it.  Being one byte too generous
    /// would turn a relocation into a failed `bind`; being one byte too strict
    /// would relocate a project that did not need it.
    #[test]
    fn the_limit_is_exactly_what_bind_accepts() {
        let dir = a_dir();
        let root = dir.path().canonicalize().expect("canonicalize");
        let room = MAX_SOCKET_PATH - root.as_os_str().len() - 1;

        let longest = root.join("s".repeat(room));
        assert_eq!(longest.as_os_str().len(), MAX_SOCKET_PATH);
        std::os::unix::net::UnixListener::bind(&longest)
            .expect("a path of exactly the limit must bind");

        let over = root.join("s".repeat(room + 1));
        assert_eq!(over.as_os_str().len(), MAX_SOCKET_PATH + 1);
        assert!(
            std::os::unix::net::UnixListener::bind(&over).is_err(),
            "one byte over the limit must not bind"
        );
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
