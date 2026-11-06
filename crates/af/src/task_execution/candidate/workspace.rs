use super::*;
/// Scenario lifecycle staging uses the exact native accepted candidate, never a Worker's
/// filesystem claim. This is an isolated object export; only the separately installed adapter
/// can create a source-repository branch/worktree or publish it under signed authority.
pub(crate) fn candidate_source(
    request: &crate::scenario::workspace::SourceRequest,
    repo: &Path,
    state: &Path,
    into: &Path,
) -> Result<crate::scenario::workspace::SourceReceipt, String> {
    use crate::scenario::workspace::SourceReceipt;
    request.validate()?;
    let receipt = &request.receipt;
    let opened = open_task(&receipt.task_id, repo, Some(state))?;
    let (result, _) = opened.view()?;
    if opened.task.revision_id != receipt.revision_id
        || opened.task.plan_id.as_deref() != Some(&receipt.plan_id)
        || !matches!(&opened.task.phase, TaskPhaseV1::Finished { result_id } if result_id == &receipt.result_id)
        || result.as_ref() != Some(&receipt.result)
    {
        return Err("Candidate staging requires the exact accepted candidate Task".into());
    }
    let selected = select(&opened, None)?;
    if !selected.verified || selected.snapshot_id != request.candidate_snapshot_id {
        return Err("Candidate staging requires the accepted candidate Snapshot".into());
    }
    let source_id = selected
        .source_id
        .ok_or("Candidate has no original source")?;
    if let Some(expected) = &request.predecessor_snapshot_id {
        super::scenario_template::experiment_source_matches(&opened.cas, &source_id, expected)?;
    }
    let candidate_port = receipt
        .result
        .outputs
        .get("snapshot")
        .ok_or("Candidate lacks its candidate output")?;
    let source_port = opened
        .task
        .revision
        .inputs
        .get("source")
        .ok_or("Candidate lacks its original source input")?;
    for (port, snapshot) in [
        (candidate_port, selected.snapshot_id.as_str()),
        (source_port, source_id.as_str()),
    ] {
        let [artifact_id] = port.artifact_ids.as_slice() else {
            return Err("Candidate staging requires one exact SourceTree artifact".into());
        };
        if port.artifact_type != SOURCE_TREE_V1
            || port.cardinality != PortCardinality::One
            || input_bindings::source_tree_snapshot(&opened.cas, artifact_id)? != snapshot
        {
            return Err("Candidate source artifact differs from its Snapshot metadata".into());
        }
    }
    let (base, old) = review_source_git::task::read_snapshot(&opened.cas, &source_id)?;
    let (head, new) = review_source_git::task::read_snapshot(&opened.cas, &selected.snapshot_id)?;
    let origin = review_source_git::task::read_origin(&opened.cas, &base.origin_id)?;
    let head_origin = review_source_git::task::read_origin(&opened.cas, &head.origin_id)?;
    if !review_source_git::task::descends_from(&opened.cas, &selected.snapshot_id, &source_id)? {
        return Err("Candidate does not descend from its Task source".into());
    }
    if head_origin.repository_id() != origin.repository_id() {
        return Err("Candidate candidate belongs to a different repository".into());
    }
    if new.entries.len() > 32768
        || new.entries.iter().any(|e| e.path.len() > 4096)
        || new.entries.iter().map(|e| u128::from(e.size)).sum::<u128>() > 64 * 1024 * 1024
    {
        return Err("Candidate staging exceeds supported Snapshot size".into());
    }
    let repo = std::fs::canonicalize(repo).map_err(|e| e.to_string())?;
    let state = std::fs::canonicalize(state).map_err(|e| e.to_string())?;
    let parent = into
        .parent()
        .ok_or("Staging needs an existing private parent")?;
    let parent = std::fs::canonicalize(parent).map_err(|e| e.to_string())?;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let info = std::fs::metadata(&parent).map_err(|e| e.to_string())?;
    if info.uid() != nix::unistd::geteuid().as_raw() || info.mode() & 0o077 != 0 {
        return Err("Candidate staging parent must be owner-only".into());
    }
    let target = parent.join(into.file_name().ok_or("Staging needs a leaf")?);
    crate::scenario::source::isolated_destination(&target)?;
    if target.starts_with(&repo)
        || repo.starts_with(&target)
        || target.starts_with(&state)
        || state.starts_with(&target)
    {
        return Err("Candidate staging overlaps repository or Task Store".into());
    }
    // A killed native exporter must not accumulate unbounded random scratch trees across
    // reconciliations. Serialize this parent's exports on one retained lock inode, then
    // admit at most four uncertain staging directories. Never adopt or remove their bytes.
    use std::os::unix::fs::OpenOptionsExt;
    let capture_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(parent.join("af-candidate-capture.lock"))
        .map_err(|e| e.to_string())?;
    let lock_info = capture_lock.metadata().map_err(|e| e.to_string())?;
    if !lock_info.is_file()
        || lock_info.uid() != nix::unistd::geteuid().as_raw()
        || lock_info.mode() & 0o777 != 0o600
        || lock_info.nlink() != 1
        || lock_info.len() != 0
    {
        return Err("Candidate capture lock is not a private regular file".into());
    }
    fs2::FileExt::try_lock_exclusive(&capture_lock).map_err(|e| e.to_string())?;
    capture_lock.sync_all().map_err(|e| e.to_string())?;
    std::fs::File::open(&parent)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())?;
    let mut uncertain = 0;
    for entry in std::fs::read_dir(&parent).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry
            .file_name()
            .as_bytes()
            .starts_with(b"af-candidate-stage-")
        {
            uncertain += 1;
            if uncertain >= 4 {
                return Err(
                    "Candidate capture staging budget exhausted; retain uncertain directories"
                        .into(),
                );
            }
        }
    }
    let scratch = Scratch::initialize(
        tempfile::Builder::new()
            .prefix("af-candidate-stage-")
            .tempdir_in(&parent)
            .map_err(|e| e.to_string())?,
    )?;
    let native = review_source_git::Repo::open(&repo, scratch.directory.path());
    if native.repository_id().map_err(|e| e.to_string())? != origin.repository_id() {
        return Err("Candidate belongs to a different repository".into());
    }
    let committed = Capture::new(&native, &opened.cas)
        .committed(&request.source_commit)
        .map_err(|e| e.to_string())?;
    if committed.content_digest != old.content_digest() {
        return Err("Candidate source bytes differ from the installed commit".into());
    }
    let entries: Vec<_> = new.entries.iter().collect();
    let blobs = scratch.blobs(&opened.cas, &entries)?;
    let tree = scratch.tree("candidate.index", &entries, &blobs)?;
    let transport = scratch.commit(&tree, None, &selected.snapshot_id)?;
    let expected = SourceReceipt {
        schema: "af/git-candidate-source@1".into(),
        request_digest: request.digest()?,
        repository_id: origin.repository_id().into(),
        source_snapshot: source_id,
        candidate_snapshot: selected.snapshot_id,
        predecessor_snapshot: request.predecessor_snapshot_id.clone(),
        source_content_digest: base.content_digest,
        source_commit: request.source_commit.clone(),
        candidate_tree: tree.clone(),
        transport_commit: transport.clone(),
        repository: target.join("repository.git"),
    };
    // Publish only a complete capture. A process crash cannot expose an incomplete target
    // and a concurrent or foreign leaf is never replaced, even when it is an empty directory.
    let stage = scratch.directory.path().join("capture");
    std::fs::create_dir(&stage).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;
    let mut pin = scratch.git(None);
    pin.args(["update-ref", "refs/heads/af-candidate", &transport]);
    run_git(pin, None, "pinning candidate staging objects")?;
    std::fs::rename(
        scratch.directory.path().join("objects.git"),
        stage.join("repository.git"),
    )
    .map_err(|e| e.to_string())?;
    let mut marker = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(stage.join("source.json"))
        .map_err(|e| e.to_string())?;
    use std::io::Write;
    marker
        .write_all(&serde_json::to_vec(&expected).map_err(|e| e.to_string())?)
        .and_then(|()| marker.sync_all())
        .map_err(|e| e.to_string())?;
    std::fs::File::open(&stage)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())?;
    match rustix::fs::renameat_with(
        rustix::fs::CWD,
        &stage,
        rustix::fs::CWD,
        &target,
        rustix::fs::RenameFlags::NOREPLACE,
    ) {
        Ok(()) => {
            std::fs::File::open(&parent)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
        }
        Err(rustix::io::Errno::EXIST) => {
            let info = std::fs::symlink_metadata(&target).map_err(|e| e.to_string())?;
            if !info.is_dir()
                || info.uid() != nix::unistd::geteuid().as_raw()
                || info.mode() & 0o077 != 0
            {
                return Err("Candidate capture is not an owned private directory".into());
            }
            if SourceReceipt::read(&target.join("source.json"))? != expected {
                return Err("Existing candidate capture has different authority".into());
            }
        }
        Err(e) => return Err(e.to_string()),
    }
    let mut check = git(&expected.repository, scratch.directory.path());
    check
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .args(["rev-parse", &format!("{transport}^{{tree}}")]);
    if String::from_utf8_lossy(&run_git(check, None, "verifying candidate staging tree")?).trim()
        != tree
    {
        return Err("Private staging tree differs from accepted Snapshot".into());
    }
    let mut fsck = git(&expected.repository, scratch.directory.path());
    fsck.env("GIT_NO_REPLACE_OBJECTS", "1").args([
        "fsck",
        "--strict",
        "--no-reflogs",
        "--no-dangling",
    ]);
    run_git(fsck, None, "verifying candidate staging objects")?;
    Ok(expected)
}
