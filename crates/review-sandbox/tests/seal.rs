//! Sealing: what a node changed is derived, never reported.

mod common;

use common::fixture_repo;
use review_check::{Arg, CheckDefinition, CheckRunner, Command};
use review_sandbox::{Mode, Sandbox};
use review_source_git::{Capture, Entry, EntryKind, Manifest, PathEncoding};

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

#[test]
fn a_legacy_encoded_baseline_seals_in_its_own_key_space() {
    let directory = tempfile::tempdir().unwrap();
    let cas = review_store::Cas::open(directory.path().join("cas")).unwrap();
    let leading = cas.put(b"leading").unwrap();
    let percent_space = cas.put(b"percent and space").unwrap();
    let manifest = Manifest {
        path_encoding: PathEncoding::LegacyV1,
        entries: vec![
            Entry {
                path: " notes.md".into(),
                kind: EntryKind::File,
                content: leading,
                size: 7,
            },
            Entry {
                path: "docs/50%25 off.md".into(),
                kind: EntryKind::File,
                content: percent_space,
                size: 17,
            },
        ],
    };
    let sandbox = Sandbox::materialize(&manifest, &cas, Mode::EphemeralWrite).unwrap();

    let sealed = sandbox.seal().unwrap();
    assert!(sealed.unchanged(), "{:?}", sealed.mutations);
    assert_eq!(sealed.final_manifest.path_encoding, PathEncoding::LegacyV1);
    assert_eq!(sealed.final_manifest, manifest);
}

/// A mutable node may leave a directory unreadable. Seal restores traversal permissions before
/// reading it, so a completed review is not discarded merely because its sandbox was hostile.
#[test]
#[cfg(unix)]
fn an_unreadable_directory_does_not_prevent_sealing() {
    use std::os::unix::fs::PermissionsExt;

    let (_dir, sandbox, _cas) = sandbox_of(Mode::EphemeralWrite);
    let source = sandbox.root().join("src");
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o000)).unwrap();

    let sealed = sandbox.seal().unwrap();
    assert!(sealed.unchanged(), "directory modes are not file mutations");
    let restored = std::fs::metadata(sealed.root().join("src"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(
        restored & 0o700,
        0o500,
        "sealing restores traversal without widening the directory to writable"
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

/// Manual evidence for the 5,000-file / ~200 MiB sandbox preparation and sealing budgets.
#[test]
#[ignore = "manual 5,000-file / 200 MiB sandbox measurement"]
fn unchanged_large_tree_sandbox_measurement() {
    eprintln!(
        "shared infrastructure workers: {}",
        review_parallel::worker_limit()
    );
    let dir = tempfile::tempdir().unwrap();
    let cas = review_store::Cas::open(dir.path().join("cas")).unwrap();
    let mut entries = Vec::with_capacity(5_000);
    for index in 0_u64..4_500 {
        let mut bytes = vec![0xA5; 46_603];
        bytes[..8].copy_from_slice(&index.to_be_bytes());
        entries.push(Entry {
            path: format!("files/{index:04}.bin"),
            kind: EntryKind::File,
            content: cas.put(&bytes).unwrap(),
            size: bytes.len() as u64,
        });
    }
    for index in 0..500 {
        let target = format!("../files/{index:04}.bin").into_bytes();
        entries.push(Entry {
            path: format!("links/{index:04}.bin"),
            kind: EntryKind::Symlink,
            content: cas.put(&target).unwrap(),
            size: target.len() as u64,
        });
    }
    let manifest = Manifest::new(entries).unwrap();
    let started = std::time::Instant::now();
    let template = review_sandbox::SandboxTemplate::materialize(&manifest, &cas).unwrap();
    let materialize_elapsed = started.elapsed();

    for mode in [Mode::EphemeralWrite, Mode::ReadOnly] {
        let started = std::time::Instant::now();
        let sandbox = Sandbox::from_template(&template, mode).unwrap();
        let clone_elapsed = started.elapsed();

        let started = std::time::Instant::now();
        let sealed = sandbox.seal().unwrap();
        let seal_elapsed = started.elapsed();

        assert!(sealed.unchanged(), "{:?}", sealed.mutations);
        let started = std::time::Instant::now();
        drop(sealed);
        let teardown_elapsed = started.elapsed();
        eprintln!(
            "5,000 entries / 199 MiB / {mode:?}: materialize {:.3}s, clone+permissions {:.3}s, seal {:.3}s, teardown {:.3}s",
            materialize_elapsed.as_secs_f64(),
            clone_elapsed.as_secs_f64(),
            seal_elapsed.as_secs_f64(),
            teardown_elapsed.as_secs_f64()
        );
    }

    let sandbox = Sandbox::from_template(&template, Mode::EphemeralWrite).unwrap();
    for shard in 0..100 {
        let directory = sandbox.root().join(format!("target/{shard:03}"));
        std::fs::create_dir_all(&directory).unwrap();
        for file in 0..100 {
            std::fs::write(directory.join(format!("{file:03}.o")), b"generated").unwrap();
        }
    }
    let started = std::time::Instant::now();
    let added = sandbox.seal().unwrap();
    let added_seal_elapsed = started.elapsed();
    assert_eq!(added.mutations.added.len(), 10_000);
    eprintln!(
        "5,000 baseline entries + 10,000 added files: seal {:.3}s",
        added_seal_elapsed.as_secs_f64()
    );

    let repeated_bytes = vec![0x5A; 41_943];
    let repeated_content = cas.put(&repeated_bytes).unwrap();
    let repeated = Manifest::new(
        (0..5_000)
            .map(|index| Entry {
                path: format!("repeated/{index:04}.bin"),
                kind: EntryKind::File,
                content: repeated_content.clone(),
                size: repeated_bytes.len() as u64,
            })
            .collect(),
    )
    .unwrap();
    let started = std::time::Instant::now();
    let _repeated_template = review_sandbox::SandboxTemplate::materialize(&repeated, &cas).unwrap();
    eprintln!(
        "5,000 repeated-content files / 199 MiB: materialize {:.3}s",
        started.elapsed().as_secs_f64()
    );
}

/// Manual growth-shape evidence: many distinct duplicated blobs must not become one resident
/// cache. Run this test under `/usr/bin/time -l` to record maximum resident set size.
#[test]
#[ignore = "manual 256 MiB many-distinct-repeated materialization measurement"]
fn many_distinct_repeated_blobs_have_bounded_resident_memory() {
    let dir = tempfile::tempdir().unwrap();
    let cas = review_store::Cas::open(dir.path().join("cas")).unwrap();
    let mut entries = Vec::with_capacity(256);
    for index in 0_u64..128 {
        let mut bytes = vec![0xC3; 1024 * 1024];
        bytes[..8].copy_from_slice(&index.to_be_bytes());
        let content = cas.put(&bytes).unwrap();
        for copy in 0..2 {
            entries.push(Entry {
                path: format!("pairs/{index:03}-{copy}.bin"),
                kind: EntryKind::File,
                content: content.clone(),
                size: bytes.len() as u64,
            });
        }
    }
    let manifest = Manifest::new(entries).unwrap();
    let started = std::time::Instant::now();
    let template = review_sandbox::SandboxTemplate::materialize(&manifest, &cas).unwrap();
    let sandbox = Sandbox::from_template(&template, Mode::EphemeralWrite).unwrap();
    assert!(sandbox.root().join("pairs/127-1.bin").is_file());
    eprintln!(
        "128 distinct duplicated 1 MiB blobs / 256 MiB output: materialize {:.3}s",
        started.elapsed().as_secs_f64()
    );
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
