//! Codex framing for generic typed Task Workers, preserving failed and malformed usage.
use super::*;
use review_runner::task::{MAX_WORKER_BYTES, ModelWorkerReturn, WorkerModelAdapter};
use rustix::fs::{Mode, OFlags, open, openat};
use std::io::Read;

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
    fn credential_mode(&self) -> review_core::BrokerCredentialModeV1 {
        review_core::BrokerCredentialModeV1::TrustedUnsafe
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
        writable: bool,
    ) -> ModelWorkerReturn {
        let staging = match tempfile::tempdir() {
            Ok(directory) => directory,
            Err(error) => {
                return ModelWorkerReturn::failed(RunnerError::Unavailable(error.to_string()));
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
                return ModelWorkerReturn::failed(RunnerError::Unavailable(format!(
                    "Cannot hold Codex Worker output directory: {error}"
                )));
            }
        };
        let command = codex_command(
            &self.program,
            &self.model_flags,
            workdir,
            if writable {
                "workspace-write"
            } else {
                "read-only"
            },
            Some(&last_message),
        );
        let mut runner = ModelRunner::new(workdir, timeout);
        if let Some(home) = &self.codex_home {
            runner = runner.with_grant("CODEX_HOME", home);
        }
        let capture = runner.capture_settled_with_stdin(cas, &command, input);
        let events = TaskEvents::parse(&capture.stdout);
        let mut returned = ModelWorkerReturn {
            message: Err("Codex Worker framing failed".into()),
            usage: None,
            raw_artifact_ids: capture.raw_artifact_ids,
        };
        if events.usage.input_tokens.is_some() {
            returned.usage = Some(events.usage);
        }
        if !capture.status.as_ref().is_ok_and(|status| status.success()) || events.error.is_some() {
            returned.message = Err(format!("Codex Worker failed with {:?}", capture.status));
            return returned;
        }
        returned.message = read_final_message(&output_directory).and_then(|bytes| {
            bytes
                .or_else(|| events.final_message.map(String::into_bytes))
                .ok_or_else(|| "Codex Worker returned no final message".into())
        });
        returned
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
    final_message: Option<String>,
    error: Option<String>,
}

impl TaskEvents {
    /// Fold the JSONL stream. Unknown event types are ignored — the CLI adds kinds freely —
    /// but the three that matter are pinned by fixtures captured from a real run.
    fn parse(stdout: &[u8]) -> TaskEvents {
        let mut events = TaskEvents::default();
        for line in stdout.split(|b| *b == b'\n') {
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
                continue;
            };
            match value.get("type").and_then(|t| t.as_str()) {
                Some("turn.completed") => {
                    if let Some(usage) = value.get("usage") {
                        let count =
                            |key: &str| usage.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
                        let input = count("input_tokens");
                        let cache_read = count("cached_input_tokens");
                        let output = count("output_tokens");
                        let reasoning = count("reasoning_output_tokens");
                        let cache_write = count("cache_write_input_tokens");
                        let chargeable =
                            u128::from(input.saturating_sub(cache_read)) + u128::from(output);
                        add_task_usage(&mut events.usage.input_tokens, input);
                        add_task_usage(&mut events.usage.output_tokens, output);
                        add_task_usage(&mut events.usage.cache_read_tokens, cache_read);
                        add_task_usage(&mut events.usage.cache_write_tokens, cache_write);
                        add_task_usage(&mut events.usage.reasoning_tokens, reasoning);
                        events.usage.chargeable_tokens =
                            (events.usage.chargeable_tokens.get() + chargeable).into();
                    }
                }
                Some("item.completed") => {
                    if let Some(item) = value.get("item")
                        && item.get("type").and_then(|t| t.as_str()) == Some("agent_message")
                        && let Some(text) = item.get("text").and_then(|t| t.as_str())
                    {
                        events.final_message = Some(text.to_string());
                    }
                }
                Some("error") | Some("turn.failed") => {
                    let message = value
                        .get("message")
                        .or_else(|| value.get("error").and_then(|e| e.get("message")))
                        .and_then(|m| m.as_str());
                    if let Some(message) = message {
                        events.error = Some(message.to_string());
                    }
                }
                _ => {}
            }
        }
        events
    }
}

// Each parsed component is u64; a completed turn consumes more than two bytes.
// Thus even a usize::MAX-byte stream on supported 64-bit platforms cannot overflow
// u128 when summing two chargeable components per turn. Legacy Events stays unchanged.
fn add_task_usage(total: &mut Option<review_core::task::usage::DecimalU128>, amount: u64) {
    *total = Some((total.map_or(0, |n| n.get()) + u128::from(amount)).into());
}
