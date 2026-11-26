//! Antigravity framing for generic typed Task Workers, preserving failed and malformed usage.

use std::path::{Path, PathBuf};
use std::time::Duration;

use af_contracts::Command;
use af_contracts::task::usage::{DecimalU128, TaskTokenUsageV3, TaskUsageObservationV1};
use af_runner::cli::{
    WorkerStderr, classify_without_result, ended_without_result, startup_failure,
    strongest_native_failure, transient_ending, transport_outcome,
};
use af_runner::models::diagnostic;
use af_runner::native_failure::{NativeFailureKind, classify_native_failure, native_auth_seen};
use af_runner::task::reply_schema::ReplyContract;
use af_runner::task::usage::NativeCounter;
use af_runner::task::{
    MAX_WORKER_BYTES, ModelWorkerReturn, Unstarted, WorkerAccess, WorkerModelAdapter,
};
use af_runner::{ModelRunner, RunnerError};
use af_store::Cas;
use serde_json::{Value, json};

use crate::{ANTIGRAVITY_INVOCATION, antigravity_command, antigravity_model_flags};

/// Build the adapter-owned session settings JSON for one Attempt from [`WorkerAccess`] alone.
///
/// Antigravity evaluates permissions strictly as `deny > ask > allow`. Denying URL, MCP, and
/// out-of-scope write/command capabilities here ensures candidate repositories and Worker
/// packages cannot widen authority beyond what the kernel granted.
pub fn task_settings(access: WorkerAccess) -> Value {
    let (allow, deny): (Vec<&str>, Vec<&str>) = match access {
        WorkerAccess::ReadOnly => (
            vec![],
            vec![
                "command(*)",
                "write_file(*)",
                "mcp(*)",
                "read_url(*)",
                "execute_url(*)",
            ],
        ),
        WorkerAccess::ExecuteChecks => (
            vec!["command(*)"],
            vec!["write_file(*)", "mcp(*)", "read_url(*)", "execute_url(*)"],
        ),
        WorkerAccess::WriteSource => (
            vec!["write_file(*)"],
            vec!["command(*)", "mcp(*)", "read_url(*)", "execute_url(*)"],
        ),
        WorkerAccess::WriteSourceWithShell => (
            vec!["write_file(*)", "command(*)"],
            vec!["mcp(*)", "read_url(*)", "execute_url(*)"],
        ),
    };
    json!({
        "sandbox": {
            "enabled": true,
            "preset": "Default",
        },
        "permissions": {
            "allow": allow,
            "deny": deny,
        },
    })
}

pub struct AntigravityTaskAdapter {
    program: String,
    model_flags: Vec<String>,
    model: String,
    auth_dir: Option<PathBuf>,
    home: Option<String>,
    user: Option<String>,
    keep_transcripts: bool,
    provider: Option<String>,
}

impl AntigravityTaskAdapter {
    pub fn new(command: &Command) -> Result<Self, String> {
        let model_flags = antigravity_model_flags(command)?;
        let model = model_flags
            .chunks_exact(2)
            .find(|pair| pair[0] == "--model")
            .map(|pair| pair[1].clone())
            .ok_or("Antigravity Task Worker requires an explicit --model")?;
        for pair in model_flags.chunks_exact(2) {
            diagnostic::check_binding(&pair[0], &pair[1])?;
        }
        Ok(Self {
            program: command.program.clone(),
            model_flags,
            model,
            auth_dir: None,
            home: None,
            user: None,
            keep_transcripts: false,
            provider: None,
        })
    }

    pub fn for_provider(mut self, id: impl Into<String>) -> Self {
        self.provider = Some(id.into());
        self
    }

    pub fn with_auth(
        mut self,
        auth_dir: PathBuf,
        user: Option<String>,
        home: Option<String>,
    ) -> Self {
        self.auth_dir = Some(auth_dir);
        self.user = user;
        self.home = home;
        self
    }

    pub fn keeping_transcripts(mut self, keep: bool) -> Self {
        self.keep_transcripts = keep;
        self
    }

