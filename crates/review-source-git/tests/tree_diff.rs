//! Typed tree-to-tree diff behavior and byte-safe parsing.

mod common;

use common::{Fixture, cas_of, repo_of};
use review_source_git::{Capture, TreeChangeKind};

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
}

#[cfg(unix)]
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
fn a_revalidated_worktree_is_diffed_as_an_isolated_synthetic_tree() {
    let fixture = Fixture::new();
    fixture.write("src/main.rs", b"fn old() {}\n");
    let base_revision = fixture.commit_all("base");
    fixture.write("src/main.rs", b"fn new() {}\n");
    fixture.write("src/added.rs", b"pub fn added() {}\n");

    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    let snapshot = Capture::new(&repo, &cas).dirty().unwrap();
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
}
