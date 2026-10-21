//! Codex framing for generic typed Task Workers, preserving failed and malformed usage.
use super::*;
use review_runner::cli::{
    WorkerStderr, classify_without_result, ended_without_result, startup_failure,
    strongest_native_failure,
};
use review_runner::native_failure::{NativeFailureKind, classify_native_failure};
use review_runner::task::{
    MAX_WORKER_BYTES, ModelWorkerReturn, Unstarted, WorkerAccess, WorkerModelAdapter,
};
use rustix::fs::{Mode, OFlags, open, openat};
use std::io::Read;

/// The native sandbox one Attempt runs under, derived from its access alone. Both writable
/// modes are rooted at the sandbox by `-C`; a package cannot supply `-s` or any other flag.
pub fn task_sandbox_mode(access: WorkerAccess) -> &'static str {
    if access.writes_sandbox() {
        "workspace-write"
    } else {
        "read-only"
    }
}

pub struct CodexTaskAdapter {
    program: String,
    model_flags: Vec<String>,
    codex_home: Option<String>,
}
impl CodexTaskAdapter {
    pub fn new(command: &Command) -> Result<Self, String> {
        Ok(Self {
            program: command.program.clone(),
            model_flags: codex_model_flags(command)?,
            codex_home: None,
        })
    }
    pub fn with_codex_home(mut self, home: String) -> Self {
        self.codex_home = Some(home);
        self
    }
}

impl WorkerModelAdapter for CodexTaskAdapter {
    fn credential_mode(&self) -> review_core::CredentialModeV1 {
        review_core::CredentialModeV1::TrustedUnsafe
    }

