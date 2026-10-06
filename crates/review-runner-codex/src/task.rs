//! Codex framing for generic typed Task Workers, preserving failed and malformed usage.
use super::*;
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
                native_failure: None,
                usage_observation: None,
                message: Err("Worker invocation was cancelled before starting".into()),
                usage: Some(review_core::task::usage::TaskTokenUsageV3::charge_only(0)),
                raw_artifact_ids: vec![],
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
        let mut runner = ModelRunner::new(workdir, timeout);
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
        let capture = runner.capture_filtered(
            cas,
            &command,
            input,
            cancellation,
            |failed, stdout, stderr| {
                // Parse the bill before privacy filtering removes an authentication envelope.
                events = TaskEvents::parse(stdout);
                if failed || events.upstream_failure.is_some() {
                    // Framed events speak only through their error fields: an agent message
                    // that quotes an auth error is model output, never a native diagnostic.
                    let candidates = [
                        events
                            .upstream_failure
                            .unwrap_or(NativeFailureKind::Unknown),
                        classify_native_failure(&String::from_utf8_lossy(stderr)),
                        classify_native_failure(&unframed(stdout)),
                    ];
                    // Any recognized auth error makes both streams sensitive, even if an
                    // earlier error concerned another failure or stdout was not valid JSONL.
                    failure = candidates
                        .iter()
                        .copied()
                        .find(|kind| kind.is_auth())
                        .or_else(|| {
                            candidates
                                .into_iter()
                                .find(|kind| *kind != NativeFailureKind::Unknown)
                        });
                    failure
                        .unwrap_or(NativeFailureKind::Unknown)
                        .redact_auth_capture(stdout, stderr);
                }
            },
        );
        let mut returned = ModelWorkerReturn {
            native_failure: None,
            usage_observation: None,
            message: Err("Codex Worker framing failed".into()),
            usage: None,
            raw_artifact_ids: capture.raw_artifact_ids,
        };
        returned.usage = events.reported_usage();
        returned.usage_observation = events.observation();
        if !capture.status.as_ref().is_ok_and(|status| status.success()) || events.error.is_some() {
            returned.native_failure = failure.filter(|kind| *kind != NativeFailureKind::Unknown);
            returned.message = Err(
                if let Some(kind) = failure.filter(|kind| *kind != NativeFailureKind::Unknown) {
                    let mut message = format!(
                        "Codex Worker failed: {}; transport: {:?}",
                        kind.diagnostic(),
                        capture.status
                    );
                    if events.malformed_usage {
                        message.push_str("; Codex Worker returned malformed native usage");
                    }
                    message
                } else if events.malformed_usage {
                    format!(
                        "Codex Worker returned malformed native usage: {:?}",
                        events.error
                    )
                } else {
                    // Keep the historical valid-usage failure diagnostic and artifact identity.
                    format!("Codex Worker failed with {:?}", capture.status)
                },
            );
            return if capture.started {
                Ok(returned)
            } else {
                Err(Unstarted(Box::new(returned)))
            };
        }
        // The `-o` file is the only final message: an absent or empty file is no reply.
        returned.message = read_final_message(&output_directory)
            .and_then(|bytes| bytes.ok_or_else(|| "Codex Worker returned no final message".into()));
        Ok(returned)
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
