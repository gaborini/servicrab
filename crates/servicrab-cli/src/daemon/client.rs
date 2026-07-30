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
    stream
        .write_all(line.as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|e| ClientError::Failed(format!("could not send the request: {e}")))?;

    let mut reader = BufReader::new(&stream);
    let mut first = String::new();
    reader
        .read_line(&mut first)
        .map_err(|e| ClientError::Failed(format!("could not read the response: {e}")))?;
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
