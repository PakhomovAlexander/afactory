//! Sealing: what a node changed is derived, never reported.

mod common;

use common::fixture_repo;
use review_check::{Arg, CheckDefinition, CheckRunner, Command};
use review_sandbox::{Mode, Sandbox};
use review_source_git::Capture;

fn sandbox_of(mode: Mode) -> (tempfile::TempDir, Sandbox, review_store::Cas) {
    let (dir, repo, cas) = fixture_repo();
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let sandbox = Sandbox::materialize(&snapshot.manifest, &cas, mode).unwrap();
    (dir, sandbox, cas)
}

/// The full mutation vocabulary, in one node's run.
#[test]
fn every_kind_of_mutation_is_captured() {
    let (_dir, sandbox, cas) = sandbox_of(Mode::EphemeralWrite);
    let runner = CheckRunner::new(&cas, sandbox.root());

    let tdd = CheckDefinition::new(
        "tdd",
        Command::new(
            "/bin/sh",
            vec![
                Arg::literal("-c"),
                Arg::literal(
                    "echo 'fn main() { /* fixed */ }' > src/main.rs; \
                     echo '#[test] fn t() {}' > src/main_test.rs; \
                     rm README.md",
                ),
            ],
        ),
    );
    assert!(runner.run(&tdd).passed());

    let sealed = sandbox.seal().unwrap();
    assert_eq!(sealed.mutations.modified, vec!["src/main.rs"]);
    assert_eq!(sealed.mutations.added, vec!["src/main_test.rs"]);
    assert_eq!(sealed.mutations.deleted, vec!["README.md"]);
    assert_eq!(
        sealed.mutations.paths(),
        vec!["README.md", "src/main.rs", "src/main_test.rs"],
        "the declared path set of a patch proposal must equal exactly this"
    );
    assert!(!sealed.unchanged());
}

/// A node that leaves the tree alone seals clean — including one that only reads.
#[test]
fn a_node_that_changed_nothing_seals_clean() {
    let (_dir, sandbox, cas) = sandbox_of(Mode::EphemeralWrite);
    let runner = CheckRunner::new(&cas, sandbox.root());
    let reader = CheckDefinition::new(
        "read-only-reviewer",
        Command::new(
            "/bin/sh",
            vec![
                Arg::literal("-c"),
                Arg::literal("cat src/main.rs > /dev/null"),
            ],
        ),
    );
    assert!(runner.run(&reader).passed());

    let sealed = sandbox.seal().unwrap();
    assert!(sealed.unchanged(), "{:?}", sealed.mutations);
    assert_eq!(
        sealed.final_manifest.content_digest(),
        sealed.baseline.content_digest(),
        "an untouched sandbox must still be the snapshot it was given"
    );
}

/// A diagnostic mutation left behind is visible, which is what makes the auto-apply rule
/// checkable: the patch must equal the computed diff, so an unreverted probe fails it.
#[test]
fn an_unreverted_diagnostic_mutation_is_visible() {
    let (_dir, sandbox, cas) = sandbox_of(Mode::EphemeralWrite);
    let runner = CheckRunner::new(&cas, sandbox.root());

    let reviewer = CheckDefinition::new(
        "perf",
        Command::new(
            "/bin/sh",
            vec![
                Arg::literal("-c"),
                // The intended fix, plus a printf debug the reviewer forgot to remove.
                Arg::literal(
                    "echo 'fn main() { /* fixed */ }' > src/main.rs; \
                     echo 'eprintln!(\"here\");' > src/scratch-probe.rs",
                ),
            ],
        ),
    );
    assert!(runner.run(&reviewer).passed());

    let sealed = sandbox.seal().unwrap();
    let declared = vec!["src/main.rs".to_string()]; // what the proposal claims to touch
    assert_ne!(
        sealed.mutations.paths(),
        declared,
        "the seal must expose the extra file, or an auto-applied patch would carry it"
    );
    assert!(
        sealed
            .mutations
            .added
            .contains(&"src/scratch-probe.rs".to_string())
    );
}

/// Kind is part of identity: swapping a file for a symlink to identical bytes is a change.
#[test]
fn replacing_a_file_with_a_symlink_counts_as_a_mutation() {
    let (_dir, sandbox, cas) = sandbox_of(Mode::EphemeralWrite);
    let runner = CheckRunner::new(&cas, sandbox.root());
    let swap = CheckDefinition::new(
        "swap",
        Command::new(
            "/bin/sh",
            vec![
                Arg::literal("-c"),
                Arg::literal("rm README.md && ln -s src/main.rs README.md"),
            ],
        ),
    );
    assert!(runner.run(&swap).passed());

    let sealed = sandbox.seal().unwrap();
    assert_eq!(sealed.mutations.modified, vec!["README.md"]);
    assert!(sealed.mutations.deleted.is_empty());
}

