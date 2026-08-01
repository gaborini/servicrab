//! Request types sent from the CLI (or other clients) to the daemon.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// How many services one `subscribe` may name.
///
/// The filter is consulted for every event and every subscriber, and the list
/// arrives from the socket before anything has authenticated it, so it needs a
/// bound.  A project with more services than this exists, but a subscriber that
/// wants that many is asking for all of them — which an empty list already says,
/// at no cost.
pub const MAX_SUBSCRIBE_SERVICES: usize = 256;

/// A request that a client sends to the servicrab daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Request {
    /// Ask the daemon whether it is alive.
    Ping {
        /// Which revision of this wire format the client speaks.
        ///
        /// Optional because a 0.3 client does not send it, and the daemon has to
        /// keep answering those: absent means "did not say", not "version 0".
        /// A daemon that hears a number below its own says so in its log, which
        /// is the one place an operator looking at a version-skew problem will
        /// already be.  See [`crate::PROTOCOL_VERSION`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<u32>,
    },

    /// Ask the daemon for the current state of every service.
    Status,

    /// Ask the daemon to stop the whole stack and exit.
    Shutdown,

    /// Start one service that is currently stopped.
    StartService {
        /// Service name as declared in `servicrab.toml`.
        name: String,
    },

    /// Stop one service, leaving the rest of the stack alone.
    StopService {
        /// Service name as declared in `servicrab.toml`.
        name: String,
    },

    /// Stop one service and start it again.
    RestartService {
        /// Service name as declared in `servicrab.toml`.
        name: String,
    },

    /// Re-read the configuration file and apply the difference.
    Reload,

    /// Follow the daemon's event stream.
    ///
    /// The daemon answers `ok` and then keeps writing
    /// [`crate::Response::Event`] lines until the client disconnects, so this
    /// is the only request that turns a connection one-way.
    Subscribe {
        /// Only report these services; empty means all of them.
        ///
        /// A set rather than a list: the daemon has to answer "is this service
        /// wanted?" for every event and every subscriber, and duplicates in the
        /// request would otherwise be paid for on each one.  Truncated to
        /// [`MAX_SUBSCRIBE_SERVICES`] names on decoding, because the request
        /// arrives before anything has vouched for it.
        #[serde(
            default,
            deserialize_with = "bounded_services",
            skip_serializing_if = "BTreeSet::is_empty"
        )]
        services: BTreeSet<String>,

        /// Whether captured stdout/stderr lines are part of the stream.
        #[serde(default = "yes")]
        logs: bool,
    },

    /// A request this build has no name for, because a newer client sent it.
    ///
    /// Without this the daemon answered `malformed message: unknown variant …`
    /// and counted the line against the connection, so the wildcard arm in its
    /// dispatcher — the one whose comment promised that "an older daemon can
    /// still be asked something it does not know about" — could not be reached
    /// from a socket at all.  A newer client now hears that this daemon does
    /// not support the request, which is the difference between "upgrade the
    /// daemon" and "your client is broken".
    ///
    /// It still costs the connection a strike: a request nobody can act on is
    /// as good a sign of a broken or probing client as a malformed line, and
    /// three of them is enough courtesy either way.  The cost of dropping that
    /// is a connection that can be talked to forever for free.
    #[serde(other)]
    Unknown,
}

impl Request {
    /// The `type` tags this build understands, in the order the README
    /// documents them.
    ///
    /// It exists because the tag is the one thing a refusal cannot recover from
    /// the decoded value: `#[serde(other)]` needs a unit variant, so
    /// [`Request::Unknown`] carries nothing.  Quoting the tag back tells a
    /// client author about their typo; listing these tells them what to write
    /// instead, which is what serde's own `expected one of …` used to do for
    /// free before an unrecognised request stopped being an error.
    ///
    /// A list beside an enum is a list that can go stale, so it is not trusted
    /// on its word: `every_supported_tag_decodes_to_a_real_request` proves each
    /// entry still names a variant, and `every_variant_is_listed_as_supported`
    /// fails to compile if a variant is added without being named here.
    pub const SUPPORTED: &'static [&'static str] = &[
        "ping",
        "status",
        "shutdown",
        "start_service",
        "stop_service",
        "restart_service",
        "reload",
        "subscribe",
    ];

    /// A ping that says which wire format this build speaks.
    ///
    /// Every caller wants the same thing, and one that spelled the field out
    /// would be the one that forgot to update it.
    pub fn ping() -> Self {
        Request::Ping {
            version: Some(crate::PROTOCOL_VERSION),
        }
    }
}

