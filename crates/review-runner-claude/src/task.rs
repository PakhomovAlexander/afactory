//! Claude framing for generic typed Task Workers. No review-result parser or retry loop.
use super::*;
use review_runner::task::{ModelWorkerReturn, WorkerModelAdapter};

mod model_usage;
mod structured;

pub struct ClaudeTaskAdapter {
    program: String,
    model_flags: Vec<String>,
    /// The explicit `--model` restriction every reported model's usage is checked against.
    model: String,
    grants: Vec<(String, String)>,
}

impl ClaudeTaskAdapter {
    /// Refuses a command without an explicit `--model`: usage accounting needs the selected
    /// model to tell its own spend from unexpected model activity.
    pub fn new(command: &Command) -> Result<Self, String> {
        let model_flags = claude_model_flags(command)?;
        let model = model_flags
            .chunks_exact(2)
            .find(|pair| pair[0] == "--model")
            .map(|pair| pair[1].clone())
            .ok_or("Claude Task Worker requires an explicit --model")?;
        Ok(Self {
            program: command.program.clone(),
            model_flags,
            model,
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
    fn credential_mode(&self) -> review_core::BrokerCredentialModeV1 {
        review_core::BrokerCredentialModeV1::TrustedUnsafe
    }

    fn provider_kind(&self) -> &'static str {
        "claude"
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
        writable: bool,
    ) -> ModelWorkerReturn {
        self.invoke_inner(cas, workdir, input, timeout, writable, None, &[])
    }

    fn invoke_controlled(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        writable: bool,
        broker: Option<&dyn review_runner::ExactBrokerClient>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
    ) -> ModelWorkerReturn {
        if broker.is_some() {
            return self.invoke_with_broker(cas, workdir, input, timeout, writable, broker);
        }
        self.invoke_inner(cas, workdir, input, timeout, writable, cancellation, &[])
    }

    fn invoke_controlled_with_environment(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        writable: bool,
        broker: Option<&dyn review_runner::ExactBrokerClient>,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
        environment: &[(String, String)],
    ) -> ModelWorkerReturn {
        if broker.is_some() {
            return self.invoke_with_broker(cas, workdir, input, timeout, writable, broker);
        }
        self.invoke_inner(
            cas,
            workdir,
            input,
            timeout,
            writable,
            cancellation,
            environment,
        )
    }
}

impl ClaudeTaskAdapter {
    #[allow(clippy::too_many_arguments)]
    fn invoke_inner(
        &self,
        cas: &Cas,
        workdir: &Path,
        input: Vec<u8>,
        timeout: Duration,
        writable: bool,
        cancellation: Option<&std::sync::atomic::AtomicBool>,
        environment: &[(String, String)],
    ) -> ModelWorkerReturn {
        if cancellation.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire)) {
            return ModelWorkerReturn {
                usage_observation: None,
                message: Err("Worker invocation was cancelled before starting".into()),
                usage: Some(review_core::task::usage::TaskTokenUsageV3::charge_only(0)),
                raw_artifact_ids: vec![],
            };
        }
        let output_schema = match structured::output_schema(&input) {
            Ok(schema) => schema,
            Err(error) => {
                return ModelWorkerReturn {
                    usage_observation: None,
                    message: Err(error),
                    usage: Some(review_core::task::usage::TaskTokenUsageV3::charge_only(0)),
                    raw_artifact_ids: vec![],
                };
            }
        };
        // The common Task path installs no session layer (its Attempts record
        // `host_unsupported`), so the command carries no session flags.
        let mut command = claude_command(&self.program, &self.model_flags, None);
        if let Some(schema) = &output_schema {
            command
                .args
                .extend([Arg::literal("--json-schema"), Arg::literal(schema)]);
        }
        if writable {
            // The same restricted root and customization isolation as review. The write role
            // adds only native file edits; a package cannot supply Bash, MCP or permission flags.
            for arg in &mut command.args {
                if arg.value == "Read,Glob,Grep" {
                    *arg = Arg::literal("Read,Glob,Grep,Edit,Write");
                }
            }
        }
        // Native 2.1.272 uses these guards to latch automatic title generation before its
        // auxiliary inference. They preserve OAuth/auth grants, unlike --bare. This is a
        // bounded client mitigation, not proof that every internal model request is disabled.
        let mut runner = ModelRunner::new(workdir, timeout)
            .with_env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
            .with_env("CLAUDE_CODE_DISABLE_TERMINAL_TITLE", "1");
        for (name, value) in &self.grants {
            runner = runner.with_env(name, value);
        }
        // Sandbox-local, non-secret context resolved by the kernel for this exact Attempt.
        for (name, value) in environment {
            runner = runner.with_env(name, value);
        }
        let capture =
            runner.capture_settled_with_stdin_controlled(cas, &command, input, cancellation);
        let parsed = serde_json::from_slice::<serde_json::Value>(&capture.stdout).ok();
        let accounting = model_usage::account(parsed.as_ref(), &self.model);
        let success = accounting.error.is_none()
            && accounting.observation.is_none()
            && capture.status.as_ref().is_ok_and(|status| status.success())
            && parsed.as_ref().is_some_and(|v| {
                v.get("is_error").and_then(serde_json::Value::as_bool) == Some(false)
            });
        let message = if success {
            structured::message(
                parsed.as_ref().expect("successful envelope"),
                output_schema.is_some(),
            )
        } else if let Some(error) = accounting.error {
            Err(error.into())
        } else {
            Err(format!("Claude Worker failed with {:?}", capture.status))
        };
        ModelWorkerReturn {
            usage_observation: accounting.observation,
            message,
            usage: accounting.usage,
            raw_artifact_ids: capture.raw_artifact_ids,
        }
    }
}

