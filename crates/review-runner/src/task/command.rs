//! The isolated command Worker transport: a private runtime home, cooperative cancellation and
//! the sandbox-local variables the kernel resolved for this exact Attempt.
use super::*;
use crate::ModelRunner;
use review_core::Command;

#[allow(clippy::too_many_arguments)]
pub fn invoke_command(
    cas: &Cas,
    workdir: &Path,
    runtime_root: &Path,
    command: &Command,
    contract: &WorkerContract,
    context_id: &str,
    timeout: Duration,
    cancellation: Option<&AtomicBool>,
    environment: &[(String, String)],
) -> WorkerReturn {
    let returned = match contract.read_context(cas, context_id) {
        Ok((_, bytes)) => invoke_command_bytes(
            cas,
            workdir,
            runtime_root,
            command,
            bytes,
            timeout,
            cancellation,
            environment,
        ),
        Err(error) => ModelWorkerReturn {
            message: Err(error),
            usage: Some(review_core::task::usage::TaskTokenUsageV3::charge_only(0)),
            usage_observation: None,
            raw_artifact_ids: vec![],
        },
    };
    let (reply, feedback_code) = match returned.message {
        Ok(bytes) => {
            let reply = contract.validate_reply(&bytes);
            let code = reply
                .is_err()
                .then_some(TaskFeedbackCodeV1::InvalidOutputContract);
            (reply, code)
        }
        Err(error) => (Err(error), Some(TaskFeedbackCodeV1::ProcessFailure)),
    };
    WorkerReturn {
        reply,
        feedback_code,
        usage: returned.usage,
        usage_observation: None,
        raw_artifact_ids: returned.raw_artifact_ids,
    }
}

/// The same transport with an already framed input and an independently installed business
/// parser. No scheduler or retry loop; the caller supplies an already-started Attempt's
/// remaining time.
#[allow(clippy::too_many_arguments)]
pub fn invoke_command_bytes(
    cas: &Cas,
    workdir: &Path,
    runtime_root: &Path,
    command: &Command,
    bytes: Vec<u8>,
    timeout: Duration,
    cancellation: Option<&AtomicBool>,
    environment: &[(String, String)],
) -> ModelWorkerReturn {
    let runner = match command_runner(workdir, runtime_root, timeout, environment) {
        Ok(runner) => runner,
        Err(error) => {
            let mut value = ModelWorkerReturn::failed(error);
            value.usage = Some(review_core::task::usage::TaskTokenUsageV3::charge_only(0));
            return value;
        }
    };
    let capture = runner.capture(cas, command, bytes, cancellation);
    let mut raw_artifact_ids = capture.raw_artifact_ids;
    let message = match capture.status {
        Ok(status) if status.success() => {
            // A successful command's evidence is its single stdout ID.
            match cas.put(&capture.stdout) {
                Ok(id) => {
                    raw_artifact_ids = vec![id];
                    Ok(capture.stdout)
                }
                Err(error) => Err(format!("storing raw output: {error}")),
            }
        }
        Ok(status) => Err(format!("Command Worker exited with {status}")),
        Err(error) => Err(error.to_string()),
    };
    ModelWorkerReturn {
        message,
        usage: Some(review_core::task::usage::TaskTokenUsageV3::charge_only(0)),
        usage_observation: None,
        raw_artifact_ids,
    }
}

fn command_runner(
    workdir: &Path,
    runtime_root: &Path,
    timeout: Duration,
    additional_environment: &[(String, String)],
) -> Result<ModelRunner, RunnerError> {
    let deadline = std::time::Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| RunnerError::Refused("Command deadline overflow".into()))?;
    let mut environment = Vec::new();
    for (key, directory) in [
        ("HOME", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_STATE_HOME", "state"),
        ("TMPDIR", "tmp"),
    ] {
        let path = runtime_root.join(directory);
        std::fs::create_dir_all(&path).map_err(|e| RunnerError::Unavailable(e.to_string()))?;
        environment.push((
            key,
            path.to_str()
                .ok_or_else(|| RunnerError::Refused("Worker runtime path is not UTF-8".into()))?
                .to_owned(),
        ));
    }
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() {
        return Err(RunnerError::TimedOut {
            after_ms: timeout.as_millis().try_into().unwrap_or(u64::MAX),
        });
    }
    let mut runner = ModelRunner::new(workdir, remaining);
    for (key, value) in environment {
        runner = runner.with_env(key, value);
    }
    for (key, value) in additional_environment {
        if key.is_empty()
            || key.contains('=')
            || key.chars().any(char::is_control)
            || value.contains('\0')
        {
            return Err(RunnerError::Refused(
                "Worker environment contains an invalid variable".into(),
            ));
        }
        runner = runner.with_env(key, value);
    }
    Ok(runner)
}
