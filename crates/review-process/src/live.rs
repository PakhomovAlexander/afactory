//! The process groups this process supervises right now, for a host that must stop at once
//! (ADR-0126). Every supervised leader holds one slot from its spawn until just before its
//! reap, so a listed id is always reserved by an unreaped leader. A full table only means a
//! group is not listed: its own cancellation and deadline still stop it.

use std::sync::atomic::{AtomicU32, Ordering};

const SLOTS: usize = 256;

static LIVE: [AtomicU32; SLOTS] = [const { AtomicU32::new(0) }; SLOTS];

/// One leader's entry in a table; [`Registration::release`] empties it.
pub(crate) struct Registration {
    table: &'static [AtomicU32],
    slot: Option<usize>,
    pid: u32,
}

impl Registration {
    pub(crate) fn new(pid: u32) -> Self {
        register(&LIVE, pid)
    }

    /// Idempotent: only this leader's own entry is cleared.
    pub(crate) fn release(&mut self) {
        if let Some(slot) = self.slot.take() {
            let _ =
                self.table[slot].compare_exchange(self.pid, 0, Ordering::AcqRel, Ordering::Acquire);
        }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.release();
    }
}

fn register(table: &'static [AtomicU32], pid: u32) -> Registration {
    let slot = (pid != 0)
        .then(|| {
            table.iter().position(|entry| {
                entry
                    .compare_exchange(0, pid, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            })
        })
        .flatten();
    Registration { table, slot, pid }
}

fn listed(table: &[AtomicU32]) -> Vec<u32> {
    table
        .iter()
        .map(|entry| entry.load(Ordering::Acquire))
        .filter(|pid| *pid != 0)
        .collect()
}

/// The leader pids of every supervised process group not yet reaped.
pub fn live_process_groups() -> Vec<u32> {
    listed(&LIVE)
}

/// Best-effort `SIGKILL` to every supervised process group not yet reaped, for a host that is
/// about to exit without waiting for its supervisors. An ordinary stop is a cancellation flag.
pub fn kill_live_process_groups() {
    for pid in listed(&LIVE) {
        crate::kill_process_group(pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registration_lists_its_leader_until_released_and_never_clears_another() {
        static TABLE: [AtomicU32; 2] = [const { AtomicU32::new(0) }; 2];
        let mut first = register(&TABLE, 41);
        let second = register(&TABLE, 42);
        assert_eq!(listed(&TABLE), vec![41, 42]);
        let full = register(&TABLE, 43);
        assert_eq!(full.slot, None, "a full table leaves the group unlisted");
        first.release();
        first.release();
        assert_eq!(listed(&TABLE), vec![42]);
        let reused = register(&TABLE, 44);
        assert_eq!(listed(&TABLE), vec![44, 42]);
        drop(first);
        assert_eq!(
            listed(&TABLE),
            vec![44, 42],
            "a released entry stays released"
        );
        drop(second);
        drop(reused);
        assert!(listed(&TABLE).is_empty());
        assert_eq!(register(&TABLE, 0).slot, None, "no process has pid 0");
    }

    #[test]
    fn a_supervised_leader_is_listed_while_it_runs_and_not_after_its_reap() {
        let base = std::env::temp_dir().join(format!(
            "review-process-live-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let (marker, release) = (base.with_extension("pid"), base.with_extension("release"));
        let _ = std::fs::remove_file(&marker);
        let _ = std::fs::remove_file(&release);
        // The leader runs until the observer has looked, however slow the machine is.
        let mut command = std::process::Command::new("/bin/sh");
        command.args([
            "-c",
            &format!(
                "echo $$ > '{0}.tmp' && mv '{0}.tmp' '{0}'; while [ ! -e '{1}' ]; do sleep 0.01; done",
                marker.display(),
                release.display()
            ),
        ]);
        let observer = std::thread::spawn({
            let (marker, release) = (marker.clone(), release.clone());
            move || {
                let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
                let observed = loop {
                    if let Some(pid) = std::fs::read_to_string(&marker)
                        .ok()
                        .and_then(|text| text.trim().parse::<u32>().ok())
                    {
                        break (pid, live_process_groups().contains(&pid));
                    }
                    if std::time::Instant::now() >= until {
                        break (0, false);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                };
                std::fs::write(&release, b"").unwrap();
                observed
            }
        });
        let output =
            crate::run_supervised(&mut command, None, std::time::Duration::from_secs(60)).unwrap();
        let (pid, listed_while_running) = observer.join().unwrap();
        let _ = std::fs::remove_file(&marker);
        let _ = std::fs::remove_file(&release);
        assert!(output.status.success());
        assert_ne!(pid, 0, "leader did not start");
        assert!(listed_while_running, "a running leader is listed");
        assert!(
            !live_process_groups().contains(&pid),
            "a reaped leader is never listed"
        );
    }
}
