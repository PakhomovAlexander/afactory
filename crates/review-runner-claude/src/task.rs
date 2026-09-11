//! Claude framing for generic typed Task Workers. No review-result parser or retry loop.
use super::*;
use review_runner::task::{ModelWorkerReturn, WorkerModelAdapter};

pub struct ClaudeTaskAdapter {
    program: String,
    model_flags: Vec<String>,
    grants: Vec<(String, String)>,
}

impl ClaudeTaskAdapter {
    pub fn new(command: &Command) -> Result<Self, String> {
        Ok(Self {
            program: command.program.clone(),
            model_flags: claude_model_flags(command)?,
            grants: vec![],
        })
    }
    pub fn with_auth(mut self, config_dir: Option<String>, user: String, home: String) -> Self {
        self.grants = vec![("USER".into(), user), ("HOME".into(), home)];
        if let Some(config) = config_dir {
            self.grants.push(("CLAUDE_CONFIG_DIR".into(), config));
        }
        self
    }
}

impl WorkerModelAdapter for ClaudeTaskAdapter {
    fn provider_kind(&self) -> &'static str {
        "claude"
    }
    fn model_settings(&self) -> Option<(String, String)> {
        let value = |flag| {
            self.model_flags
                .chunks_exact(2)
                .find(|args| args[0] == flag)
                .map(|args| args[1].clone())
        };
        value("--model").zip(value("--effort"))
    }
    fn invoke(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        writable: bool,
    ) -> ModelWorkerReturn {
        let mut command = claude_command(&self.program, &self.model_flags);
        if writable {
            // The same restricted root and customization isolation as review. The write role
            // adds only native file edits; a package cannot supply Bash, MCP or permission flags.
            for arg in &mut command.args {
                if arg.value == "Read,Glob,Grep" {
                    *arg = Arg::literal("Read,Glob,Grep,Edit,Write");
                }
            }
        }
        let mut runner = ModelRunner::new(workdir, timeout);
        for (name, value) in &self.grants {
            runner = runner.with_env(name, value);
        }
        let capture = match runner.capture_with_stdin(cas, &command, input) {
            Ok(capture) => capture,
            Err(error) => return ModelWorkerReturn::failed(error),
        };
        let parsed = serde_json::from_slice::<serde_json::Value>(&capture.stdout).ok();
        let usage = parsed
            .as_ref()
            .and_then(|value| value.get("usage"))
            .filter(|u| u.is_object());
        let count = |key| {
            usage
                .and_then(|u| u.get(key))
                .and_then(serde_json::Value::as_u64)
        };
        let input = count("input_tokens");
        let output = count("output_tokens");
        let cache_write = count("cache_creation_input_tokens");
        let charge = input
            .zip(output)
            .and_then(|(i, o)| i.checked_add(o))
            .and_then(|n| n.checked_add(cache_write.unwrap_or(0)));
        let usage = charge.map(|chargeable_tokens| TokenUsage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: count("cache_read_input_tokens"),
            cache_write_tokens: cache_write,
            reasoning_tokens: None,
            chargeable_tokens,
        });
        let success = capture.status.success()
            && parsed.as_ref().is_some_and(|v| {
                v.get("is_error").and_then(serde_json::Value::as_bool) == Some(false)
            });
        let message = if success {
            parsed
                .as_ref()
                .and_then(|v| v.get("result"))
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.as_bytes().to_vec())
                .ok_or_else(|| "Claude Worker returned no final message".into())
        } else {
            Err(format!("Claude Worker failed with {}", capture.status))
        };
        ModelWorkerReturn {
            message,
            usage,
            raw_artifact_ids: vec![capture.raw_artifact],
        }
    }
}