fn parse_usage(
    value: Option<&serde_json::Value>,
) -> (
    Option<review_core::task::usage::TaskTokenUsageV3>,
    Option<review_core::task::usage::TaskUsageObservationV1>,
) {
    use review_core::task::usage::{TaskTokenUsageV3, TaskUsageObservationV1};
    use review_runner::task::usage::NativeCounter;
    let Some(value) = value else {
        return (None, None);
    };
    let Some(usage) = value.get("usage").filter(|value| value.is_object()) else {
        return (
            None,
            Some(TaskUsageObservationV1 {
                reported_usage: None,
                charge_complete: false,
            }),
        );
    };
    let input = NativeCounter::read(usage, "input_tokens").value();
    let output = NativeCounter::read(usage, "output_tokens").value();
    let write = NativeCounter::read(usage, "cache_creation_input_tokens");
    let read = NativeCounter::read(usage, "cache_read_input_tokens");
    let complete = input.is_some() && output.is_some() && write.optional_zero().is_some();
    let malformed = !complete || read == NativeCounter::Invalid;
    let reported =
        (input.is_some() || output.is_some() || write.value().is_some() || read.value().is_some())
            .then(|| TaskTokenUsageV3 {
                input_tokens: input.map(|n| u128::from(n).into()),
                output_tokens: output.map(|n| u128::from(n).into()),
                cache_read_tokens: read.value().map(|n| u128::from(n).into()),
                cache_write_tokens: write.value().map(|n| u128::from(n).into()),
                reasoning_tokens: None,
                // Claude input excludes cache reads; cache creation is an additional billed
                // component. Keep each independently valid contribution when another is malformed.
                chargeable_tokens: (u128::from(input.unwrap_or(0))
                    + u128::from(output.unwrap_or(0))
                    + u128::from(write.value().unwrap_or(0)))
                .into(),
            });
    let observation = malformed.then(|| TaskUsageObservationV1 {
        reported_usage: reported.clone(),
        charge_complete: complete,
    });
    (reported, observation)
}

#[cfg(test)]
mod usage_tests;
