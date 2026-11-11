//! Provider-neutral supervision for a model-backed reviewer process.
//!
//! A model CLI is a subprocess like any other, with three extra hazards the `command` adapter
//! never has: it can hang (a stuck stream, a provider outage), it needs a credential, and its
//! output is expensive enough that losing it to a parse failure must never lose the bytes.
//! This module owns exactly those three:
//!
//! - **A deadline, enforced by killing.** A reviewer that has not answered by the deadline is
//!   killed and reported [`RunnerError::TimedOut`]. Retrying is *not* done here — the kernel
//!   owns retries, because a retry is a new attempt that must fence its predecessor and
//!   reserve its own budget.
//! - **Grants, not inheritance.** The child's environment is rebuilt from scratch; a credential
//!   reaches it only as an explicit [`Grant`]. Every grant's value is redacted from everything
//!   this module stores or reports — a model CLI that echoes its environment into an error
//!   message must not turn the event log into a credential store.
//! - **The bytes survive.** Redacted stdout is stored to the CAS before any parsing is
//!   attempted, so "the model returned garbage" is always an inspectable claim.
//!
//! What this module deliberately does not do: parse. A provider's output framing (Codex JSONL,
//! some other envelope) is the provider adapter's job, behind
//! [`WorkerModelAdapter`](crate::task::WorkerModelAdapter).

use std::path::{Path, PathBuf};
use std::time::Duration;

use af_contracts::Command;
use af_store::Cas;

use af_process::{
    ExitPolicy, MAX_CAPTURED_STREAM_BYTES, SupervisedError,
    run_supervised_captured_bounded_observed, sanitized_path,
};

use crate::native_failure::{NativeFailureKind, classify_native_failure};
use crate::stream::{HarnessStream, Observation};

/// Why a supervised process yielded no capture, or an input could not be composed. Each is a
/// typed outcome, never an empty result: a Worker that crashed and a Worker that found nothing
/// must never be indistinguishable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerError {
    /// The command was refused before execution — an untrusted value in an option position.
    Refused(String),
    /// The program could not be started at all.
    Unavailable(String),
    /// The reviewer ran and failed.
    Failed {
        exit_code: i32,
        stderr_excerpt: String,
    },
    /// The reviewer did not answer by its deadline and was killed. Whatever it spent is gone;
    /// whether to retry is the kernel's decision, not this layer's.
    TimedOut { after_ms: u64 },
}

impl std::fmt::Display for RunnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunnerError::Refused(why) => write!(f, "reviewer command refused: {why}"),
            RunnerError::Unavailable(why) => write!(f, "reviewer unavailable: {why}"),
            RunnerError::Failed {
                exit_code,
                stderr_excerpt,
            } => write!(f, "reviewer failed (exit {exit_code}): {stderr_excerpt}"),
            RunnerError::TimedOut { after_ms } => {
                write!(f, "reviewer {TIMED_OUT} {after_ms}ms and was killed")
            }
        }
    }
}

impl std::error::Error for RunnerError {}

/// The words of a timeout after the Worker's name.
const TIMED_OUT: &str = "did not answer within";

/// Whether a Worker failure `message` says this layer killed the Worker at its deadline
/// (`… did not answer within 500ms and was killed`), wherever an adapter wrapped it.
pub fn names_timed_out(message: &str) -> bool {
    message.contains(&format!(" {TIMED_OUT} ")) && message.contains("ms and was killed")
}

/// A Worker failure `message` that names the Worker of a Task slot: a timeout this layer
/// reported for `reviewer`, Review's own Worker, names `worker` instead (`implementer did not
/// answer within …`). Every other message is returned unchanged.
pub fn name_timed_out_worker(message: &str, worker: &str) -> String {
    message.replacen(
        &format!("reviewer {TIMED_OUT} "),
        &format!("{worker} {TIMED_OUT} "),
        1,
    )
}

#[cfg(test)]
mod tests {
    use super::{note_cleanup_fallback, stream_totals};