/// The mode round-trip for executables, in both sandbox modes. Regression: read-only used to
/// flatten every file to 0o444, so a tree with scripts sealed as "everything executable
/// mutated" — first observed not by a test but by the hub's own tree on the first live run.
#[test]
fn executables_survive_both_modes_and_seal_clean() {
    let (dir, repo, cas) = fixture_repo();
    let script = repo.workdir().join("tool.sh");
    std::fs::write(&script, "#!/bin/sh\ntrue\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(repo.workdir())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    git(&["add", "-A"]);
    git(&[
        "-c",
        "user.email=s@example.invalid",
        "-c",
        "user.name=S",
        "commit",
        "-q",
        "-m",
        "x",
    ]);
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();

    for mode in [Mode::ReadOnly, Mode::EphemeralWrite] {
        let sandbox = Sandbox::materialize(&snapshot.manifest, &cas, mode).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let bits = std::fs::metadata(sandbox.root().join("tool.sh"))
                .unwrap()
                .permissions()
                .mode();
            assert!(
                bits & 0o111 != 0,
                "{mode:?} must keep the exec bit: {bits:o}"
            );
        }
        let sealed = sandbox.seal().unwrap();
        assert!(
            sealed.unchanged(),
            "{mode:?}: an untouched tree with executables must seal clean: {:?}",
            sealed.mutations
        );
    }
    drop(dir);
}

/// A read-only sandbox must not strand its materialized tree. Its directories are 0o555, and
/// unlinking needs write on the parent — so without the restore-on-drop the whole tree leaks
/// into TMPDIR. Both the sealed path and the dropped-without-seal path must reclaim it.
#[test]
#[cfg(unix)]
fn a_read_only_sandbox_cleans_up_its_tree() {
    for seal_it in [true, false] {
        let (_dir, sandbox, _cas) = sandbox_of(Mode::ReadOnly);
        let root = sandbox.root().to_path_buf();
        // The TempDir itself is root's parent; dropping the sandbox must remove it.
        let tempdir = root.parent().unwrap().to_path_buf();
        assert!(root.exists(), "materialized tree should exist");

        if seal_it {
            let _ = sandbox.seal().unwrap();
        } else {
            drop(sandbox);
        }
        assert!(
            !tempdir.exists(),
            "read-only sandbox leaked its tree at {} (sealed={seal_it})",
            tempdir.display()
        );
    }
}

/// Seal must not read or hash files the reviewer added — they are `added` whatever their bytes.
/// This is the difference between sealing a sandbox where a reviewer ran a build (thousands of
/// new files) cheaply versus SHA-256-ing a gigabyte for nothing.
#[test]
fn added_files_are_not_hashed() {
    let (_dir, sandbox, cas) = sandbox_of(Mode::EphemeralWrite);
    // A reviewer leaves a large new file behind (as a build would).
    let big = vec![0xABu8; 4 * 1024 * 1024];
    std::fs::write(sandbox.root().join("target-artifact.bin"), &big).unwrap();

    let sealed = sandbox.seal().unwrap();
    assert!(
        sealed
            .mutations
            .added
            .contains(&"target-artifact.bin".to_string()),
        "the added file is detected: {:?}",
        sealed.mutations.added
    );
    // Its manifest entry carries the size but no content hash — the bytes were never read.
    let entry = sealed
        .final_manifest
        .entries
        .iter()
        .find(|e| e.path == "target-artifact.bin")
        .expect("added file is in the final manifest");
    assert_eq!(entry.size, big.len() as u64);
    assert!(
        entry.content.is_empty(),
        "an added file must not be hashed; content = {:?}",
        entry.content
    );
    // The CAS never received those 4 MiB.
    assert!(!cas.contains(&review_source_git::digest_bytes(&big)));
}

/// A COW clone must isolate writes: two sandboxes cloned from one template are independent,
/// and neither can reach the template. This is the property that lets the snapshot be
/// materialized once and cloned per node.
#[test]
fn clones_from_a_template_isolate_their_writes() {
    let (_dir, repo, cas) = fixture_repo();
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let template = review_sandbox::SandboxTemplate::materialize(&snapshot.manifest, &cas).unwrap();

    let a = Sandbox::from_template(&template, Mode::EphemeralWrite).unwrap();
    let b = Sandbox::from_template(&template, Mode::EphemeralWrite).unwrap();

    // Both clones start as faithful copies.
    let path = "src/main.rs";
    let original = std::fs::read(a.root().join(path)).unwrap();
    assert_eq!(std::fs::read(b.root().join(path)).unwrap(), original);

    // A write to one clone touches neither the other clone nor the template.
    std::fs::write(a.root().join(path), b"fn main() { /* only in a */ }\n").unwrap();
    assert_ne!(std::fs::read(a.root().join(path)).unwrap(), original);
    assert_eq!(
        std::fs::read(b.root().join(path)).unwrap(),
        original,
        "the sibling clone must be untouched"
    );

    // b, cloned from the template, seals clean — it changed nothing.
    let sealed_b = b.seal().unwrap();
    assert!(sealed_b.unchanged(), "{:?}", sealed_b.mutations);
    // a's edit is the only mutation.
    let sealed_a = a.seal().unwrap();
    assert_eq!(sealed_a.mutations.modified, vec![path.to_string()]);
}
