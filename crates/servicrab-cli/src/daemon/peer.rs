//! Who is on the other end of a socket connection.
//!
//! The socket's mode is the first line of defence, but it is not the only one
//! that should exist.  A socket can end up somewhere a stranger can reach — a
//! directory an operator loosened, a project shared over a network filesystem
//! that does not honour Unix modes — and an unauthenticated connection is
//! enough to start, stop and restart every service in the project.
//!
//! Asking the kernel who connected costs nothing and does not depend on any
//! filesystem keeping its promises.
//!
//! Root is not granted an exception: it does not need one — it can read the
//! pidfile, signal the daemon, or become the user — and every accepted uid is
//! one more principal with full authority over the stack.

use std::os::unix::io::AsFd;

/// The uid of the process on the other end of `stream`.
///
/// Linux answers `SO_PEERCRED`, the BSDs and macOS `LOCAL_PEERCRED`; nix wraps
/// both, and both report the credentials as they were when the connection was
/// made, so a peer cannot shed them afterwards.
pub fn peer_uid(stream: &impl AsFd) -> Result<u32, String> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};

        getsockopt(stream, PeerCredentials)
            .map(|creds| creds.uid())
            .map_err(|errno| format!("could not read the peer's credentials: {errno}"))
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        use nix::sys::socket::{getsockopt, sockopt::LocalPeerCred};

        getsockopt(stream, LocalPeerCred)
            .map(|cred| cred.uid())
            .map_err(|errno| format!("could not read the peer's credentials: {errno}"))
    }
}

/// What a peer we will not serve is told, naming both uids.
///
/// Refusing in silence is what the first version did, and every client reported
/// it as "the daemon closed the connection without answering" — which is true
/// and gives an operator no way to guess that a uid mismatch was the cause.
/// `sudo servicrab status` against a user's daemon is a mistake the generated
/// systemd unit's `User=` field invites, so it has to explain itself.
///
/// Neither number is a secret: a peer knows its own uid, and the daemon's is
/// readable from the socket's owner.  Nothing else appears here — no path, no
/// pid, no service name — because nothing else is already known.
pub fn wrong_user_message(ours: u32, theirs: u32) -> String {
    format!(
        "this daemon runs as uid {ours}; you are uid {theirs} — \
         servicrab only answers the user that started it"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_own_connection_is_recognised() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let path = dir.path().join("sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");

        let client = std::os::unix::net::UnixStream::connect(&path).expect("connect");
        let (server, _) = listener.accept().expect("accept");

        assert_eq!(
            peer_uid(&server).expect("peer uid"),
            nix::unistd::getuid().as_raw()
        );
        // Both ends can ask, and both see the same process.
        assert_eq!(
            peer_uid(&client).expect("peer uid"),
            nix::unistd::getuid().as_raw()
        );
    }

    /// A cross-uid connection needs two users, which a test suite does not
    /// have, so the message is checked on its own; the daemon's use of it and
    /// the client's pass-through are covered in the integration tests.
    #[test]
    fn the_refusal_names_both_uids() {
        let message = wrong_user_message(501, 0);

        assert!(message.contains("uid 501"), "{message}");
        assert!(message.contains("uid 0"), "{message}");
        // Which is which has to be unambiguous, or the operator cannot tell
        // whose daemon they have reached.
        assert!(
            message.starts_with("this daemon runs as uid 501;"),
            "{message}"
        );
        assert!(message.contains("you are uid 0"), "{message}");
    }

    /// The refusal is sent to somebody we have just decided not to trust, so it
    /// must say nothing beyond the two uids they could already look up.
    #[test]
    fn the_refusal_says_nothing_else() {
        let message = wrong_user_message(501, 0);

        assert!(!message.contains('/'), "no paths: {message}");
        assert!(
            !message.to_ascii_lowercase().contains("pid"),
            "no pids: {message}"
        );
        assert!(!message.contains("servicrab.toml"), "no config: {message}");
    }
}