    fn resolve_attempt_home(&self, staging: &Path) -> Result<String, String> {
        let Some(auth_dir) = self.auth_dir.as_deref() else {
            let home = staging.join("home");
            std::fs::create_dir(&home)
                .map_err(|error| format!("Cannot create Antigravity Worker home: {error}"))?;
            return home
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| "Antigravity Worker home path is not UTF-8".to_owned());
        };
        // When the registered auth_dir is `<parent>/.gemini/antigravity-cli`, passing
        // `HOME=<parent>` lets `agy` resolve both `~/.gemini/antigravity-cli` and the user's
        // keychain/config directly. For any other directory (such as an isolated profile or test
        // fixture), materialize `<staging>/home/.gemini/antigravity-cli` as a symlink to it.
        if auth_dir.file_name().and_then(|n| n.to_str()) == Some("antigravity-cli")
            && let Some(gemini_dir) = auth_dir.parent()
            && gemini_dir.file_name().and_then(|n| n.to_str()) == Some(".gemini")
            && let Some(parent_home) = gemini_dir.parent()
        {
            return parent_home
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| "Antigravity Worker home path is not UTF-8".to_owned());
        }
        let home = staging.join("home");
        let gemini = home.join(".gemini");
        std::fs::create_dir_all(&gemini)
            .map_err(|error| format!("Cannot create Antigravity Worker home: {error}"))?;
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(auth_dir, gemini.join("antigravity-cli"))
                .map_err(|error| format!("Cannot link Antigravity auth directory: {error}"))?;
            let config = auth_dir.join("config");
            if config.is_dir() {
                let _ = std::os::unix::fs::symlink(&config, gemini.join("config"));
            }
        }
        home.to_str()
            .map(str::to_owned)
            .ok_or_else(|| "Antigravity Worker home path is not UTF-8".to_owned())
    }

    fn remove_transcripts(&self, conversation_id: Option<&str>) {
        if self.keep_transcripts {
            return;
        }
        let Some(auth_dir) = self.auth_dir.as_deref() else {
            return;
        };
        let Some(id) = conversation_id.filter(|id| is_safe_conversation_id(id)) else {
            return;
        };
        let pb = auth_dir.join("conversations").join(format!("{id}.pb"));
        if pb.is_file() {
            let _ = std::fs::remove_file(&pb);
        }
        let brain = auth_dir.join("brain").join(id);
        if brain.is_dir() {
            let _ = std::fs::remove_dir_all(&brain);
        }
    }
}

fn is_safe_conversation_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

impl WorkerModelAdapter for AntigravityTaskAdapter {
    fn credential_mode(&self) -> af_contracts::CredentialModeV1 {
        af_contracts::CredentialModeV1::TrustedUnsafe
    }

    fn provider_kind(&self) -> &'static str {
        "antigravity"
    }

    fn model_settings(&self) -> Option<(String, String)> {
        let effort = self
            .model_flags
            .chunks_exact(2)
            .find(|args| args[0] == "--effort")
            .map(|args| args[1].clone())?;
        Some((self.model.clone(), effort))
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
        self.invoke_constrained(
            cas,
            workdir,
            input,
            timeout,
            access,
            cancellation,
            environment,
            ReplyContract::Request,
        )
    }

    fn invoke_constrained(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        access: WorkerAccess,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
        environment: &[(String, String)],
        contract: ReplyContract,
    ) -> Result<ModelWorkerReturn, Unstarted> {
        self.invoke_activity(
            cas,
            workdir,
            input,
            timeout,
            access,
            cancellation,
            environment,
            contract,
            None,
        )
    }

    fn invoke_with_activity(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        access: WorkerAccess,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
        environment: &[(String, String)],
        _allowance: af_runner::task::RemainingTokenAllowance,
        contract: ReplyContract,
        activity: Option<af_runner::activity::Sink>,
    ) -> Result<ModelWorkerReturn, Unstarted> {
        self.invoke_activity(
            cas,
            workdir,
            input,
            timeout,
            access,
            cancellation,
            environment,
            contract,
            activity,
        )
    }
}

