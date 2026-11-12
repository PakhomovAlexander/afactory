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
pub enum Method {
    Reconcile,
    Execute,
    Observe,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
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
pub enum Outcome {
    Done,
    Pending,
    Absent,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
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
#[derive(Debug, Clone)]
pub enum Settlement {
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
        .env("PATH", af_process::sanitized_path());
    // An installed adapter owns credential access; workflow text never names an environment
    // variable or secret. The same machine account is the only ambient authority it receives.
    for key in ["HOME", "XDG_CONFIG_HOME"] {
        if let Some(v) = std::env::var_os(key) {
            command.env(key, v);
        }
    }
    let input = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    let capture = af_process::run_supervised_captured_bounded(
        &mut command,
        Some(input),
        Duration::from_millis(timeout_ms.min(adapter.max_wall_ms)),
        af_process::ExitPolicy::KillDescendants,
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
// External effects are reached through the durable runtime, after intent admission.
pub(crate) fn dispatch(
    host: &dyn crate::Host,
    policy: &Policy,
    dispatch: &Dispatch,
    cancellation: &AtomicBool,
) -> Result<Settlement> {
    match &dispatch.definition.operation {
        Kind::Task { target } => host.run_task(target, dispatch.timeout_ms, cancellation),
        Kind::FreshTask { .. } => host.run_task(
            dispatch
                .target
                .as_ref()
                .ok_or("Fresh Task has no committed native plan")?,
            dispatch.timeout_ms,
            cancellation,
        ),
        Kind::Wait { grant, condition } => {
            let request = Request {
                schema: "af/scenario-adapter-request@4".into(),
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
            let started = Instant::now();
            let remaining = || {
                dispatch.timeout_ms.saturating_sub(
                    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                )
            };
            let a = policy
                .adapters
                .get(&grant.adapter)
                .ok_or("Action adapter not in machine policy")?;
            if grant.operation == Operation::PublishPerformancePrV1 {
                let scope = policy
                    .performance_publications
                    .get(&grant.adapter)
                    .ok_or("Missing standing publication machine scope")?;
                require(
                    grant.destination == format!("github:{}", scope.repository),
                    "Publication destination differs from machine scope",
                )?;
                let proof = super::performance::ProofRequest::inputs(&dispatch.inputs, scope)?;
                host.validate_performance_proof(&proof, scope)?;
                let evidence_adapter = policy
                    .adapters
                    .get(&scope.evidence_adapter)
                    .ok_or("Missing pinned native evidence adapter")?;
                let observation_request = Request {
                    schema: "af/scenario-adapter-request@4".into(),
                    method: Method::Observe,
                    intent_id: dispatch.intent.id.clone(),
                    input_digest: dispatch.intent.input_digest.clone(),
                    operation: Operation::Observe,
                    destination: scope.evidence_destination.clone(),
                    inputs: BTreeMap::from([
                        (
                            "subject".into(),
                            Datum::Text(proof.evidence.subject.clone()),
                        ),
                        (
                            "revision".into(),
                            Datum::Text(proof.evidence.revision.clone()),
                        ),
                        ("condition".into(), Datum::Text("native_verified".into())),
                    ]),
                };
                let left = remaining();
                require(
                    left > 0,
                    "Step deadline reached during native publication validation",
                )?;
                let observed = call(evidence_adapter, &observation_request, left, cancellation)?;
                require(
                    observed.outcome == Outcome::Done
                        && observed.observation.as_ref() == Some(&proof.evidence),
                    "Publication evidence differs from retained native report",
                )?;
            }
            let mut request = Request {
                schema: "af/scenario-adapter-request@4".into(),
                method: Method::Reconcile,
                intent_id: dispatch.intent.id.clone(),
                input_digest: dispatch.intent.input_digest.clone(),
                operation: grant.operation,
                destination: grant.destination.clone(),
                inputs: dispatch.inputs.clone(),
            };
            if grant.operation == Operation::PublishPerformancePrV1 {
                let scope = &policy.performance_publications[&grant.adapter];
                request.inputs.insert(
                    "machine_scope".into(),
                    Datum::Text(serde_json::to_string(scope).map_err(|e| e.to_string())?),
                );
            }
            // Reconciliation is mandatory even for a fresh intent. An absent response is a
            // positive, authoritative no-effect result, not a timeout or a missing response.
            let left = remaining();
            require(
                left > 0,
                "Step deadline reached before adapter reconciliation",
            )?;
            let mut r = match call(a, &request, left, cancellation) {
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
            match r.outcome {
                Outcome::Done => {
                    let receipt = ActionReceipt {
                        intent_id: dispatch.intent.id.clone(),
                        operation: grant.operation,
                        destination: grant.destination.clone(),
                        subject: dispatch.inputs["subject"].as_text()?.into(),
                        revision: dispatch.inputs["revision"].as_text()?.into(),
                        evidence: r.evidence,
                    };
                    if grant.operation == Operation::PrepareCandidateV1 {
                        super::workspace::validate_preparation(&receipt, &dispatch.inputs)?;
                    }
                    Ok(Settlement::Done(BTreeMap::from([(
                        "result".into(),
                        Datum::Action(receipt),
                    )])))
                }
                Outcome::Failed => Ok(Settlement::Failed(r.evidence)),
                _ => Ok(Settlement::Waiting(r.evidence)),
            }
        }
        _ => Err("Non-dispatchable Scenario step".into()),
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedDocument {
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
    let anchor = af_sandbox::open_anchor(root).map_err(|e| e.to_string())?;
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
