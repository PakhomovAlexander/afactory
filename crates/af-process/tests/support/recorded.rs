//! A process a test did not start as its own unreaped child — a detached `af` run, a Worker `af`
//! started, a fixture's escapee — recorded with its start time, observed, and never signalled
//! (issue #331 item 7).
//!
//! A process ID names its process only until that process is reaped; a group ID only while the
//! group has a member. `std::process::Child::kill` is exact because the test is the parent and
//! has not reaped the child yet: a zombie keeps its ID and its group reserved. A process the test
//! only read the ID of has another parent, which may reap it at any time, and a gate host that
//! starts hundreds of processes a second (every test, and every `git` and Worker `af` runs, leads
//! a process group of its own) recycles the whole ID space within minutes. Inspecting such an ID
//! and then signalling it is a race no start time can close: between the last look and the
//! signal the process may end, be reaped and its ID be handed to another test's group leader, and
//! a `SIGKILL` to that group ends a sibling test with every child it started.
//!
//! So the only authority to signal a group is holding its leader as an unreaped child. A test
//! ends what it holds (`Child::kill`; an `af` it holds and stops ends the Worker groups that `af`
//! holds); a fixture a test does not hold ends by itself, when the process that started it is
//! gone or when the test asks it to; and what this module records is only watched: [`Leftovers`]
//! waits a bounded time for each recorded process to end and reports any still running,
//! signalling none.
//! The identity is the start time `ps` reports (`lstart`): the same ID with another start time is
//! another process, so a recycled ID never reads as the recorded process still running. A
//! fixture that writes its own ID to a marker file is recorded from that file afterwards, only
//! when the process now under the ID provably started before the marker was written.

#![allow(dead_code)]

use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

/// A process as a test recorded it: its ID and the start time `ps` reported then.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recorded {
    pid: i32,
    started: String,
}

/// What `ps` says of an ID.
enum Seen {
    /// A process holds it, started at this time.
    Started(String),
    /// No process holds it.
    Absent,
    /// `ps` could not be asked (a loaded host may refuse a fork): nothing may be signalled.
    Unknown,
}

/// What `ps` reports for `pid`: its start time, or that no process holds that ID.
fn seen(pid: i32) -> Seen {
    if pid <= 0 {
        return Seen::Absent;
    }
    let Ok(output) = Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
    else {
        return Seen::Unknown;
    };
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    match (output.status.success(), text.is_empty()) {
        (true, false) => Seen::Started(text),
        // `ps -p` exits 1 and prints nothing for an ID no process holds.
        (false, true) if output.status.code() == Some(1) => Seen::Absent,
        _ => Seen::Unknown,
    }
}