impl AntigravityTaskAdapter {
    #[allow(clippy::too_many_arguments)]
    fn invoke_activity(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        access: WorkerAccess,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
        environment: &[(String, String)],
        contract: ReplyContract,
        activity: Option<af_runner::activity::Sink>,
    ) -> Result<ModelWorkerReturn, Unstarted> {
        if cancellation.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire)) {
            return Ok(ModelWorkerReturn {
                notes: Default::default(),
                usage_observation: None,
                message: Err("Worker invocation was cancelled before starting".into()),
                usage: Some(TaskTokenUsageV3::charge_only(0)),
                raw_artifact_ids: vec![],
                native_failure: None,
            });
        }
        let prompt = match String::from_utf8(input.clone()) {
            Ok(text) => text,
            Err(_) => {
                return Ok(ModelWorkerReturn::failed(RunnerError::Refused(
                    "Antigravity Worker request must be UTF-8".into(),
                )));
            }
        };
        let staging = match tempfile::tempdir() {
            Ok(directory) => directory,
            Err(error) => {
                return Ok(ModelWorkerReturn::failed(RunnerError::Unavailable(
                    error.to_string(),
                )));
            }
        };
        let settings_file = staging.path().join("settings.json");
        if let Err(error) = std::fs::write(&settings_file, task_settings(access).to_string()) {
            return Ok(ModelWorkerReturn::failed(RunnerError::Unavailable(
                format!("Cannot write Antigravity Worker settings: {error}"),
            )));
        }
        let schema = match contract.strict(&input) {
            Ok(schema) => schema,
            Err(error) => {
                return Ok(ModelWorkerReturn {
                    usage_observation: None,
                    message: Err(format!("Antigravity Worker request refused: {error}")),
                    usage: Some(TaskTokenUsageV3::charge_only(0)),
                    raw_artifact_ids: vec![],
                    native_failure: Some(NativeFailureKind::UnsupportedInvocation),
                    notes: Default::default(),
                });
            }
        };
        let schema_file = staging.path().join("reply-schema.json");
        if let Some(schema) = &schema
            && let Err(error) = std::fs::write(&schema_file, schema.to_string())
        {
            return Ok(ModelWorkerReturn::failed(RunnerError::Unavailable(
                format!("Cannot write Antigravity Worker reply schema: {error}"),
            )));
        }
        let request = schema.is_some().then(|| input.clone());
        let command = antigravity_command(
            &self.program,
            &self.model_flags,
            &settings_file,
            schema.is_some().then_some(schema_file.as_path()),
        );
        let attempt_home = match self.resolve_attempt_home(staging.path()) {
            Ok(home) => home,
            Err(error) => {
                return Ok(ModelWorkerReturn::failed(RunnerError::Unavailable(error)));
            }
        };
        let mut stdin_bytes = match serde_json::to_vec(&json!({
            "event": "user",
            "message": { "content": prompt }
        })) {
            Ok(bytes) => bytes,
            Err(error) => {
                return Ok(ModelWorkerReturn::failed(RunnerError::Unavailable(
                    format!("Cannot encode Antigravity stream-json input: {error}"),
                )));
            }
        };
        stdin_bytes.push(b'\n');

        let mut runner = ModelRunner::new(workdir, timeout)
            .isolating_ticket_auth()
            .with_env("HOME", attempt_home)
            .with_harness("google", self.model_settings().map(|(model, _)| model))
            .with_activity(activity);
        if let Some(user) = &self.user {
            runner = runner.with_env("USER", user);
        }
        if access.has_shell() {
            runner = runner.killing_process_group_on_exit();
        }
        for (name, value) in environment {
            runner = runner.with_env(name, value);
        }

        let mut events = StreamEvents::default();
        let mut failure = None;
        let mut said = WorkerStderr::default();
        let mut stdout_blank = false;
        let mut ending_text = String::new();

        let capture = runner.capture_observed(
            cas,
            &command,
            stdin_bytes,
            cancellation,
            &af_runner::stream::HarnessStream::antigravity(),
            |failed, stdout, stderr| {
                stdout_blank = stdout.iter().all(u8::is_ascii_whitespace);
                events = StreamEvents::parse(stdout);
                said = WorkerStderr::read(stderr, stdout);

                let success_envelope = !failed
                    && events.result_status.as_deref() == Some("SUCCESS")
                    && events.error.is_none()
                    && events.reply_bytes.is_some();
                if !success_envelope {
                    let native_stdout = unframed_stdout(stdout);
                    ending_text = format!(
                        "{}\n{native_stdout}\n{}",
                        String::from_utf8_lossy(stderr),
                        events.error.as_deref().unwrap_or_default()
                    );
                    events.schema_rejected |= String::from_utf8_lossy(stderr)
                        .contains("invalid_json_schema")
                        || native_stdout.contains("invalid_json_schema")
                        || events
                            .error
                            .as_deref()
                            .is_some_and(|e| e.contains("invalid_json_schema"));
                    let candidates = [
                        events
                            .upstream_failure
                            .unwrap_or(NativeFailureKind::Unknown),
                        classify_native_failure(&String::from_utf8_lossy(stderr)),
                        classify_native_failure(&native_stdout),
                    ];
                    let kind = if events.schema_rejected {
                        NativeFailureKind::UnsupportedInvocation
                    } else {
                        classify_without_result(stderr, &native_stdout, &candidates)
                    };
                    failure = (kind != NativeFailureKind::Unknown).then_some(kind);
                    let auth_seen = strongest_native_failure(&candidates).is_auth()
                        || events.auth_seen
                        || native_auth_seen(&ending_text);
                    said.redact_auth(auth_seen);
                    kind.redact_capture(auth_seen, stdout, stderr);
                }
                if !NativeFailureKind::replaced_capture(stdout, stderr) {
                    *stdout = kept_output(stdout);
                }
            },
        );

        self.remove_transcripts(events.conversation_id.as_deref());

        let model_switched = events
            .init_model
            .as_deref()
            .is_some_and(|reported| reported != self.model);
        if model_switched {
            failure = Some(NativeFailureKind::ModelUnavailable);
        }

        let usage = events.reported_usage();
        let usage_observation = events.observation();
        let status_ok = capture.status.as_ref().is_ok_and(|status| status.success());
        let envelope_ok = status_ok
            && !model_switched
            && !events.malformed_usage
            && events.result_status.as_deref() == Some("SUCCESS")
            && events.error.is_none()
            && usage.is_some();

        let message = if envelope_ok {
            match events.reply_bytes {
                Some(bytes) if bytes.len() <= MAX_WORKER_BYTES => Ok(match &request {
                    Some(request) => contract.normalize_strict_reply(request, &bytes),
                    None => bytes,
                }),
                Some(_) => Err("Antigravity Worker final message exceeds its byte bound".into()),
                None => Err("Antigravity Worker returned no final message".into()),
            }
        } else if model_switched {
            let reported = events.init_model.as_deref().unwrap_or("unknown");
            Err(format!(
                "Antigravity Worker switched from bound model `{}` to `{}` (provider: {})",
                self.model,
                reported,
                self.provider.as_deref().unwrap_or("antigravity")
            ))
        } else if events.schema_rejected {
            Err(format!(
                "Antigravity Worker request rejected: invalid_json_schema; check the captured output-port strict schema; native evidence: {}",
                capture.raw_artifact_ids.join(", ")
            ))
        } else {
            let kind = failure
                .or(events.upstream_failure)
                .unwrap_or(NativeFailureKind::Unknown);
            let ended = ended_without_result(&capture.status, &said);
            Err(
                if kind == NativeFailureKind::UnsupportedInvocation || !events.framed {
                    startup_failure(
                        "Antigravity Worker",
                        &ANTIGRAVITY_INVOCATION,
                        &capture.status,
                        &said,
                        kind,
                    )
                } else if events.malformed_usage {
                    format!(
                        "Antigravity Worker returned malformed or missing native usage; transport: {}",
                        transport_outcome(&capture.status)
                    )
                } else if kind != NativeFailureKind::Unknown {
                    format!(
                        "Antigravity Worker failed: {}; it {ended}",
                        kind.diagnostic()
                    )
                } else if let Some(err) = &events.error {
                    format!(
                        "Antigravity Worker failed: {}; transport: {}",
                        af_runner::cli::bounded_cli_line(err),
                        transport_outcome(&capture.status)
                    )
                } else {
                    format!(
                        "Antigravity Worker failed; transport: {}",
                        transport_outcome(&capture.status)
                    )
                },
            )
        };

        let native_failure = message
            .is_err()
            .then_some(failure.or(events.upstream_failure))
            .flatten();
        let transient = (message.is_err()
            && (events.result_status.is_none() || native_failure.is_some()))
        .then(|| {
            transient_ending(
                &capture.status,
                native_failure.unwrap_or(NativeFailureKind::Unknown),
                stdout_blank,
                &ending_text,
            )
        })
        .flatten();

        let returned = ModelWorkerReturn {
            usage_observation,
            native_failure,
            message,
            usage,
            raw_artifact_ids: capture.raw_artifact_ids,
            notes: af_runner::task::CallNotes {
                native_failure,
                transient,
                events: Vec::new(),
                refusal_category: None,
                environment: native_failure == Some(NativeFailureKind::Environment),
            },
        };
        if capture.started {
            Ok(returned)
        } else {
            Err(Unstarted(Box::new(returned)))
        }
    }
}

