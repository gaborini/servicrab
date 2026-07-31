//! Where the daemon keeps its socket, pidfile and log.
//!
//! Everything lives next to the config file so that two projects never fight
//! over one daemon, and so that removing the project directory removes its
//! runtime state with it.

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
    /// A deeply nested project would overflow the socket path limit, so in
    /// that case the socket (and only the socket) moves to the temp dir under
    /// a name derived from the config path.
    pub fn for_config(config: &Path) -> Self {
        let dir = config
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(".servicrab");
        let socket = dir.join("daemon.sock");
        let socket = if socket.as_os_str().len() > MAX_SOCKET_PATH {
            std::env::temp_dir().join(format!("servicrab-{}.sock", digest(config)))
        } else {
            socket
        };

        Self {
            socket,
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

/// A short, stable, filesystem-safe digest of a path.
fn digest(path: &Path) -> String {
    // FNV-1a: tiny, dependency-free, and a collision only costs a confusing
    // "address already in use" for two projects with the same hash.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.as_os_str().to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_lives_next_to_the_config() {
        let paths = DaemonPaths::for_config(Path::new("/srv/app/servicrab.toml"));
        assert_eq!(paths.dir, PathBuf::from("/srv/app/.servicrab"));
        assert_eq!(
            paths.socket,
            PathBuf::from("/srv/app/.servicrab/daemon.sock")
        );
        assert_eq!(paths.pid, PathBuf::from("/srv/app/.servicrab/daemon.pid"));
    }

    #[test]
    fn a_very_long_path_moves_the_socket_to_the_temp_dir() {
        let deep = PathBuf::from("/".to_string() + &"nested/".repeat(30)).join("servicrab.toml");
        let paths = DaemonPaths::for_config(&deep);

        assert!(paths.dir.ends_with(".servicrab"));
        assert!(paths.socket.starts_with(std::env::temp_dir()));
    }

    #[test]
    fn different_projects_get_different_sockets() {
        let one = PathBuf::from("/".to_string() + &"nested/".repeat(30)).join("a.toml");
        let two = PathBuf::from("/".to_string() + &"nested/".repeat(30)).join("b.toml");
        assert_ne!(
            DaemonPaths::for_config(&one).socket,
            DaemonPaths::for_config(&two).socket
        );
    }
}
