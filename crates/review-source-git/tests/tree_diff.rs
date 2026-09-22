//! Typed tree-to-tree diff behavior and byte-safe parsing.

mod common;

use common::{Fixture, cas_of, repo_of};
use review_source_git::{Capture, Entry, EntryKind, Manifest, TreeChangeKind};

fn object_store_state(root: &std::path::Path) -> Vec<(std::path::PathBuf, u64)> {
    fn walk(
        root: &std::path::Path,
        at: &std::path::Path,
        out: &mut Vec<(std::path::PathBuf, u64)>,
    ) {
        for entry in std::fs::read_dir(at).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.push((
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    entry.metadata().unwrap().len(),
                ));
            }
        }
    }
    let mut state = Vec::new();
    walk(root, root, &mut state);
    state.sort();
    state
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn resolved_trees_produce_typed_changes_and_a_fixed_patch() {
    let fixture = Fixture::new();
    fixture.write("old-name.txt", b"one\ntwo\nthree\nfour\nfive\n");
    fixture.write("modified.txt", b"before\n");
    fixture.write("odd\t\"name.txt", b"before\n");
    let base_revision = fixture.commit_all("base");

    fixture.git(&["mv", "old-name.txt", "new-name.txt"]);
    fixture.write("modified.txt", b"after\n");
    fixture.write("odd\t\"name.txt", b"after\n");
    fixture.write(" notes.md", b"leading whitespace is legal in Git\n");
    let head_revision = fixture.commit_all("head");

    let repo = repo_of(&fixture);
    let base = repo.resolve_tree(&base_revision).unwrap();
    let head = repo.resolve_tree(&head_revision).unwrap();
    let diff = repo.tree_diff(&base, &head).unwrap();

    assert!(diff.changes.iter().any(|change| {
        matches!(change.kind, TreeChangeKind::Renamed { similarity: 100 })
            && change.old_path.as_deref() == Some(b"old-name.txt".as_slice())
            && change.new_path.as_deref() == Some(b"new-name.txt".as_slice())
    }));
    assert!(diff.changes.iter().any(|change| {
        matches!(change.kind, TreeChangeKind::Modified)
            && change.old_path.as_deref() == Some(b"odd\t\"name.txt".as_slice())
            && change.new_path.as_deref() == Some(b"odd\t\"name.txt".as_slice())
    }));
    assert!(
        diff.patch().starts_with(b"diff --git a/"),
        "patch retained a raw/patch separator: {:?}",
        diff.patch().first()
    );
    assert!(
        contains(diff.patch(), b"diff --git a/old-name.txt b/new-name.txt"),
        "patch did not retain fixed a/ and b/ prefixes: {}",
        String::from_utf8_lossy(diff.patch())
    );
    assert!(contains(diff.patch(), b"similarity index 100%"));
    assert!(diff.git_version.starts_with("git version "));
    assert_eq!(
        diff.diff_policy,
        review_source_git::git::TREE_DIFF_POLICY_VERSION
    );

    let change_set = diff
        .change_set(
            format!("sha256:{}", "a".repeat(64)),
            format!("sha256:{}", "b".repeat(64)),
        )
        .unwrap();
    assert_eq!(
        change_set.changed_paths,
        [
            "%20notes.md",
            "modified.txt",
            "new-name.txt",
            "odd\t\"name.txt",
            "old-name.txt"
        ]
    );
    assert_eq!(change_set.renames.len(), 1);
    assert_eq!(change_set.renames[0].old_path, "old-name.txt");
    assert_eq!(change_set.renames[0].new_path, "new-name.txt");
}

#[test]
fn a_file_to_symlink_type_change_has_two_patch_stanzas() {
    let fixture = Fixture::new();
    fixture.write("kind.txt", b"ordinary file\n");
    let base_revision = fixture.commit_all("base");
    std::fs::remove_file(fixture.repo_path().join("kind.txt")).unwrap();
    fixture.symlink("target.txt", "kind.txt");
    let head_revision = fixture.commit_all("head");

    let repo = repo_of(&fixture);
    let diff = repo
        .tree_diff(
            &repo.resolve_tree(&base_revision).unwrap(),
            &repo.resolve_tree(&head_revision).unwrap(),
        )
        .unwrap();
    assert_eq!(diff.changes.len(), 1);
    assert!(matches!(diff.changes[0].kind, TreeChangeKind::TypeChanged));
    assert!(contains(diff.patch(), b"deleted file mode 100644"));
    assert!(contains(diff.patch(), b"new file mode 120000"));
}

