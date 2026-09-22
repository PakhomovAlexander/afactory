//! Warm Workspaces: a stable template root per node, materialized once, re-based per head with
//! digest verification, reused untouched on an unchanged head, and failing closed into a full
//! materialization with a recorded reason whenever the root cannot be trusted.

use std::collections::BTreeMap;
use std::path::Path;

use review_core::{WorkspaceBasisV1, WorkspaceFallbackReasonV1, is_workspace_id};
use review_sandbox::{
    Mode, RecordedPreparation, Sandbox, WorkspacePreparation, WorkspaceRoot, prepare_workspace,
    workspace_id,
};
use review_source_git::{Entry, EntryKind, Manifest, PathEncoding, materialize, scan_tree};
use review_store::Cas;

fn snapshot(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn manifest(cas: &Cas, files: &[(&str, &str)]) -> Manifest {
    Manifest::new(
        files
            .iter()
            .map(|(path, text)| Entry {
                path: (*path).into(),
                kind: EntryKind::File,
                content: cas.put(text.as_bytes()).unwrap(),
                size: text.len() as u64,
            })
            .collect(),
    )
    .unwrap()
}

/// Every regular file below `root`, keyed by relative path.
fn tree_bytes(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, at: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let key = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.insert(key, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn root_under(dir: &Path) -> WorkspaceRoot {
    let id = workspace_id("run", &snapshot('c'), "correctness");
    WorkspaceRoot::new(&dir.join("workspaces"), &id).unwrap()
}

fn head_one(cas: &Cas) -> Manifest {
    manifest(
        cas,
        &[
            ("a.rs", "one\n"),
            ("b.rs", "two\n"),
            ("src/deep/c.rs", "three\n"),
        ],
    )
}

fn head_two(cas: &Cas) -> Manifest {
    manifest(
        cas,
        &[
            ("a.rs", "uno\n"),
            ("d.rs", "four\n"),
            ("src/deep/c.rs", "three\n"),
        ],
    )
}

/// What the Campaign log would hold after `prepared` was recorded under `snapshot`.
fn recorded(prepared: &WorkspacePreparation, snapshot: &str) -> RecordedPreparation {
    RecordedPreparation {
        snapshot_id: snapshot.to_string(),
        verified_digest: prepared.verified_digest.clone(),
    }
}

fn expect_full(
    prepared: &WorkspacePreparation,
    reason: WorkspaceFallbackReasonV1,
    from: Option<&str>,
    what: &str,
) {
    assert_eq!(prepared.basis, WorkspaceBasisV1::Full, "{what}");
    assert_eq!(prepared.fallback, Some(reason), "{what}");
    assert_eq!(prepared.from_snapshot_id.as_deref(), from, "{what}");
}

#[test]
fn a_stable_root_is_materialized_once_rebased_per_head_and_reused_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let root = root_under(dir.path());
    let head_one = head_one(&cas);
    let head_two = head_two(&cas);

    let first = prepare_workspace(&root, &head_one, &snapshot('1'), &cas, None).unwrap();
    expect_full(
        &first,
        WorkspaceFallbackReasonV1::NoVerifiedTemplate,
        None,
        "the first Round has no template to re-base",
    );
    assert_eq!(first.verified_digest, head_one.content_digest());
    assert_eq!(first.entries_touched, 3);
    // The template is the stable tree itself, not an equal copy elsewhere: a file dropped into
    // `root.tree()` shows up in the next clone.
    let sentinel = root.tree().join("sentinel.txt");
    std::fs::write(&sentinel, b"stable root\n").unwrap();
    let probe = Sandbox::from_template(&first.template, Mode::EphemeralWrite).unwrap();
    assert_eq!(
        std::fs::read(probe.root().join("sentinel.txt")).unwrap(),
        b"stable root\n",
        "the template is rooted at the stable workspace tree"
    );
    drop(probe);
    std::fs::remove_file(&sentinel).unwrap();
    let clone = Sandbox::from_template(&first.template, Mode::EphemeralWrite).unwrap();
    assert_eq!(
        clone.baseline(),
        &head_one,
        "clones start from the verified head"
    );
    assert_eq!(
        scan_tree(clone.root(), PathEncoding::LegacyV1).unwrap(),
        head_one
    );
    drop(clone);
    assert_eq!(
        scan_tree(root.tree(), PathEncoding::LegacyV1).unwrap(),
        head_one
    );

    let record = recorded(&first, &snapshot('1'));
    let second = prepare_workspace(&root, &head_two, &snapshot('2'), &cas, Some(&record)).unwrap();
    assert_eq!(second.basis, WorkspaceBasisV1::Rebased);
    assert_eq!(second.fallback, None);
    assert_eq!(
        second.from_snapshot_id.as_deref(),
        Some(snapshot('1').as_str()),
        "lineage comes from the record"
    );
    assert_eq!(second.verified_digest, head_two.content_digest());
    assert_eq!(
        second.entries_touched, 3,
        "a.rs modified, b.rs removed, d.rs added"
    );
    let fresh = dir.path().join("fresh");
    materialize(&head_two, &cas, &fresh).unwrap();
    assert_eq!(
        tree_bytes(&root.tree()),
        tree_bytes(&fresh),
        "the re-based template is byte-identical to a full materialization"
    );
    assert_eq!(
        scan_tree(root.tree(), PathEncoding::LegacyV1).unwrap(),
        head_two
    );
    assert!(!root.tree().join("b.rs").exists());

    // The same tree under a new Snapshot ID: nothing is materialized and nothing is touched,
    // though the tree is read back and verified before it is served.
    let untouched = std::fs::metadata(root.tree().join("a.rs"))
        .unwrap()
        .modified()
        .unwrap();
    let record = recorded(&second, &snapshot('2'));
    let third = prepare_workspace(&root, &head_two, &snapshot('3'), &cas, Some(&record)).unwrap();
    assert_eq!(third.basis, WorkspaceBasisV1::Reused);
    assert_eq!(third.fallback, None);
    assert_eq!(
        third.from_snapshot_id.as_deref(),
        Some(snapshot('2').as_str())
    );
    assert_eq!(third.entries_touched, 0);
    assert_eq!(
        std::fs::metadata(root.tree().join("a.rs"))
            .unwrap()
            .modified()
            .unwrap(),
        untouched
    );

    // Per-Attempt sandboxes remain fresh, isolated clones; the template never sees a write.
    let a = Sandbox::from_template(&third.template, Mode::EphemeralWrite).unwrap();
    let b = Sandbox::from_template(&third.template, Mode::EphemeralWrite).unwrap();
    std::fs::write(a.root().join("a.rs"), b"only in a\n").unwrap();
    std::fs::write(a.root().join("scratch.txt"), b"scratch").unwrap();
    assert_eq!(std::fs::read(b.root().join("a.rs")).unwrap(), b"uno\n");
    assert_eq!(std::fs::read(root.tree().join("a.rs")).unwrap(), b"uno\n");
    assert!(!root.tree().join("scratch.txt").exists());
    let sealed = a.seal().unwrap();
    assert_eq!(sealed.mutations.added, vec!["scratch.txt"]);
    assert_eq!(sealed.mutations.modified, vec!["a.rs"]);
    drop(sealed);
    drop(b);
    assert!(
        root.tree().join("a.rs").is_file(),
        "dropping clones never removes the stable root"
    );
}

#[test]
fn an_untrusted_root_fails_closed_into_a_full_materialization_that_records_why() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let root = root_under(dir.path());
    let head_one = head_one(&cas);
    let head_two = head_two(&cas);
    let first = prepare_workspace(&root, &head_one, &snapshot('1'), &cas, None).unwrap();
    let record = recorded(&first, &snapshot('1'));

    // A drifted template: an entry the next head does not touch changed under the marker. The
    // clone is scanned before any entry is unlinked, disagrees with the previous manifest, and
    // the tree is rebuilt from the CAS without the rebase ever running.
    std::fs::write(root.tree().join("src/deep/c.rs"), b"tampered\n").unwrap();
    let corrupted =
        prepare_workspace(&root, &head_two, &snapshot('2'), &cas, Some(&record)).unwrap();
    expect_full(
        &corrupted,
        WorkspaceFallbackReasonV1::TemplateCorrupt,
        Some(&snapshot('1')),
        "a drifted template",
    );
    assert_eq!(corrupted.entries_touched, 3);
    assert_eq!(corrupted.verified_digest, head_two.content_digest());
    let fresh = dir.path().join("fresh");
    materialize(&head_two, &cas, &fresh).unwrap();
    assert_eq!(tree_bytes(&root.tree()), tree_bytes(&fresh));
    assert_eq!(
        std::fs::read(root.tree().join("src/deep/c.rs")).unwrap(),
        b"three\n"
    );

    // A preparation that ended before its marker was written left nothing to trust; the
    // previous head is still whatever the log recorded.
    let record = recorded(&corrupted, &snapshot('2'));
    std::fs::remove_file(root.path().join("head.json")).unwrap();
    let unmarked =
        prepare_workspace(&root, &head_two, &snapshot('3'), &cas, Some(&record)).unwrap();
    expect_full(
        &unmarked,
        WorkspaceFallbackReasonV1::NoVerifiedTemplate,
        Some(&snapshot('2')),
        "a missing marker",
    );

    // A marker that contradicts its manifest is corrupt, whatever the tree holds.
    let record = recorded(&unmarked, &snapshot('3'));
    std::fs::write(root.path().join("manifest.json"), b"{}").unwrap();
    let contradicted =
        prepare_workspace(&root, &head_two, &snapshot('4'), &cas, Some(&record)).unwrap();
    expect_full(
        &contradicted,
        WorkspaceFallbackReasonV1::TemplateCorrupt,
        Some(&snapshot('3')),
        "a contradicted manifest",
    );

    // The rebuilt root is trusted again.
    let record = recorded(&contradicted, &snapshot('4'));
    let reused = prepare_workspace(&root, &head_two, &snapshot('5'), &cas, Some(&record)).unwrap();
    assert_eq!(reused.basis, WorkspaceBasisV1::Reused);
    assert_eq!(
        reused.from_snapshot_id.as_deref(),
        Some(snapshot('4').as_str())
    );

    // A file the previous manifest names is now a directory: the clone no longer holds the
    // previous manifest, so the head is rebuilt and no leftover tree remains.
    let record = recorded(&reused, &snapshot('5'));
    std::fs::remove_file(root.tree().join("a.rs")).unwrap();
    std::fs::create_dir(root.tree().join("a.rs")).unwrap();
    let head_three = manifest(&cas, &[("a.rs", "ein\n"), ("src/deep/c.rs", "three\n")]);
    let replaced =
        prepare_workspace(&root, &head_three, &snapshot('6'), &cas, Some(&record)).unwrap();
    expect_full(
        &replaced,
        WorkspaceFallbackReasonV1::TemplateCorrupt,
        Some(&snapshot('5')),
        "a file replaced by a directory",
    );
    assert_eq!(
        scan_tree(root.tree(), PathEncoding::LegacyV1).unwrap(),
        head_three
    );
    assert!(!root.path().join("tree.next").exists());
    assert!(!root.path().join("tree.old").exists());
}

#[test]
fn an_unchanged_head_is_verified_before_it_is_reused() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let root = root_under(dir.path());
    let head = head_one(&cas);
    let fresh = dir.path().join("fresh");
    materialize(&head, &cas, &fresh).unwrap();
    let mut prepared = prepare_workspace(&root, &head, &snapshot('1'), &cas, None).unwrap();
    let mut round = 2_u32;
    type Corruption = (&'static str, Box<dyn Fn(&Path)>);
    let corruptions: [Corruption; 4] = [
        (
            "changed bytes",
            Box::new(|tree| std::fs::write(tree.join("a.rs"), b"drift\n").unwrap()),
        ),
        (
            "a missing file",
            Box::new(|tree| std::fs::remove_file(tree.join("b.rs")).unwrap()),
        ),
        (
            "a stray file",
            Box::new(|tree| std::fs::write(tree.join("stray.rs"), b"stray\n").unwrap()),
        ),
        (
            "a type change",
            Box::new(|tree| {
                std::fs::remove_file(tree.join("b.rs")).unwrap();
                std::os::unix::fs::symlink("a.rs", tree.join("b.rs")).unwrap();
            }),
        ),
    ];
    for (what, corrupt) in corruptions {
        let previous = char::from_digit(round - 1, 10).unwrap();
        let record = recorded(&prepared, &snapshot(previous));
        corrupt(&root.tree());
        let current = char::from_digit(round, 10).unwrap();
        let rebuilt =
            prepare_workspace(&root, &head, &snapshot(current), &cas, Some(&record)).unwrap();
        expect_full(
            &rebuilt,
            WorkspaceFallbackReasonV1::TemplateCorrupt,
            Some(&snapshot(previous)),
            what,
        );
        assert_eq!(rebuilt.entries_touched, 3, "{what}");
        assert_eq!(tree_bytes(&root.tree()), tree_bytes(&fresh), "{what}");
        assert_eq!(
            scan_tree(root.tree(), PathEncoding::LegacyV1).unwrap(),
            head,
            "{what}"
        );
        // Verified again, the same head is reused without a write.
        let record = recorded(&rebuilt, &snapshot(current));
        let next = char::from_digit(round + 1, 10).unwrap();
        let reused = prepare_workspace(&root, &head, &snapshot(next), &cas, Some(&record)).unwrap();
        assert_eq!(reused.basis, WorkspaceBasisV1::Reused, "{what}");
        assert_eq!(reused.entries_touched, 0, "{what}");
        prepared = reused;
        round += 2;
    }
}

#[test]
fn a_marker_the_log_never_recorded_is_rebuilt_and_supplies_no_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let root = root_under(dir.path());
    let head_one = head_one(&cas);
    let head_two = head_two(&cas);
    let first = prepare_workspace(&root, &head_one, &snapshot('1'), &cas, None).unwrap();

    // The preparation swapped the tree and wrote its marker, but its record never became
    // durable: the next preparation finds a marker the log does not know and rebuilds.
    let lost = prepare_workspace(&root, &head_one, &snapshot('2'), &cas, None).unwrap();
    expect_full(
        &lost,
        WorkspaceFallbackReasonV1::UnrecordedPreparation,
        None,
        "a marker without a record",
    );
    assert_eq!(lost.entries_touched, 3);

    // The marker's Snapshot ID is informational: forged, it never surfaces, because lineage
    // comes from the record and trust from the verified digest.
    let record = recorded(&lost, &snapshot('2'));
    let marker = root.path().join("head.json");
    let forged = std::fs::read_to_string(&marker)
        .unwrap()
        .replace(&snapshot('2'), &snapshot('9'));
    std::fs::write(&marker, forged).unwrap();
    let rebased = prepare_workspace(&root, &head_two, &snapshot('3'), &cas, Some(&record)).unwrap();
    assert_eq!(rebased.basis, WorkspaceBasisV1::Rebased);
    assert_eq!(
        rebased.from_snapshot_id.as_deref(),
        Some(snapshot('2').as_str()),
        "the record's head, never the marker's"
    );
    assert_eq!(
        scan_tree(root.tree(), PathEncoding::LegacyV1).unwrap(),
        head_two
    );

    // A marker claiming a digest the log never verified is a marker the log never recorded.
    let record = recorded(&rebased, &snapshot('3'));
    let forged = std::fs::read_to_string(&marker)
        .unwrap()
        .replace(&head_two.content_digest(), &first.verified_digest);
    std::fs::write(&marker, forged).unwrap();
    let rebuilt = prepare_workspace(&root, &head_two, &snapshot('4'), &cas, Some(&record)).unwrap();
    assert_eq!(rebuilt.basis, WorkspaceBasisV1::Full);
    assert!(
        matches!(
            rebuilt.fallback,
            Some(
                WorkspaceFallbackReasonV1::UnrecordedPreparation
                    | WorkspaceFallbackReasonV1::TemplateCorrupt
            )
        ),
        "{:?}",
        rebuilt.fallback
    );
    assert_eq!(
        rebuilt.from_snapshot_id.as_deref(),
        Some(snapshot('3').as_str())
    );

    // Once the record and the marker agree, the root is trusted again.
    let record = recorded(&rebuilt, &snapshot('4'));
    let reused = prepare_workspace(&root, &head_two, &snapshot('5'), &cas, Some(&record)).unwrap();
    assert_eq!(reused.basis, WorkspaceBasisV1::Reused);
    assert_eq!(
        reused.from_snapshot_id.as_deref(),
        Some(snapshot('4').as_str())
    );
}

