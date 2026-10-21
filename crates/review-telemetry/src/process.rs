//! Process spans (design §7.2): every process `review-process` supervises gets a `process` span,
//! recorded by the crate that receives its result — `review-runner` (`af.harness`),
//! `review-check` (`af.check`), `review-source-git` (`af.git`), and `review-sandbox` and
//! `review-pipeline` (`af.check` through a container, `af.process` for the rest).
//! `review-process` stays the dependency-neutral leaf of ADR-0026 and links nothing of this
//! crate: the recording crates map its result into [`ProcessFacts`].
//!
//! [`process_span!`](crate::process_span) declares every field the end records, so a site
//! names only its own attributes; [`finish`] records the facts and the end status. A figure the
//! platform did not deliver is absent from the span, never written as zero.

/// What a supervised process used, as the supervisor delivered it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResourceUse {
    pub user_ms: u64,
    pub system_ms: u64,
    /// Absent when the platform did not deliver the process's own peak.
    pub max_rss_bytes: Option<u64>,
}

/// What a process span records at its end.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessFacts {
    /// `process.pid`; absent when nothing was started.
    pub pid: Option<u32>,
    /// `process.exit_code`: the code the process exited with, absent when it did not exit.
    pub exit_code: Option<i32>,
    /// `process.signal`: the signal that ended it, when one did.
    pub signal: Option<i32>,
    /// `af.rusage.*`; absent when the platform delivered nothing.
    pub rusage: Option<ResourceUse>,
    /// `af.stdout_bytes` and `af.stderr_bytes`: what the supervisor collected.
    pub stdout_bytes: Option<u64>,
    pub stderr_bytes: Option<u64>,
    /// `af.stderr_held`: a descendant held stderr past the drain grace (ADR-0072).
    pub stderr_held: bool,
}

impl ProcessFacts {
    /// The exit code or signal of an exit status.
    pub fn with_status(mut self, status: &std::process::ExitStatus) -> Self {
        use std::os::unix::process::ExitStatusExt;
        self.exit_code = status.code();
        self.signal = status.signal();
        self
    }
}

/// Open a process span: `process_span!("af.git", process.command = "git", af.git.op = op)`.
/// The span is of kind `process` and declares every attribute [`finish`] records, the end
/// status and the failure class and reason among them.
#[macro_export]
macro_rules! process_span {
    ($name:literal $(, $($fields:tt)*)?) => {
        $crate::__tracing::info_span!(
            $name,
            af.kind = "process",
            process.pid = $crate::__tracing::field::Empty,
            process.exit_code = $crate::__tracing::field::Empty,
            process.signal = $crate::__tracing::field::Empty,
            af.rusage.user_ms = $crate::__tracing::field::Empty,
            af.rusage.system_ms = $crate::__tracing::field::Empty,
            af.rusage.max_rss_bytes = $crate::__tracing::field::Empty,
            af.stdout_bytes = $crate::__tracing::field::Empty,
            af.stderr_bytes = $crate::__tracing::field::Empty,
            af.stderr_held = $crate::__tracing::field::Empty,
            af.outcome = $crate::__tracing::field::Empty,
            af.failure.class = $crate::__tracing::field::Empty,
            af.failure.reason = $crate::__tracing::field::Empty,
            error.type = $crate::__tracing::field::Empty,
            af.status = $crate::__tracing::field::Empty,
            $($($fields)*)?
        )
    };
}

/// Record a process's facts on its span and end it `ok`, or `error` with the kernel's failure
/// class (`error.type` equal to it) and reason. Nothing is recorded on a disabled span.
pub fn finish(span: &tracing::Span, facts: &ProcessFacts, failure: Option<(&str, &str)>) {
    if span.is_disabled() {
        return;
    }
    if let Some(pid) = facts.pid {
        span.record("process.pid", i64::from(pid));
    }
    if let Some(code) = facts.exit_code {
        span.record("process.exit_code", i64::from(code));
    }
    if let Some(signal) = facts.signal {
        span.record("process.signal", i64::from(signal));
    }
    if let Some(rusage) = facts.rusage {
        span.record("af.rusage.user_ms", rusage.user_ms);
        span.record("af.rusage.system_ms", rusage.system_ms);
        if let Some(bytes) = rusage.max_rss_bytes {
            span.record("af.rusage.max_rss_bytes", bytes);
        }
    }
    if let Some(bytes) = facts.stdout_bytes {
        span.record("af.stdout_bytes", bytes);
    }
    if let Some(bytes) = facts.stderr_bytes {
        span.record("af.stderr_bytes", bytes);
    }
    span.record("af.stderr_held", facts.stderr_held);
    outcome(span, failure);
}

/// End a span `ok` (`af.outcome = succeeded`), or `error` with the kernel's failure class and
/// reason (`af.outcome = failed`, `error.type` equal to the class).
pub fn outcome(span: &tracing::Span, failure: Option<(&str, &str)>) {
    if span.is_disabled() {
        return;
    }
    match failure {
        None => {
            span.record("af.outcome", "succeeded");
            span.record("af.status", "ok");
        }
        Some((class, reason)) => {
            span.record("af.outcome", "failed");
            span.record("af.status", "error");
            span.record("af.failure.class", class);
            span.record("error.type", class);
            span.record("af.failure.reason", reason);
        }
    }
}

/// Whether a phase opened now belongs to a run ([`crate::subscriber::in_run`]): a phase outside
/// one (before the run opens, or on a thread no span was handed to) is not recorded. `false`
/// at `off`, where nothing is opened.
pub fn in_run() -> bool {
    crate::subscriber::in_run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exit_status_gives_a_code_or_a_signal() {
        use std::os::unix::process::ExitStatusExt;
        let exited =
            ProcessFacts::default().with_status(&std::process::ExitStatus::from_raw(3 << 8));
        assert_eq!((exited.exit_code, exited.signal), (Some(3), None));
        let killed = ProcessFacts::default().with_status(&std::process::ExitStatus::from_raw(9));
        assert_eq!((killed.exit_code, killed.signal), (None, Some(9)));
    }
}