/// How long ago `pid` started, from `ps`'s `etime` (`[[dd-]hh:]mm:ss`), or `None` when no
/// process holds that ID.
fn elapsed(pid: i32) -> Option<Duration> {
    let output = Command::new("ps")
        .args(["-o", "etime=", "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !output.status.success() || text.is_empty() {
        return None;
    }
    let (days, clock) = text.split_once('-').unwrap_or(("0", &text));
    let seconds = clock.split(':').try_fold(0_u64, |total, part| {
        Some(total * 60 + part.parse::<u64>().ok()?)
    })?;
    Some(Duration::from_secs(
        days.parse::<u64>().ok()? * 86_400 + seconds,
    ))
}

impl Recorded {
    /// Record `pid` as it is now; `None` when no process holds it (or `ps` cannot tell).
    pub fn now(pid: i32) -> Option<Self> {
        match seen(pid) {
            Seen::Started(started) => Some(Self { pid, started }),
            Seen::Absent | Seen::Unknown => None,
        }
    }

    /// Record the process a fixture named in `marker` (its ID, as text) when it started: `None`
    /// when the marker cannot be read, no process holds the ID, `ps` cannot tell, or nothing
    /// proves the process under it started before the marker was written. `ps` counts whole
    /// seconds down, so the latest the process can have started is `now - etime` with `now`
    /// read after `ps` answered; that must not be later than the marker, with no slack, and the
    /// process must be the same one before and after.
    pub fn from_marker(marker: &std::path::Path) -> Option<Self> {
        let pid = std::fs::read_to_string(marker)
            .ok()?
            .trim()
            .parse::<i32>()
            .ok()?;
        let written = std::fs::metadata(marker).ok()?.modified().ok()?;
        let recorded = Self::now(pid)?;
        let elapsed = elapsed(pid)?;
        let latest_start = SystemTime::now().checked_sub(elapsed)?;
        (latest_start <= written && Self::now(pid).as_ref() == Some(&recorded)).then_some(recorded)
    }

    /// Record `pid` as it is now; a test that just read a live process's ID fails when it is
    /// already gone, because nothing could be signalled safely later.
    pub fn live(pid: u32) -> Self {
        let pid = i32::try_from(pid).expect("a process ID");
        Self::now(pid).unwrap_or_else(|| panic!("process {pid} ended before it was recorded"))
    }

    pub fn pid(&self) -> i32 {
        self.pid
    }

    /// Whether the ID names the recorded process now: alive under its recorded start time.
    pub fn alive(&self) -> bool {
        matches!(seen(self.pid), Seen::Started(now) if now == self.started)
    }

    /// Whether the ID may name another process than the one recorded: another start time, no
    /// process, or no answer from `ps`.
    pub fn replaced(&self) -> bool {
        !self.alive()
    }

    /// Wait up to `within` for the recorded process to end by itself, or its ID to name
    /// another process; whether it did. Nothing is signalled.
    pub fn ended_within(&self, within: Duration) -> bool {
        let until = Instant::now() + within;
        loop {
            if self.replaced() {
                return true;
            }
            if Instant::now() >= until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// How long a dropped [`Leftovers`] waits for its processes to end by themselves.
pub const LEFTOVER_GRACE: Duration = Duration::from_secs(10);

/// Processes a test recorded but does not hold. When the guard is dropped, however the test
/// ended, each still running is given [`LEFTOVER_GRACE`] to end by itself and then reported on
/// stderr; none is signalled, because nothing but its own parent can prove the ID still names
/// it at the moment a signal lands.
#[derive(Default)]
pub struct Leftovers(pub Vec<Recorded>);

impl Drop for Leftovers {
    fn drop(&mut self) {
        let until = Instant::now() + LEFTOVER_GRACE;
        for recorded in &self.0 {
            let left = until.saturating_duration_since(Instant::now());
            if !recorded.ended_within(left) {
                eprintln!(
                    "left running: process {} (started {}) is not this test's child, so it is \
                     not signalled; its own parent ends it",
                    recorded.pid, recorded.started
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A recorded process reads alive while its ID names it; once it is reaped its ID is no
    /// longer recorded as it, and a process with another start time is never taken for it.
    /// Only its holder ends it.
    #[test]
    fn a_recorded_process_is_only_watched_and_its_holder_ends_it() {
        let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let recorded = Recorded::live(child.id());
        assert!(!recorded.replaced());
        let stranger = Recorded {
            pid: recorded.pid(),
            started: "Thu Jan  1 00:00:00 1970".into(),
        };
        assert!(stranger.replaced(), "another start time is another process");
        // Watching it signals nothing: it still runs.
        assert!(!recorded.ended_within(Duration::from_millis(100)));
        assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(recorded.ended_within(Duration::from_secs(5)));
        assert_eq!(
            Recorded::now(recorded.pid()),
            None,
            "a reaped ID records nothing"
        );
    }

    /// A process that started after its marker was written is never taken for the marker's
    /// writer, however close the two are: the guard proves the start came first, with no slack.
    #[test]
    fn a_process_started_after_its_marker_is_never_the_marker_s() {
        let marker = std::env::temp_dir().join(format!(
            "af-recorded-after-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        std::fs::write(&marker, child.id().to_string()).unwrap();
        // The marker as if written a moment before this process started, within the same
        // second: a recycled ID's new process.
        let before = SystemTime::now() - Duration::from_millis(300);
        std::fs::File::options()
            .write(true)
            .open(&marker)
            .unwrap()
            .set_modified(before)
            .unwrap();
        assert_eq!(Recorded::from_marker(&marker), None);
        child.kill().unwrap();
        child.wait().unwrap();
        std::fs::remove_file(&marker).unwrap();
    }

    /// A marker names its writer only while the process under its ID started before it was
    /// written.
    #[test]
    fn a_marker_names_a_process_that_started_before_it_was_written() {
        let marker = std::env::temp_dir().join(format!(
            "af-recorded-marker-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        std::fs::write(&marker, child.id().to_string()).unwrap();
        // Proven only once its start lies a whole `ps` second before the marker.
        std::thread::sleep(Duration::from_millis(2_100));
        std::fs::write(&marker, child.id().to_string()).unwrap();
        let recorded = Recorded::from_marker(&marker).expect("the marker's writer runs");
        assert_eq!(recorded, Recorded::live(child.id()));
        // The same ID in a marker written long before this process started is not this process.
        let long_ago = SystemTime::now() - Duration::from_secs(3_600);
        std::fs::File::options()
            .write(true)
            .open(&marker)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        assert_eq!(Recorded::from_marker(&marker), None);
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(
            Recorded::from_marker(&marker),
            None,
            "a reaped ID records nothing"
        );
        std::fs::remove_file(&marker).unwrap();
    }
}
