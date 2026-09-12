//! Codex framing for generic typed Task Workers, preserving failed and malformed usage.
use super::*;
use review_runner::task::{MAX_WORKER_BYTES, ModelWorkerReturn, WorkerModelAdapter};
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
        let events = Events::parse(&capture.stdout);
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
        let read_final = || -> Result<Option<Vec<u8>>, String> {
            let file = match std::fs::File::open(&last_message) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error.to_string()),
            };
            let mut bytes = Vec::new();
            file.take(MAX_WORKER_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > MAX_WORKER_BYTES {
                return Err("Codex Worker final message exceeds its byte bound".into());
            }
            Ok((!bytes.is_empty()).then_some(bytes))
        };
        returned.message = read_final().and_then(|bytes| {
            bytes
                .or_else(|| events.final_message.map(String::into_bytes))
                .ok_or_else(|| "Codex Worker returned no final message".into())
        });
        returned
    }
}
