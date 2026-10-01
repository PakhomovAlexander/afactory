//! SIGINT and SIGTERM while af runs Task work (ADR-0126). The signals are blocked before any
//! thread starts and taken by one watcher thread with `sigwait`, so no handler code runs in
//! signal context. The first signal records itself and requests the ADR-0089 cancellation of
//! every execution in progress or started later; supervision then stops and reaps each Worker
//! process group. A second signal kills the listed groups and exits at once. Either way af
//! ends by its own signal, so a shell sees the conventional 130 or 143 and a calling script
//! sees an interrupt. Children start with an empty signal mask, because the standard library
//! resets it before `exec`.

use nix::sys::signal::{SigSet, Signal};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};

/// The error a host returns once interrupted, after its executions have stopped.
pub(crate) const INTERRUPTED: &str = "Task execution was interrupted";

struct State {
    signal: Option<Signal>,
    task: Option<String>,
}

static STATE: Mutex<State> = Mutex::new(State {
    signal: None,
    task: None,
});
static CHANGED: Condvar = Condvar::new();

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn signals() -> SigSet {
    let mut set = SigSet::empty();
    set.add(Signal::SIGINT);
    set.add(Signal::SIGTERM);
    set
}

/// Take over SIGINT and SIGTERM for the rest of this command. Call from the main thread before
/// it starts any other thread: threads inherit the blocked mask, so only the watcher sees them.
pub(crate) fn install() -> Result<(), String> {
    signals()
        .thread_block()
        .map_err(|error| format!("blocking interrupt signals: {error}"))?;
    std::thread::Builder::new()
        .name("af-interrupt".into())
        .spawn(|| {
            let set = signals();
            loop {
                let Ok(signal) = set.wait() else { continue };
                receive(signal);
            }
        })
        .map_err(|error| format!("starting the interrupt watcher: {error}"))?;
    Ok(())
}

fn receive(signal: Signal) {
    let first = {
        let mut state = state();
        let first = state.signal.is_none();
        if first {
            state.signal = Some(signal);
        }
        first
    };
    if first {
        CHANGED.notify_all();
        eprintln!(
            "af: {signal} received; stopping Worker processes. Press Ctrl-C again to exit at once."
        );
        return;
    }
    review_process::kill_live_process_groups();
    eprintln!("af: {signal} received again; killed Worker process groups and exiting");
    exit(signal);
}

/// The conventional exit status for a command ended by `signal`: 130 for SIGINT, 143 for
/// SIGTERM.
pub(crate) fn exit_code(signal: Signal) -> i32 {
    128 + signal as i32
}

/// End af by `signal` with its default action, which af never changed: raised at this thread,
/// where the watcher's `sigwait` cannot take it, and then unblocked here. Where the
/// environment ignores the signal, exit with its conventional status instead.
pub(crate) fn exit(signal: Signal) -> ! {
    let mut set = SigSet::empty();
    set.add(signal);
    if nix::sys::signal::raise(signal).is_ok() {
        let _ = set.thread_unblock();
    }
    std::process::exit(exit_code(signal));
}

/// The first interrupt this command received, if any.
pub(crate) fn received() -> Option<Signal> {
    state().signal
}

/// Refuse to continue past an interrupt, so no result is assembled or finished from work
/// that was cancelled.
pub(crate) fn check() -> Result<(), String> {
    match received() {
        Some(_) => Err(INTERRUPTED.into()),
        None => Ok(()),
    }
}

/// Name the common Task this command runs, so an interrupt can say how to resume it.
pub(crate) fn note_task(id: &str) {
    state().task = Some(id.to_owned());
}

/// The common Task noted by this command, if any.
pub(crate) fn noted_task() -> Option<String> {
    state().task.clone()
}

/// Run `work` with an interrupt forwarded to its execution's cancellation flag. The flag is
/// the same one its heartbeat sets (ADR-0089); an interrupt received earlier sets it at once.
pub(crate) fn forwarding<T>(cancellation: &AtomicBool, work: impl FnOnce() -> T) -> T {
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let state = CHANGED
                .wait_while(state(), |state| {
                    state.signal.is_none() && !done.load(Ordering::Acquire)
                })
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.signal.is_some() {
                cancellation.store(true, Ordering::Release);
            }
        });
        struct Finish<'a>(&'a AtomicBool);
        impl Drop for Finish<'_> {
            fn drop(&mut self) {
                // Under the lock, so the forwarder cannot miss this between test and wait.
                let _state = state();
                self.0.store(true, Ordering::Release);
                CHANGED.notify_all();
            }
        }
        let _finish = Finish(&done);
        work()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conventional_interrupt_statuses() {
        assert_eq!(exit_code(Signal::SIGINT), 130);
        assert_eq!(exit_code(Signal::SIGTERM), 143);
    }

    #[test]
    fn forwarding_returns_without_an_interrupt_and_leaves_the_flag_clear() {
        if received().is_some() {
            return;
        }
        let cancellation = AtomicBool::new(false);
        assert_eq!(forwarding(&cancellation, || 7), 7);
        assert!(!cancellation.load(Ordering::Acquire));
        assert_eq!(check(), Ok(()));
    }
}
