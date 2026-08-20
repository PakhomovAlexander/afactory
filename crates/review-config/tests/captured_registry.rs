use std::collections::BTreeMap;

use review_config::lock::{Lockfile, Registry};
use review_core::SubjectKind;

#[test]
fn captured_registry_never_returns_to_mutated_package_paths() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("reviewers/architecture");
    std::fs::create_dir_all(&package).unwrap();
    let original = b"name = \"architecture\"\nversion = \"1.0.0\"\nsubjects = [\"whole-tree\"]\n\n[runner]\nprogram = \"original\"\nargs = []\n";
    std::fs::write(package.join("reviewer.toml"), original).unwrap();
    let disk = Registry::new([directory.path().join("reviewers")]);
    let mut lockfile = Lockfile::empty();
    lockfile.reviewers.insert(
        "architecture".into(),
        Lockfile::pin("architecture", &disk).unwrap(),
    );

    let captured = Registry::captured(BTreeMap::from([(
        "architecture".into(),
        BTreeMap::from([("reviewer.toml".into(), original.to_vec())]),
    )]));
    let mut mutated = original.to_vec();
    let offset = mutated
        .windows(b"original".len())
        .position(|window| window == b"original")
        .unwrap();
    mutated[offset..offset + b"mutated!".len()].copy_from_slice(b"mutated!");
    std::fs::write(package.join("reviewer.toml"), mutated).unwrap();

    let resolved = lockfile
        .resolve_for_subject("architecture", &captured, SubjectKind::WholeTree)
        .unwrap();
    assert_eq!(resolved.runner.program, "original");
    assert_eq!(resolved.file("reviewer.toml"), Some(original.as_slice()));
}