    fn provider_kind(&self) -> &'static str {
        "codex"
    }
    fn model_settings(&self) -> Option<(String, String)> {
        let value = |flag| {
            self.model_flags
                .chunks_exact(2)
                .find(|args| args[0] == flag)
                .map(|args| args[1].clone())
        };
        let effort = value("-c")?;
        let effort = effort.strip_prefix("model_reasoning_effort=")?;
        let effort = serde_json::from_str::<String>(effort)
            .ok()
            .or_else(|| review_core::task::is_name(effort).then(|| effort.to_owned()))?;
        value("--model").map(|model| (model, effort))
    }
    fn invoke(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        access: WorkerAccess,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
        environment: &[(String, String)],
    ) -> ModelWorkerReturn {
        self.invoke_started(
            cas,
            workdir,
            input,
            timeout,
            access,
            cancellation,
            environment,
        )
        .unwrap_or_else(|unstarted| *unstarted.0)
    }
    fn invoke_started(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        access: WorkerAccess,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
        environment: &[(String, String)],
    ) -> Result<ModelWorkerReturn, Unstarted> {
        if cancellation.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire)) {
            return Ok(ModelWorkerReturn {
                usage_observation: None,
                message: Err("Worker invocation was cancelled before starting".into()),
                usage: Some(review_core::task::usage::TaskTokenUsageV3::charge_only(0)),
                raw_artifact_ids: vec![],
                native_failure: None,
            });
        }
        let staging = match tempfile::tempdir() {
            Ok(directory) => directory,
            Err(error) => {
                return Ok(ModelWorkerReturn::failed(RunnerError::Unavailable(
                    error.to_string(),
                )));
            }
        };
        let last_message = staging.path().join("last-message");
        // Hold our output directory before the native process can write its response. The
        // final read resolves only its single filename beneath this same directory.
        let output_directory = match open(
            staging.path(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(directory) => directory,
            Err(error) => {
                return Ok(ModelWorkerReturn::failed(RunnerError::Unavailable(
                    format!("Cannot hold Codex Worker output directory: {error}"),
                )));
            }
        };
        let command = codex_command(
            &self.program,
            &self.model_flags,
            workdir,
            task_sandbox_mode(access),
            &last_message,
        );
        let mut runner = ModelRunner::new(workdir, timeout)
            .with_harness("openai", self.model_settings().map(|(model, _)| model));
        if access.has_shell() {
            // A shell child must not outlive the Attempt that started it.
            runner = runner.killing_process_group_on_exit();
        }
        if let Some(home) = &self.codex_home {
            runner = runner.with_grant("CODEX_HOME", home);
        }
        // Sandbox-local, non-secret context resolved by the kernel for this exact Attempt.
        for (name, value) in environment {
            runner = runner.with_env(name, value);
        }
        let mut events = TaskEvents::default();
        let mut failure = None;
        let mut said = WorkerStderr::default();
        // The `-o` final message as a reply, only for a process that exited 0 with no `error`
        // or `turn.failed` event; `None` for every other ending.
        let mut reply: Option<Result<Option<Vec<u8>>, String>> = None;
        let mut capture = runner.capture_observed_with_output(
            cas,
            &command,
            input,
            cancellation,
            &review_runner::stream::HarnessStream::codex(),
            |failed, stdout, stderr, further| {
                'classify: {
                    // Parse the bill before privacy filtering removes an authentication envelope.
                    events = TaskEvents::parse(stdout);
                    // The Worker's own last words, bounded and filtered, before the privacy filter
                    // below may replace the stream.
                    said = WorkerStderr::read(stderr, stdout);
                    // The final message is harness output too: Telemetry's release decision
                    // judges the whole file of every observed process before the tap ends,
                    // whatever the process's ending, and one it cannot read whole withholds the
                    // process's pending content (design §7.3).
                    let qualifies = !failed && events.error.is_none();
                    let read = (qualifies || further.observed())
                        .then(|| read_final_message(&output_directory));
                    match &read {
                        Some(Ok(Some(bytes))) => further.include(bytes),
                        Some(Err(_)) => further.unjudgeable(),
                        Some(Ok(None)) | None => {}
                    }
                    if qualifies {
                        reply = read;
                    }
                    if matches!(reply, Some(Ok(Some(_)))) {
                        break 'classify;
                    }
                    // No parsable result, whatever the exit status, the framed events or the
                    // transport outcome: the one place its classification is decided. Framed events
                    // speak only through their error fields: an agent message that quotes an auth or
                    // argument error is model output, never a native diagnostic.
                    let native_stdout = unframed(stdout);
                    let candidates = [
                        events
                            .upstream_failure
                            .unwrap_or(NativeFailureKind::Unknown),
                        classify_native_failure(&String::from_utf8_lossy(stderr)),
                        classify_native_failure(&native_stdout),
                    ];
                    let kind = classify_without_result(stderr, &native_stdout, &candidates);
                    failure = (kind != NativeFailureKind::Unknown).then_some(kind);
                    // Any recognized auth error makes both streams sensitive, even if an earlier
                    // error concerned another failure, stdout was not valid JSONL, or an argument
                    // error took the classification.
                    kind.redact_capture(
                        strongest_native_failure(&candidates).is_auth(),
                        stdout,
                        stderr,
                    );
                }
                // The raw-output rule: the Store keeps the event lines accounting and failure
                // classification read, whatever came of them above, or the adapter's
                // whole-capture replacement when it took one; Telemetry has the rest. Native text
                // outside the framing was classified above, from the complete capture.
                if !NativeFailureKind::replaced_capture(stdout, stderr) {
                    *stdout = kept_output(stdout);
                }
            },
        );
        let mut returned = ModelWorkerReturn {
            usage_observation: events.observation(),
            message: Err("Codex Worker framing failed".into()),
            usage: events.reported_usage(),
            raw_artifact_ids: capture.raw_artifact_ids,
            native_failure: None,
        };
        // The only parsable result is the `-o` final message of a process that exited 0 with no
        // `error` or `turn.failed` event. Every other ending, framed events or not, carries the
        // exit and the Worker's own last words under the one classification decided above. A
        // transport that failed after the process exited, storing its raw output, admits no
        // message the process printed: the process left a result, so nothing was classified.
        // The `-o` last-message file is the reply's raw evidence, kept with the lines above.
        if let Some(Ok(Some(bytes))) = &reply {
            match cas.put(bytes) {
                Ok(id) if !returned.raw_artifact_ids.contains(&id) => {
                    returned.raw_artifact_ids.push(id);
                }
                Ok(_) => {}
                Err(error) => {
                    capture.status = Err(RunnerError::Unavailable(format!(
                        "storing raw output: {error}"
                    )));
                }
            }
        }
        let reply = reply.filter(|_| capture.status.as_ref().is_ok_and(|status| status.success()));
        let delivered = reply.is_some();
        returned.message = match reply {
            Some(Ok(Some(bytes))) => Ok(bytes),
            reply => {
                // An `error` or `turn.failed` event is a native failure even when its message is
                // unclassified: its closed diagnostic names that, never the bare exit status.
                returned.native_failure = failure.or(events.upstream_failure);
                let ended = ended_without_result(&capture.status, &said);
                let kind = failure.unwrap_or(NativeFailureKind::Unknown);
                Err(
                    if kind == NativeFailureKind::UnsupportedInvocation
                        || !delivered && !events.framed
                    {
                        // A refused invocation, whatever events came first, or no event at all:
                        // the process's exit and its own last words are all there is.
                        startup_failure(
                            "Codex Worker",
                            &CODEX_INVOCATION,
                            &capture.status,
                            &said,
                            kind,
                        )
                    } else if let Some(kind) =
                        failure.or(events.upstream_failure.filter(|_| !events.malformed_usage))
                    {
                        let mut message =
                            format!("Codex Worker failed: {}; it {ended}", kind.diagnostic());
                        if events.malformed_usage {
                            message.push_str("; Codex Worker returned malformed native usage");
                        }
                        message
                    } else if events.malformed_usage {
                        format!(
                            "Codex Worker returned malformed native usage: {:?}; it {ended}",
                            events.error
                        )
                    } else if let Some(read) = reply {
                        // Exit 0 without a failure event: the `-o` file is the only final
                        // message, and an absent or empty file is no reply.
                        let error = read
                            .err()
                            .unwrap_or_else(|| "Codex Worker returned no final message".into());
                        format!("{error}; it {ended}")
                    } else {
                        // Only events that carry no failure, such as `thread.started`.
                        startup_failure(
                            "Codex Worker",
                            &CODEX_INVOCATION,
                            &capture.status,
                            &said,
                            kind,
                        )
                    },
                )
            }
        };
        if capture.started {
            Ok(returned)
        } else {
            Err(Unstarted(Box::new(returned)))
        }
    }
}

