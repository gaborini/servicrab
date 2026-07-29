//! Stub runner for platforms other than Linux and macOS.
//!
//! Windows support is out of scope for now; the stub exists so that the
//! workspace still compiles everywhere.

use crate::config::{RestartPolicy, Service};
use crate::error::RuntimeError;
use crate::runtime::event::EventSink;
use crate::runtime::{RunOptions, RunOutcome, ShutdownRx};

/// Placeholder runner that always reports an unsupported platform.
pub struct ServiceRunner<'a> {
    service: &'a Service,
    policy: RestartPolicy,
}

impl<'a> ServiceRunner<'a> {
    /// Build a runner for a validated service.
    pub fn new(service: &'a Service, options: RunOptions) -> Self {
        Self {
            policy: options.effective_policy(service.restart),
            service,
        }
    }

    /// Accepted for API parity; the stub never emits events.
    pub fn with_events(self, _events: EventSink) -> Self {
        self
    }

    /// The effective restart policy after `--no-restart` is taken into account.
    pub fn policy(&self) -> RestartPolicy {
        self.policy
    }

    /// Always fails with [`RuntimeError::UnsupportedPlatform`].
    pub async fn run(&mut self, _shutdown: &mut ShutdownRx) -> Result<RunOutcome, RuntimeError> {
        Err(RuntimeError::UnsupportedPlatform {
            service: self.service.name.to_string(),
        })
    }
}

/// Placeholder foreground runner.
pub struct ForegroundRunner<'a> {
    runner: ServiceRunner<'a>,
}

impl<'a> ForegroundRunner<'a> {
    /// Build a runner for a validated service.
    pub fn new(service: &'a Service, options: RunOptions) -> Self {
        Self {
            runner: ServiceRunner::new(service, options),
        }
    }

    /// Accepted for API parity; the stub never emits events.
    pub fn with_events(self, _events: EventSink) -> Self {
        self
    }

    /// The effective restart policy after `--no-restart` is taken into account.
    pub fn policy(&self) -> RestartPolicy {
        self.runner.policy()
    }

    /// Always fails with [`RuntimeError::UnsupportedPlatform`].
    pub async fn run(&mut self) -> Result<RunOutcome, RuntimeError> {
        let (_tx, mut rx) = crate::runtime::shutdown_channel();
        self.runner.run(&mut rx).await
    }
}

/// Placeholder signal watcher.
pub struct SignalWatcher {
    tx: crate::runtime::ShutdownTx,
}

impl SignalWatcher {
    /// Signal handling is unavailable on this platform, but installing the
    /// watcher still succeeds so that callers can share one code path.
    pub fn install(_label: &str) -> Result<Self, RuntimeError> {
        let (tx, _rx) = crate::runtime::shutdown_channel();
        Ok(Self { tx })
    }

    /// A fresh receiver for the shutdown channel.
    pub fn subscribe(&self) -> ShutdownRx {
        self.tx.subscribe()
    }

    /// A clone of the shutdown sender.
    pub fn sender(&self) -> crate::runtime::ShutdownTx {
        self.tx.clone()
    }
}
