use super::*;
use std::path::PathBuf;

const KIND: &str = "schema = \"af.task-kind/1\"\nname = \"team/feature-kind\"\nversion = \"1.0.0\"\nkind = \"team/feature\"\nprofile = \"implementation\"\n";

fn package(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("feature-kind");
    std::fs::create_dir(&root).unwrap();
    for (path, text) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    (dir, root)
}

fn digest(root: &Path) -> String {
    TaskPlanCompiler::local_package(root).unwrap().digest
}

fn refused(root: &Path) -> String {
    TaskPlanCompiler::local_package(root).unwrap_err()
}

#[test]
fn a_package_digests_with_the_pin_recipe_and_reports_its_manifest_identity() {
    let (_dir, root) = package(&[("kind.toml", KIND), ("notes/readme.md", "notes\n")]);
    let local = TaskPlanCompiler::local_package(&root).unwrap();
    assert_eq!(local.name, "team/feature-kind");
    assert_eq!(local.version, "1.0.0");
    assert_eq!(
        local.digest,
        crate::lock::package_digest("feature-kind", &root).unwrap()
    );
    let files = BTreeMap::from([
        ("kind.toml".to_string(), KIND.as_bytes().to_vec()),
        ("notes/readme.md".to_string(), b"notes\n".to_vec()),
    ]);
    assert_eq!(local.digest, package_digest_from_files(&files));
    assert!(review_core::is_digest(&local.digest), "{}", local.digest);
    for suffix in ["/", "/.", "//./"] {
        let mut spelled = root.clone().into_os_string();
        spelled.push(suffix);
        assert_eq!(digest(Path::new(&spelled)), local.digest, "root{suffix}");
    }
}

#[test]
fn every_file_path_and_byte_contributes_and_nothing_else_does() {
    let (_dir, root) = package(&[("kind.toml", KIND), ("instructions.md", "a\n")]);
    let base = digest(&root);

    std::fs::write(root.join("instructions.md"), "b\n").unwrap();
    let edited = digest(&root);
    assert_ne!(edited, base, "a changed byte changes the digest");
    std::fs::write(root.join("instructions.md"), "a\n").unwrap();
    assert_eq!(digest(&root), base, "restored bytes restore the digest");

    std::fs::rename(root.join("instructions.md"), root.join("guide.md")).unwrap();
    assert_ne!(digest(&root), base, "a renamed file changes the digest");
    std::fs::rename(root.join("guide.md"), root.join("instructions.md")).unwrap();

    std::fs::create_dir(root.join("outputs")).unwrap();
    assert_eq!(digest(&root), base, "an empty directory holds no file");
    std::fs::write(root.join("outputs/extra.json"), "{}").unwrap();
    assert_ne!(
        digest(&root),
        base,
        "an added nested file changes the digest"
    );
    std::fs::remove_file(root.join("outputs/extra.json")).unwrap();

    std::fs::write(root.join("kind.toml"), KIND.replace("1.0.0", "1.0.1")).unwrap();
    assert_ne!(
        digest(&root),
        base,
        "the manifest is digested like any file"
    );
    std::fs::write(root.join("kind.toml"), KIND).unwrap();

    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        root.join("instructions.md"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert_eq!(digest(&root), base, "file modes are not content");

    let (_other, moved) = package(&[("kind.toml", KIND), ("instructions.md", "a\n")]);
    assert_eq!(digest(&moved), base, "the package location is not content");
}

#[test]
fn a_missing_or_wrongly_shaped_root_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let absent = dir.path().join("absent");
    assert!(refused(&absent).starts_with("No Task package directory at "));

    let file = dir.path().join("kind.toml");
    std::fs::write(&file, KIND).unwrap();
    assert!(refused(&file).ends_with("is not a directory"));

    let (_package, root) = package(&[("kind.toml", KIND)]);
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&root, &link).unwrap();
    assert!(refused(&link).contains("is a symlink"));
    for suffix in ["/", "/.", "//./"] {
        let mut spelled = link.clone().into_os_string();
        spelled.push(suffix);
        assert!(
            refused(Path::new(&spelled)).contains("is a symlink"),
            "link{suffix}"
        );
    }

    let empty = dir.path().join("empty");
    std::fs::create_dir_all(empty.join("nested")).unwrap();
    assert!(refused(&empty).ends_with("has no files"));
}

#[test]
fn an_incomplete_package_names_its_missing_or_ambiguous_manifest() {
    let (_dir, root) = package(&[("instructions.md", "a\n")]);
    let error = refused(&root);
    assert!(error.contains("exactly one pipeline.toml, worker.toml or kind.toml"));
    assert!(error.ends_with("found none"), "{error}");

    let (_dir, root) = package(&[("nested/kind.toml", KIND)]);
    assert!(refused(&root).ends_with("found none"));

    let (_dir, root) = package(&[("kind.toml", KIND), ("worker.toml", "name = 1\n")]);
    assert!(refused(&root).ends_with("found worker.toml, kind.toml"));
}

