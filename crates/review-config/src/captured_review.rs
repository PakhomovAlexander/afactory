//! Load Review definitions only from captured Campaign authority. The legacy CLI and
//! Task compatibility compiler share these package, policy and Snapshot reachability checks.

use crate::{
    Definition, Loaded,
    lock::{Lockfile, Registry},
};
use review_core::{CampaignBudgetV1, CampaignManifestV1, ReviewerPackageV1, SourceSnapshot};
use review_source_git::Manifest;
use review_store::{Cas, ConvergencePolicy};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewMode {
    Light,
    Heavy,
}
impl ReviewMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Heavy => "heavy",
        }
    }
    pub fn convergence(self, configured: &ConvergencePolicy) -> ConvergencePolicy {
        match self {
            Self::Light => ConvergencePolicy {
                clean_rounds: 1,
                max_rounds: 1,
                gate: configured.gate,
            },
            Self::Heavy => *configured,
        }
    }
}

/// Re-read captured packages and source reachability on every load. The caller supplies
/// the Campaign Manifest selected by the Store; no live project files are consulted.
pub fn load_captured_review(
    cas: &Cas,
    manifest: &CampaignManifestV1,
    mode: ReviewMode,
) -> Result<Loaded, String> {
    manifest.validate()?;
    let pipeline = cas
        .get(&manifest.pipeline.artifact_id)
        .map_err(|error| error.to_string())?;
    let lock = cas
        .get(&manifest.reviewer_lock.artifact_id)
        .map_err(|error| error.to_string())?;
    let pipeline = std::str::from_utf8(&pipeline).map_err(|error| error.to_string())?;
    let lockfile =
        Lockfile::from_toml(std::str::from_utf8(&lock).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let mut packages: BTreeMap<String, BTreeMap<String, Vec<u8>>> = BTreeMap::new();
    let mut captured: BTreeMap<String, (ReviewerPackageV1, BTreeMap<String, Vec<u8>>)> =
        BTreeMap::new();
    for binding in &manifest.reviewers {
        if !captured.contains_key(&binding.package_artifact_id) {
            let package: ReviewerPackageV1 = serde_json::from_value(
                cas.get_json(&binding.package_artifact_id)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            package.validate()?;
            let mut files = BTreeMap::new();
            for (path, artifact_id) in &package.files {
                files.insert(
                    path.clone(),
                    cas.get(artifact_id).map_err(|error| error.to_string())?,
                );
            }
            let recomputed = crate::lock::package_digest_from_files(&files);
            if recomputed != package.digest {
                return Err(format!(
                    "captured reviewer package `{}` claims digest {} but contains {recomputed}",
                    package.name, package.digest
                ));
            }
            captured.insert(binding.package_artifact_id.clone(), (package, files));
        }
        let (package, files) = captured
            .get(&binding.package_artifact_id)
            .expect("captured package inserted");
        if package.name != binding.name
            || package.version != binding.version
            || package.digest != binding.digest
        {
            return Err(format!(
                "captured reviewer package for node `{}` disagrees with CampaignManifest@1",
                binding.node
            ));
        }
        if packages
            .insert(package.name.clone(), files.clone())
            .is_some_and(|prior| &prior != files)
        {
            return Err(format!(
                "CampaignManifest@1 binds package `{}` to inconsistent bytes",
                package.name
            ));
        }
    }
    let registry = Registry::captured(packages);
    let loaded = Definition::from_toml(pipeline)
        .map_err(|error| error.to_string())?
        .load_with(&lockfile, &registry)
        .map_err(|error| error.to_string())?;
    if loaded.subject_kind() != manifest.subject_kind {
        return Err("captured pipeline disagrees with CampaignManifest Subject kind".into());
    }
    validate_manifest_authority(cas, manifest, &loaded, &captured, mode)?;
    Ok(loaded)
}

fn validate_manifest_authority(
    cas: &Cas,
    manifest: &CampaignManifestV1,
    loaded: &Loaded,
    captured: &BTreeMap<String, (ReviewerPackageV1, BTreeMap<String, Vec<u8>>)>,
    mode: ReviewMode,
) -> Result<(), String> {
    let convergence = mode.convergence(loaded.convergence());
    if manifest.convergence.clean_rounds != convergence.clean_rounds
        || manifest.convergence.max_rounds != convergence.max_rounds
        || manifest.convergence.gate != format!("{:?}", convergence.gate).to_lowercase()
    {
        return Err(format!(
            "CampaignManifest convergence differs from requested {} mode; resume with the mode that opened this Campaign",
            mode.as_str()
        ));
    }
    let budgets = loaded.budgets().map(|budget| CampaignBudgetV1 {
        attempt_tokens: budget.attempt,
        run_tokens: budget.run,
    });
    if manifest.budgets != budgets {
        return Err("CampaignManifest budgets differ from captured pipeline authority".into());
    }
    if manifest.check_timeout_seconds != loaded.check_timeout_seconds() {
        return Err(
            "CampaignManifest check timeout differs from captured pipeline authority".into(),
        );
    }
    if manifest.reviewers.len() != loaded.packages().len() {
        return Err("CampaignManifest reviewer bindings are incomplete".into());
    }
    for (node, package) in loaded.packages() {
        let binding = manifest
            .reviewers
            .iter()
            .find(|binding| binding.node == *node)
            .ok_or_else(|| format!("CampaignManifest has no reviewer binding for `{node}`"))?;
        if binding.name != package.name
            || binding.version != package.version
            || binding.digest != package.digest
        {
            return Err(format!(
                "CampaignManifest reviewer binding for `{node}` differs from resolved authority"
            ));
        }
    }
    let expected_execution: BTreeSet<String> =
        std::iter::once(manifest.pipeline.artifact_id.clone())
            .chain(
                manifest
                    .reviewers
                    .iter()
                    .map(|binding| binding.package_artifact_id.clone()),
            )
            .collect();
    if manifest
        .execution_policy_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        != expected_execution
    {
        return Err("CampaignManifest execution policy IDs are not the resolved authority".into());
    }
    for policy in &manifest.project_policy_ids {
        cas.get(policy).map_err(|error| error.to_string())?;
    }
    for (id, kind) in [
        (&manifest.finding_genesis_id, "finding-set-genesis@1"),
        (&manifest.demand_genesis_id, "demand-set-genesis@1"),
    ] {
        let root = cas.get_json(id).map_err(|error| error.to_string())?;
        if root["kind"] != kind || root["authority_snapshot_id"] != manifest.authority_snapshot_id {
            return Err(format!("CampaignManifest has an invalid `{kind}` root"));
        }
    }

    let authority: SourceSnapshot = serde_json::from_value(
        cas.get_json(&manifest.authority_snapshot_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let authority_manifest_id = authority
        .artifact_manifest
        .ok_or("Authority Snapshot has no artifact manifest")?;
    let tree: Manifest = serde_json::from_value(
        cas.get_json(&authority_manifest_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if tree.content_digest() != authority.content_digest
        || tree
            .get(&manifest.pipeline.path)
            .map(|entry| &entry.content)
            != Some(&manifest.pipeline.artifact_id)
        || tree
            .get(&manifest.reviewer_lock.path)
            .map(|entry| &entry.content)
            != Some(&manifest.reviewer_lock.artifact_id)
    {
        return Err("CampaignManifest authority files are not reachable from its Snapshot".into());
    }
    let root = std::path::Path::new(&manifest.pipeline.path)
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::to_str);
    if root != Some(".af") {
        return Err("the pipeline path must live under `.af/pipelines/`".into());
    }
    for (package, _) in captured.values() {
        for (path, artifact_id) in &package.files {
            let authority_path = format!(".af/workers/{}/{path}", package.name);
            if tree.get(&authority_path).map(|entry| &entry.content) != Some(artifact_id) {
                return Err(format!(
                    "captured reviewer file `{authority_path}` is not authority Snapshot content"
                ));
            }
        }
    }
    Ok(())
}