    /// A Task slot's timeout names its own Worker, wherever the adapter wrapped the message;
    /// any other failure is left as it was.
    #[test]
    fn a_timeout_names_the_slot_that_did_not_answer() {
        let timed_out = super::RunnerError::TimedOut {
            after_ms: 10_797_336,
        };
        assert_eq!(
            super::name_timed_out_worker(
                &format!("Claude Worker ended ({timed_out})"),
                "implementer"
            ),
            "Claude Worker ended (implementer did not answer within 10797336ms and was killed)"
        );
        let failed = super::RunnerError::Failed {
            exit_code: 1,
            stderr_excerpt: "boom".into(),
        }
        .to_string();
        assert_eq!(super::name_timed_out_worker(&failed, "implementer"), failed);
        assert!(super::names_timed_out(&format!(
            "Claude Worker ended ({timed_out})"
        )));
        assert!(super::names_timed_out(&super::name_timed_out_worker(
            &timed_out.to_string(),
            "implementer"
        )));
        assert!(!super::names_timed_out(&failed));
        assert!(!super::names_timed_out(
            "Provider network request failed: request timed out"
        ));
    }

    /// A stream the output cap truncated ends where its drain read, not where the kept prefix
    /// ends: the tap's gap protocol compares the observed total (design §7.3).
    #[test]
    fn stream_totals_are_what_the_drains_read() {
        use std::os::unix::process::ExitStatusExt;
        let mut output = af_process::SupervisedCapture {
            status: Ok(std::process::ExitStatus::from_raw(0)),
            stdout: vec![b'x'; 4096],
            stderr: b"err\n".to_vec(),
            stderr_held: false,
            pid: None,
            rusage: None,
            truncation: af_process::Truncation {
                stdout: None,
                stderr: None,
                cap: 4096,
            },
        };
        assert_eq!(stream_totals(&output), (4096, 4));
        output.truncation.stdout = Some(10_000);
        assert_eq!(stream_totals(&output), (10_000, 4));
        output.truncation.stderr = Some(5_000);
        assert_eq!(stream_totals(&output), (10_000, 5_000));
    }

    /// V4 of the security close-out: an Attempt that ran while af could not become a child
    /// subreaper records, in its stored stderr, that descendant cleanup fell back to the
    /// process-group kill; with the subreaper set nothing is added.
    #[test]
    fn an_attempt_records_that_descendant_cleanup_fell_back() {
        let mut stderr = b"worker said".to_vec();
        note_cleanup_fallback(&mut stderr, None);
        assert_eq!(stderr, b"worker said");
        note_cleanup_fallback(
            &mut stderr,
            Some("descendant cleanup fell back to the process-group kill: EPERM"),
        );
        assert_eq!(
            stderr,
            b"worker said\ndescendant cleanup fell back to the process-group kill: EPERM\n"
        );
    }
}

/// A credential granted to the reviewer process by name and value. The value is what gets
/// scrubbed from captured output.
#[derive(Debug, Clone)]
pub struct Grant {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextEntry {
    pub name: String,
    pub required_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_type: Option<String>,
    pub rendered_bytes: u64,
    pub estimated_tokens: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextManifest {
    pub entries: Vec<ContextEntry>,
    pub rendered_bytes: u64,
    pub estimated_tokens: u64,
}

impl ContextManifest {
    pub fn record(
        &mut self,
        name: impl Into<String>,
        required_by: impl Into<String>,
        artifact_id: Option<String>,
        artifact_type: Option<String>,
        rendered_bytes: usize,
    ) {
        self.entries.push(ContextEntry {
            name: name.into(),
            required_by: required_by.into(),
            artifact_id,
            artifact_type,
            rendered_bytes: rendered_bytes as u64,
            estimated_tokens: estimate_tokens(rendered_bytes),
        });
    }

