//! The only effect boundary. No workflow supplies a command, argv or environment. Machine
//! policy pins executable bytes; the bounded versioned protocol binds every request to intent.
use super::contract::*;
use super::state::Dispatch;
use super::store::read;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use std::time::Instant;
const OWNED_BYTES: u64 = 400_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Method {
    Reconcile,
    Execute,
    Recover,
    Observe,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub schema: String,
    pub method: Method,
    pub intent_id: String,
    pub input_digest: String,
    pub operation: Operation,
    pub destination: String,
    pub inputs: BTreeMap<String, Datum>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Outcome {
    Done,
    Pending,
    Absent,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Response {
    pub schema: String,
    pub intent_id: String,
    pub input_digest: String,
    pub outcome: Outcome,
    pub evidence: String,
    pub observation: Option<Observation>,
}
impl Response {
    fn validate(&self, request: &Request) -> Result<()> {
        require(
            self.schema == "af/scenario-adapter-response@1"
                && self.intent_id == request.intent_id
                && self.input_digest == request.input_digest,
            "Adapter returned stale or unrelated evidence",
        )?;
        text(&self.evidence)?;
        if let Some(o) = &self.observation {
            o.validate()?;
        }
        require(
            request.method != Method::Observe
                || self.observation.is_some()
                || self.outcome != Outcome::Done,
            "Observer omitted typed evidence",
        )?;
        require(
            request.method == Method::Observe || self.observation.is_none(),
            "Action returned observation instead of action evidence",
        )
    }
}
#[derive(Debug)]
pub(super) enum Settlement {
    Done(BTreeMap<String, Datum>),
    TaskDone {
        outputs: BTreeMap<String, Datum>,
        unknown_usage: bool,
    },
    Waiting(String),
    Failed(String),
    TaskFailed {
        diagnostic: String,
        charged_tokens: u64,
        unknown_usage: bool,
    },
    Uncertain(String),
}
fn call(
    adapter: &Adapter,
    request: &Request,
    timeout_ms: u64,
    cancellation: &AtomicBool,
) -> Result<Response> {
    require(
        request.method != Method::Recover
            || matches!(
                request.operation,
                Operation::ImplPrepareV1 | Operation::ImplPublishV1 | Operation::ImplCleanupV1
            ),
        "Recovery is restricted to native lifecycle operations",
    )?;
    require(
        digest(&read(&adapter.executable, MAX_BYTES)?) == adapter.digest,
        "Pinned adapter executable changed",
    )?;
    let mut command = std::process::Command::new(&adapter.executable);
    command.current_dir(
        adapter
            .executable
            .parent()
            .ok_or("Adapter has no installation directory")?,
    );
    command
        .env_clear()
        .env("PATH", review_process::sanitized_path());
    // An installed adapter owns credential access; workflow text never names an environment
    // variable or secret. The same machine account is the only ambient authority it receives.
    for key in ["HOME", "XDG_CONFIG_HOME"] {
        if let Some(v) = std::env::var_os(key) {
            command.env(key, v);
        }
    }
    let input = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    let capture = review_process::run_supervised_captured_bounded(
        &mut command,
        Some(input),
        Duration::from_millis(timeout_ms.min(adapter.max_wall_ms)),
        review_process::ExitPolicy::KillDescendants,
        Some(cancellation),
        adapter.max_output_bytes as usize,
    );
    let output = capture
        .into_result()
        .map_err(|e| format!("Adapter outcome uncertain: {e}"))?;
    require(
        output.status.success() && !output.truncation.is_truncated(),
        "Adapter failed or exceeded its output bound; outcome uncertain",
    )?;
    let response: Response = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("Invalid adapter response: {e}"))?;
    response.validate(request)?;
    Ok(response)
}
pub(super) fn dispatch(
    policy: &Policy,
    dispatch: &Dispatch,
    cancellation: &AtomicBool,
) -> Result<Settlement> {
    match &dispatch.definition.operation {
        Kind::Task { target } => task(dispatch, target, cancellation),
        Kind::FreshTask { .. } => task(
            dispatch,
            dispatch
                .target
                .as_ref()
                .ok_or("Fresh Task has no committed native plan")?,
            cancellation,
        ),
        Kind::Wait { grant, condition } => {
            let request = Request {
                schema: "af/scenario-adapter-request@3".into(),
                method: Method::Observe,
                intent_id: dispatch.intent.id.clone(),
                input_digest: dispatch.intent.input_digest.clone(),
                operation: Operation::Observe,
                destination: grant.destination.clone(),
                inputs: dispatch.inputs.clone(),
            };
            let a = policy
                .adapters
                .get(&grant.adapter)
                .ok_or("Observer not in machine policy")?;
            let mut request = request;
            request
                .inputs
                .insert("condition".into(), Datum::Text(condition.clone()));
            let r = call(a, &request, dispatch.timeout_ms, cancellation)?;
            if r.outcome == Outcome::Failed {
                return Ok(Settlement::Failed(r.evidence));
            }
            if let Some(o) = r.observation {
                require(
                    o.subject == dispatch.inputs["subject"].as_text()?
                        && o.revision == dispatch.inputs["revision"].as_text()?
                        && o.condition == *condition,
                    "Observed object revision changed; previous checks and decisions are stale",
                )?;
                if o.satisfied && r.outcome == Outcome::Done {
                    return Ok(Settlement::Done(BTreeMap::from([(
                        "result".into(),
                        Datum::Observation(o),
                    )])));
                }
            }
            Ok(Settlement::Waiting(r.evidence))
        }
        Kind::Action { grant, .. } if grant.adapter == "builtin" => file_action(dispatch, grant),
        Kind::Action { grant, .. } => {
            super::implementation::validate_inputs(grant.operation, &dispatch.inputs)?;
            let a = policy
                .adapters
                .get(&grant.adapter)
                .ok_or("Action adapter not in machine policy")?;
            let mut request = Request {
                schema: "af/scenario-adapter-request@3".into(),
                method: Method::Reconcile,
                intent_id: dispatch.intent.id.clone(),
                input_digest: dispatch.intent.input_digest.clone(),
                operation: grant.operation,
                destination: grant.destination.clone(),
                inputs: dispatch.inputs.clone(),
            };
            // Reconciliation is mandatory even for a fresh intent. An absent response is a
            // positive, authoritative no-effect result, not a timeout or a missing response.
            let started = Instant::now();
            let mut r = match call(a, &request, dispatch.timeout_ms, cancellation) {
                Ok(r) => r,
                Err(why) => return Ok(Settlement::Uncertain(why)),
            };
            if r.outcome == Outcome::Absent {
                request.method = Method::Execute;
                let left = dispatch.timeout_ms.saturating_sub(
                    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                );
                if left == 0 {
                    return Ok(Settlement::Uncertain(
                        "Step deadline reached before execute; reconcile the recorded intent"
                            .into(),
                    ));
                }
                r = match call(a, &request, left, cancellation) {
                    Ok(r) => r,
                    Err(why) => return Ok(Settlement::Uncertain(why)),
                };
                require(r.outcome != Outcome::Absent, "Execute cannot return absent")?;
            }
            if r.outcome == Outcome::Pending
                && matches!(
                    grant.operation,
                    Operation::ImplPrepareV1 | Operation::ImplPublishV1 | Operation::ImplCleanupV1
                )
            {
                // Recovery is explicit execution authority for this already reserved lifecycle
                // intent. Reconcile itself never executes a remaining lifecycle substep.
                request.method = Method::Recover;
                let left = dispatch.timeout_ms.saturating_sub(
                    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                );
                if left == 0 {
                    return Ok(Settlement::Uncertain(
                        "Lifecycle recovery reached its original step deadline".into(),
                    ));
                }
                r = match call(a, &request, left, cancellation) {
                    Ok(r) => r,
                    Err(why) => return Ok(Settlement::Uncertain(why)),
                };
                require(
                    r.outcome != Outcome::Absent,
                    "Recovery cannot return absent",
                )?;
            }
            match r.outcome {
                Outcome::Done => {
                    if grant.operation == Operation::ImplPrepareV1 {
                        super::implementation::validate_preparation(
                            &ActionReceipt {
                                intent_id: dispatch.intent.id.clone(),
                                operation: grant.operation,
                                destination: grant.destination.clone(),
                                subject: dispatch.inputs["subject"].as_text()?.into(),
                                revision: dispatch.inputs["revision"].as_text()?.into(),
                                evidence: r.evidence.clone(),
                            },
                            &dispatch.inputs,
                        )?;
                    }
                    Ok(Settlement::Done(BTreeMap::from([(
                        "result".into(),
                        Datum::Action(ActionReceipt {
                            intent_id: dispatch.intent.id.clone(),
                            operation: grant.operation,
                            destination: grant.destination.clone(),
                            subject: dispatch.inputs["subject"].as_text()?.into(),
                            revision: dispatch.inputs["revision"].as_text()?.into(),
                            evidence: r.evidence,
                        }),
                    )])))
                }
                Outcome::Failed => Ok(Settlement::Failed(r.evidence)),
                _ => Ok(Settlement::Waiting(r.evidence)),
            }
        }
        _ => Err("Non-dispatchable Scenario step".into()),
    }
}
fn task(d: &Dispatch, target: &TaskTarget, cancellation: &AtomicBool) -> Result<Settlement> {
    fn checked(target: &TaskTarget) -> Result<crate::task_execution::OpenedTask> {
        let task =
            crate::task_execution::open_task(&target.task_id, &target.repo, Some(&target.state))?;
        require(
            task.task.revision_id == target.revision_id
                && task.task.plan_id.as_deref() == Some(&target.plan_id)
                && task.task.revision.limits.tokens <= target.max_tokens,
            "Pinned Task revision, plan or budget changed",
        )?;
        Ok(task)
    }
    let task = checked(target)?;
    if !matches!(
        task.task.phase,
        review_core::task::TaskPhaseV1::Finished { .. }
    ) {
        drop(task);
        let mut command =
            std::process::Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
        command
            .args([
                "task",
                "run",
                &target.task_id,
                "--confirm-plan",
                &target.plan_id,
                "--quiet",
                "--json",
                "--repo",
            ])
            .arg(&target.repo)
            .arg("--state")
            .arg(&target.state);
        let capture = review_process::run_supervised_captured_bounded(
            &mut command,
            None,
            Duration::from_millis(d.timeout_ms),
            review_process::ExitPolicy::KillDescendants,
            Some(cancellation),
            1_048_576,
        );
        // The Task Store, never exit status or stdout, supplies completion and acceptance.
        // A lost child is resumed under the same Task revision and its own recovery rules.
        let _ = capture;
    }
    let task = checked(target)?;
    let (result, _) = task.view()?;
    let Some(result) = result else {
        return Ok(Settlement::Waiting(format!(
            "Task {} has no accepted result yet",
            target.task_id
        )));
    };
    let charged_tokens = task.task.execution.as_ref().map_or(Ok(0), |e| {
        u64::try_from(e.budget.committed_tokens()).map_err(|_| "Task charge exceeds u64")
    })?;
    let unknown_usage = task.task.execution.as_ref().is_some_and(|e| {
        e.attempt_accounting()
            .iter()
            .any(|a| a.unknown_usage.is_some())
    });
    if result.acceptance != review_core::task::TaskAcceptanceV1::Satisfied
        || result.execution != review_core::task::TaskExecutionV1::Completed
    {
        return Ok(Settlement::TaskFailed {
            diagnostic: format!(
                "Task {} did not satisfy acceptance: {:?}",
                target.task_id, result.acceptance
            ),
            charged_tokens,
            unknown_usage,
        });
    }
    let review_core::task::TaskPhaseV1::Finished { result_id } = &task.task.phase else {
        return Err("Task result lacks durable completion".into());
    };
    let receipt = TaskReceipt {
        task_id: target.task_id.clone(),
        plan_id: target.plan_id.clone(),
        revision_id: target.revision_id.clone(),
        result_id: result_id.clone(),
        charged_tokens,
        result,
    };
    Ok(Settlement::TaskDone {
        outputs: BTreeMap::from([("result".into(), Datum::Task(receipt))]),
        unknown_usage,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OwnedDocument {
    pub schema: String,
    pub owner: String,
    pub content: String,
}
fn file_action(d: &Dispatch, grant: &Grant) -> Result<Settlement> {
    use rustix::fs::{Mode, OFlags, openat};
    let root = Path::new(&grant.destination);
    require(
        root.is_absolute() && root.canonicalize().map_err(|e| e.to_string())? == root,
        "Owned file root changed or contains a symlink",
    )?;
    let anchor = review_sandbox::open_anchor(root).map_err(|e| e.to_string())?;
    if grant.operation == Operation::WriteOwnedFile {
        let file = d.inputs["file"].as_text()?;
        name(file)?;
        let document = OwnedDocument {
            schema: "af/scenario-owned-file@1".into(),
            owner: d.intent.id.clone(),
            content: d.inputs["content"].as_text()?.into(),
        };
        let bytes = serde_json::to_vec(&document).map_err(|e| e.to_string())?;
        // The temporary file is private and complete before the no-replace link publishes it.
        let mut temp = tempfile::NamedTempFile::new_in(root).map_err(|e| e.to_string())?;
        temp.write_all(&bytes)
            .and_then(|()| temp.as_file().sync_all())
            .map_err(|e| e.to_string())?;
        let temp_name = temp
            .path()
            .file_name()
            .ok_or("Missing temporary file name")?;
        match rustix::fs::linkat(
            &anchor,
            temp_name,
            &anchor,
            file,
            rustix::fs::AtFlags::empty(),
        ) {
            Ok(()) => {}
            Err(rustix::io::Errno::EXIST) => {}
            Err(e) => return Err(e.to_string()),
        }
        let fd = openat(
            &anchor,
            file,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| e.to_string())?;
        let mut held = std::fs::File::from(fd);
        let meta = held.metadata().map_err(|e| e.to_string())?;
        require(
            meta.is_file() && meta.len() <= OWNED_BYTES,
            "Owned file is not a bounded regular file",
        )?;
        let mut current = Vec::new();
        Read::by_ref(&mut held)
            .take(OWNED_BYTES + 1)
            .read_to_end(&mut current)
            .map_err(|e| e.to_string())?;
        require(
            current == bytes,
            "Destination is unowned or modified; preserving it",
        )?;
        rustix::fs::fsync(&anchor).map_err(|e| e.to_string())?;
        let owned = OwnedFile {
            root: root.into(),
            file: file.into(),
            digest: digest(&bytes),
            owner: d.intent.id.clone(),
            device: meta.dev(),
            inode: meta.ino(),
        };
        return Ok(Settlement::Done(BTreeMap::from([(
            "result".into(),
            Datum::OwnedFile(owned),
        )])));
    }
    require(
        grant.operation == Operation::CleanupOwnedFile,
        "Unknown builtin action",
    )?;
    let Some(Datum::OwnedFile(owned)) = d.inputs.get("owned") else {
        return Err("Cleanup requires a kernel ownership receipt".into());
    };
    require(
        owned.root == root,
        "Cleanup destination differs from ownership receipt",
    )?;
    let claim = format!(
        "af-cleanup-{}",
        d.intent
            .id
            .strip_prefix("sha256:")
            .ok_or("Invalid cleanup intent")?
    );
    match rustix::fs::mkdirat(&anchor, claim.as_str(), Mode::from_raw_mode(0o700)) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
        Err(e) => return Err(e.to_string()),
    }
    let claim_fd = openat(
        &anchor,
        claim.as_str(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| e.to_string())?;
    let existing = openat(
        &claim_fd,
        "file",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    );
    if let Err(rustix::io::Errno::NOENT) = existing {
        // Inspect before claiming so an already modified entry, a symlink or a special file
        // remains at its original name. Recheck after the atomic claim to cover replacements.
        match openat(
            &anchor,
            owned.file.as_str(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => {
                let mut current = std::fs::File::from(fd);
                let meta = current.metadata().map_err(|e| e.to_string())?;
                let mut bytes = Vec::new();
                Read::by_ref(&mut current)
                    .take(OWNED_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| e.to_string())?;
                if !meta.is_file()
                    || meta.dev() != owned.device
                    || meta.ino() != owned.inode
                    || digest(&bytes) != owned.digest
                {
                    let _ = rustix::fs::unlinkat(
                        &anchor,
                        claim.as_str(),
                        rustix::fs::AtFlags::REMOVEDIR,
                    );
                    return Ok(Settlement::Failed(
                        "Cleanup preserved modified or unowned content at its original name".into(),
                    ));
                }
            }
            Err(rustix::io::Errno::NOENT) => {}
            Err(_) => {
                let _ =
                    rustix::fs::unlinkat(&anchor, claim.as_str(), rustix::fs::AtFlags::REMOVEDIR);
                return Ok(Settlement::Failed(
                    "Cleanup preserved an unexpected file type at its original name".into(),
                ));
            }
        }
        match rustix::fs::renameat(&anchor, owned.file.as_str(), &claim_fd, "file") {
            Ok(()) => {}
            Err(rustix::io::Errno::NOENT) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    let held = openat(
        &claim_fd,
        "file",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    );
    match held {
        Ok(fd) => {
            let mut file = std::fs::File::from(fd);
            let meta = file.metadata().map_err(|e| e.to_string())?;
            let mut bytes = Vec::new();
            Read::by_ref(&mut file)
                .take(OWNED_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            let valid = meta.is_file()
                && meta.dev() == owned.device
                && meta.ino() == owned.inode
                && digest(&bytes) == owned.digest
                && serde_json::from_slice::<OwnedDocument>(&bytes).is_ok_and(|r| {
                    r.schema == "af/scenario-owned-file@1" && r.owner == owned.owner
                });
            if !valid {
                // Restore without replacing any new user entry. If the original name is now
                // occupied, retain the claim and name it in the diagnostic; never discard it.
                if rustix::fs::linkat(
                    &claim_fd,
                    "file",
                    &anchor,
                    owned.file.as_str(),
                    rustix::fs::AtFlags::empty(),
                )
                .is_ok()
                {
                    rustix::fs::unlinkat(&claim_fd, "file", rustix::fs::AtFlags::empty())
                        .map_err(|e| e.to_string())?;
                    let _ = rustix::fs::unlinkat(
                        &anchor,
                        claim.as_str(),
                        rustix::fs::AtFlags::REMOVEDIR,
                    );
                    return Ok(Settlement::Failed(
                        "Cleanup preserved modified or unowned content at its original name".into(),
                    ));
                }
                return Ok(Settlement::Failed(format!(
                    "Cleanup preserved unexpected content in {}/{}",
                    root.display(),
                    claim
                )));
            }
            rustix::fs::unlinkat(&claim_fd, "file", rustix::fs::AtFlags::empty())
                .map_err(|e| e.to_string())?;
            rustix::fs::fsync(&claim_fd).map_err(|e| e.to_string())?;
        }
        Err(rustix::io::Errno::NOENT) => {}
        Err(e) => {
            return Ok(Settlement::Failed(format!(
                "Cleanup preserved unexpected entry in {}/{}: {e}",
                root.display(),
                claim
            )));
        }
    }
    if let Err(e) = rustix::fs::unlinkat(&anchor, claim.as_str(), rustix::fs::AtFlags::REMOVEDIR) {
        return Ok(Settlement::Failed(format!(
            "Owned file removed; unexpected content preserved in {}/{}: {e}; no rollback claimed",
            root.display(),
            claim
        )));
    }
    rustix::fs::fsync(&anchor).map_err(|e| e.to_string())?;
    Ok(Settlement::Done(BTreeMap::from([(
        "result".into(),
        Datum::Action(ActionReceipt {
            intent_id: d.intent.id.clone(),
            operation: grant.operation,
            destination: grant.destination.clone(),
            subject: owned.file.clone(),
            revision: owned.digest.clone(),
            evidence:
                "Owned file absent after identity- and content-checked cleanup; no rollback claimed"
                    .into(),
        }),
    )])))
}
