//! Talking to a running daemon from the CLI.
//!
//! The client side is deliberately synchronous: every command sends one
//! request and reads one response, so there is nothing an async runtime would
//! buy here.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use servicrab_protocol::{decode, encode, Request, Response};

/// How long a single request may take before it is considered lost.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Why talking to the daemon failed.
#[derive(Debug)]
pub enum ClientError {
    /// No daemon is listening on the socket.
    NotRunning,
    /// The daemon is there but the exchange went wrong.
    Failed(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::NotRunning => f.write_str("no daemon is running for this project"),
            ClientError::Failed(message) => f.write_str(message),
        }
    }
}

/// Send one request and read the response.
pub fn send(socket: &Path, request: &Request) -> Result<Response, ClientError> {
    let mut stream = connect(socket)?;

    let _ = stream.set_read_timeout(Some(TIMEOUT));
    let _ = stream.set_write_timeout(Some(TIMEOUT));

    let line = encode(request).map_err(|e| ClientError::Failed(e.to_string()))?;
    // A daemon that refuses us answers and closes without reading, so our write
    // can fail with EPIPE while the reason is already waiting in the socket.
    // Reading first and only reporting the write error if nothing came back
    // keeps that reason instead of replacing it with "broken pipe".
    let sent = stream
        .write_all(line.as_bytes())
        .and_then(|()| stream.flush());

    let mut reply = String::new();
    let read = BufReader::new(&stream).read_line(&mut reply);
    if reply.trim().is_empty() {
        sent.map_err(|e| ClientError::Failed(format!("could not send the request: {e}")))?;
        read.map_err(|e| ClientError::Failed(format!("could not read the response: {e}")))?;
        return Err(ClientError::Failed(
            "the daemon closed the connection without answering".to_string(),
        ));
    }

    decode(&reply).map_err(|e| ClientError::Failed(e.to_string()))
}

/// Send a subscribe request and hand every streamed response to `on_event`.
///
/// The stream has no timeout: it lasts until the daemon exits, the callback
/// returns `false`, or the user interrupts the command.
pub fn subscribe<F>(socket: &Path, request: &Request, mut on_event: F) -> Result<(), ClientError>
where
    F: FnMut(&str, Response) -> bool,
{
    let mut stream = connect(socket)?;
    let _ = stream.set_write_timeout(Some(TIMEOUT));

    let line = encode(request).map_err(|e| ClientError::Failed(e.to_string()))?;
    // Same reasoning as in `send`: a refusal is written and the connection
    // closed without reading, so the write can fail with the answer already in
    // flight.
    let sent = stream
        .write_all(line.as_bytes())
        .and_then(|()| stream.flush());

    let mut reader = BufReader::new(&stream);
    let mut first = String::new();
    let read = reader.read_line(&mut first);
    if first.trim().is_empty() {
        sent.map_err(|e| ClientError::Failed(format!("could not send the request: {e}")))?;
        read.map_err(|e| ClientError::Failed(format!("could not read the response: {e}")))?;
        return Err(ClientError::Failed(
            "the daemon closed the connection without answering".to_string(),
        ));
    }
    match decode::<Response>(&first) {
        Ok(Response::Ok { .. }) => {}
        Ok(Response::Error { message }) => return Err(ClientError::Failed(message)),
        Ok(other) => {
            return Err(ClientError::Failed(format!(
                "unexpected response from the daemon: {other:?}"
            )))
        }
        Err(err) => return Err(ClientError::Failed(err.to_string())),
    }

    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            // The daemon closed the socket, which is how a shutdown looks.
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(err) => {
                return Err(ClientError::Failed(format!(
                    "the event stream broke: {err}"
                )))
            }
        }
        if line.trim().is_empty() {
            continue;
        }
        let response = decode::<Response>(&line).map_err(|e| ClientError::Failed(e.to_string()))?;
        if !on_event(line.trim_end(), response) {
            return Ok(());
        }
    }
}

/// Open the socket, mapping "nothing is listening" to [`ClientError::NotRunning`].
fn connect(socket: &Path) -> Result<UnixStream, ClientError> {
    UnixStream::connect(socket).map_err(|err| match err.kind() {
        // A socket file left behind by a crashed daemon refuses connections.
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            ClientError::NotRunning
        }
        _ => ClientError::Failed(format!("could not connect to {}: {err}", socket.display())),
    })
}

