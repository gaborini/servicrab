//! Signal handling for the window before the async runtime exists.
//!
//! Two requirements in [`super::server::serve`] point in opposite directions.
//!
//! * [`servicrab_core::SignalWatcher`] needs a tokio runtime: it builds its
//!   `Signal` streams from the runtime's signal driver and spawns a task to read
//!   them.  It cannot be installed before the runtime is built.
//! * `bind_socket` must run *before* the runtime is built.  The umask that keeps
//!   the socket private from the instant it exists is process-global, so no other
//!   thread may be creating a file while it is in effect.
//!
//! So the socket — and with it the whole start/stop/shutdown authority of the
//! project — becomes reachable while SIGTERM, SIGINT and SIGHUP still have their
//! default disposition, which is to end the process outright.  A signal in that
//! window took the daemon down with no graceful shutdown and left the socket file
//! on disk for the next `start` to trip over.
//!
//! The handler cannot be moved earlier, but the *disposition* can.  This module
//! claims those three signals before anything of ours exists, with a handler that
//! only records which one arrived.  The process survives, the operator's request
//! is remembered, and [`arrived`] hands it to the real watcher as soon as the
//! runtime is up.
//!
//! Blocking the signals with `pthread_sigmask` instead looks tidier — the kernel
//! remembers a pending signal for us and no handler is needed.  It is wrong here:
//! the mask has to be set before the runtime starts its threads (otherwise a
//! signal is simply delivered to a worker thread that does not block it), those
//! threads inherit it, and on this platform a spawned process inherits it too.
//! Supervised services would come up with SIGTERM blocked and could then only be
//! stopped with SIGKILL.  Measured, not assumed.

use std::sync::atomic::{AtomicI32, Ordering};

use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal};
use servicrab_core::ShutdownReason;

/// The signals whose default disposition ends the process, and which therefore
/// have to be claimed before the daemon is reachable.
///
/// SIGHUP is here for the same reason it is in the real watcher: closing a
/// terminal must not kill a supervisor and orphan every process group it owns.
const CLAIMED: [Signal; 3] = [Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP];

/// The signal number that arrived before the runtime was ready, or zero.
///
/// A plain integer rather than a channel or a lock: this is written from a signal
/// handler, where almost nothing is safe to call.
static EARLY: AtomicI32 = AtomicI32::new(0);

/// Record the first signal to arrive and return.
///
/// Async-signal-safe by construction: one lock-free compare-and-exchange on an
/// `i32` and no other work at all — no allocation, no locks, no `write(2)`.
///
/// The first signal wins.  A second one means "stop waiting", which is a
/// judgement only the real watcher can act on, and by the time anybody could
/// send one the watcher is normally already installed.
extern "C" fn record(signum: i32) {
    let _ = EARLY.compare_exchange(0, signum, Ordering::SeqCst, Ordering::SeqCst);
}

/// Take over SIGINT, SIGTERM and SIGHUP so that they stop being fatal.
///
/// Call this before anything makes the daemon observable — before the pidfile is
/// created and long before the socket is bound.  Once the real watcher is
/// installed it registers its own handler on top; ours stays harmlessly in the
/// chain, storing into an integer nobody reads any more.
///
/// The disposition is process-wide rather than per-thread, and `execve` resets a
/// caught signal to its default, so nothing here is inherited by a supervised
/// service.
pub fn claim() -> Result<(), String> {
    // `SA_RESTART` so that an interrupted syscall in the startup path — the
    // `flock`, the `bind` — resumes instead of failing with `EINTR`.
    let action = SigAction::new(
        SigHandler::Handler(record),
        SaFlags::SA_RESTART,
        SigSet::empty(),
    );
    for signal in CLAIMED {
        // SAFETY: `record` does one atomic store and nothing else, which is
        // permitted in a signal handler.  `sigaction` is unsafe only because the
        // handler it installs could do something that is not.
        unsafe { nix::sys::signal::sigaction(signal, &action) }.map_err(|errno| {
            format!(
                "could not take over {} before the daemon becomes reachable: {errno}",
                signal.as_str()
            )
        })?;
    }
    Ok(())
}

/// The shutdown that was asked for before the runtime existed, if there was one.
///
/// Call this once the real watcher is installed, so that no signal can fall
/// between the two: anything earlier is in [`EARLY`], anything later is the
/// watcher's.
pub fn arrived() -> Option<ShutdownReason> {
    match EARLY.load(Ordering::SeqCst) {
        0 => None,
        signum if signum == Signal::SIGINT as i32 => Some(ShutdownReason::UserInterrupt),
        signum if signum == Signal::SIGHUP as i32 => Some(ShutdownReason::HangUp),
        // SIGTERM, and any signal we did not claim but somehow ran our handler:
        // being asked to terminate is the safe reading of an unexpected one.
        _ => Some(ShutdownReason::Terminated),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The claim has to survive being made twice: `serve` runs once per process,
    /// but the unit tests share one.
    #[test]
    fn claiming_the_signals_reports_success() {
        claim().expect("claim");
        claim().expect("claim again");
    }

    /// The heart of the fix: a signal that arrives in the window is *remembered*
    /// rather than being fatal or being dropped.  `raise` is thread-directed, so
    /// this only exercises the calling thread's disposition — which is
    /// process-wide, and is exactly what the daemon relies on.
    ///
    /// If the claim did not work, this test would not fail: the whole test
    /// process would be killed by the default disposition.
    #[test]
    fn a_signal_in_the_window_is_recorded_rather_than_fatal() {
        claim().expect("claim");
        EARLY.store(0, Ordering::SeqCst);
        assert_eq!(arrived(), None, "nothing has been signalled yet");

        nix::sys::signal::raise(Signal::SIGTERM).expect("raise");

        assert_eq!(arrived(), Some(ShutdownReason::Terminated));
        EARLY.store(0, Ordering::SeqCst);
    }

    #[test]
    fn each_claimed_signal_maps_to_its_shutdown_reason() {
        for (signal, expected) in [
            (Signal::SIGINT, ShutdownReason::UserInterrupt),
            (Signal::SIGTERM, ShutdownReason::Terminated),
            (Signal::SIGHUP, ShutdownReason::HangUp),
        ] {
            EARLY.store(signal as i32, Ordering::SeqCst);
            assert_eq!(arrived(), Some(expected), "{}", signal.as_str());
        }
        EARLY.store(0, Ordering::SeqCst);
    }
}