#[cfg(unix)]
#[test]
fn a_drifted_symlink_can_reach_nothing_outside_the_root() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let root = root_under(dir.path());
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("victim"), b"external\n").unwrap();
    std::fs::write(outside.join("kept"), b"kept\n").unwrap();

    let head_one = manifest(&cas, &[("dir/victim", "inside\n"), ("keep.rs", "keep\n")]);
    let head_two = manifest(&cas, &[("keep.rs", "keep\n")]);
    let first = prepare_workspace(&root, &head_one, &snapshot('1'), &cas, None).unwrap();
    let record = recorded(&first, &snapshot('1'));

    // The template drifted: its directory is now a symlink to a directory outside the root
    // that holds a file of the same name, and a leftover clone points outside as well.
    std::fs::remove_dir_all(root.tree().join("dir")).unwrap();
    std::os::unix::fs::symlink(&outside, root.tree().join("dir")).unwrap();
    std::os::unix::fs::symlink(&outside, root.path().join("tree.next")).unwrap();
    let rebuilt = prepare_workspace(&root, &head_two, &snapshot('2'), &cas, Some(&record)).unwrap();
    expect_full(
        &rebuilt,
        WorkspaceFallbackReasonV1::TemplateCorrupt,
        Some(&snapshot('1')),
        "a symlinked directory",
    );
    assert_eq!(
        std::fs::read(outside.join("victim")).unwrap(),
        b"external\n",
        "the head's removal of dir/victim never reached through the symlink"
    );
    assert_eq!(std::fs::read(outside.join("kept")).unwrap(), b"kept\n");
    assert!(
        !root.path().join("tree.next").exists(),
        "the leftover symlink was unlinked, not traversed"
    );
    assert_eq!(
        scan_tree(root.tree(), PathEncoding::LegacyV1).unwrap(),
        head_two
    );
}

