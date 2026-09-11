//! Select an installed domain's policy and environment without manufacturing code authority.
use super::*;
use review_pipeline::task::host::{DataTaskEnvironment, TaskEnvironment};
use std::io::Write;

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
            if artifact.artifact_type != review_core::task::document::DOCUMENT_V1 {
                return Err("Markdown output requires a typed Document".into());
            }
            let document: review_core::task::document::DocumentV1 =
                serde_json::from_value(artifact.payload).map_err(|e| e.to_string())?;
            document.validate()?;
            document.text.into_bytes()
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
    } else {
        let policy: CodeTaskPolicy = serde_json::from_value(
            cas.get_json(authority.code_policy_id()?)
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        policy.validate()?;
        Ok(Box::new(SnapshotTaskEnvironment {
            policy: policy.isolation(),
        }))
    }
}
