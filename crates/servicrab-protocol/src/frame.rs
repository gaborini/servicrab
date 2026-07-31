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
    fn subscribe_defaults_to_every_service_and_logs() {
        let request: Request = decode("{\"type\":\"subscribe\"}\n").unwrap();
        assert_eq!(
            request,
            Request::Subscribe {
                services: std::collections::BTreeSet::new(),
                logs: true,
            }
        );
    }

    #[test]
    fn a_subscribe_request_survives_a_round_trip() {
        let request = Request::Subscribe {
            services: ["api".to_string()].into_iter().collect(),
            logs: false,
        };
        let line = encode(&request).unwrap();
        assert!(line.contains("\"services\":[\"api\"]"));
        assert_eq!(decode::<Request>(&line).unwrap(), request);
    }

    #[test]
    fn a_streamed_event_survives_a_round_trip() {
        let response = Response::Event {
            service: "api".to_string(),
            event: crate::Event::Log {
                stream: crate::Stream::Stderr,
                line: "boom".to_string(),
            },
        };
        let line = encode(&response).unwrap();
        assert!(line.contains("\"kind\":\"log\""));
        assert_eq!(decode::<Response>(&line).unwrap(), response);
    }

    #[test]
    fn event_payloads_omit_absent_exit_details() {
        let response = Response::Event {
            service: "api".to_string(),
            event: crate::Event::Exited {
                reason: "exited with code 0".to_string(),
                code: Some(0),
                signal: None,
                uptime_ms: 1234,
            },
        };
        let line = encode(&response).unwrap();
        assert!(line.contains("\"code\":0"));
        assert!(!line.contains("signal"));
        assert_eq!(decode::<Response>(&line).unwrap(), response);
    }

    #[test]
    fn a_lag_notice_survives_a_round_trip() {
        let response = Response::Lagged { skipped: 7 };
        let line = encode(&response).unwrap();
        assert!(line.contains("\"type\":\"lagged\""));
        assert_eq!(decode::<Response>(&line).unwrap(), response);
    }

    #[test]
    fn garbage_is_reported_not_panicked_on() {
        assert!(decode::<Request>("not json").is_err());
        assert!(decode::<Request>("{\"type\":\"fly\"}").is_err());
    }
}