fn read_final_message(directory: &rustix::fd::OwnedFd) -> Result<Option<Vec<u8>>, String> {
    // NONBLOCK prevents opening a FIFO from waiting for a writer. NOFOLLOW and metadata
    // on the opened descriptor ensure that neither symlinks nor special files get read.
    let fd = match openat(
        directory,
        "last-message",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(format!("Cannot open Codex Worker final message: {error}")),
    };
    let file: std::fs::File = fd.into();
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("Codex Worker final message must be a regular file".into());
    }
    if metadata.len() > MAX_WORKER_BYTES as u64 {
        return Err("Codex Worker final message exceeds its byte bound".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_WORKER_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_WORKER_BYTES {
        return Err("Codex Worker final message exceeds its byte bound".into());
    }
    Ok((!bytes.is_empty()).then_some(bytes))
}

#[derive(Default)]
struct TaskEvents {
    usage: review_core::task::usage::TaskTokenUsageV3,
    error: Option<String>,
    upstream_failure: Option<NativeFailureKind>,
    incomplete_charge: bool,
    malformed_usage: bool,
    /// Whether stdout held any JSONL event at all: a CLI that refused its arguments frames none.
    framed: bool,
}

impl TaskEvents {
    fn reported_usage(&self) -> Option<review_core::task::usage::TaskTokenUsageV3> {
        let u = &self.usage;
        (u.input_tokens.is_some()
            || u.output_tokens.is_some()
            || u.cache_read_tokens.is_some()
            || u.cache_write_tokens.is_some()
            || u.reasoning_tokens.is_some())
        .then(|| u.clone())
    }

    fn observation(&self) -> Option<review_core::task::usage::TaskUsageObservationV1> {
        self.malformed_usage
            .then(|| review_core::task::usage::TaskUsageObservationV1 {
                reported_usage: self.reported_usage(),
                charge_complete: !self.incomplete_charge,
            })
    }

    fn add_usage(&mut self, value: Option<&serde_json::Value>) {
        use review_runner::task::usage::NativeCounter;
        let Some(value) = value.filter(|v| v.is_object()) else {
            self.malformed_usage = true;
            self.incomplete_charge = true;
            self.error = Some("Codex turn.completed requires a usage object".into());
            return;
        };
        let input = NativeCounter::read(value, "input_tokens").value();
        let output = NativeCounter::read(value, "output_tokens").value();
        let cache = NativeCounter::read(value, "cached_input_tokens").optional_zero();
        let write = NativeCounter::read(value, "cache_write_input_tokens").optional_zero();
        let reasoning = NativeCounter::read(value, "reasoning_output_tokens").optional_zero();
        // A malformed discount does not establish any uncached input contribution. A valid
        // output counter and all preceding turns still establish their exact paid floor.
        let uncached = input
            .zip(cache)
            .and_then(|(input, cache)| input.checked_sub(cache));
        let complete = uncached.is_some() && output.is_some();
        self.incomplete_charge |= !complete;
        if !complete || write.is_none() || reasoning.is_none() {
            self.malformed_usage = true;
            self.error = Some("Codex returned malformed native usage".into());
        }
        for (total, amount) in [
            (&mut self.usage.input_tokens, input),
            (&mut self.usage.output_tokens, output),
            (&mut self.usage.cache_read_tokens, cache),
            (&mut self.usage.cache_write_tokens, write),
            (&mut self.usage.reasoning_tokens, reasoning),
        ] {
            if let Some(amount) = amount {
                add_task_usage(total, amount);
            }
        }
        self.usage.chargeable_tokens = (self.usage.chargeable_tokens.get()
            + u128::from(uncached.unwrap_or(0))
            + u128::from(output.unwrap_or(0)))
        .into();
    }

    /// Fold the JSONL stream for usage and errors. Unknown event types are ignored — the CLI
    /// adds kinds freely — and the reply itself is read from the `-o` file, never from stdout.
    fn parse(stdout: &[u8]) -> TaskEvents {
        let mut events = TaskEvents::default();
        for line in stdout.split(|b| *b == b'\n') {
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
                continue;
            };
            events.framed |= value.get("type").is_some_and(serde_json::Value::is_string);
            match value.get("type").and_then(|t| t.as_str()) {
                Some("turn.completed") => {
                    events.add_usage(value.get("usage"));
                }
                Some("error") | Some("turn.failed") => {
                    let message = value
                        .get("message")
                        .or_else(|| value.get("error").and_then(|e| e.get("message")))
                        .and_then(|m| m.as_str());
                    let kind = message
                        .map(classify_native_failure)
                        .unwrap_or(NativeFailureKind::Unknown);
                    // Preserve the upstream cause independently of later usage diagnostics.
                    // No raw provider error becomes an ordinary diagnostic.
                    if events.upstream_failure.is_none_or(|prior| {
                        prior == NativeFailureKind::Unknown || !prior.is_auth() && kind.is_auth()
                    }) {
                        events.upstream_failure = Some(kind);
                    }
                    events.error = Some(kind.diagnostic().into());
                }
                _ => {}
            }
        }
        events
    }
}

