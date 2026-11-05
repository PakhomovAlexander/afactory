//! Explicit local JSON input capture for the installed structured profile.
use super::*;
use review_core::task::structured::*;
use std::io::Read;

/// Descriptor-relative no-follow walk: neither a leaf nor an ancestor can redirect capture.
fn read(root: &Path, relative: &str) -> Result<Vec<u8>, String> {
    use rustix::fs::{Mode, OFlags, open, openat};
    if !review_config::task::shared::safe_relative_path(relative) {
        return Err("Structured input must be a safe path relative to the Task file".into());
    }
    let root = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
    let mut directory = open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| e.to_string())?;
    let parts: Vec<_> = relative.split('/').collect();
    for part in &parts[..parts.len() - 1] {
        directory = openat(
            &directory,
            *part,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| {
            format!("Structured input ancestor must be a regular directory, without symlinks: {e}")
        })?;
    }
    let file: std::fs::File = openat(
        &directory,
        parts[parts.len() - 1],
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| format!("Cannot open structured input without symlinks: {e}"))?
    .into();
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_STRUCTURED_BYTES as u64 {
        return Err("Structured input must be a regular file of at most 64 KiB".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_STRUCTURED_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_STRUCTURED_BYTES {
        return Err("Structured input exceeds 64 KiB".into());
    }
    Ok(bytes)
}
pub(super) fn capture(
    cas: &Cas,
    options: &StartOptions,
    contract: &StructuredContractV2,
    policy_id: &str,
    revision: &mut TaskRevisionV1,
) -> Result<(), String> {
    let mut names = BTreeSet::new();
    for binding in &options.input_json {
        let (name, path) = binding
            .split_once('=')
            .ok_or("--input-json requires PORT=FILE")?;
        let declaration = contract
            .inputs
            .get(name)
            .ok_or("--input-json names an undeclared custom input port")?;
        if !names.insert(name) || revision.inputs.contains_key(name) {
            return Err("Structured input is bound more than once".into());
        }
        let root = options
            .file
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let bytes = read(root, path)?;
        let payload: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| format!("Structured input is not JSON: {e}"))?;
        review_config::task::structured::validate_payload(&declaration.schema, &payload)?;
        let raw_id = cas.put(&bytes).map_err(|e| e.to_string())?;
        let id = cas
            .put_artifact(
                &declaration.artifact_type,
                producer(),
                vec![raw_id, policy_id.into()],
                None,
                payload,
            )
            .map_err(|e| e.to_string())?
            .0;
        revision.inputs.insert(
            name.into(),
            ArtifactInputV1 {
                artifact_ids: vec![id],
                artifact_type: declaration.artifact_type.clone(),
                cardinality: PortCardinality::One,
                snapshot_id: None,
            },
        );
    }
    Ok(())
}
pub(super) fn policy(
    cas: &Cas,
    authority: &RunAuthority,
) -> Result<Option<StructuredContractV2>, String> {
    if authority.code_policy_id.is_some()
        || authority.document_policy_id.is_some()
        || authority.report_policy_id.is_some()
    {
        return Ok(None);
    }
    let name = authority
        .kind_package
        .as_ref()
        .ok_or("Task without a domain policy requires a structured kind package")?;
    let package = authority
        .packages
        .get(name)
        .ok_or("Structured kind package disappeared")?;
    let bytes = review_config::task::catalog::captured_package_file(
        cas,
        &package.artifact_id,
        "structured.json",
    )?
    .ok_or("Structured kind lost structured.json")?;
    review_config::task::structured::parse_contract(&bytes).map(Some)
}