#[derive(Default)]
struct StreamEvents {
    framed: bool,
    conversation_id: Option<String>,
    init_model: Option<String>,
    result_status: Option<String>,
    reply_bytes: Option<Vec<u8>>,
    error: Option<String>,
    upstream_failure: Option<NativeFailureKind>,
    schema_rejected: bool,
    auth_seen: bool,
    usage: Option<TaskTokenUsageV3>,
    malformed_usage: bool,
    incomplete_charge: bool,
}

impl StreamEvents {
    fn parse(stdout: &[u8]) -> Self {
        let mut parsed = Self::default();
        for line in stdout.split(|byte| *byte == b'\n') {
            let trimmed = line.trim_ascii();
            if !trimmed.starts_with(b"{") {
                continue;
            }
            let Ok(value) = serde_json::from_slice::<Value>(trimmed) else {
                continue;
            };
            let Some(event) = value.get("event").and_then(Value::as_str) else {
                continue;
            };
            parsed.framed = true;
            if let Some(id) = value.get("conversation_id").and_then(Value::as_str)
                && !id.is_empty()
            {
                parsed.conversation_id = Some(id.to_owned());
            }
            match event {
                "init" => {
                    if let Some(model) = value
                        .get("init")
                        .and_then(|init| init.get("model"))
                        .and_then(Value::as_str)
                        .filter(|m| !m.is_empty())
                    {
                        parsed.init_model = Some(model.to_owned());
                    }
                }
                "step_update" => {}
                "result" => {
                    if let Some(result) = value.get("result").filter(|v| v.is_object()) {
                        parsed.apply_result(result);
                    }
                }
                _ => {}
            }
        }
        parsed
    }