/// The raw-output rule for Codex (design §7.3): of an Attempt's JSONL stdout, what the Store
/// keeps — the `turn.completed`, `turn.failed` and `error` lines the accounting and failure
/// classification read, each as it was, and nothing else: no other event, and no native text
/// outside the event framing, which classification reads from the complete in-memory capture
/// ([`unframed`]).
pub fn kept_output(stdout: &[u8]) -> Vec<u8> {
    use review_runner::stream::{lines, message_type};
    lines(stdout)
        .filter(|line| {
            message_type(line).is_some_and(|kind| {
                matches!(kind.as_str(), "turn.completed" | "turn.failed" | "error")
            })
        })
        .flatten()
        .copied()
        .collect()
}

/// Stdout lines outside the JSONL event framing: native text a failing client wrote around
/// or instead of its events. Framed events, including all model output, are excluded.
fn unframed(stdout: &[u8]) -> String {
    let mut text = String::new();
    for line in stdout.split(|b| *b == b'\n') {
        let framed = serde_json::from_slice::<serde_json::Value>(line)
            .is_ok_and(|value| value.get("type").is_some_and(serde_json::Value::is_string));
        if !framed {
            text.push_str(&String::from_utf8_lossy(line));
            text.push('\n');
        }
    }
    text
}

// Each parsed component is u64; a completed turn consumes more than two bytes.
// Thus even a usize::MAX-byte stream on supported 64-bit platforms cannot overflow
// u128 when summing two chargeable components per turn. Legacy Events stays unchanged.
fn add_task_usage(total: &mut Option<review_core::task::usage::DecimalU128>, amount: u64) {
    *total = Some((total.map_or(0, |n| n.get()) + u128::from(amount)).into());
}

#[cfg(test)]
mod usage_tests;
