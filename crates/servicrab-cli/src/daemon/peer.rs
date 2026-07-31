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

/// Whether the peer is the user this daemon runs as.
///
/// Root is not granted an exception: it does not need one — it can read the
/// pidfile, signal the daemon, or become the user — and every accepted uid is
/// one more principal with full authority over the stack.
pub fn is_the_same_user(stream: &impl AsFd) -> Result<bool, String> {
    peer_uid(stream).map(|uid| uid == nix::unistd::getuid().as_raw())
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
        assert!(is_the_same_user(&server).expect("same user"));
        // Both ends can ask, and both see the same process.
        assert!(is_the_same_user(&client).expect("same user"));
    }
}
