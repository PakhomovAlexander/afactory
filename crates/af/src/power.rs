//! A power assertion while af runs Task work (ADR-0200). On macOS `af task run` and
//! `af task start --execute` start `/usr/bin/caffeinate -i -s -w <af's pid>`, so the machine does
//! not idle-sleep (and, on AC power, does not sleep) while a Worker runs; `caffeinate` ends by
//! itself when af ends, however af ends. It runs in its own process group, so a Ctrl-C meant for
//! af does not stop it before af has stopped its Workers. Elsewhere, or without `caffeinate`,
//! nothing is started. The workspace forbids `unsafe`, so af asks `caffeinate` for the
//! `PreventUserIdleSystemSleep` assertion instead of calling IOKit itself.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// The one program af starts for it, by absolute path: never one found on `PATH`.
const CAFFEINATE: &str = "/usr/bin/caffeinate";

/// The power assertion af requests for process `pid` on operating system `os`, or None where it
/// requests none.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub program: PathBuf,
    pub args: Vec<String>,
}

pub(crate) fn request(os: &str, caffeinate: Option<&Path>, pid: u32) -> Option<Request> {
    if os != "macos" {
        return None;
    }
    Some(Request {
        program: caffeinate?.to_path_buf(),
        // -i: no idle sleep; -s: no system sleep on AC power; -w: until `pid` exits.
        args: ["-i", "-s", "-w"]
            .into_iter()
            .map(str::to_owned)
            .chain([pid.to_string()])
            .collect(),
    })
}

/// The held assertion: dropping it ends `caffeinate` at once, which would otherwise end within
/// a second of af.
pub(crate) struct Held(Child);

impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = review_process::reap::wait(&mut self.0);
    }
}

/// What `hold` has to say under `verbose`, until [`say`] prints it: after the run's first line,
/// which says what the run does with an interrupted node (ADR-0200).
static NOTE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The widest line af prints here; a longer one continues on lines indented by two spaces.
const COLUMNS: usize = 100;

/// `text` wrapped at word boundaries to [`COLUMNS`], each continuation line indented by two
/// spaces. A single word longer than a line stays whole.
pub(crate) fn wrapped(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split(' ').filter(|word| !word.is_empty()) {
        let indent = if lines.is_empty() { "" } else { "  " };
        if !line.is_empty() && indent.len() + line.len() + 1 + word.len() > COLUMNS {
            lines.push(format!("{indent}{line}"));
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    let indent = if lines.is_empty() { "" } else { "  " };
    lines.push(format!("{indent}{line}"));
    lines.join("\n")
}

/// Print, once, what [`hold`] noted under `verbose`.
pub(crate) fn say() {
    let note = NOTE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(note) = note {
        eprintln!("{note}");
    }
}

fn note(text: String) {
    *NOTE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(wrapped(&text));
}

/// Hold the assertion for the rest of this command, and note it under `verbose`, for [`say`]
/// to print after the run's first line. A `caffeinate` that cannot start is no reason to refuse
/// the work: the lease and the recovery of a lost writer still hold, so af says so under
/// `verbose` and runs on.
pub(crate) fn hold(verbose: bool) -> Option<Held> {
    let pid = std::process::id();
    let caffeinate = Path::new(CAFFEINATE);
    let request = request(
        std::env::consts::OS,
        caffeinate.is_file().then_some(caffeinate),
        pid,
    )?;
    let started = Command::new(&request.program)
        .args(&request.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    match started {
        Ok(child) => {
            if verbose {
                note(format!(
                    "af: holding a power assertion while this Task runs ({} {}): the Mac does \
                     not idle-sleep, nor sleep on AC power",
                    request.program.display(),
                    request.args.join(" ")
                ));
            }
            Some(Held(child))
        }
        Err(error) => {
            if verbose {
                note(format!(
                    "af: no power assertion ({} could not start: {error}); a sleep longer than \
                     the Task's lease ends this run, and `af task run TASK_ID` resumes it",
                    request.program.display()
                ));
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The power line and the resume line, as long as they get, fit in 100 columns.
    #[test]
    fn long_lines_wrap_at_100_columns_with_an_indent() {
        let text = "af: holding a power assertion while this Task runs (/usr/bin/caffeinate -i \
                    -s -w 68588): the Mac does not idle-sleep, nor sleep on AC power";
        let lines = wrapped(text);
        assert_eq!(
            lines,
            "af: holding a power assertion while this Task runs (/usr/bin/caffeinate -i -s -w \
             68588): the Mac\n  does not idle-sleep, nor sleep on AC power"
        );
        assert!(lines.lines().all(|line| line.len() <= COLUMNS));
        assert_eq!(lines.split_whitespace().collect::<Vec<_>>().join(" "), text);
        assert_eq!(super::wrapped("short"), "short");
    }

    #[test]
    fn macos_requests_caffeinate_for_this_process_and_every_other_system_requests_nothing() {
        let caffeinate = Path::new("/usr/bin/caffeinate");
        assert_eq!(
            request("macos", Some(caffeinate), 4242),
            Some(Request {
                program: caffeinate.to_path_buf(),
                args: vec!["-i".into(), "-s".into(), "-w".into(), "4242".into()],
            })
        );
        assert_eq!(
            request("macos", None, 4242),
            None,
            "no caffeinate, no request"
        );
        for os in ["linux", "freebsd", "windows"] {
            assert_eq!(request(os, Some(caffeinate), 4242), None, "{os}");
        }
    }

    /// On this machine: a Mac holds the assertion as a live `caffeinate` child, and any other
    /// system starts nothing.
    #[test]
    fn hold_starts_caffeinate_only_on_a_mac() {
        let held = hold(false);
        if cfg!(target_os = "macos") && Path::new(CAFFEINATE).is_file() {
            let mut held = held.expect("a Mac holds the assertion");
            assert!(held.0.try_wait().unwrap().is_none(), "caffeinate runs");
        } else {
            assert!(held.is_none());
        }
    }
}
