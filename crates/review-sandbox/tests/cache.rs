mod common;

use common::fixture_repo;
use review_sandbox::{
    CacheKind, CacheLimits, CacheSource, Mode, Sandbox, materialize_cache,
    remove_materialized_caches,
};
use review_source_git::Capture;

fn source(root: &std::path::Path) -> CacheSource {
    CacheSource {
        kind: CacheKind::Cargo,
        source: root.to_path_buf(),
        limits: CacheLimits {
            max_bytes: 1024 * 1024,
            max_files: 100,
            max_copy_bytes: 1024 * 1024,
        },
    }
}

#[test]
fn cargo_cache_is_snapshotted_offline_and_removed_before_seal() {
    let (directory, repo, cas) = fixture_repo();
    let cache = directory.path().join("cargo-cache");
    let crate_file = cache.join("registry/cache/index/example.crate");
    std::fs::create_dir_all(crate_file.parent().unwrap()).unwrap();
    std::fs::write(&crate_file, b"cached crate").unwrap();

    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let sandbox = Sandbox::materialize(&snapshot.manifest, &cas, Mode::EphemeralWrite).unwrap();
    let receipt = materialize_cache(&source(&cache), &sandbox, &cas).unwrap();

    assert_eq!(receipt.kind, CacheKind::Cargo);
    assert_eq!(receipt.bytes, 12);
    assert_eq!(receipt.files, 1);
    assert!(cas.verify(&receipt.source_digest).is_ok());
    assert_eq!(
        std::fs::read(
            sandbox
                .root()
                .join(".af-cache/cargo/registry/cache/index/example.crate")
        )
        .unwrap(),
        b"cached crate"
    );
    let environment = CacheKind::Cargo.environment(sandbox.root());
    assert!(
        environment
            .local
            .contains(&("CARGO_NET_OFFLINE".into(), "true".into()))
    );
    assert!(
        environment
            .container
            .contains(&("CARGO_HOME".into(), "/work/.af-cache/cargo".into()))
    );

    remove_materialized_caches(&sandbox).unwrap();
    assert!(!sandbox.root().join(".af-cache").exists());
    assert!(sandbox.seal().unwrap().unchanged());
}

#[test]
fn cargo_cache_refuses_credential_shaped_and_over_limit_content() {
    let (directory, repo, cas) = fixture_repo();
    let cache = directory.path().join("cargo-cache");
    std::fs::create_dir_all(cache.join("registry/cache")).unwrap();
    std::fs::write(cache.join("registry/cache/.git-credentials"), b"secret").unwrap();
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let sandbox = Sandbox::materialize(&snapshot.manifest, &cas, Mode::EphemeralWrite).unwrap();
    assert!(
        materialize_cache(&source(&cache), &sandbox, &cas)
            .unwrap_err()
            .operator_detail()
            .contains("credential-shaped")
    );

    std::fs::remove_file(cache.join("registry/cache/.git-credentials")).unwrap();
    std::fs::write(cache.join("registry/cache/large.crate"), vec![0_u8; 32]).unwrap();
    let mut bounded = source(&cache);
    bounded.limits.max_bytes = 16;
    bounded.limits.max_copy_bytes = 16;
    assert!(
        materialize_cache(&bounded, &sandbox, &cas)
            .unwrap_err()
            .operator_detail()
            .contains("byte limit")
    );
}

#[test]
#[cfg(unix)]
fn cargo_cache_never_follows_symlinks() {
    use std::os::unix::fs::symlink;

    let (directory, repo, cas) = fixture_repo();
    let cache = directory.path().join("cargo-cache");
    std::fs::create_dir_all(cache.join("registry/cache")).unwrap();
    std::fs::write(directory.path().join("outside"), b"outside").unwrap();
    symlink(
        directory.path().join("outside"),
        cache.join("registry/cache/linked.crate"),
    )
    .unwrap();
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let sandbox = Sandbox::materialize(&snapshot.manifest, &cas, Mode::EphemeralWrite).unwrap();
    assert!(
        materialize_cache(&source(&cache), &sandbox, &cas)
            .unwrap_err()
            .operator_detail()
            .contains("following links")
    );
}

