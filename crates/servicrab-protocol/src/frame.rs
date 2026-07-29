//! Framing for the daemon socket.
//!
//! Messages are newline-delimited JSON: one value per line, in both
//! directions.  That keeps the transport debuggable with `nc` and needs no
//! length bookkeeping, at the cost of forbidding raw newlines inside a
//! message — which JSON escapes anyway.

use serde::de::DeserializeOwned;
use serde::Serialize;

/// Something went wrong while encoding or decoding a message.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The payload was not valid JSON for the expected type.
    #[error("malformed message: {0}")]
    Malformed(#[from] serde_json::Error),
}

/// Encode a message as a single line, including the trailing newline.
pub fn encode<T: Serialize>(value: &T) -> Result<String, FrameError> {
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    Ok(line)
}

/// Decode a single line produced by [`encode`].
pub fn decode<T: DeserializeOwned>(line: &str) -> Result<T, FrameError> {
    Ok(serde_json::from_str(line.trim_end_matches(['\r', '\n']))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Request, Response};

    #[test]
    fn a_request_survives_a_round_trip() {
        let line = encode(&Request::Status).unwrap();
        assert!(line.ends_with('\n'));
        assert!(!line.trim_end().contains('\n'));
        assert_eq!(decode::<Request>(&line).unwrap(), Request::Status);
    }

    #[test]
    fn a_response_survives_a_round_trip() {
        let response = Response::Pong {
            project: "demo".to_string(),
            pid: 42,
        };
        let line = encode(&response).unwrap();
        assert_eq!(decode::<Response>(&line).unwrap(), response);
    }

    #[test]
    fn carriage_returns_are_tolerated() {
        let line = "{\"type\":\"ping\"}\r\n";
        assert_eq!(decode::<Request>(line).unwrap(), Request::Ping);
    }

    #[test]
    fn garbage_is_reported_not_panicked_on() {
        assert!(decode::<Request>("not json").is_err());
        assert!(decode::<Request>("{\"type\":\"fly\"}").is_err());
    }
}
