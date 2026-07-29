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
    let mut stream = UnixStream::connect(socket).map_err(|err| match err.kind() {
        // A socket file left behind by a crashed daemon refuses connections.
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            ClientError::NotRunning
        }
        _ => ClientError::Failed(format!("could not connect to {}: {err}", socket.display())),
    })?;

    let _ = stream.set_read_timeout(Some(TIMEOUT));
    let _ = stream.set_write_timeout(Some(TIMEOUT));

    let line = encode(request).map_err(|e| ClientError::Failed(e.to_string()))?;
    stream
        .write_all(line.as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|e| ClientError::Failed(format!("could not send the request: {e}")))?;

    let mut reply = String::new();
    BufReader::new(&stream)
        .read_line(&mut reply)
        .map_err(|e| ClientError::Failed(format!("could not read the response: {e}")))?;
    if reply.trim().is_empty() {
        return Err(ClientError::Failed(
            "the daemon closed the connection without answering".to_string(),
        ));
    }

    decode(&reply).map_err(|e| ClientError::Failed(e.to_string()))
}

/// Whether a daemon is listening and answering.
pub fn is_running(socket: &Path) -> bool {
    matches!(send(socket, &Request::Ping), Ok(Response::Pong { .. }))
}

/// Poll until the daemon answers, or `timeout` elapses.
pub fn wait_until_running(socket: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if is_running(socket) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
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