#[test]
fn cargo_cache_refuses_uncurated_registry_and_git_layouts() {
    let (directory, repo, cas) = fixture_repo();
    let cache = directory.path().join("cargo-cache");
    std::fs::create_dir_all(cache.join("registry/src/index")).unwrap();
    std::fs::write(cache.join("registry/src/index/lib.rs"), b"source").unwrap();
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let sandbox = Sandbox::materialize(&snapshot.manifest, &cas, Mode::EphemeralWrite).unwrap();
    assert!(
        materialize_cache(&source(&cache), &sandbox, &cas)
            .unwrap_err()
            .operator_detail()
            .contains("outside registry/cache")
    );

    std::fs::remove_dir_all(cache.join("registry")).unwrap();
    std::fs::create_dir_all(cache.join("git/db")).unwrap();
    std::fs::write(cache.join("git/db/config"), b"credential helper").unwrap();
    assert!(
        materialize_cache(&source(&cache), &sandbox, &cas)
            .unwrap_err()
            .operator_detail()
            .contains("outside registry/cache")
    );
}

#[test]
fn cargo_cache_counts_directories_before_allocating_an_unbounded_tree() {
    let (directory, repo, cas) = fixture_repo();
    let cache = directory.path().join("cargo-cache");
    for index in 0..8 {
        std::fs::create_dir_all(cache.join(format!("registry/cache/d{index}"))).unwrap();
    }
    std::fs::write(cache.join("registry/cache/item.crate"), b"crate").unwrap();
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let sandbox = Sandbox::materialize(&snapshot.manifest, &cas, Mode::EphemeralWrite).unwrap();
    let mut bounded = source(&cache);
    bounded.limits.max_files = 5;
    assert!(
        materialize_cache(&bounded, &sandbox, &cas)
            .unwrap_err()
            .operator_detail()
            .contains("filesystem-entry limit")
    );
}

#[test]
#[cfg(target_os = "macos")]
fn macos_reflinks_a_retained_descriptor_above_the_plain_copy_limit() {
    let (directory, repo, cas) = fixture_repo();
    let cache = directory.path().join("cargo-cache");
    let crate_file = cache.join("registry/cache/index/sparse.crate");
    std::fs::create_dir_all(crate_file.parent().unwrap()).unwrap();
    std::fs::File::create(&crate_file)
        .unwrap()
        .set_len(2 * 1024 * 1024)
        .unwrap();
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let sandbox = Sandbox::materialize(&snapshot.manifest, &cas, Mode::EphemeralWrite).unwrap();
    let mut bounded = source(&cache);
    bounded.limits.max_bytes = 4 * 1024 * 1024;
    bounded.limits.max_copy_bytes = 1;

    let receipt = materialize_cache(&bounded, &sandbox, &cas).unwrap();
    assert_eq!(
        receipt.materialization,
        review_sandbox::CacheMaterialization::Reflink
    );
}

#[test]
#[cfg(target_os = "macos")]
fn macos_reflink_strips_unmanifested_extended_metadata() {
    use exacl::{AclEntry, Perm};
    use std::os::unix::fs::PermissionsExt;

    let (directory, repo, cas) = fixture_repo();
    let cache = directory.path().join("cargo-cache");
    let crate_file = cache.join("registry/cache/index/metadata.crate");
    std::fs::create_dir_all(crate_file.parent().unwrap()).unwrap();
    std::fs::write(&crate_file, b"manifested data fork").unwrap();
    xattr::set(&crate_file, "com.afactory.secret", b"not manifest data").unwrap();
    exacl::setfacl(
        &[&crate_file],
        &[AclEntry::allow_user(
            "ABCDEFAB-CDEF-ABCD-EFAB-CDEF0000000C",
            Perm::READ,
            None,
        )],
        None,
    )
    .unwrap();

    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();
    let sandbox = Sandbox::materialize(&snapshot.manifest, &cas, Mode::EphemeralWrite).unwrap();
    let receipt = materialize_cache(&source(&cache), &sandbox, &cas).unwrap();
    assert_eq!(
        receipt.materialization,
        review_sandbox::CacheMaterialization::Reflink
    );

    let target = sandbox
        .root()
        .join(".af-cache/cargo/registry/cache/index/metadata.crate");
    assert_eq!(
        xattr::get(&target, "com.afactory.secret").unwrap(),
        None,
        "source xattrs must not survive materialization"
    );
    assert!(exacl::getfacl(&target, None).unwrap().is_empty());
    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(std::fs::read(target).unwrap(), b"manifested data fork");
}
