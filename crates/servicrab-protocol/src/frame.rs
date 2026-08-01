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

/// How much of a `type` tag [`tag`] will hand back.
///
/// Every tag either side of this protocol will ever use is a word.  The bound
/// exists because the caller of `tag` is quoting an unrecognised one back to
/// whoever sent it, and a line may be tens of kilobytes: without it, a peer
/// could make a refusal carry its own payload back out again.
///
/// Public because it is the promise `tag` makes about its result, and a caller
/// sizing a message around that result needs the number rather than a guess.
pub const MAX_TAG: usize = 48;

/// The `type` a line claims to be, when that is all a caller needs from it.
///
/// [`decode`] answers what a line *means*, and for a message from a later
/// release the answer is an `Unknown` fallback — which by construction cannot
/// carry the tag, because `#[serde(other)]` only applies to a unit variant.  So
/// the tag has to come back out of the line, and this is the one place that
/// knows how: the same trimming rule as `decode`, `type` because that is the
/// discriminant both directions are tagged by, and a string because a tag that
/// is not one could never have named a variant.
///
/// Truncated to [`MAX_TAG`] characters — characters rather than bytes, so a
/// multi-byte tag is cut where it can still be printed.  `None` for a line that
/// is not a JSON object, or has no string `type`.
pub fn tag(line: &str) -> Option<String> {
    let value: serde_json::Value =
        serde_json::from_str(line.trim_end_matches(['\r', '\n'])).ok()?;
    let named = value.get("type")?.as_str()?;
    Some(named.chars().take(MAX_TAG).collect())
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
            version: Some(crate::PROTOCOL_VERSION),
        };
        let line = encode(&response).unwrap();
        assert_eq!(decode::<Response>(&line).unwrap(), response);
    }

    #[test]
    fn carriage_returns_are_tolerated() {
        let line = "{\"type\":\"ping\"}\r\n";
        assert_eq!(
            decode::<Request>(line).unwrap(),
            Request::Ping { version: None }
        );
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
    fn a_dropped_line_notice_survives_a_round_trip() {
        let response = Response::Event {
            service: "api".to_string(),
            event: crate::Event::LogLinesDropped { count: 42 },
        };
        let line = encode(&response).unwrap();
        assert!(line.contains("\"kind\":\"log_lines_dropped\""), "{line}");
        assert!(line.contains("\"count\":42"), "{line}");
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

    /// The tag has to come back out of the line because the decoded value
    /// cannot carry it: `#[serde(other)]` only applies to a unit variant, so
    /// `Request::Unknown` is all the daemon has to go on when it wants to quote
    /// an unrecognised request back to whoever sent it.
    #[test]
    fn the_type_of_a_line_can_be_recovered_after_it_decodes_to_the_fallback() {
        assert_eq!(
            tag("{\"type\":\"strat\",\"name\":\"api\"}"),
            Some("strat".into())
        );
        // The same trimming rule as `decode`, or a tag would come back with the
        // newline still attached to it.
        assert_eq!(tag("{\"type\":\"drain\"}\r\n"), Some("drain".into()));
        assert_eq!(tag("{\"type\":\"ping\"}"), Some("ping".into()));
    }

    /// A caller quotes the tag back to the peer that sent it, so a line with
    /// nothing usable in it has to be answerable without one.
    #[test]
    fn a_line_with_no_usable_type_has_no_tag() {
        for line in [
            "not json",
            "[1,2,3]",
            "{}",
            "{\"type\":7}",
            "{\"type\":null}",
            "{\"kind\":\"log\"}",
        ] {
            assert_eq!(tag(line), None, "{line:?}");
        }
    }

    /// The refusal that quotes a tag is written back down the same socket the
    /// tag arrived on, and a line may be tens of kilobytes.  Without a bound, a
    /// peer could use the refusal to carry its own payload back out again.
    #[test]
    fn an_absurd_tag_is_truncated_rather_than_quoted_in_full() {
        let huge = "x".repeat(4096);
        let line = format!("{{\"type\":\"{huge}\"}}");

        let recovered = tag(&line).expect("a long tag is still a tag");

        assert_eq!(recovered.len(), MAX_TAG);
        // Cut by characters, not bytes, so a multi-byte tag stays printable.
        let wide = "\u{1f980}".repeat(200);
        let recovered = tag(&format!("{{\"type\":\"{wide}\"}}")).expect("a wide tag");
        assert_eq!(recovered.chars().count(), MAX_TAG);
    }

    #[test]
    fn garbage_is_reported_not_panicked_on() {
        assert!(decode::<Request>("not json").is_err());
        assert!(decode::<Request>("[1,2,3]").is_err());
        assert!(decode::<Request>("{}").is_err());
    }

    /// The one that used to assert the opposite.
    ///
    /// `#[non_exhaustive]` is a promise to downstream *crates*, and serde's
    /// internally tagged representation rejects an unrecognised tag outright, so
    /// a newer client's request used to come back as `malformed message: unknown
    /// variant …` — and the daemon's own "an older daemon can still be asked
    /// something it does not know" arm was unreachable from a socket.
    #[test]
    fn an_unknown_request_decodes_instead_of_failing() {
        assert_eq!(
            decode::<Request>("{\"type\":\"fly\"}").unwrap(),
            Request::Unknown
        );
        // Whatever it carried, too: a future request will not be a bare tag.
        assert_eq!(
            decode::<Request>("{\"type\":\"fly\",\"altitude\":9000}").unwrap(),
            Request::Unknown
        );
    }

    /// A `ping` is the one exchange both sides can rely on across versions, so
    /// it is where the version goes — and it has to survive both directions of
    /// skew, because 0.3 sends no number and will be answered by daemons that
    /// do.
    #[test]
    fn a_version_travels_with_a_ping_but_is_not_required() {
        let line = encode(&Request::ping()).unwrap();
        assert!(line.contains("\"version\":1"), "{line}");

        assert_eq!(
            decode::<Request>("{\"type\":\"ping\"}").unwrap(),
            Request::Ping { version: None },
            "a 0.3 client sends no version and must still be understood"
        );
        assert_eq!(
            decode::<Request>("{\"type\":\"ping\",\"version\":7}").unwrap(),
            Request::Ping { version: Some(7) },
            "a newer client's version must arrive intact, not be rounded down"
        );
    }

    /// The same for the answer: a 0.3 daemon's `pong` has no version, and a
    /// client that treated silence as a mismatch would refuse to talk to one.
    #[test]
    fn a_pong_without_a_version_is_still_a_pong() {
        let reply =
            decode::<Response>("{\"type\":\"pong\",\"project\":\"demo\",\"pid\":7}").unwrap();

        assert_eq!(
            reply,
            Response::Pong {
                project: "demo".to_string(),
                pid: 7,
                version: None,
            }
        );
    }

    /// The defect this exists to prevent: one line a client cannot name used to
    /// end the whole event stream, so a single new event kind in 1.1 would have
    /// killed `servicrab events` on every 1.0 client, mid-run.
    #[test]
    fn an_unknown_event_kind_decodes_instead_of_ending_the_stream() {
        let line = r#"{"type":"event","service":"api","event":{"kind":"teleported","to":"mars"}}"#;

        let response =
            decode::<Response>(line).expect("an unknown event kind is not a broken line");

        assert_eq!(
            response,
            Response::Event {
                service: "api".to_string(),
                event: crate::Event::Unknown,
            },
            "the service still has to survive, or a client cannot even say who the event was about"
        );
    }

    #[test]
    fn an_unknown_response_type_decodes_instead_of_ending_the_stream() {
        assert_eq!(
            decode::<Response>("{\"type\":\"weather\",\"outlook\":\"grim\"}").unwrap(),
            Response::Unknown
        );
    }

    /// A status snapshot is a `pong`'s worth of forward compatibility spread
    /// over every service: one state or health verdict this build cannot name
    /// used to fail the whole `Response::Status`, so `status`, `start --wait`
    /// and `down` all reported a parse error rather than what they were told.
    #[test]
    fn an_unknown_state_or_health_leaves_the_rest_of_a_status_readable() {
        let line = r#"{"type":"status","services":[
            {"name":"api","state":"hibernating","restarts":0,"health":"degraded"},
            {"name":"db","state":"running","restarts":2,"health":"healthy"}
        ]}"#;

        let Response::Status { services } =
            decode::<Response>(line).expect("one unknown state must not fail the snapshot")
        else {
            panic!("expected a status");
        };

        assert_eq!(services[0].state, crate::ServiceState::Unknown);
        assert_eq!(services[0].health, crate::Health::Unknown);
        // The point of decoding it at all: everything else is still there.
        assert_eq!(services[1].name, "db");
        assert_eq!(services[1].state, crate::ServiceState::Running);
        assert_eq!(services[1].restarts, 2);
    }

    #[test]
    fn an_unknown_log_stream_still_carries_its_line() {
        let line = r#"{"type":"event","service":"api","event":{"kind":"log","stream":"trace","line":"boom"}}"#;

        let response = decode::<Response>(line).expect("an unknown stream is not a broken line");

        assert_eq!(
            response,
            Response::Event {
                service: "api".to_string(),
                event: crate::Event::Log {
                    stream: crate::Stream::Unknown,
                    line: "boom".to_string(),
                },
            }
        );
    }

    /// `unknown` is a reserved word in the wire format as a consequence of the
    /// fallbacks: a future release that named a real variant `unknown` would be
    /// silently classified as "not understood" by every earlier client, which is
    /// exactly the silence these fallbacks were added to remove.
    #[test]
    fn the_fallback_tag_round_trips_so_it_cannot_be_reused() {
        for line in [
            encode(&Response::Unknown).unwrap(),
            encode(&Request::Unknown).unwrap(),
        ] {
            assert!(line.contains(crate::UNKNOWN), "{line}");
        }
        assert_eq!(
            decode::<Response>(&format!("{{\"type\":\"{}\"}}", crate::UNKNOWN)).unwrap(),
            Response::Unknown
        );
    }
}