    pub fn finish(&mut self, rendered_bytes: usize) {
        self.rendered_bytes = rendered_bytes as u64;
        self.estimated_tokens = estimate_tokens(rendered_bytes);
    }
}

pub fn estimate_tokens(rendered_bytes: usize) -> u64 {
    (rendered_bytes as u64).div_ceil(4)
}

/// Process evidence remains available for usage accounting even when the deadline or CAS
/// fails. A failed status never authorizes a business output, including a complete message
/// printed before a timeout. Raw bytes have already had credential grants redacted.
pub struct SettledCapture {
    /// False when the program could not be started at all: it received no input.
    pub started: bool,
    pub status: Result<std::process::ExitStatus, RunnerError>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub raw_artifact_ids: Vec<String>,
}

pub struct ModelRunner {
    workdir: PathBuf,
    timeout: Duration,
    grants: Vec<Grant>,
    /// Every spelling of a granted value scrubbed from what is stored, longest first.
    redactions: Vec<String>,
    environment: Vec<Grant>,
    exit_policy: ExitPolicy,
    /// What the `af.harness` span names: the GenAI provider and the bound model.
    harness: Option<(&'static str, Option<String>)>,
    output_cap: usize,
}

impl ModelRunner {
    pub fn new(workdir: impl AsRef<Path>, timeout: Duration) -> ModelRunner {
        ModelRunner {
            workdir: workdir.as_ref().to_path_buf(),
            timeout,
            grants: Vec::new(),
            redactions: Vec::new(),
            environment: Vec::new(),
            exit_policy: ExitPolicy::PreserveProcessGroup,
            harness: None,
            output_cap: MAX_CAPTURED_STREAM_BYTES,
        }
    }

    /// Name the harness on its `af.harness` span (design §7.1): the GenAI provider
    /// (`gen_ai.provider.name`, `anthropic` or `openai`) and the model the binding requested.
    /// Telemetry only: nothing about the process changes.
    pub fn with_harness(mut self, provider: &'static str, model: Option<String>) -> Self {
        self.harness = Some((provider, model));
        self
    }

    /// Keep at most `cap` bytes of each stream instead of
    /// [`MAX_CAPTURED_STREAM_BYTES`]. A process that writes more fails its Attempt either way.
    pub fn with_output_cap(mut self, cap: usize) -> Self {
        self.output_cap = cap;
        self
    }

    /// End the whole process group when the model process exits, as a check does, and on Linux
    /// every descendant that left it (`ExitPolicy::KillDescendants`). An Attempt that may run a
    /// shell sets this so no background child outlives it and keeps writing into the sandbox
    /// the kernel is about to seal; deadline and cancellation already kill the group.
    pub fn killing_process_group_on_exit(mut self) -> Self {
        self.exit_policy = ExitPolicy::KillDescendants;
        self
    }

    /// Grant one credential to the child. The value never appears in anything stored: it is
    /// scrubbed from stdout and stderr before either is kept or quoted. A value that is an
    /// absolute path is scrubbed in its canonical spelling too (`/var/…` and `/private/var/…`
    /// on macOS, a symbolic link resolved), since a CLI may print the path it resolved.
    pub fn with_grant(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        let value = value.into();
        let path = Path::new(&value);
        let canonical = path
            .is_absolute()
            .then(|| std::fs::canonicalize(path).ok())
            .flatten()
            .and_then(|canonical| canonical.to_str().map(str::to_owned));
        for spelling in std::iter::once(value.clone()).chain(canonical) {
            if !spelling.is_empty() && !self.redactions.contains(&spelling) {
                self.redactions.push(spelling);
            }
        }
        self.redactions
            .sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        self.grants.push(Grant {
            name: name.into(),
            value,
        });
        self
    }

    /// Set non-secret process context without treating ordinary text such as a username or
    /// home path as credential material to redact from reviewer findings.
    pub fn with_env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.environment.push(Grant {
            name: name.into(),
            value: value.into(),
        });
        self
    }

