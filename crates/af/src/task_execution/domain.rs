//! Select an installed domain's policy and environment without manufacturing code authority.
use super::*;
use review_pipeline::task::host::{DataTaskEnvironment, TaskEnvironment};
use review_pipeline::task::remote_check::RemoteCheckHost;
use std::io::Write;

pub(super) fn code_domain(
    cas: &Cas,
    policy_id: &str,
    graph: CompiledTask,
    state: &Path,
) -> Result<CodeTaskDomain, String> {
    let mapping = std::env::var_os("AF_TASK_RUST_TOOLCHAIN_POLICY_FILE").map(PathBuf::from);
    Ok(CodeTaskDomain::captured(cas, policy_id, graph)?
        .with_rust_toolchain_mapping(mapping)
        .with_remote_checks(remote_checks(state)?))
}

/// The machine-local Remote Check configuration (ADR-0139): the operator's mapping, resolved
/// once here as the Rust toolchain mapping is, and the owner every gate commit names. Candidate
/// commands never receive the variable or the path.
pub(super) fn remote_checks(state: &Path) -> Result<RemoteCheckHost, String> {
    let database = state.join("events.sqlite");
    Ok(RemoteCheckHost {
        mapping: remote_check_mapping()?,
        owner: Some(std::sync::Arc::new(move |task_id: &str| {
            task_owner(&database, task_id)
        })),
        github_pr: Default::default(),
    })
}