    fn apply_result(&mut self, result: &Value) {
        if let Some(id) = result.get("conversation_id").and_then(Value::as_str)
            && !id.is_empty()
        {
            self.conversation_id = Some(id.to_owned());
        }
        let status = result
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let is_success = status.as_deref() == Some("SUCCESS");
        self.result_status = status;

        if let Some(err) = result
            .get("error")
            .and_then(Value::as_str)
            .filter(|e| !e.is_empty())
        {
            self.error = Some(err.to_owned());
            let kind = classify_native_failure(err);
            if kind != NativeFailureKind::Unknown {
                self.upstream_failure = Some(kind);
            }
            self.auth_seen |= native_auth_seen(err);
        }

        if is_success {
            if let Some(structured) = result
                .get("structured_output")
                .filter(|value| !value.is_null())
            {
                if let Ok(bytes) = serde_json::to_vec(structured) {
                    self.reply_bytes = Some(bytes);
                }
            } else if let Some(response) = result
                .get("response")
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty())
            {
                self.reply_bytes = Some(response.as_bytes().to_vec());
            }
        }

        self.parse_usage(result.get("usage"), is_success);
    }

    fn parse_usage(&mut self, value: Option<&Value>, is_success: bool) {
        let Some(usage) = value.filter(|v| v.is_object()) else {
            if is_success {
                self.malformed_usage = true;
                self.incomplete_charge = true;
            }
            return;
        };
        let input = NativeCounter::read(usage, "input_tokens").value();
        let output = NativeCounter::read(usage, "output_tokens").value();
        let thinking = NativeCounter::read(usage, "thinking_tokens").optional_zero();
        let cache_read = NativeCounter::read(usage, "cache_read_tokens").optional_zero();
        let total = NativeCounter::read(usage, "total_tokens").optional_zero();

        let (Some(input), Some(output), Some(thinking), Some(cache_read), Some(total)) =
            (input, output, thinking, cache_read, total)
        else {
            self.malformed_usage = true;
            self.incomplete_charge = true;
            return;
        };

        // On an error envelope before any inference ran, `agy` emits all-zero counters.
        // Record unknown usage (None) rather than fabricating a 0-token completed bill when an
        // error occurred without any observed tokens (ADR-0143).
        if !is_success && input == 0 && output == 0 && thinking == 0 && cache_read == 0 {
            self.usage = None;
            return;
        }

        // Empirically verified against `agy 1.5.18`: `input_tokens` already excludes
        // `cache_read_tokens` (`input_tokens + output_tokens == total_tokens` while
        // `cache_read_tokens` is reported separately). Include `thinking_tokens` when `total_tokens`
        // shows they are accounted separately from `output_tokens`.
        let base = input.saturating_add(output);
        let extra_thinking = if total >= base.saturating_add(thinking) {
            thinking
        } else {
            0
        };
        let chargeable = base.saturating_add(extra_thinking);

        self.usage = Some(TaskTokenUsageV3 {
            chargeable_tokens: DecimalU128::from(u128::from(chargeable)),
            input_tokens: Some(DecimalU128::from(u128::from(input))),
            output_tokens: Some(DecimalU128::from(u128::from(output))),
            cache_read_tokens: Some(DecimalU128::from(u128::from(cache_read))),
            cache_write_tokens: None,
            reasoning_tokens: Some(DecimalU128::from(u128::from(thinking))),
        });
    }

    fn reported_usage(&self) -> Option<TaskTokenUsageV3> {
        self.usage.clone()
    }

    fn observation(&self) -> Option<TaskUsageObservationV1> {
        self.malformed_usage.then(|| TaskUsageObservationV1 {
            reported_usage: self.reported_usage(),
            charge_complete: !self.incomplete_charge,
        })
    }
}

fn unframed_stdout(stdout: &[u8]) -> String {
    let mut out = String::new();
    for line in stdout.split(|byte| *byte == b'\n') {
        let trimmed = line.trim_ascii();
        if trimmed.is_empty() {
            continue;
        }
        let is_frame = trimmed.starts_with(b"{")
            && serde_json::from_slice::<Value>(trimmed)
                .ok()
                .and_then(|v| v.get("event").and_then(Value::as_str).map(|_| ()))
                .is_some();
        if !is_frame {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&String::from_utf8_lossy(trimmed));
        }
    }
    out
}

