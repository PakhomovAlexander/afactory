//! Ending the descendants a run leaves outside its process group ([`crate::ExitPolicy::KillDescendants`]).
//!
//! A process-group kill misses a descendant that moved to a session (`setsid`) or group of its
//! own (`set -m`): it keeps running, and can write into a tree after the kernel sealed it. On
//! Linux the supervisor marks itself a child subreaper (`PR_SET_CHILD_SUBREAPER`) before such a
//! run starts, so a descendant orphaned by the run becomes the supervisor's child instead of
//! init's. Every such run is a [`Tree`]: its leader starts with a variable naming the run
//! ([`TREE_VARIABLE`]), which every descendant inherits whatever its session or group. When
//! the run's leader has exited, and before it is reaped, [`Tree::end_escaped`] reads `/proc`
//! and ends, layer by layer until none is left:
//!
//! - every member of the leader's process group (the group kill; exact while the leader is
//!   unreaped);
//! - every child of the supervisor that started with this run's variable: an orphan of this
//!   run's tree, in whatever session or group, and never one of another run's, however many
//!   runs one supervisor has at once;
//! - while no other such run is live in this process, also every child of the supervisor in
//!   another session than the supervisor's and without a controlling terminal, started after
//!   the leader, that is no supervised leader: an orphan that called `setsid` with a cleared
//!   environment. A child the supervisor starts on a pseudo-terminal of its own (a Provider
//!   probe) has one, and is never taken for an orphan;
//! - every descendant of the leader or of any of these, found through its parent.
//!
//! Only the supervisor's own children are signalled and reaped by pid, which is exact: a child's
//! pid cannot be reused before its parent reaps it. A descendant of a live parent is reached
//! once that parent has died and it has become the supervisor's child. A process the supervisor
//! did not start, directly or through a run, is never its child, and never touched.
//!
//! The guarantee holds on Linux only while the subreaper is set. When it cannot be set, the
//! failure is said once per process on stderr and kept ([`descendant_cleanup_fallback`]), and
//! every check and Attempt record names it: cleanup fell back to the process-group kill.
//! Elsewhere this module ends the group alone.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// The variable every process of a [`Tree`] starts with: the run that started it.
pub const TREE_VARIABLE: &str = "AF_PROCESS_TREE";

/// Why descendant cleanup in this process fell back to the process-group kill, once known.
static FALLBACK: OnceLock<String> = OnceLock::new();

/// The [`Tree`]s live in this process.
static LIVE_TREES: AtomicUsize = AtomicUsize::new(0);

/// Why descendant cleanup in this process falls back to the process-group kill: on Linux, the
/// reason af could not become a child subreaper, recorded by the first run that needed it.
/// `None` while the guarantee holds, and before any such run. A caller records it in the check
/// or Attempt it seals.
pub fn descendant_cleanup_fallback() -> Option<&'static str> {
    FALLBACK.get().map(String::as_str)
}

/// Make this process a child subreaper, once; later calls do nothing. A failure is recorded
/// once, never silently claimed as the guarantee.
pub(crate) fn adopt_orphans() {
    #[cfg(target_os = "linux")]
    {
        static ADOPTED: std::sync::Once = std::sync::Once::new();
        ADOPTED.call_once(|| {
            let adopted = nix::sys::prctl::set_child_subreaper(true);
            record_adoption(&FALLBACK, adopted.map_err(|errno| errno.to_string()));
        });
    }
}

/// Record the outcome of becoming a subreaper: a failure is kept in `fallback` and said on
/// stderr, once per `fallback`.
#[cfg(any(target_os = "linux", test))]
fn record_adoption(fallback: &OnceLock<String>, adopted: Result<(), String>) {
    let Err(error) = adopted else {
        return;
    };
    let reason = format!(
        "descendant cleanup fell back to the process-group kill: af could not become a child \
         subreaper (PR_SET_CHILD_SUBREAPER: {error}), so a descendant that leaves its run's \
         process group can outlive the run"
    );
    if fallback.set(reason.clone()).is_ok() {
        eprintln!("af: warning: {reason}");
    }
}