/// The authority of a plan whose graph has remote checks (ADR-0139): the pipeline chose them,
/// so the plan says what they do. It gains the effect `publish-gate` and the data destination
/// `github:<owner/name>` of this machine's push target for the source Snapshot's repository,
/// which `af task plan` prints on its EFFECTS and SEND lines; confirming the plan confirms
/// them. Without a target the pipeline cannot be planned on this machine. A graph without
/// remote checks never reads the mapping and keeps the authority it had.
pub(super) fn remote_check_authority(
    cas: &Cas,
    revision: &mut TaskRevisionV1,
    graph: &CompiledTask,
) -> Result<(), String> {
    use review_core::task::remote_check::{
        PUBLISH_GATE_EFFECT, github_destination, github_of_destination,
    };
    use review_pipeline::task::remote_check::{MAPPING_KNOB, RemoteCheckMapping};
    // A refreshed revision starts from its predecessor's authority: what it publishes is
    // decided again here, from this graph and this machine's target.
    revision
        .authority
        .allowed_effects
        .remove(PUBLISH_GATE_EFFECT);
    revision
        .authority
        .data_destinations
        .retain(|destination| github_of_destination(destination).is_none());
    let remote = graph.remote_checks();
    if remote.is_empty() {
        return Ok(());
    }
    let named = remote
        .iter()
        .map(|(node, checks)| {
            format!(
                "{node} lists {}",
                checks.iter().cloned().collect::<Vec<_>>().join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let source = revision.inputs.get("source").ok_or_else(|| {
        format!("Remote checks ({named}) need the Task's source Snapshot to name its repository")
    })?;
    let (_, snapshot, _) = review_pipeline::task::source::source_snapshot(cas, source)?;
    let origin = review_source_git::task::read_origin(cas, &snapshot.origin_id)?;
    let repository = origin.repository_id();
    let mapping = match remote_check_mapping()? {
        Some(path) => RemoteCheckMapping::read(&path)?,
        None => None,
    };
    let target = mapping
        .as_ref()
        .and_then(|mapping| mapping.target(repository))
        .ok_or_else(|| {
            format!(
                "This pipeline runs remote checks ({named}), but {MAPPING_KNOB} names no push \
                 target for repository {repository} on this machine; add a [[github_pr]] entry \
                 for that repository, or plan a pipeline whose check node lists these checks \
                 in `checks`"
            )
        })?;
    revision
        .authority
        .allowed_effects
        .insert(PUBLISH_GATE_EFFECT.into());
    revision
        .authority
        .data_destinations
        .insert(github_destination(&target.github));
    Ok(())
}

/// `AF_TASK_REMOTE_CHECK_POLICY_FILE` when set (it must be absolute), otherwise
/// `$XDG_CONFIG_HOME/af/remote-checks.toml`. No locatable configuration home is no mapping.
fn remote_check_mapping() -> Result<Option<PathBuf>, String> {
    if let Some(path) = std::env::var_os("AF_TASK_REMOTE_CHECK_POLICY_FILE") {
        let path = PathBuf::from(path);
        if path.as_os_str().is_empty() || !path.is_absolute() {
            return Err("AF_TASK_REMOTE_CHECK_POLICY_FILE must be an absolute path".into());
        }
        return Ok(Some(path));
    }
    Ok(crate::config::config_home()
        .ok()
        .map(|home| home.join("af").join("remote-checks.toml")))
}

/// The Task's durable identity in this Store: a digest of the transition that opened its log.
/// It survives every resume, since a log only grows, and differs between Stores, since the
/// opening names a writer that carries 64 bits of operating-system randomness beside the PID
/// and the Store's own clock (ADR-0139).
fn task_owner(database: &Path, task_id: &str) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let store = EventStore::open_read_only(database).map_err(|e| e.to_string())?;
    let run_id = review_store::store::task::task_run_id(task_id).map_err(|e| e.to_string())?;
    let first = store
        .replay(&run_id)
        .map_err(|e| e.to_string())?
        .into_iter()
        .next()
        .ok_or("Task has no log to own gate commits")?;
    let transition =
        review_store::store::task::read_task_transition(&first).map_err(|e| e.to_string())?;
    if !matches!(
        transition.change,
        review_core::task::event::TaskChangeV1::Opened { .. }
    ) {
        return Err("Task log does not begin with its opening".into());
    }
    let mut digest = Sha256::new();
    digest.update(b"af/remote-check-owner/v1\0");
    digest.update(task_id.as_bytes());
    digest.update(b"\0");
    digest.update(serde_json::to_vec(&first.payload).map_err(|e| e.to_string())?);
    Ok(format!(
        "sha256:{}",
        review_core::hex::encode(&digest.finalize())
    ))
}

pub(super) fn capture_policy<T: Serialize + serde::de::DeserializeOwned>(
    cas: &Cas,
    manifest: &Manifest,
    path: Option<&str>,
    validate: impl FnOnce(&T) -> Result<(), String>,
) -> Result<Option<String>, String> {
    let Some(path) = path else {
        return Ok(None);
    };
    if !review_config::task::shared::safe_relative_path(path) {
        return Err("Domain policy path must be project-relative".into());
    }
    let policy: T = parse(Path::new(path), &captured_file(cas, manifest, path)?)?;
    validate(&policy)?;
    cas.put_json(&serde_json::to_value(policy).map_err(|e| e.to_string())?)
        .map(Some)
        .map_err(|e| e.to_string())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn write_output(
    id: &str,
    port: &str,
    format: &str,
    output: &Path,
    repo: &Path,
    state: Option<&Path>,
    json: bool,
) -> Result<i32, String> {
    let (_, state) = state_path(repo, state)?;
    let cas = Cas::open_existing(state.join("cas")).map_err(|e| e.to_string())?;
    let store =
        EventStore::open_read_only(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    let task = store
        .task_projection(&cas, id)
        .map_err(|e| e.to_string())?
        .ok_or("Unknown Task")?;
    let TaskPhaseV1::Finished { result_id } = &task.phase else {
        return Err("Task has no finished result to export".into());
    };
    let result: TaskResultV1 = artifact(&cas, result_id, TASK_RESULT_V1)?;
    let value = result
        .outputs
        .get(port)
        .ok_or("Task result has no such output port")?;
    if value.cardinality != PortCardinality::One || value.artifact_ids.len() != 1 {
        return Err("Output command requires a single artifact port".into());
    }
    let artifact: ArtifactEnvelope = serde_json::from_value(
        cas.get_json(&value.artifact_ids[0])
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    validate_envelope(&artifact)?;
    if artifact.artifact_id != value.artifact_ids[0]
        || artifact.artifact_type != value.artifact_type
        || artifact.subject_snapshot_id != value.snapshot_id
    {
        return Err("Task output differs from its recorded artifact contract".into());
    }
    let bytes = match format {
        "json" => serde_json::to_vec_pretty(&artifact).map_err(|e| e.to_string())?,
        "markdown" => {
            if artifact.artifact_type == review_core::task::optimization::OPTIMIZATION_REPORT_V1 {
                let report: review_core::task::optimization::OptimizationReportV1 =
                    serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
                report.render_markdown()?.into_bytes()
            } else if artifact.artifact_type == review_core::task::document::DOCUMENT_V1 {
                let document: review_core::task::document::DocumentV1 =
                    serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
                document.validate()?;
                document.text.into_bytes()
            } else if artifact.artifact_type
                == review_core::task::measurement::MEASUREMENT_COMPARISON_V1
            {
                let comparison: review_core::task::measurement::MeasurementComparisonV1 =
                    serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
                comparison.render_markdown()?.into_bytes()
            } else {
                return Err(
                    "Markdown output requires a typed Document, OptimizationReport or MeasurementComparison"
                        .into(),
                );
            }
        }
        _ => return Err("Unsupported output format".into()),
    };
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    temporary
        .write_all(&bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    temporary.persist_noclobber(output).map_err(|e| {
        format!(
            "Output file must be absent; nothing was overwritten: {}",
            e.error
        )
    })?;
    std::fs::File::open(parent)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())?;
    if json {
        println!(
            "{}",
            json!({"schema":"af.task-output/1","task_id":id,"artifact_id":value.artifact_ids[0],"port":port,"path":output,"format":format,"acceptance":result.acceptance})
        );
    } else {
        println!(
            "Wrote {port} to {} (Task acceptance: {:?})",
            output.display(),
            result.acceptance
        );
    }
    Ok(0)
}

pub(super) fn environment(
    cas: &Cas,
    authority: &RunAuthority,
    profile: TaskKindProfile,
) -> Result<Box<dyn TaskEnvironment>, String> {
    authority.invocation_policy_id()?;
    if let Some(id) = &authority.document_policy_id {
        let policy: DocumentTaskPolicy =
            serde_json::from_value(cas.get_json(id).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        policy.validate()?;
        Ok(Box::new(DataTaskEnvironment {
            policy: policy.isolation(),
        }))
    } else if let Some(id) = &authority.report_policy_id {
        // A report Worker reads the exact source Snapshot; an author with `execute-checks`
        // gets the clone ADR-0118 gives a reviewer. No warm layer or cache reaches it.
        let policy: ReportTaskPolicy =
            serde_json::from_value(cas.get_json(id).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        policy.validate()?;
        Ok(Box::new(SnapshotTaskEnvironment {
            policy: policy.isolation(),
        }))
    } else {
        let policy: CodeTaskPolicy = serde_json::from_value(
            cas.get_json(authority.code_policy_id()?)
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        policy.validate()?;
        let source = SnapshotTaskEnvironment {
            policy: policy.isolation(),
        };
        // The same installed source environment consumes an adopted Cargo cache selection for
        // ordinary supported command checks and for protected optimizer arms. Profile selection
        // cannot create a trial-only cache implementation.
        let _ = profile;
        Ok(Box::new(
            review_pipeline::task::optimization_configuration::OptimizationEnvironment::new(
                source,
                authority.code_policy_id()?.to_owned(),
            )
            .with_cache_source_resolver(crate::caches::resolve_kind),
        ))
    }
}
