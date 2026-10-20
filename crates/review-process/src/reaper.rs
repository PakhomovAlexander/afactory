//! Ending the descendants a run leaves outside its process group ([`crate::ExitPolicy::KillDescendants`]).
//!
//! A process-group kill misses a descendant that moved to a session (`setsid`) or group of its
//! own: it keeps running, and can write into a tree after the kernel sealed it. On Linux the
//! supervisor marks itself a child subreaper (`PR_SET_CHILD_SUBREAPER`) before such a run
//! starts, so a descendant orphaned by the run becomes the supervisor's child instead of
//! init's. When the run's leader has exited, and before it is reaped, [`end_escaped`] reads
//! `/proc` and ends, layer by layer until none is left:
//!
//! - every member of the leader's process group (the group kill; exact while the leader is
//!   unreaped);
//! - every child of the supervisor in another session than the supervisor's and without a
//!   controlling terminal, started after the leader, that is no supervised leader: an orphan that
//!   called `setsid`. A child the supervisor starts on a pseudo-terminal of its own (a Provider
//!   probe) has one, and is never taken for an orphan;
//! - every descendant of the leader or of either, found through its parent.
//!
//! Only the supervisor's own children are signalled and reaped by pid, which is exact: a child's
//! pid cannot be reused before its parent reaps it. A descendant of a live parent is reached
//! once that parent has died and it has become the supervisor's child. A descendant that moved to
//! a group of its own without a new session and whose parent had already exited is not told from
//! the supervisor's other children and is left. Elsewhere this module does nothing.

/// Make this process a child subreaper, once; later calls do nothing.
pub(crate) fn adopt_orphans() {
    #[cfg(target_os = "linux")]
    {
        static ADOPTED: std::sync::Once = std::sync::Once::new();
        ADOPTED.call_once(|| {
            let _ = nix::sys::prctl::set_child_subreaper(true);
        });
    }
}

/// End every descendant the run led by `leader` left behind, as the module documents. The
/// leader itself is left for its own reap. Bounded by [`crate::OUTPUT_DRAIN_GRACE`].
pub(crate) fn end_escaped(leader: u32) {
    #[cfg(target_os = "linux")]
    linux::end_escaped(leader);
    #[cfg(not(target_os = "linux"))]
    let _ = leader;
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::{Duration, Instant};

    use nix::sys::signal::{Signal, kill, killpg};
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
    use nix::unistd::Pid;

    /// One process as `/proc/<pid>/stat` describes it.
    #[derive(Clone, Copy)]
    struct Stat {
        zombie: bool,
        parent: i32,
        group: i32,
        session: i32,
        /// The controlling terminal's device number; 0 for none.
        terminal: i32,
        /// Clock ticks after boot: with the pid, the process's identity.
        start: u64,
    }

    fn stat(pid: i32) -> Option<Stat> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // The command name is in parentheses and may hold anything, a `)` included.
        let rest = text.get(text.rfind(')')? + 1..)?;
        let fields: Vec<&str> = rest.split_whitespace().collect();
        Some(Stat {
            zombie: *fields.first()? == "Z",
            parent: fields.get(1)?.parse().ok()?,
            group: fields.get(2)?.parse().ok()?,
            session: fields.get(3)?.parse().ok()?,
            terminal: fields.get(4)?.parse().ok()?,
            start: fields.get(19)?.parse().ok()?,
        })
    }

    fn processes() -> BTreeMap<i32, Stat> {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return BTreeMap::new();
        };
        entries
            .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<i32>().ok())
            .filter_map(|pid| Some((pid, stat(pid)?)))
            .collect()
    }

    pub(super) fn end_escaped(leader: u32) {
        let me = std::process::id() as i32;
        let leader = leader as i32;
        let (Some(own), Some(led)) = (stat(me), stat(leader)) else {
            return;
        };
        let deadline = Instant::now() + crate::OUTPUT_DRAIN_GRACE;
        // Every process found to belong to the run, by pid and start time.
        let mut tracked: BTreeMap<i32, u64> = BTreeMap::new();
        loop {
            let table = processes();
            tracked.retain(|pid, start| table.get(pid).is_some_and(|s| s.start == *start));
            let supervised: BTreeSet<i32> = crate::live_process_groups()
                .into_iter()
                .map(|pid| pid as i32)
                .collect();
            for (pid, s) in &table {
                if *pid == leader || *pid == me || supervised.contains(pid) {
                    continue;
                }
                let member = s.group == leader;
                let escaped = s.parent == me
                    && s.session != own.session
                    && s.terminal == 0
                    && s.start >= led.start;
                if member || escaped {
                    tracked.insert(*pid, s.start);
                }
            }
            loop {
                let found: Vec<(i32, u64)> = table
                    .iter()
                    .filter(|(pid, s)| {
                        !tracked.contains_key(pid)
                            && *pid != &leader
                            && (s.parent == leader || tracked.contains_key(&s.parent))
                    })
                    .map(|(pid, s)| (*pid, s.start))
                    .collect();
                if found.is_empty() {
                    break;
                }
                tracked.extend(found);
            }
            // What is left to end: anything alive, and a zombie only this process can reap.
            let pending: Vec<i32> = tracked
                .keys()
                .copied()
                .filter(|pid| !table[pid].zombie || table[pid].parent == me)
                .collect();
            if pending.is_empty() || Instant::now() >= deadline {
                return;
            }
            let _ = killpg(Pid::from_raw(leader), Signal::SIGKILL);
            let ours: Vec<i32> = pending
                .iter()
                .copied()
                .filter(|pid| table[pid].parent == me)
                .collect();
            for pid in &ours {
                let _ = kill(Pid::from_raw(*pid), Signal::SIGKILL);
            }
            for pid in &ours {
                reap(*pid, deadline);
            }
            if ours.is_empty() {
                // Only descendants of dying parents are left: they become this process's
                // children as their parents exit.
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    /// Reap one killed child of this process, polling until `deadline`.
    fn reap(pid: i32, deadline: Instant) {
        loop {
            match waitpid(Pid::from_raw(pid), Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(nix::errno::Errno::EINTR) => {}
                _ => return,
            }
        }
    }
}