#[test]
fn a_malformed_manifest_or_identity_is_refused() {
    for (text, needle) in [
        ("name = [", "kind.toml: "),
        ("version = \"1.0.0\"\n", "declares no string `name`"),
        ("name = \"team/k\"\n", "declares no string `version`"),
        (
            "name = \"Team Kind\"\nversion = \"1.0.0\"\n",
            "not a package name",
        ),
        (
            "name = \"team/k\"\nversion = \"1.0\"\n",
            "not one exact major.minor.patch",
        ),
    ] {
        let (_dir, root) = package(&[("kind.toml", text)]);
        let error = refused(&root);
        assert!(error.contains(needle), "{text:?}: {error}");
    }
    let unknown = KIND.replace("implementation", "no-such-profile");
    let (_dir, root) = package(&[("kind.toml", &unknown)]);
    assert!(refused(&root).starts_with("Task package "));

    let (_dir, root) = package(&[("pipeline.toml", "name = \"team/p\"\nversion = \"1.0.0\"\n")]);
    assert!(refused(&root).starts_with("Task package "));
    let (_dir, root) = package(&[("worker.toml", "name = \"team/w\"\nversion = \"1.0.0\"\n")]);
    assert!(refused(&root).starts_with("Task package "));
}

#[test]
fn files_capture_cannot_hold_are_refused_never_skipped() {
    let (_dir, root) = package(&[("kind.toml", KIND)]);
    std::os::unix::fs::symlink(root.join("kind.toml"), root.join("alias.toml")).unwrap();
    assert!(refused(&root).contains("contains a symlink at"));

    let (_dir, root) = package(&[("kind.toml", KIND), ("back\\slash.md", "x")]);
    assert!(refused(&root).contains("a path a catalog cannot capture"));

    let (_dir, root) = package(&[("kind.toml", KIND)]);
    std::fs::write(root.join("large.bin"), vec![0u8; MAX_BYTES]).unwrap();
    assert!(refused(&root).contains("exceeds the capture bounds"));

    // A sparse file far past the bounds is refused by its size, before any of it is read.
    let (_dir, root) = package(&[("kind.toml", KIND)]);
    let sparse = std::fs::File::create(root.join("sparse.bin")).unwrap();
    sparse.set_len(4 * 1024 * 1024 * 1024).unwrap();
    drop(sparse);
    assert!(refused(&root).contains("exceeds the capture bounds"));
}

/// Digest `root`, running `swap` once on it after its no-follow check and before the walk opens
/// it: the window a check-then-open read leaves.
fn race(root: &Path, swap: impl Fn(&Path)) -> Result<LocalTaskPackage, String> {
    let mut swapped = false;
    let result = TaskPlanCompiler::local_package_with(root, &mut |path| {
        if path == root && !swapped {
            swapped = true;
            swap(path);
        }
    });
    assert!(swapped, "the read never reached {}", root.display());
    result
}

#[test]
fn a_root_replaced_by_another_valid_package_after_its_check_is_refused() {
    let (dir, root) = package(&[("kind.toml", KIND)]);
    let other = dir.path().join("other");
    std::fs::create_dir(&other).unwrap();
    let replacement = KIND.replace("1.0.0", "2.0.0");
    std::fs::write(other.join("kind.toml"), &replacement).unwrap();
    let replacement_digest = digest(&other);
    let moved = dir.path().join("moved");

    let error = race(&root, |path| {
        std::fs::rename(path, &moved).unwrap();
        std::fs::rename(&other, path).unwrap();
    })
    .unwrap_err();
    assert!(
        error.contains("was replaced while the package was read"),
        "{error}"
    );
    assert!(!error.contains(&replacement_digest), "{error}");
    // The replacement is a valid package in its own right: read afresh, it digests.
    assert_eq!(digest(&root), replacement_digest);
}

#[test]
fn a_root_replaced_by_a_symlink_after_its_check_is_refused() {
    let (dir, root) = package(&[("kind.toml", KIND)]);
    let (_other_dir, other) = package(&[("kind.toml", &KIND.replace("1.0.0", "2.0.0"))]);
    let moved = dir.path().join("moved");
    for suffix in ["", "/", "/."] {
        let mut spelled = root.clone().into_os_string();
        spelled.push(suffix);
        let spelled = PathBuf::from(spelled);
        let error = race(&spelled, |_| {
            std::fs::rename(&root, &moved).unwrap();
            std::os::unix::fs::symlink(&other, &root).unwrap();
        })
        .unwrap_err();
        assert!(error.contains("symlink"), "root{suffix}: {error}");
        std::fs::remove_file(&root).unwrap();
        std::fs::rename(&moved, &root).unwrap();
    }
}

#[test]
fn an_unswapped_root_reads_as_the_unhooked_read_does() {
    let (_dir, root) = package(&[("kind.toml", KIND), ("notes.md", "n\n")]);
    let local = race(&root, |_| {}).unwrap();
    assert_eq!(local, TaskPlanCompiler::local_package(&root).unwrap());
}
