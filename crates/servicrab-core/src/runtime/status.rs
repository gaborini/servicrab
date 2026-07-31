//! A live view of what every supervised service is doing.
//!
//! The runtime publishes events; this module folds them into a snapshot that
//! the daemon can hand out over its socket. It is deliberately passive: it
//! never touches processes and never blocks, so a slow client can never stall
//! supervision.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::config::ServiceName;
use crate::lifecycle::ServiceState;
use crate::runtime::event::{EventKind, ServiceEvent};

/// What the health checks say about a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// The service declares no health check.
    None,
    /// A health check exists but has not passed yet.
    Starting,
    /// The last probe succeeded.
    Healthy,
    /// The service exhausted its retry budget.
    Unhealthy,
}

/// A point-in-time report about one service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceStatus {
    /// The service this report is about.
    pub name: ServiceName,
    /// Current lifecycle state.
    pub state: ServiceState,
    /// Process-group id, while a process is running.
    pub pid: Option<i32>,
    /// How long the current process has been up.
    pub uptime: Option<Duration>,
    /// How many times the service has been restarted.
    pub restarts: u32,
    /// Health-check verdict.
    pub health: Health,
    /// The most recent noteworthy message.
    pub message: Option<String>,
}

/// Folds the runtime event stream into per-service status.
#[derive(Debug)]
pub struct StatusRegistry {
    services: BTreeMap<ServiceName, Entry>,
    order: Vec<ServiceName>,
}

#[derive(Debug)]
struct Entry {
    state: ServiceState,
    pid: Option<i32>,
    started_at: Option<Instant>,
    restarts: u32,
    health: Health,
    message: Option<String>,
}

impl Entry {
    fn new(has_health: bool) -> Self {
        Self {
            state: ServiceState::Pending,
            pid: None,
            started_at: None,
            restarts: 0,
            health: if has_health {
                Health::Starting
            } else {
                Health::None
            },
            message: None,
        }
    }
}

impl StatusRegistry {
    /// Build a registry for the given services, in the order they will be
    /// reported. `has_health` marks the ones that declare a health check.
    pub fn new(services: impl IntoIterator<Item = (ServiceName, bool)>) -> Self {
        let mut order = Vec::new();
        let mut map = BTreeMap::new();
        for (name, has_health) in services {
            order.push(name.clone());
            map.insert(name, Entry::new(has_health));
        }
        Self {
            services: map,
            order,
        }
    }

    /// Bring the registry in line with a reloaded configuration.
    ///
    /// Services that are still present keep their state, so a reload does not
    /// reset uptime or restart counters for anything it did not touch.
    pub fn sync(&mut self, services: impl IntoIterator<Item = (ServiceName, bool)>) {
        let mut order = Vec::new();
        for (name, has_health) in services {
            self.services
                .entry(name.clone())
                .or_insert_with(|| Entry::new(has_health));
            order.push(name);
        }
        self.services.retain(|name, _| order.contains(name));
        self.order = order;
    }

