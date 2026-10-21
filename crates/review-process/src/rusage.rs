//! What a reaped child used: its CPU time and peak memory (the telemetry design, §7.2).
//!
//! `wait4(2)` reports a child's resource use at its reap, but no safe wrapper of it exists in
//! this workspace's dependencies (`nix` 0.31 exposes none) and the workspace forbids `unsafe`
//! code, so the supervisor takes the same figures from `getrusage(RUSAGE_CHILDREN)`: the kernel
//! adds a child's resource use (its own and that of the descendants it waited for, exactly what
//! `wait4` returns) to its parent's children totals at the moment it is reaped. Every reap this
//! crate performs runs under one process-wide lock between two readings of those totals, and so
//! does every reap of the rest of the workspace, which reaps only through [`crate::reap`] (a
//! source test holds every crate to that), so no other child of the process is reaped between
//! the readings and the difference is the one reaped child's: its user and system time. The totals keep only the
//! largest peak resident size of any reaped child, so a child's own peak is known exactly when
//! its reap raised that maximum, and is absent otherwise. A figure the platform did not deliver
//! is absent, never zero.

use std::sync::Mutex;

use nix::sys::resource::{UsageWho, getrusage};

/// One reaped child's resource use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rusage {
    /// User CPU time, milliseconds.
    pub user_ms: u64,
    /// System CPU time, milliseconds.
    pub system_ms: u64,
    /// Peak resident set size in bytes, normalised per platform (`ru_maxrss` is bytes on macOS
    /// and kibibytes on Linux). `None` when this reap did not raise the process's largest peak
    /// of any reaped child: the child's own peak is then not delivered, and never reported as 0.
    pub max_rss_bytes: Option<u64>,
}

/// The children totals at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Totals {
    user_us: i128,
    system_us: i128,
    max_rss_bytes: u64,
}

/// Held across every reap this crate performs and its two readings of the children totals.
static REAP: Mutex<()> = Mutex::new(());

/// Bytes per unit of `ru_maxrss` on this platform.
const MAX_RSS_UNIT: u64 = if cfg!(any(target_os = "macos", target_os = "ios")) {
    1
} else {
    1024
};

fn totals() -> Option<Totals> {
    let usage = getrusage(UsageWho::RUSAGE_CHILDREN).ok()?;
    let micros = |time: nix::sys::time::TimeVal| {
        i128::from(time.tv_sec()) * 1_000_000 + i128::from(time.tv_usec())
    };
    Some(Totals {
        user_us: micros(usage.user_time()),
        system_us: micros(usage.system_time()),
        max_rss_bytes: u64::try_from(usage.max_rss())
            .ok()?
            .checked_mul(MAX_RSS_UNIT)?,
    })
}

impl Rusage {
    /// The one child reaped between `before` and `after`. `None` when the totals went backwards
    /// (they never do for a reap), so nothing is ever invented.
    fn between(before: Totals, after: Totals) -> Option<Self> {
        let millis = |delta: i128| u64::try_from(delta).ok().map(|micros| micros / 1000);
        Some(Self {
            user_ms: millis(after.user_us - before.user_us)?,
            system_ms: millis(after.system_us - before.system_us)?,
            max_rss_bytes: (after.max_rss_bytes > before.max_rss_bytes)
                .then_some(after.max_rss_bytes),
        })
    }
}

/// Run one reap under the process-wide lock between two readings of the children totals.
/// `reaped` says whether the call reaped the child (a `try_wait` that found it running did
/// not); only then is a resource use returned.
pub(crate) fn measured<T>(
    reap: impl FnOnce() -> std::io::Result<T>,
    reaped: impl FnOnce(&T) -> bool,
) -> (std::io::Result<T>, Option<Rusage>) {
    let _guard = REAP.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let before = totals();
    let result = reap();
    let rusage = match (&result, before) {
        (Ok(value), Some(before)) if reaped(value) => {
            totals().and_then(|after| Rusage::between(before, after))
        }
        _ => None,
    };
    (result, rusage)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn totals(user_us: i128, system_us: i128, max_rss_bytes: u64) -> Totals {
        Totals {
            user_us,
            system_us,
            max_rss_bytes,
        }
    }

    #[test]
    fn the_difference_is_the_reaped_childs_and_a_peak_only_when_it_rose() {
        assert_eq!(
            Rusage::between(totals(1_000, 2_000, 4096), totals(13_999, 2_500, 8192)),
            Some(Rusage {
                user_ms: 12,
                system_ms: 0,
                max_rss_bytes: Some(8192),
            })
        );
        // A smaller child leaves the maximum where it was: its peak was not delivered.
        assert_eq!(
            Rusage::between(totals(0, 0, 8192), totals(5_000, 1_000, 8192)).unwrap(),
            Rusage {
                user_ms: 5,
                system_ms: 1,
                max_rss_bytes: None,
            }
        );
        assert_eq!(
            Rusage::between(totals(5_000, 0, 0), totals(4_000, 0, 0)),
            None
        );
    }

    /// Design §7.2 (P3): a supervised process's resource use is never another child's. A
    /// CPU-bound child has exited, unreaped, when the supervisor takes its first reading for a
    /// trivial one; another thread then reaps the CPU-bound child — through [`crate::reap`], as
    /// every reap of the workspace does — while the supervisor's measurement is open. That reap
    /// waits for the lock, so the CPU-bound child's time lands on its own reap and never on the
    /// supervised child's.
    #[test]
    fn telemetry_rusage_is_never_another_childs() {
        use std::process::Command;
        fn exited(child: &std::process::Child) {
            let mut watch = crate::ExitWatch::new(child.id());
            while watch.exited() == Some(false) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        let mut busy = Command::new("/bin/sh")
            .args(["-c", "i=0; while [ $i -lt 300000 ]; do i=$((i+1)); done"])
            .spawn()
            .unwrap();
        exited(&busy);
        let mut supervised = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        exited(&supervised);
        let (started, entered) = std::sync::mpsc::channel();
        let mut other = None;
        let (result, own) = measured(
            || {
                other = Some(std::thread::spawn(move || {
                    started.send(()).unwrap();
                    crate::reap::wait_measured(&mut busy)
                }));
                entered.recv().unwrap();
                // The other reap is attempted inside this measurement's window.
                std::thread::sleep(std::time::Duration::from_millis(200));
                supervised.try_wait()
            },
            Option::is_some,
        );
        assert!(result.unwrap().unwrap().success());
        let (status, theirs) = other.unwrap().join().unwrap();
        assert!(status.unwrap().success());
        let cpu = |usage: Rusage| usage.user_ms + usage.system_ms;
        let (own, theirs) = (cpu(own.unwrap()), cpu(theirs.unwrap()));
        assert!(theirs >= 50, "the CPU-bound child used {theirs} ms");
        assert!(
            own * 4 < theirs,
            "the supervised child carries {own} ms against the other's {theirs} ms"
        );
    }

    #[test]
    fn nothing_is_measured_for_a_call_that_reaped_nothing() {
        let (result, rusage) = measured(|| Ok(None::<u32>), Option::is_some);
        assert!(matches!(result, Ok(None)));
        assert_eq!(rusage, None);
        let (result, rusage) = measured(
            || Err::<Option<u32>, _>(std::io::Error::other("fixture")),
            |_| true,
        );
        assert!(result.is_err());
        assert_eq!(rusage, None);
    }
}