#[test]
fn revision_like_options_cannot_become_tree_operands() {
    let fixture = Fixture::new();
    fixture.write("file.txt", b"content\n");
    fixture.commit_all("base");
    let repo = repo_of(&fixture);

    assert!(repo.resolve_tree("--help").is_err());
}

#[test]
fn an_over_limit_rename_search_records_truncation_without_losing_scope_paths() {
    let fixture = Fixture::new();
    for index in 0..=1000 {
        fixture.write(
            &format!("old/{index:04}.txt"),
            format!("old content {index:04}\n").as_bytes(),
        );
    }
    let base_revision = fixture.commit_all("base");

    for index in 0..=1000 {
        std::fs::remove_file(fixture.repo_path().join(format!("old/{index:04}.txt"))).unwrap();
        fixture.write(
            &format!("new/{index:04}.txt"),
            format!("unrelated new content {index:04}\n").as_bytes(),
        );
    }
    let head_revision = fixture.commit_all("head");

    let repo = repo_of(&fixture);
    let diff = repo
        .tree_diff(
            &repo.resolve_tree(&base_revision).unwrap(),
            &repo.resolve_tree(&head_revision).unwrap(),
        )
        .unwrap();
    assert!(diff.rename_detection_truncated);
    let change_set = diff
        .change_set(
            format!("sha256:{}", "a".repeat(64)),
            format!("sha256:{}", "b".repeat(64)),
        )
        .unwrap();
    assert!(change_set.rename_detection_truncated);
    assert_eq!(change_set.changed_paths.len(), 2_002);
    assert!(review_core::contains_report_path(
        &change_set.changed_paths,
        "old/0000.txt"
    ));
    assert!(review_core::contains_report_path(
        &change_set.changed_paths,
        "new/1000.txt"
    ));
}

#[test]
fn a_revalidated_worktree_is_diffed_as_an_isolated_synthetic_tree() {
    let fixture = Fixture::new();
    fixture.write("src/main.rs", b"fn old() {}\n");
    let base_revision = fixture.commit_all("base");
    fixture.write("src/main.rs", b"fn new() {}\n");
    fixture.write("src/added.rs", b"pub fn added() {}\n");

    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    let snapshot = Capture::new(&repo, &cas).dirty().unwrap();
    let before_worktree = review_source_git::worktree_state(&repo).unwrap();
    let objects = fixture.repo_path().join(".git/objects");
    let before_objects = object_store_state(&objects);
    let (tree, diff) = repo
        .tree_diff_synthetic_head(
            &repo.resolve_tree(&base_revision).unwrap(),
            &snapshot.manifest,
            &cas,
        )
        .unwrap();

    assert!(!tree.as_str().is_empty());
    assert!(contains(diff.patch(), b"+fn new() {}"));
    assert!(contains(diff.patch(), b"diff --git a/src/added.rs"));
    assert_eq!(
        review_source_git::worktree_state(&repo).unwrap(),
        before_worktree
    );
    assert_eq!(object_store_state(&objects), before_objects);
    repo.synthetic_tree(&snapshot.manifest, &cas).unwrap();
    assert_eq!(object_store_state(&objects), before_objects);
}

#[test]
fn a_synthetic_tree_refuses_an_unverified_manifest_size_before_framing() {
    let fixture = Fixture::new();
    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    let content = cas.put(b"blob\ndone\n").unwrap();
    let manifest = Manifest::new(vec![Entry {
        path: "payload".into(),
        kind: EntryKind::File,
        content,
        size: 4,
    }])
    .unwrap();

    let error = repo.synthetic_tree(&manifest, &cas).unwrap_err();
    assert!(error.to_string().contains("manifest size disagrees"));
}
