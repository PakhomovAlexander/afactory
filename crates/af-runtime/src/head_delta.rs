//! Native knowledge uses exact Manifest identities and the canonical head delta policy.
use af_contracts::{PathRenameV1, TreeView};
use af_source_git::git::TREE_DIFF_POLICY_VERSION;
use af_source_git::{Manifest, TreeChangeKind, manifest_diff};
use af_store::Cas;
pub(crate) fn head_to_head(
    cas: &Cas,
    from: &Manifest,
    head: &Manifest,
) -> Result<(Vec<PathRenameV1>, String, bool), String> {
    if from.content_digest() == head.content_digest() {
        return Ok((Vec::new(), TREE_DIFF_POLICY_VERSION.to_string(), false));
    }
    let diff = manifest_diff(from, head, cas);
    let diff = diff.map_err(|error| error.to_string())?;
    let mut renames = Vec::new();
    for change in &diff.changes {
        let TreeChangeKind::Renamed { similarity } = &change.kind else {
            continue;
        };
        let (Some(old_path), Some(new_path)) = (&change.old_path, &change.new_path) else {
            continue;
        };
        renames.push(PathRenameV1 {
            old_path: af_contracts::encode_path(old_path),
            new_path: af_contracts::encode_path(new_path),
            similarity: *similarity,
        });
    }
    Ok((renames, diff.diff_policy, diff.rename_detection_truncated))
}

pub(crate) fn tree_view(manifest: &Manifest) -> TreeView {
    manifest
        .entries
        .iter()
        .map(|entry| {
            let identity = format!("{}:{}", entry.kind.mode(), entry.content);
            (entry.path.clone(), identity)
        })
        .collect()
}