#[cfg(unix)]
#[test]
fn a_preparation_failure_names_no_host_path() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let cache = dir.path().join("distinctive-cache-root-7f3a");
    let id = workspace_id("run", &snapshot('c'), "correctness");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::create_dir_all(dir.path().join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("elsewhere"), cache.join(&id)).unwrap();
    let root = WorkspaceRoot::new(&cache, &id).unwrap();
    let error = match prepare_workspace(&root, &head_one(&cas), &snapshot('1'), &cas, None) {
        Err(error) => error,
        Ok(_) => panic!("a symlinked root must be refused"),
    };
    let durable = error.to_string();
    assert_eq!(durable, "the warm workspace root is unavailable");
    assert!(
        !durable.contains("distinctive-cache-root") && !durable.contains(&id),
        "the durable message carries no path: {durable}"
    );
    assert!(error.operator_detail().contains("distinctive-cache-root"));
}

#[test]
fn workspace_identities_are_opaque_stable_and_node_private() {
    let dir = tempfile::tempdir().unwrap();
    let one = workspace_id("run", &snapshot('c'), "correctness");
    assert!(is_workspace_id(&one));
    assert_eq!(one, workspace_id("run", &snapshot('c'), "correctness"));
    assert_ne!(one, workspace_id("run", &snapshot('c'), "bugs"));
    assert_ne!(one, workspace_id("other", &snapshot('c'), "correctness"));
    assert_ne!(one, workspace_id("run", &snapshot('d'), "correctness"));
    let root = WorkspaceRoot::new(dir.path(), &one).unwrap();
    assert_eq!(root.path(), dir.path().join(&one));
    assert_eq!(root.tree(), dir.path().join(&one).join("tree"));
    assert!(
        WorkspaceRoot::new(Path::new("relative/cache"), &one).is_err(),
        "the cache root must be absolute"
    );
    assert!(
        WorkspaceRoot::new(dir.path(), "../escape").is_err(),
        "only an identity names a root"
    );
}
