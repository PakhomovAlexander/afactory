//! Warm Workspaces: a stable template root per node, materialized once, re-based per head with
//! digest verification, reused untouched on an unchanged head, and failing closed into a full
//! materialization with a recorded reason whenever the root cannot be trusted.

use std::collections::BTreeMap;
use std::path::Path;

use review_core::{WorkspaceBasisV1, WorkspaceFallbackReasonV1, is_workspace_id};
use review_sandbox::{Mode, Sandbox, WorkspaceRoot, prepare_workspace, workspace_id};
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

#[test]
fn a_stable_root_is_materialized_once_rebased_per_head_and_reused_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let root = root_under(dir.path());
    let head_one = head_one(&cas);
    let head_two = head_two(&cas);

    let first = prepare_workspace(&root, &head_one, &snapshot('1'), &cas).unwrap();
    assert_eq!(first.basis, WorkspaceBasisV1::Full);
    assert_eq!(
        first.fallback,
        Some(WorkspaceFallbackReasonV1::NoVerifiedTemplate),
        "the first Round has no template to re-base"
    );
    assert_eq!(first.from_snapshot_id, None);
    assert_eq!(first.verified_digest, head_one.content_digest());
    assert_eq!(first.entries_touched, 3);
    assert_eq!(first.template.root(), root.tree());
    assert_eq!(first.template.manifest(), &head_one);
    assert_eq!(
        scan_tree(root.tree(), PathEncoding::LegacyV1).unwrap(),
        head_one
    );

    let second = prepare_workspace(&root, &head_two, &snapshot('2'), &cas).unwrap();
    assert_eq!(second.basis, WorkspaceBasisV1::Rebased);
    assert_eq!(second.fallback, None);
    assert_eq!(
        second.from_snapshot_id.as_deref(),
        Some(snapshot('1').as_str())
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

    // The same tree under a new Snapshot ID: nothing is materialized and nothing is touched.
    let untouched = std::fs::metadata(root.tree().join("a.rs"))
        .unwrap()
        .modified()
        .unwrap();
    let third = prepare_workspace(&root, &head_two, &snapshot('3'), &cas).unwrap();
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
    prepare_workspace(&root, &head_one, &snapshot('1'), &cas).unwrap();

    // A corrupted template: an entry the next head does not touch changed under the marker.
    // The rebase applies cleanly, the verification scan disagrees with the head, and the tree
    // is rebuilt from the CAS.
    std::fs::write(root.tree().join("src/deep/c.rs"), b"tampered\n").unwrap();
    let corrupted = prepare_workspace(&root, &head_two, &snapshot('2'), &cas).unwrap();
    assert_eq!(corrupted.basis, WorkspaceBasisV1::Full);
    assert_eq!(
        corrupted.fallback,
        Some(WorkspaceFallbackReasonV1::DigestMismatch)
    );
    assert_eq!(
        corrupted.from_snapshot_id.as_deref(),
        Some(snapshot('1').as_str())
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

    // A preparation that ended before its marker was written left nothing to trust.
    std::fs::remove_file(root.path().join("head.json")).unwrap();
    let unmarked = prepare_workspace(&root, &head_two, &snapshot('3'), &cas).unwrap();
    assert_eq!(unmarked.basis, WorkspaceBasisV1::Full);
    assert_eq!(
        unmarked.fallback,
        Some(WorkspaceFallbackReasonV1::NoVerifiedTemplate)
    );
    assert_eq!(unmarked.from_snapshot_id, None);

    // A marker that contradicts its manifest is corrupt, whatever the tree holds.
    std::fs::write(root.path().join("manifest.json"), b"{}").unwrap();
    let contradicted = prepare_workspace(&root, &head_two, &snapshot('4'), &cas).unwrap();
    assert_eq!(contradicted.basis, WorkspaceBasisV1::Full);
    assert_eq!(
        contradicted.fallback,
        Some(WorkspaceFallbackReasonV1::TemplateCorrupt)
    );
    assert_eq!(contradicted.from_snapshot_id, None);

    // The rebuilt root is trusted again.
    let reused = prepare_workspace(&root, &head_two, &snapshot('5'), &cas).unwrap();
    assert_eq!(reused.basis, WorkspaceBasisV1::Reused);
    assert_eq!(
        reused.from_snapshot_id.as_deref(),
        Some(snapshot('4').as_str())
    );

    // A diff that cannot be applied to the clone: the previous manifest names a file where the
    // tree now holds a directory. The clone is discarded and the head rebuilt from the CAS.
    std::fs::remove_file(root.tree().join("a.rs")).unwrap();
    std::fs::create_dir(root.tree().join("a.rs")).unwrap();
    let head_three = manifest(&cas, &[("a.rs", "ein\n"), ("src/deep/c.rs", "three\n")]);
    let unapplied = prepare_workspace(&root, &head_three, &snapshot('6'), &cas).unwrap();
    assert_eq!(unapplied.basis, WorkspaceBasisV1::Full);
    assert_eq!(
        unapplied.fallback,
        Some(WorkspaceFallbackReasonV1::ApplyFailed)
    );
    assert_eq!(
        scan_tree(root.tree(), PathEncoding::LegacyV1).unwrap(),
        head_three
    );
    assert!(!root.path().join("tree.next").exists());
    assert!(!root.path().join("tree.old").exists());
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
    assert_eq!(root.id(), one);
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