/// One run whose descendants must not outlive it: its leader is marked with [`TREE_VARIABLE`],
/// and the run counts as live in this process until dropped.
pub(crate) struct Tree {
    token: String,
}

impl Tree {
    /// Mark `command` as a new run's leader, after making this process a subreaper.
    pub(crate) fn new(command: &mut std::process::Command) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        adopt_orphans();
        let token = format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        command.env(TREE_VARIABLE, &token);
        LIVE_TREES.fetch_add(1, Ordering::AcqRel);
        Self { token }
    }

    /// End every descendant the run led by `leader` left behind, as the module documents. The
    /// leader itself is left for its own reap. Bounded by [`crate::OUTPUT_DRAIN_GRACE`].
    pub(crate) fn end_escaped(&self, leader: u32) {
        #[cfg(target_os = "linux")]
        linux::end_escaped(
            leader,
            &format!("{TREE_VARIABLE}={}", self.token),
            LIVE_TREES.load(Ordering::Acquire) == 1,
        );
        #[cfg(not(target_os = "linux"))]
        let _ = (leader, &self.token);
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        LIVE_TREES.fetch_sub(1, Ordering::AcqRel);
    }
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

    /// Whether the process `pid` started with exactly `entry` (`NAME=value`) in its environment.
    fn started_with(pid: i32, entry: &str) -> bool {
        std::fs::read(format!("/proc/{pid}/environ")).is_ok_and(|environ| {
            environ
                .split(|byte| *byte == 0)
                .any(|variable| variable == entry.as_bytes())
        })
    }

    /// `marker` is this run's `NAME=value`; `alone` whether no other run is live in this
    /// process, so that an unmarked `setsid` orphan can only be this run's.
    pub(super) fn end_escaped(leader: u32, marker: &str, alone: bool) {
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
                if *pid == leader
                    || *pid == me
                    || supervised.contains(pid)
                    || tracked.contains_key(pid)
                {
                    continue;
                }
                let member = s.group == leader;
                let child = s.parent == me;
                let marked = child && !s.zombie && started_with(*pid, marker);
                let escaped = alone
                    && child
                    && s.session != own.session
                    && s.terminal == 0
                    && s.start >= led.start;
                if member || marked || escaped {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A subreaper that cannot be set is recorded once, as the fallback every record names; a
    /// subreaper that is set records nothing (audit V4).
    #[test]
    fn a_failed_subreaper_is_recorded_once_as_the_fallback() {
        let fallback = OnceLock::new();
        record_adoption(&fallback, Ok(()));
        assert_eq!(fallback.get(), None);
        record_adoption(&fallback, Err("EPERM: Operation not permitted".into()));
        let reason = fallback.get().unwrap();
        assert!(
            reason.starts_with("descendant cleanup fell back to the process-group kill"),
            "{reason}"
        );
        assert!(reason.contains("PR_SET_CHILD_SUBREAPER: EPERM"), "{reason}");
        record_adoption(&fallback, Err("EINVAL".into()));
        assert_eq!(fallback.get(), Some(reason), "once per process");
    }

    /// Every run marks its leader with a token of its own, and counts as live until dropped.
    #[test]
    fn every_tree_marks_its_leader_with_its_own_token() {
        let mut first = std::process::Command::new("true");
        let mut second = std::process::Command::new("true");
        let (a, b) = (Tree::new(&mut first), Tree::new(&mut second));
        assert_ne!(a.token, b.token);
        let value = |command: &std::process::Command| {
            command
                .get_envs()
                .find(|(key, _)| *key == TREE_VARIABLE)
                .and_then(|(_, value)| value)
                .map(|value| value.to_string_lossy().into_owned())
        };
        assert_eq!(value(&first).as_deref(), Some(a.token.as_str()));
        assert_eq!(value(&second).as_deref(), Some(b.token.as_str()));
        assert!(LIVE_TREES.load(Ordering::Acquire) >= 2);
    }
}
