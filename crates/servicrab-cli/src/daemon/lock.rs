//! One daemon per project, enforced by the pidfile.
//!
//! Checking whether the socket answers and then binding it is a
//! time-of-check/time-of-use race: two `servicrab start` invocations can both
//! find no daemon, both unlink the socket — the second one unlinking a *live*
//! socket, leaving its owner listening on an unreachable inode — and both
//! supervise the same stack.  Duplicate processes and duplicate port binds
//! follow.
//!
//! An advisory `flock` on the pidfile closes the window: the kernel hands it to
//! exactly one process, the daemon holds it for its entire life, and it is
//! released by the kernel however the daemon dies — including `SIGKILL`, where
//! no cleanup code of ours would run.

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};

/// How many times to retry when the pidfile is replaced while we are opening
/// it.
///
/// The exiting daemon unlinks the pidfile it holds, so a start that opened the
/// old inode a moment earlier would take a lock nobody else can see.  Checking
/// the inode and retrying is bounded work: each round needs another daemon to
/// exit at exactly the wrong moment.
const ATTEMPTS: usize = 8;

/// Why the project lock could not be taken.
#[derive(Debug)]
pub enum LockError {
    /// Another daemon holds it, which is the whole point.
    Held,
    /// The lock could not be evaluated at all.
    Failed(String),
}

/// An exclusive claim on one project, released when this value is dropped or
/// the process dies.
#[derive(Debug)]
pub struct ProjectLock {
    /// Held only for its `Drop`: it unlocks and closes the pidfile.  The
    /// underscore says the value is never read, only kept alive.
    _file: Flock<File>,
    path: PathBuf,
}

impl ProjectLock {
    /// Take the lock and record our pid in the file.
    ///
    /// The pid is written for humans and for `ps`; nothing in servicrab reads
    /// it, because the lock — not its contents — is what says a daemon is
    /// alive.
    pub fn acquire(path: &Path) -> Result<Self, LockError> {
        for _ in 0..ATTEMPTS {
            let file = OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                // Not truncated on open: we may be looking at a pidfile another
                // daemon still holds, and emptying it before losing the race
                // would erase a live pid.  `write_pid` truncates once the lock
                // is ours.
                .truncate(false)
                // The pid is not a secret, but the lock is authority: only the
                // owner may write the file that grants it.
                .mode(0o600)
                .open(path)
                .map_err(|e| {
                    LockError::Failed(format!("could not open {}: {e}", path.display()))
                })?;

            let mut file = match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
                Ok(locked) => locked,
                Err((_, Errno::EWOULDBLOCK)) => return Err(LockError::Held),
                Err((_, errno)) => {
                    return Err(LockError::Failed(format!(
                        "could not lock {}: {errno}",
                        path.display()
                    )))
                }
            };

            // A lock on an unlinked inode excludes nobody: whoever creates the
            // next pidfile would lock that one and run beside us.
            if !still_at(&file, path) {
                continue;
            }

            write_pid(&mut file).map_err(|e| {
                LockError::Failed(format!("could not write {}: {e}", path.display()))
            })?;

            return Ok(Self {
                _file: file,
                path: path.to_path_buf(),
            });
        }

        Err(LockError::Failed(format!(
            "{} kept being replaced while taking the daemon lock",
            path.display()
        )))
    }
}

impl Drop for ProjectLock {
    fn drop(&mut self) {
        // Unlinking before the lock is released keeps the next start from
        // locking a file we are about to remove.  Our own `drop` body runs
        // before the field that holds the lock is dropped.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Whether the locked file is still the one living at `path`.
fn still_at(file: &File, path: &Path) -> bool {
    let Ok(locked) = file.metadata() else {
        return false;
    };
    let Ok(current) = std::fs::metadata(path) else {
        return false;
    };
    locked.dev() == current.dev() && locked.ino() == current.ino()
}

fn write_pid(file: &mut File) -> std::io::Result<()> {
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(format!("{}\n", std::process::id()).as_bytes())?;
    file.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn the_lock_records_the_pid() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("daemon.pid");

        let lock = ProjectLock::acquire(&path).expect("lock");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            format!("{}\n", std::process::id())
        );

        drop(lock);
        assert!(!path.exists(), "the pidfile outlived the lock");
    }

    #[test]
    fn the_pidfile_is_not_group_readable() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("daemon.pid");
        let _lock = ProjectLock::acquire(&path).expect("lock");

        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "pidfile mode is {mode:o}");
    }

    /// Two locks in one process is the weaker half of the guarantee — `flock`
    /// is per open file description, so this exercises the same code path a
    /// second daemon process takes.  The cross-process half is in
    /// `tests/daemon.rs`.
    #[test]
    fn a_second_lock_on_the_same_path_is_refused() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("daemon.pid");

        let first = ProjectLock::acquire(&path).expect("lock");
        assert!(matches!(ProjectLock::acquire(&path), Err(LockError::Held)));

        drop(first);
        // And the claim is only as long-lived as its holder.
        ProjectLock::acquire(&path).expect("lock after release");
    }
}