    /// Fold one event into the registry.
    pub fn apply(&mut self, event: &ServiceEvent) {
        let Some(entry) = self.services.get_mut(&event.service) else {
            return;
        };

        match &event.kind {
            EventKind::State(state) => {
                entry.state = *state;
                if !matches!(state, ServiceState::Running) {
                    entry.pid = None;
                    entry.started_at = None;
                }
                // A service that is starting again has not proven itself yet,
                // and whatever ended its previous run is now history.
                if matches!(state, ServiceState::Starting) {
                    entry.message = None;
                    if entry.health != Health::None {
                        entry.health = Health::Starting;
                    }
                }
            }
            EventKind::Started { pgid } => {
                entry.pid = Some(*pgid);
                entry.started_at = Some(Instant::now());
            }
            EventKind::Backoff { attempt, .. } => {
                entry.restarts = *attempt;
            }
            EventKind::Skipped { dependency } => {
                entry.message = Some(format!("skipped: {dependency} never became available"));
            }
            EventKind::Healthy => entry.health = Health::Healthy,
            EventKind::Unhealthy { message } => {
                entry.health = Health::Unhealthy;
                entry.message = Some(format!("unhealthy: {message}"));
            }
            EventKind::HealthProbeFailed {
                message,
                consecutive,
                retries,
            } => {
                entry.message = Some(format!("probe failed ({consecutive}/{retries}): {message}"));
            }
            EventKind::Failed { message } => entry.message = Some(message.clone()),
            EventKind::Finished { summary } => entry.message = Some(summary.clone()),
            EventKind::WatchTriggered { path, changed } => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                entry.message = Some(if *changed == 1 {
                    format!("restarting: {name} changed")
                } else {
                    format!("restarting: {name} and {} more changed", changed - 1)
                });
            }
            EventKind::WatchFailed { message } => {
                entry.message = Some(format!("watch: {message}"));
            }
            EventKind::WatchTruncated { limit } => {
                entry.message = Some(format!(
                    "watch: more than {limit} files; narrow `paths` or add `ignore` entries"
                ));
            }
            EventKind::LogLinesDropped { count } => {
                entry.message = Some(format!("dropped {count} output line(s)"));
            }
            // Log lines are far too frequent to keep, and `Exited` is already
            // reflected by the state transition that follows it.
            EventKind::Log { .. } | EventKind::Exited { .. } | EventKind::Stopping { .. } => {}
        }
    }

    /// The current status of every service, in start order.
    pub fn snapshot(&self) -> Vec<ServiceStatus> {
        self.order
            .iter()
            .filter_map(|name| {
                let entry = self.services.get(name)?;
                Some(ServiceStatus {
                    name: name.clone(),
                    state: entry.state,
                    pid: entry.pid,
                    uptime: entry.started_at.map(|at| at.elapsed()),
                    restarts: entry.restarts,
                    health: entry.health,
                    message: entry.message.clone(),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::ExitReason;

    fn name(text: &str) -> ServiceName {
        crate::validation::validate_service_name(text).expect("valid name")
    }

    fn registry() -> StatusRegistry {
        StatusRegistry::new([(name("db"), true), (name("api"), false)])
    }

    fn event(service: &str, kind: EventKind) -> ServiceEvent {
        ServiceEvent::new(name(service), kind)
    }

    #[test]
    fn services_start_out_pending() {
        let snapshot = registry().snapshot();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0].name.as_str(), "db");
        assert_eq!(snapshot[0].state, ServiceState::Pending);
        assert_eq!(snapshot[0].health, Health::Starting);
        // The second service has no health check at all.
        assert_eq!(snapshot[1].health, Health::None);
        assert!(snapshot[0].pid.is_none());
    }

    #[test]
    fn sync_adds_and_drops_services_without_disturbing_the_rest() {
        let mut registry = registry();
        registry.apply(&event("db", EventKind::Started { pgid: 42 }));
        registry.apply(&event("db", EventKind::State(ServiceState::Running)));

        registry.sync([(name("db"), true), (name("cache"), false)]);

        let snapshot = registry.snapshot();
        let names: Vec<&str> = snapshot.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["db", "cache"]);
        // The service that survived the reload kept its state.
        assert_eq!(snapshot[0].state, ServiceState::Running);
        assert_eq!(snapshot[0].pid, Some(42));
        assert_eq!(snapshot[1].state, ServiceState::Pending);
    }

    #[test]
    fn sync_reports_services_in_the_new_order() {
        let mut registry = registry();
        registry.sync([(name("api"), false), (name("db"), true)]);

        let names: Vec<String> = registry
            .snapshot()
            .iter()
            .map(|s| s.name.to_string())
            .collect();
        assert_eq!(names, vec!["api".to_string(), "db".to_string()]);
    }

    #[test]
    fn events_for_dropped_services_are_ignored() {
        let mut registry = registry();
        registry.sync([(name("api"), false)]);
        registry.apply(&event("db", EventKind::Started { pgid: 7 }));

        assert_eq!(registry.snapshot().len(), 1);
    }

    #[test]
    fn a_running_service_reports_its_pid_and_uptime() {
        let mut registry = registry();
        registry.apply(&event("db", EventKind::State(ServiceState::Starting)));
        registry.apply(&event("db", EventKind::Started { pgid: 1234 }));
        registry.apply(&event("db", EventKind::State(ServiceState::Running)));

        let db = &registry.snapshot()[0];
        assert_eq!(db.state, ServiceState::Running);
        assert_eq!(db.pid, Some(1234));
        assert!(db.uptime.is_some());
    }

    #[test]
    fn a_stopped_service_has_no_pid() {
        let mut registry = registry();
        registry.apply(&event("db", EventKind::State(ServiceState::Starting)));
        registry.apply(&event("db", EventKind::Started { pgid: 1234 }));
        registry.apply(&event("db", EventKind::State(ServiceState::Running)));
        registry.apply(&event("db", EventKind::State(ServiceState::Stopped)));

        let db = &registry.snapshot()[0];
        assert_eq!(db.state, ServiceState::Stopped);
        assert!(db.pid.is_none());
        assert!(db.uptime.is_none());
    }

    #[test]
    fn restarts_are_counted_from_backoff_attempts() {
        let mut registry = registry();
        for attempt in 1..=3 {
            registry.apply(&event(
                "db",
                EventKind::Backoff {
                    delay: Duration::from_millis(100),
                    attempt,
                },
            ));
        }
        assert_eq!(registry.snapshot()[0].restarts, 3);
    }

    #[test]
    fn health_follows_the_probes() {
        let mut registry = registry();
        registry.apply(&event("db", EventKind::Healthy));
        assert_eq!(registry.snapshot()[0].health, Health::Healthy);

        registry.apply(&event(
            "db",
            EventKind::Unhealthy {
                message: "connection refused".to_string(),
            },
        ));
        let db = &registry.snapshot()[0];
        assert_eq!(db.health, Health::Unhealthy);
        assert!(db
            .message
            .as_deref()
            .unwrap()
            .contains("connection refused"));

        // A restart puts the verdict back to "not proven yet", and drops the
        // note about the previous run.
        registry.apply(&event("db", EventKind::State(ServiceState::Starting)));
        assert_eq!(registry.snapshot()[0].health, Health::Starting);
        assert!(registry.snapshot()[0].message.is_none());
    }

    #[test]
    fn a_service_without_a_health_check_never_reports_health() {
        let mut registry = registry();
        registry.apply(&event("api", EventKind::State(ServiceState::Starting)));
        assert_eq!(registry.snapshot()[1].health, Health::None);
    }

    #[test]
    fn noisy_events_are_ignored() {
        let mut registry = registry();
        registry.apply(&event(
            "db",
            EventKind::Log {
                stream: crate::runtime::event::Stream::Stdout,
                line: "hello".to_string(),
            },
        ));
        registry.apply(&event(
            "db",
            EventKind::Exited {
                reason: ExitReason::Code(0),
                uptime: Duration::from_secs(1),
            },
        ));
        assert!(registry.snapshot()[0].message.is_none());
    }

    #[test]
    fn events_for_unknown_services_are_dropped() {
        let mut registry = registry();
        registry.apply(&event("web", EventKind::State(ServiceState::Running)));
        assert_eq!(registry.snapshot().len(), 2);
    }
}
