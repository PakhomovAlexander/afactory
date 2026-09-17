//! Controlled calls share command isolation and framing with their historical None path.
use super::*;
use std::sync::atomic::AtomicBool;

#[allow(clippy::too_many_arguments)]
pub fn invoke_command_controlled(
    cas: &Cas,
    workdir: &Path,
    runtime_root: &Path,
    command: &Command,
    contract: &WorkerContract,
    context_id: &str,
    timeout: Duration,
    cancellation: Option<&AtomicBool>,
) -> WorkerReturn {
    invoke_command_controlled_with_environment(
        cas,
        workdir,
        runtime_root,
        command,
        contract,
        context_id,
        timeout,
        cancellation,
        &[],
    )
}

#[allow(clippy::too_many_arguments)]
pub fn invoke_command_controlled_with_environment(
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
    if cancellation.is_none() && environment.is_empty() {
        return invoke_command(
            cas,
            workdir,
            runtime_root,
            command,
            contract,
            context_id,
            timeout,
        );
    }
    let returned = match contract.read_context(cas, context_id) {
        Ok((_, bytes)) => invoke_command_bytes_controlled_with_environment(
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

#[allow(clippy::too_many_arguments)]
pub fn invoke_command_bytes_controlled(
    cas: &Cas,
    workdir: &Path,
    runtime_root: &Path,
    command: &Command,
    bytes: Vec<u8>,
    timeout: Duration,
    cancellation: Option<&AtomicBool>,
) -> ModelWorkerReturn {
    invoke_command_bytes_controlled_with_environment(
        cas,
        workdir,
        runtime_root,
        command,
        bytes,
        timeout,
        cancellation,
        &[],
    )
}

/// The isolated command transport with an already framed input, cooperative cancellation and
/// sandbox-local variables the kernel resolved for this exact Attempt.
#[allow(clippy::too_many_arguments)]
pub fn invoke_command_bytes_controlled_with_environment(
    cas: &Cas,
    workdir: &Path,
    runtime_root: &Path,
    command: &Command,
    bytes: Vec<u8>,
    timeout: Duration,
    cancellation: Option<&AtomicBool>,
    environment: &[(String, String)],
) -> ModelWorkerReturn {
    if cancellation.is_none() && environment.is_empty() {
        return invoke_command_bytes(cas, workdir, runtime_root, command, bytes, timeout);
    }
    let runner = match command_runner_with_environment(workdir, runtime_root, timeout, environment)
    {
        Ok(runner) => runner,
        Err(error) => {
            let mut value = ModelWorkerReturn::failed(error);
            value.usage = Some(review_core::task::usage::TaskTokenUsageV3::charge_only(0));
            return value;
        }
    };
    let capture = runner.capture_settled_with_stdin_controlled(cas, command, bytes, cancellation);
    let mut raw_artifact_ids = capture.raw_artifact_ids;
    let message = match capture.status {
        Ok(status) if status.success() => {
            // Successful command framing retains the original single stdout evidence ID.
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