fn kept_output(stdout: &[u8]) -> Vec<u8> {
    let mut last_result: Option<Vec<u8>> = None;
    let mut init_line: Option<Vec<u8>> = None;
    for line in stdout.split_inclusive(|byte| *byte == b'\n') {
        let trimmed = line.trim_ascii();
        if !trimmed.starts_with(b"{") {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(trimmed) else {
            continue;
        };
        match value.get("event").and_then(Value::as_str) {
            Some("init") => {
                let mut kept = trimmed.to_vec();
                kept.push(b'\n');
                init_line = Some(kept);
            }
            Some("result") => {
                let mut kept = trimmed.to_vec();
                kept.push(b'\n');
                last_result = Some(kept);
            }
            _ => {}
        }
    }
    last_result.or(init_line).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use af_contracts::Arg;
    use std::os::unix::fs::PermissionsExt;

    fn fake_agy(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("agy");
        std::fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn settings_enforce_confinement_for_every_worker_access() {
        let ro = task_settings(WorkerAccess::ReadOnly);
        assert_eq!(ro["sandbox"]["enabled"], true);
        let ro_deny = ro["permissions"]["deny"].as_array().unwrap();
        assert!(ro_deny.contains(&json!("command(*)")));
        assert!(ro_deny.contains(&json!("write_file(*)")));
        assert!(ro_deny.contains(&json!("mcp(*)")));

        let checks = task_settings(WorkerAccess::ExecuteChecks);
        assert!(
            checks["permissions"]["allow"]
                .as_array()
                .unwrap()
                .contains(&json!("command(*)"))
        );
        assert!(
            checks["permissions"]["deny"]
                .as_array()
                .unwrap()
                .contains(&json!("write_file(*)"))
        );

        let write = task_settings(WorkerAccess::WriteSource);
        assert!(
            write["permissions"]["allow"]
                .as_array()
                .unwrap()
                .contains(&json!("write_file(*)"))
        );
        assert!(
            write["permissions"]["deny"]
                .as_array()
                .unwrap()
                .contains(&json!("command(*)"))
        );

        let full = task_settings(WorkerAccess::WriteSourceWithShell);
        let allow = full["permissions"]["allow"].as_array().unwrap();
        assert!(allow.contains(&json!("write_file(*)")));
        assert!(allow.contains(&json!("command(*)")));
        assert!(
            full["permissions"]["deny"]
                .as_array()
                .unwrap()
                .contains(&json!("mcp(*)"))
        );
    }

    #[test]
    fn stream_events_charge_uncached_input_plus_output_and_prefer_structured_output() {
        let stdout = concat!(
            "{\"event\":\"init\",\"conversation_id\":\"conv-1\",\"init\":{\"model\":\"gemini-4-neon-high\"}}\n",
            "{\"event\":\"step_update\",\"step_update\":{\"step_index\":1,\"state\":\"DONE\"}}\n",
            "{\"event\":\"result\",\"result\":{\"conversation_id\":\"conv-1\",\"status\":\"SUCCESS\",\"response\":\"ignored\",\"structured_output\":{\"schema\":\"af.worker-reply/1\",\"outputs\":{\"report\":[{\"ok\":true}]}},\"usage\":{\"input_tokens\":13825,\"output_tokens\":17,\"thinking_tokens\":0,\"cache_read_tokens\":9629,\"total_tokens\":13842}}}\n"
        );
        let events = StreamEvents::parse(stdout.as_bytes());
        assert!(events.framed);
        assert_eq!(events.conversation_id.as_deref(), Some("conv-1"));
        assert_eq!(events.init_model.as_deref(), Some("gemini-4-neon-high"));
        let usage = events.reported_usage().unwrap();
        assert_eq!(usage.chargeable_tokens.get(), 13842);
        assert_eq!(usage.input_tokens.unwrap().get(), 13825);
        assert_eq!(usage.output_tokens.unwrap().get(), 17);
        assert_eq!(usage.cache_read_tokens.unwrap().get(), 9629);
        assert_eq!(usage.reasoning_tokens.unwrap().get(), 0);
        let reply: Value = serde_json::from_slice(events.reply_bytes.as_ref().unwrap()).unwrap();
        assert_eq!(reply["schema"], "af.worker-reply/1");
    }

    #[test]
    fn stream_events_add_separate_thinking_tokens_without_double_counting() {
        let separate = concat!(
            "{\"event\":\"result\",\"result\":{\"status\":\"SUCCESS\",\"response\":\"ok\",",
            "\"usage\":{\"input_tokens\":100,\"output_tokens\":20,\"thinking_tokens\":30,\"cache_read_tokens\":50,\"total_tokens\":150}}}\n"
        );
        let usage = StreamEvents::parse(separate.as_bytes())
            .reported_usage()
            .unwrap();
        assert_eq!(usage.chargeable_tokens.get(), 150);

        let included = concat!(
            "{\"event\":\"result\",\"result\":{\"status\":\"SUCCESS\",\"response\":\"ok\",",
            "\"usage\":{\"input_tokens\":100,\"output_tokens\":50,\"thinking_tokens\":30,\"cache_read_tokens\":50,\"total_tokens\":150}}}\n"
        );
        let usage = StreamEvents::parse(included.as_bytes())
            .reported_usage()
            .unwrap();
        assert_eq!(usage.chargeable_tokens.get(), 150);
    }

    #[test]
    fn fake_agy_worker_round_trip_and_model_switch_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        let workdir = dir.path().join("work");
        let auth_dir = dir.path().join("auth");
        std::fs::create_dir_all(&workdir).unwrap();
        std::fs::create_dir_all(&auth_dir).unwrap();

        let program = fake_agy(
            dir.path(),
            r#"
read -r line
printf '%s\n' '{"event":"init","conversation_id":"conv-ok","init":{"model":"gemini-4-neon-high"}}'
printf '%s\n' '{"event":"result","result":{"conversation_id":"conv-ok","status":"SUCCESS","structured_output":{"schema":"af.worker-reply/1","outputs":{"report":[{"ok":true}]}},"usage":{"input_tokens":40,"output_tokens":10,"thinking_tokens":0,"cache_read_tokens":15,"total_tokens":50}}}'
"#,
        );
        let cmd = Command::new(
            program.display().to_string(),
            ["--model", "gemini-4-neon-high", "--effort", "high"]
                .map(Arg::literal)
                .into(),
        );
        let adapter = AntigravityTaskAdapter::new(&cmd)
            .unwrap()
            .for_provider("antigravity-main")
            .with_auth(auth_dir.clone(), None, None);

        let request = serde_json::to_vec(&json!({
            "schema": "af.worker-request/1",
            "reply_format": af_runner::task::WORKER_REPLY_FORMAT,
            "instructions": "test",
            "inputs": {},
            "feedback": [],
            "output_schemas": {
                "report": {
                    "type": "object",
                    "properties": { "ok": { "type": "boolean" } },
                    "required": ["ok"],
                    "additionalProperties": false
                }
            }
        }))
        .unwrap();

        let result = adapter.invoke(
            &cas,
            &workdir,
            request,
            Duration::from_secs(10),
            WorkerAccess::ReadOnly,
            None,
            &[],
        );
        assert!(result.message.is_ok(), "{:?}", result.message);
        assert_eq!(result.usage.unwrap().chargeable_tokens.get(), 50);

        // Silent model substitution must be refused.
        let bad_program = fake_agy(
            dir.path(),
            r#"
read -r line
printf '%s\n' '{"event":"init","conversation_id":"conv-bad","init":{"model":"gemini-3.8-flash-low"}}'
printf '%s\n' '{"event":"result","result":{"conversation_id":"conv-bad","status":"SUCCESS","response":"{}","usage":{"input_tokens":10,"output_tokens":5,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":15}}}'
"#,
        );
        let bad_cmd = Command::new(
            bad_program.display().to_string(),
            ["--model", "gemini-4-neon-high", "--effort", "high"]
                .map(Arg::literal)
                .into(),
        );
        let bad_adapter = AntigravityTaskAdapter::new(&bad_cmd)
            .unwrap()
            .with_auth(auth_dir, None, None);
        let switched = bad_adapter.invoke(
            &cas,
            &workdir,
            b"hello".to_vec(),
            Duration::from_secs(10),
            WorkerAccess::ReadOnly,
            None,
            &[],
        );
        assert!(switched.message.is_err());
        assert_eq!(
            switched.native_failure,
            Some(NativeFailureKind::ModelUnavailable)
        );
    }

    #[test]
    fn missing_and_zero_error_usage_are_reported_honestly() {
        let missing = concat!(
            "{\"event\":\"result\",\"result\":{\"status\":\"SUCCESS\",\"response\":\"ok\"}}\n"
        );
        let events = StreamEvents::parse(missing.as_bytes());
        assert!(events.malformed_usage);
        assert!(events.reported_usage().is_none());
        let obs = events.observation().expect("malformed observation");
        assert!(!obs.charge_complete);

        let error_zero = concat!(
            "{\"event\":\"result\",\"result\":{\"status\":\"ERROR\",\"error\":\"rate limit exceeded\",\"usage\":{\"input_tokens\":0,\"output_tokens\":0,\"thinking_tokens\":0,\"cache_read_tokens\":0,\"total_tokens\":0}}}\n"
        );
        let events = StreamEvents::parse(error_zero.as_bytes());
        assert!(!events.malformed_usage);
        assert!(events.reported_usage().is_none());
        assert_eq!(events.upstream_failure, Some(NativeFailureKind::Quota));
    }

    #[test]
    fn go_flag_rejection_and_auth_challenge_are_classified_and_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        let workdir = dir.path().join("work");
        let auth_dir = dir.path().join("auth");
        std::fs::create_dir_all(&workdir).unwrap();
        std::fs::create_dir_all(&auth_dir).unwrap();

        let bad_flag = fake_agy(
            dir.path(),
            r#"
echo "flags provided but not defined: -json-schema" >&2
exit 2
"#,
        );
        let cmd = Command::new(
            bad_flag.display().to_string(),
            ["--model", "gemini-4-neon-high", "--effort", "high"]
                .map(Arg::literal)
                .into(),
        );
        let adapter =
            AntigravityTaskAdapter::new(&cmd)
                .unwrap()
                .with_auth(auth_dir.clone(), None, None);
        let out = adapter.invoke(
            &cas,
            &workdir,
            b"hello".to_vec(),
            Duration::from_secs(10),
            WorkerAccess::ReadOnly,
            None,
            &[],
        );
        assert!(out.message.is_err());
        assert_eq!(
            out.native_failure,
            Some(NativeFailureKind::UnsupportedInvocation)
        );

        let auth_challenge = fake_agy(
            dir.path(),
            r#"
echo "Please open https://accounts.google.com/o/oauth2/v2/auth?client_id=123&code=secret-oauth-code" >&2
echo "Authentication required" >&2
exit 1
"#,
        );
        let cmd = Command::new(
            auth_challenge.display().to_string(),
            ["--model", "gemini-4-neon-high", "--effort", "high"]
                .map(Arg::literal)
                .into(),
        );
        let adapter = AntigravityTaskAdapter::new(&cmd)
            .unwrap()
            .with_auth(auth_dir, None, None);
        let out = adapter.invoke(
            &cas,
            &workdir,
            b"hello".to_vec(),
            Duration::from_secs(10),
            WorkerAccess::ReadOnly,
            None,
            &[],
        );
        let err = out.message.unwrap_err();
        assert!(!err.contains("secret-oauth-code"), "{err}");
        assert!(!err.contains("accounts.google.com"), "{err}");
        assert!(
            out.native_failure.is_some_and(NativeFailureKind::is_auth),
            "{:?}",
            out.native_failure
        );
        for id in &out.raw_artifact_ids {
            let bytes = cas.get(id).unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(!text.contains("secret-oauth-code"), "{text}");
            assert!(!text.contains("accounts.google.com"), "{text}");
        }
    }

    #[test]
    fn conversation_artifacts_are_cleaned_unless_kept() {
        let dir = tempfile::tempdir().unwrap();
        let cas = Cas::open(dir.path().join("cas")).unwrap();
        let workdir = dir.path().join("work");
        let auth_dir = dir.path().join("auth");
        std::fs::create_dir_all(&workdir).unwrap();
        std::fs::create_dir_all(auth_dir.join("conversations")).unwrap();
        std::fs::create_dir_all(auth_dir.join("brain/conv-clean")).unwrap();
        std::fs::write(auth_dir.join("conversations/conv-clean.pb"), b"pb").unwrap();
        std::fs::write(auth_dir.join("brain/conv-clean/note.md"), b"note").unwrap();

        let program = fake_agy(
            dir.path(),
            r#"
read -r line
printf '%s\n' '{"event":"init","conversation_id":"conv-clean","init":{"model":"gemini-4-neon-high"}}'
printf '%s\n' '{"event":"result","result":{"conversation_id":"conv-clean","status":"SUCCESS","response":"ok","usage":{"input_tokens":10,"output_tokens":5,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":15}}}'
"#,
        );
        let cmd = Command::new(
            program.display().to_string(),
            ["--model", "gemini-4-neon-high", "--effort", "high"]
                .map(Arg::literal)
                .into(),
        );
        let adapter =
            AntigravityTaskAdapter::new(&cmd)
                .unwrap()
                .with_auth(auth_dir.clone(), None, None);
        let out = adapter.invoke(
            &cas,
            &workdir,
            b"hello".to_vec(),
            Duration::from_secs(10),
            WorkerAccess::ReadOnly,
            None,
            &[],
        );
        assert!(out.message.is_ok(), "{:?}", out.message);
        assert!(!auth_dir.join("conversations/conv-clean.pb").exists());
        assert!(!auth_dir.join("brain/conv-clean").exists());
    }
}
