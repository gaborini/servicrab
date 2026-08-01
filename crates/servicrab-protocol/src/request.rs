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