/// Whether a daemon is listening and answering.
pub fn is_running(socket: &Path) -> bool {
    matches!(send(socket, &Request::Ping), Ok(Response::Pong { .. }))
}

/// Ask whether a daemon is there, keeping the reason when it will not talk to
/// us.
///
/// [`is_running`] answers yes or no, which is all most callers want — but a
/// daemon that refuses the connection because it belongs to another user is
/// neither. Reported as "no daemon is running" it sends the operator looking for
/// a daemon that is in fact right there, so the refusal has to survive the trip.
///
/// A `ping` can only be answered with an error for a reason the operator needs
/// to hear, so every error is passed on as it stands.
pub fn check_running(socket: &Path) -> Result<(), ClientError> {
    match send(socket, &Request::Ping) {
        Ok(Response::Pong { .. }) => Ok(()),
        Ok(Response::Error { message }) => Err(ClientError::Failed(message)),
        Ok(other) => Err(ClientError::Failed(format!(
            "unexpected response from the daemon: {other:?}"
        ))),
        Err(err) => Err(err),
    }
}

/// Poll until the daemon stops answering, or `timeout` elapses.
pub fn wait_until_stopped(socket: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !is_running(socket) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stand in for a daemon that refuses a peer: one error line, then close.
    fn a_refusing_daemon(message: &'static str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let socket = dir.path().join("daemon.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");

        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let line = encode(&Response::Error {
                message: message.to_string(),
            })
            .expect("encode");
            let _ = stream.write_all(line.as_bytes());
            let _ = stream.flush();
        });

        (dir, socket)
    }

    /// The whole point of the refusal is that the operator reads it, so it has
    /// to survive the trip through the client.  This is the half of the peer
    /// check a single-uid test suite can prove: the daemon's own decision needs
    /// two users, but "the message is not swallowed" does not.
    #[test]
    fn a_refusal_reaches_the_caller() {
        let (_dir, socket) = a_refusing_daemon("this daemon runs as uid 501; you are uid 0");

        let why = check_running(&socket).expect_err("a refusing daemon is not a running one");

        // Not `NotRunning`: there *is* a daemon, and saying otherwise would
        // send the operator looking for one to start.
        assert!(matches!(why, ClientError::Failed(_)), "{why:?}");
        assert!(why.to_string().contains("uid 501"), "{why}");
        assert!(why.to_string().contains("uid 0"), "{why}");
    }

    /// A daemon that refuses us answers and closes without reading, so our own
    /// write can fail with `EPIPE` while the reason is already in the socket.
    /// Reporting the write error would replace an actionable message with
    /// "broken pipe".
    #[test]
    fn a_refusal_survives_a_broken_pipe_on_the_way_out() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let socket = dir.path().join("daemon.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");

        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let line = encode(&Response::Error {
                message: "this daemon runs as uid 501; you are uid 0".to_string(),
            })
            .expect("encode");
            let _ = stream.write_all(line.as_bytes());
            let _ = stream.flush();
            // Closed without ever reading, which is what makes our write fail.
            let _ = stream.shutdown(std::net::Shutdown::Both);
        });

        let why = check_running(&socket).expect_err("a refusing daemon");

        assert!(why.to_string().contains("uid 501"), "{why}");
        // Not the write error: "broken pipe" is true and useless.
        assert!(!why.to_string().contains("Broken pipe"), "{why}");
    }

    /// A refused connection is not the same as an absent daemon, but
    /// [`is_running`] has only two answers and "no" is the safe one for the
    /// callers that just want to know whether to keep going.
    #[test]
    fn a_refusing_daemon_does_not_count_as_running() {
        let (_dir, socket) = a_refusing_daemon("refused");

        assert!(!is_running(&socket));
    }

    #[test]
    fn an_absent_socket_is_simply_not_running() {
        let dir = tempfile::TempDir::new().expect("temp dir");

        let why =
            check_running(&dir.path().join("absent.sock")).expect_err("nothing is listening there");

        assert!(matches!(why, ClientError::NotRunning), "{why:?}");
    }
}