    /// Run a command with its input on stdin, outside argv's platform-sized ceiling, until it
    /// exits, reaches its deadline or observes cooperative cancellation. Redacted stdout and
    /// stderr are stored to the CAS before this returns, so even a failure leaves the bytes
    /// inspectable; a storage failure refuses the output without erasing usage the adapter
    /// can still decode from stdout.
    pub fn capture(
        &self,
        cas: &Cas,
        command: &Command,
        input: Vec<u8>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> SettledCapture {
        self.capture_filtered(cas, command, input, cancellation, |_, _, _| {})
    }

    /// Parse accounting and remove sensitive native failure output before CAS publication.
    /// The filter receives grant-redacted streams and whether the transport failed. It cannot
    /// alter process status, deadline, accounting authority or whether the process started.
    pub fn capture_filtered(
        &self,
        cas: &Cas,
        command: &Command,
        input: Vec<u8>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
        filter: impl FnOnce(bool, &mut Vec<u8>, &mut Vec<u8>),
    ) -> SettledCapture {
        let mut capture = self.capture_process(command, input, cancellation);
        filter(
            !capture.status.as_ref().is_ok_and(|status| status.success()),
            &mut capture.stdout,
            &mut capture.stderr,
        );
        self.store_raw(cas, &mut capture);
        capture
    }

    /// [`Self::capture_filtered`] for a harness whose streams Telemetry taps (design §7.3): at
    /// `spans` and above the supervisor's drains offer every chunk to Telemetry's bounded
    /// observer as it is read, so its turns and tool calls are recorded live, timed by arrival.
    /// The offer never blocks and a full queue drops the chunk, so the process, its streams,
    /// its status, deadline and accounting, the filter and what the CAS keeps are the same as
    /// [`Self::capture_filtered`]'s, at every level. Once the process has ended and before any
    /// pending byte of it is published, Telemetry takes its own release decision over the
    /// complete streams ([`af_telemetry::stream::Release`]): the streams are withheld when
    /// either carries login-challenge material, when the filter replaced both (the adapter's
    /// whole-capture privacy filter), when a failed process's stderr classifies as an
    /// authentication failure, or when the output cap truncated a stream, so that the decision
    /// holds a prefix where it needs the whole; `af.harness` records `af.stream.withheld`, and
    /// with the observer's `dropped_chunks` and `gaps` it ends once the filter has read the
    /// streams.
    pub fn capture_observed(
        &self,
        cas: &Cas,
        command: &Command,
        input: Vec<u8>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
        stream: &HarnessStream,
        filter: impl FnOnce(bool, &mut Vec<u8>, &mut Vec<u8>),
    ) -> SettledCapture {
        self.capture_observed_with_output(
            cas,
            command,
            input,
            cancellation,
            stream,
            |failed, stdout, stderr, _| filter(failed, stdout, stderr),
        )
    }

    /// [`Self::capture_observed`] for a harness that also leaves output outside its streams,
    /// read by the filter once the process has ended (Codex's `-o` final-message file): every
    /// byte the filter hands to [`FurtherOutput::include`] is part of Telemetry's release
    /// decision, so login-challenge material there withholds the process's pending content as
    /// it would in a stream, and so does a further output the filter could not read whole
    /// ([`FurtherOutput::unjudgeable`]). The decision is taken before the harness tap ends;
    /// nothing about the process, the filter, classification or accounting changes.
    pub fn capture_observed_with_output(
        &self,
        cas: &Cas,
        command: &Command,
        input: Vec<u8>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
        stream: &HarnessStream,
        filter: impl FnOnce(bool, &mut Vec<u8>, &mut Vec<u8>, &mut FurtherOutput),
    ) -> SettledCapture {
        let (mut capture, harness) = self.supervise(command, input, cancellation, Some(stream));
        let failed = !capture.status.as_ref().is_ok_and(|status| status.success());
        // What the decision reads, over the complete streams, before the filter changes them;
        // read only for a process Telemetry observed, so nothing is paid for it below `spans`.
        // A stream the output cap truncated is not complete: the record says so, below.
        let observed = harness.as_ref().is_some_and(HarnessRecord::observed);
        let withheld_before = observed
            && (af_telemetry::redact::carries_auth_material(&capture.stdout)
                || af_telemetry::redact::carries_auth_material(&capture.stderr)
                || failed
                    && classify_native_failure(&String::from_utf8_lossy(&capture.stderr))
                        .is_auth());
        let mut further = FurtherOutput {
            observed,
            auth: false,
            unjudged: false,
        };
        filter(
            failed,
            &mut capture.stdout,
            &mut capture.stderr,
            &mut further,
        );
        if let Some(harness) = harness {
            let replaced = NativeFailureKind::replaced_capture(&capture.stdout, &capture.stderr);
            // The supervisor keeps a prefix of a stream the output cap truncated while the
            // observer was offered it whole: the decision cannot judge the process's output, so
            // its pending content is withheld, as for a further output not read whole.
            let truncated = harness.truncated();
            harness.finish(Some(Released {
                withheld: withheld_before
                    || further.auth
                    || further.unjudged
                    || replaced
                    || truncated,
            }));
        }
        self.store_raw(cas, &mut capture);
        capture
    }

    /// Store the redacted streams in the CAS, in order; a storage failure refuses the output.
    fn store_raw(&self, cas: &Cas, capture: &mut SettledCapture) {
        for bytes in [&capture.stdout, &capture.stderr] {
            if bytes.is_empty() {
                continue;
            }
            match cas.put(bytes) {
                Ok(id) => capture.raw_artifact_ids.push(id),
                Err(error) => {
                    capture.status = Err(RunnerError::Unavailable(format!(
                        "storing raw output: {error}"
                    )));
                }
            }
        }
    }

    fn capture_process(
        &self,
        command: &Command,
        input: Vec<u8>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> SettledCapture {
        let (capture, harness) = self.supervise(command, input, cancellation, None);
        if let Some(harness) = harness {
            harness.finish(None);
        }
        capture
    }

    /// Run the process under its `af.harness` span, observed for Telemetry when `stream` names
    /// its harness and Telemetry records streams; the span is returned open with what the
    /// supervisor delivered, for the caller to end.
    fn supervise(
        &self,
        command: &Command,
        input: Vec<u8>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
        stream: Option<&HarnessStream>,
    ) -> (SettledCapture, Option<HarnessRecord>) {
        let mut capture = SettledCapture {
            started: false,
            status: Err(RunnerError::Refused("unresolved command".into())),
            stdout: vec![],
            stderr: vec![],
            raw_artifact_ids: vec![],
        };
        let argv = command
            .resolve()
            .map_err(|e| RunnerError::Refused(e.to_string()));
        let argv = match argv {
            Ok(argv) => argv,
            Err(error) => {
                capture.status = Err(error);
                return (capture, None);
            }
        };

        let mut cmd = std::process::Command::new(&command.program);
        cmd.args(&argv);
        cmd.current_dir(&self.workdir);
        cmd.env_clear();
        cmd.env("PATH", sanitized_path());
        cmd.env("HOME", &self.workdir);
        cmd.env("LC_ALL", "C");
        for variable in &self.environment {
            cmd.env(&variable.name, &variable.value);
        }
        for grant in &self.grants {
            cmd.env(&grant.name, &grant.value);
        }
        // The harness process is the Attempt's `af.harness` span (design §7.1, §7.2): the
        // program only, never its arguments; its stream-derived children are the tap's.
        let (provider, model) = self
            .harness
            .as_ref()
            .map_or((None, None), |(provider, model)| {
                (Some(*provider), model.as_deref())
            });
        let span = if !af_telemetry::subscriber::in_run() {
            tracing::Span::none()
        } else {
            af_telemetry::process_span!(
                "af.harness",
                process.command = command.program.as_str(),
                gen_ai.operation.name =
                    af_telemetry::subscriber::Typed::enumerated("invoke_agent").field(),
                gen_ai.provider.name = provider,
                gen_ai.request.model = model,
                af.native = af_telemetry::subscriber::Typed::enumerated("unavailable").field(),
                dropped_chunks = tracing::field::Empty,
                gaps = tracing::field::Empty,
                af.stream.withheld = tracing::field::Empty,
                af.native.withheld = tracing::field::Empty,
            )
        };
        let observation = stream.and_then(|stream| {
            Observation::start(
                &span,
                stream,
                provider.unwrap_or("unknown"),
                model.map(str::to_string),
                self.grants
                    .iter()
                    .map(|grant| (grant.name.clone(), grant.value.clone()))
                    .collect(),
            )
        });
        let output = span.in_scope(|| {
            run_supervised_captured_bounded_observed(
                &mut cmd,
                Some(input),
                self.timeout,
                self.exit_policy,
                cancellation,
                observation.as_ref().map(Observation::observer),
                self.output_cap,
            )
        });
        if let Some(observation) = &observation {
            observation.close();
        }
        // Output past the cap is not evidence the kernel can keep whole: whatever the exit
        // status said, the Attempt failed (M2), and its span ends failed with the cap's reason.
        let capped = match (&output.status, output.truncation.reason()) {
            (Ok(status), Some(reason)) => Some((status.code().unwrap_or(-1), reason)),
            _ => None,
        };
        let harness = HarnessRecord::new(
            span,
            &output,
            observation,
            capped.as_ref().map(|(_, reason)| reason.as_str()),
        );
        capture.stdout = redact(output.stdout, &self.redactions);
        capture.stderr = redact(output.stderr, &self.redactions);
        if output.stderr_held {
            capture
                .stderr
                .extend_from_slice(b"\nstderr was still held after 5 seconds\n");
        }
        capture.started = !matches!(output.status, Err(SupervisedError::Spawn(_)));
        if capture.started && self.exit_policy == ExitPolicy::KillDescendants {
            note_cleanup_fallback(
                &mut capture.stderr,
                af_process::descendant_cleanup_fallback(),
            );
        }
        // The kept prefix of a capped capture is still stored for usage.
        if let Some((exit_code, reason)) = capped {
            capture.status = Err(RunnerError::Failed {
                exit_code,
                stderr_excerpt: reason,
            });
            return (capture, Some(harness));
        }
        capture.status = output.status.map_err(|error| match error {
            SupervisedError::TimedOut { .. } => RunnerError::TimedOut {
                after_ms: self.timeout.as_millis() as u64,
            },
            SupervisedError::Spawn(error) => {
                RunnerError::Unavailable(format!("{}: {error}", command.program))
            }
            error => RunnerError::Failed {
                exit_code: -1,
                stderr_excerpt: error.to_string(),
            },
        });
        (capture, Some(harness))
    }
}

/// Harness output outside the streams that Telemetry's release decision reads
/// ([`ModelRunner::capture_observed_with_output`]).
pub struct FurtherOutput {
    /// Whether Telemetry observed the process: nothing is read for one it did not.
    observed: bool,
    /// Whether any included output carries login-challenge material.
    auth: bool,
    /// Whether a further output could not be read whole, so the decision cannot judge it.
    unjudged: bool,
}

impl FurtherOutput {
    /// Whether Telemetry observed the process, so its release decision reads further output.
    pub fn observed(&self) -> bool {
        self.observed
    }

    /// Include the complete `bytes` of one further output in the release decision.
    pub fn include(&mut self, bytes: &[u8]) {
        self.auth =
            self.auth || self.observed && af_telemetry::redact::carries_auth_material(bytes);
    }

    /// A further output that exists but could not be read whole (too large, unreadable, not a
    /// regular file): what it holds cannot be judged, so the process's pending content is
    /// withheld as it is for login-challenge material (design §7.3).
    pub fn unjudgeable(&mut self) {
        self.unjudged = self.unjudged || self.observed;
    }
}

/// What Telemetry's release decision came to, for the `af.harness` span.
struct Released {
    withheld: bool,
}

/// An `af.harness` span still open, with what the supervisor delivered, and the observation
/// of its streams when there is one.
struct HarnessRecord {
    span: tracing::Span,
    facts: af_telemetry::process::ProcessFacts,
    failure: Option<(&'static str, String)>,
    /// The observation, with the bytes each stream carried in total ([`stream_totals`]).
    observation: Option<(Observation, u64, u64)>,
    /// Whether the output cap truncated a stream: the capture is a prefix of what the observer
    /// was offered.
    truncated: bool,
}

impl HarnessRecord {
    fn new(
        span: tracing::Span,
        output: &af_process::SupervisedCapture,
        observation: Option<Observation>,
        capped: Option<&str>,
    ) -> Self {
        let (facts, failure) = harness_facts(output, capped);
        let (stdout_bytes, stderr_bytes) = stream_totals(output);
        let observation = observation.map(|observation| (observation, stdout_bytes, stderr_bytes));
        Self {
            span,
            facts,
            failure,
            observation,
            truncated: output.truncation.is_truncated(),
        }
    }

    /// Whether Telemetry observed the process's streams.
    fn observed(&self) -> bool {
        self.observation.is_some()
    }

    /// Whether the output cap truncated a stream, so the release decision holds a prefix where
    /// it needs the whole.
    fn truncated(&self) -> bool {
        self.truncated
    }

    /// End the span: the process's facts, and for an observed process the observer's drops and
    /// gaps and the release decision, which is handed to the consumer for the tap.
    fn finish(self, released: Option<Released>) {
        if let Some((observation, stdout_bytes, stderr_bytes)) = &self.observation {
            let withheld = released.as_ref().is_some_and(|released| released.withheld);
            let observer = observation.observer();
            if !self.span.is_disabled() {
                self.span
                    .record("dropped_chunks", observer.dropped_chunks());
                self.span.record("gaps", observer.gaps());
                self.span.record("af.stream.withheld", withheld);
                self.span.record("af.native.withheld", withheld);
            }
            observation.release(af_telemetry::stream::Release {
                withheld,
                native_withheld: withheld,
                stdout_bytes: *stdout_bytes,
                stderr_bytes: *stderr_bytes,
            });
        }
        af_telemetry::process::finish(
            &self.span,
            &self.facts,
            self.failure
                .as_ref()
                .map(|(class, reason)| (*class, reason.as_str())),
        );
    }
}

/// The bytes each stream carried in total, where the observed stream ends for the tap's gap
/// protocol: what the supervisor kept, or, for a stream the output cap truncated, what its
/// drain read in all, every byte of which the observer was offered.
fn stream_totals(output: &af_process::SupervisedCapture) -> (u64, u64) {
    (
        output
            .truncation
            .stdout
            .unwrap_or(output.stdout.len() as u64),
        output
            .truncation
            .stderr
            .unwrap_or(output.stderr.len() as u64),
    )
}

/// What the `af.harness` span records of what the supervisor returned: the process, its resource
/// use and stream sizes, and on anything but a clean exit the kernel's class. `capped` is the cap
/// reason when the Attempt rejected the capture for its truncation: the span ends `failed`
/// with it, whatever the exit status said.
fn harness_facts(
    output: &af_process::SupervisedCapture,
    capped: Option<&str>,
) -> (
    af_telemetry::process::ProcessFacts,
    Option<(&'static str, String)>,
) {
    let mut facts = af_telemetry::process::ProcessFacts {
        pid: output.pid,
        rusage: output
            .rusage
            .map(|rusage| af_telemetry::process::ResourceUse {
                user_ms: rusage.user_ms,
                system_ms: rusage.system_ms,
                max_rss_bytes: rusage.max_rss_bytes,
            }),
        stdout_bytes: Some(output.stdout.len() as u64),
        stderr_bytes: Some(output.stderr.len() as u64),
        stderr_held: output.stderr_held,
        ..Default::default()
    };
    if let Ok(status) = &output.status {
        facts = facts.with_status(status);
    }
    let failure = match (&output.status, capped) {
        (Ok(_), Some(reason)) => Some(("failed", reason.to_string())),
        (Ok(status), None) if status.success() => None,
        (Ok(_), None) => Some(("failed", "the harness exited unsuccessfully".to_string())),
        (Err(SupervisedError::TimedOut { .. }), _) => {
            Some(("timed_out", "the harness exceeded its deadline".to_string()))
        }
        (Err(SupervisedError::Cancelled), _) => {
            Some(("refused", "the Attempt was cancelled".to_string()))
        }
        (Err(SupervisedError::Spawn(_)), _) => Some((
            "unavailable",
            "the harness could not be started".to_string(),
        )),
        (Err(error), _) => Some(("failed", error.to_string())),
    };
    (facts, failure)
}

/// Record in an Attempt's stored stderr, as a held pipe is, that its descendant cleanup fell back
/// to the process-group kill: an Attempt never claims that guarantee silently.
fn note_cleanup_fallback(stderr: &mut Vec<u8>, fallback: Option<&str>) {
    if let Some(fallback) = fallback {
        stderr.extend_from_slice(format!("\n{fallback}\n").as_bytes());
    }
}

/// Replace every occurrence of every granted spelling. Byte-level, because captured output is
/// not guaranteed to be UTF-8 and a secret split across an encoding error must still be caught
/// where it appears intact.
fn redact(bytes: Vec<u8>, redactions: &[String]) -> Vec<u8> {
    let mut out = bytes;
    for secret in redactions {
        let secret = secret.as_bytes();
        if secret.is_empty() {
            continue;
        }
        let mut scrubbed = Vec::with_capacity(out.len());
        let mut index = 0;
        while index < out.len() {
            if out[index..].starts_with(secret) {
                scrubbed.extend_from_slice("[redacted]".as_bytes());
                index += secret.len();
            } else {
                scrubbed.push(out[index]);
                index += 1;
            }
        }
        out = scrubbed;
    }
    out
}