/// Deserialize a subscribe filter, keeping at most [`MAX_SUBSCRIBE_SERVICES`]
/// names.
///
/// Truncating rather than rejecting: a client that names too many services
/// wants a stream, and the extra names are indistinguishable from asking for
/// everything.  The daemon logs it so the surprise is not silent.
fn bounded_services<'de, D>(deserializer: D) -> Result<BTreeSet<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let mut names = BTreeSet::<String>::deserialize(deserializer)?;
    while names.len() > MAX_SUBSCRIBE_SERVICES {
        names.pop_last();
    }
    Ok(names)
}

/// Serde default for flags that are on unless a client says otherwise.
fn yes() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_service_names_collapse() {
        let request: Request = serde_json::from_str(
            r#"{"type":"subscribe","services":["api","api","db"],"logs":true}"#,
        )
        .expect("valid subscribe");

        let Request::Subscribe { services, .. } = request else {
            panic!("expected a subscribe");
        };
        assert_eq!(services.len(), 2);
    }

    /// Half of what keeps [`Request::SUPPORTED`] honest: every entry still has
    /// to name a variant this build handles, so a renamed or removed request
    /// cannot leave a tag behind that the daemon then advertises.
    #[test]
    fn every_supported_tag_decodes_to_a_real_request() {
        for tag in Request::SUPPORTED {
            // Every request is either bare or takes a `name`; handing over one
            // that is not wanted is ignored, which keeps this table-driven.
            let line = format!("{{\"type\":\"{tag}\",\"name\":\"api\"}}");

            let request: Request = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("{tag:?} is listed as supported but {e}"));

            assert_ne!(
                request,
                Request::Unknown,
                "{tag:?} is listed as supported but decodes to the fallback"
            );
        }
    }

    /// The other half, and the one that cannot be forgotten: this match has no
    /// wildcard, so adding a variant to [`Request`] stops the crate compiling
    /// until it is named — and the arm you are made to write is right next to
    /// the assertion that it belongs in [`Request::SUPPORTED`].
    ///
    /// `#[non_exhaustive]` is what would otherwise make this impossible, and it
    /// does not apply inside the crate that declares the enum.
    #[test]
    fn every_variant_is_listed_as_supported() {
        fn tag_of(request: &Request) -> Option<&'static str> {
            match request {
                Request::Ping { .. } => Some("ping"),
                Request::Status => Some("status"),
                Request::Shutdown => Some("shutdown"),
                Request::StartService { .. } => Some("start_service"),
                Request::StopService { .. } => Some("stop_service"),
                Request::RestartService { .. } => Some("restart_service"),
                Request::Reload => Some("reload"),
                Request::Subscribe { .. } => Some("subscribe"),
                // The fallback is not a request anyone may send, so it is the
                // one variant that must never be advertised.
                Request::Unknown => None,
            }
        }

        let every_variant = [
            Request::ping(),
            Request::Status,
            Request::Shutdown,
            Request::StartService {
                name: "api".to_string(),
            },
            Request::StopService {
                name: "api".to_string(),
            },
            Request::RestartService {
                name: "api".to_string(),
            },
            Request::Reload,
            Request::Subscribe {
                services: BTreeSet::new(),
                logs: true,
            },
            Request::Unknown,
        ];

        let advertised: Vec<&str> = every_variant.iter().filter_map(tag_of).collect();

        assert_eq!(advertised, Request::SUPPORTED);
        assert!(!Request::SUPPORTED.contains(&crate::UNKNOWN));
    }

    /// The list arrives from the socket before anything has vouched for it, and
    /// the filter is consulted for every event and every subscriber.
    #[test]
    fn an_oversized_service_list_is_truncated() {
        let names: Vec<String> = (0..MAX_SUBSCRIBE_SERVICES * 3)
            .map(|n| format!("svc-{n:06}"))
            .collect();
        let payload = serde_json::json!({ "type": "subscribe", "services": names });

        let request: Request =
            serde_json::from_value(payload).expect("an oversized list is accepted, not rejected");

        let Request::Subscribe { services, .. } = request else {
            panic!("expected a subscribe");
        };
        assert_eq!(services.len(), MAX_SUBSCRIBE_SERVICES);
        // The names that survive are the first ones in order, so the truncation
        // is at least predictable.
        assert!(services.contains("svc-000000"));
    }
}
