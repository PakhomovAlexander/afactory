//! The executables integration tests start, owned by one coordination boundary shared by every
//! crate whose tests build or launch a workspace output (ADR-0173, ADR-0174).
//!
//! Cargo's uplifted paths (`<target>/<profile>/examples/<name>`, `<target>/<profile>/<bin>`) are
//! cargo's, not a test's. Every build, fresh or not, puts a root unit there again unless the path
//! already is the very file of its `deps` output: it removes the old file, then links or copies
//! the new one, and on macOS it always copies. So a test process that builds an output replaces
//! the path any other test process, of this crate or another, is about to start. Nextest runs
//! every test in its own process, and `review-process`, `review-pipeline` and `af` tests all run
//! in one gate.
//!
//! The boundary is one directory per profile, [`DIRECTORY`], with one lock file:
//!
//! - **A builder** ([`built`]) builds under the exclusive lock and, still under it, copies the
//!   uplifted output into a snapshot named by its bytes, renamed into place only after the copy
//!   is verified against them; a name that already holds exactly those bytes is reused.
//! - **A launcher of an output cargo built before the tests** ([`held`]), such as `af` tests'
//!   `CARGO_BIN_EXE_af`, takes the lock shared, so no builder is between its removal and its
//!   copy, and links the file it finds.
//! - **Either way** the path returned is this process's own hard link to the verified snapshot
//!   or the held file, named by those bytes or that file and by its process. No build, and no other process, moves or rewrites it,
//!   and a test starts only that path. A link whose process has exited is removed by the next
//!   process that takes the lock, and a snapshot no longer current is removed by the next build
//!   of its output; a process still running keeps its own link, so its inode stays.
//!
//! A test binary started without cargo (copied into a container, say) builds nothing and uses the
//! example or binary beside it as built.

#![allow(dead_code)]

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where every test process of every crate keeps the lock and the snapshots, beside the
/// profile's `examples`.
pub const DIRECTORY: &str = "af-test-executables";

/// The profile directory this test binary was built into: `<target>[/<triple>]/<profile>`.
pub fn profile() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent().and_then(Path::parent).unwrap().to_path_buf()
}

/// The coordination directory of `profile`, created if it is missing.
pub fn directory(profile: &Path) -> PathBuf {
    let owned = profile.join(DIRECTORY);
    std::fs::create_dir_all(&owned).unwrap();
    owned
}

/// Cargo's uplifted path of `name` (`--example` or `--bin` as `kind`) in `profile`.
pub fn uplifted(profile: &Path, kind: &str, name: &str) -> PathBuf {
    match kind {
        "--example" => profile.join("examples").join(name),
        "--bin" => profile.join(name),
        other => panic!("unknown cargo target kind {other}"),
    }
}

/// The cargo that built this test, if it can build again.
fn cargo() -> Option<PathBuf> {
    std::env::var_os("CARGO")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(env!("CARGO"))).filter(|cargo| cargo.is_file()))
}

/// The manifest of the workspace crate `package`, resolved at run time first: under `[warm]` a
/// cached test binary may have been compiled in an earlier, destroyed sandbox (ADR-0131), whose
/// root `AF_WORKSPACE_ROOT` replaces.
pub fn manifest(package: &str) -> PathBuf {
    let crates = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(|root| PathBuf::from(root).join("crates"))
        .or_else(|| std::env::var_os("CARGO_MANIFEST_DIR").map(|own| PathBuf::from(own).join("..")))
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."));
    crates.join(package).join("Cargo.toml")
}

/// Build `name` (`--example` or `--bin` as `kind`) of the workspace crate `package` into this
/// test's own target directory and profile, with the workspace's feature resolution (after a
/// whole-workspace build that is a fingerprint check, never a second, different binary,
/// ADR-0167), and return this process's own link to its verified snapshot. `triple` is the
/// relative triple directory the including crate's `build.rs` recorded.
pub fn built(kind: &str, name: &str, package: &str, triple: &str) -> PathBuf {
    built_with_features(kind, name, package, triple, &[])
}

/// [`built`] with the workspace's feature resolution plus `features` (`package/feature`), as a
/// laboratory calibration or diagnostic build compiles it. The snapshot is named by its bytes,
/// so it never stands in for the default build of the same output.
pub fn built_with_features(
    kind: &str,
    name: &str,
    package: &str,
    triple: &str,
    features: &[&str],
) -> PathBuf {
    let profile = profile();
    let path = uplifted(&profile, kind, name);
    let Some(cargo) = cargo() else {
        assert!(
            path.is_file(),
            "{} is missing and no cargo can build it: `cargo build {kind} {name}`",
            path.display()
        );
        return path;
    };
    let owned = directory(&profile);
    // Held until this process's link exists: no other process of any crate builds meanwhile.
    let _lock = exclusive(&owned.join("lock"));
    let (target, triple) = layout(&profile, triple);
    let mut build = Command::new(cargo);
    build
        .args(["build", "--locked", "--offline", "--quiet", "--workspace"])
        .args([kind, name, "--manifest-path"])
        .arg(manifest(package))
        .arg("--target-dir")
        .arg(target)
        .arg("--profile")
        .arg(match profile.file_name().and_then(|name| name.to_str()) {
            Some("debug") => "dev",
            Some(name) => name,
            None => panic!("{} has no profile name", profile.display()),
        });
    if let Some(triple) = triple {
        build.arg("--target").arg(triple);
    }
    if !features.is_empty() {
        build.arg("--features").arg(features.join(","));
    }
    let output = build.output().unwrap();
    assert!(
        output.status.success(),
        "building {kind} {name} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(path.is_file(), "{} was not built", path.display());
    let current = snapshot(&path, &owned);
    prune_snapshots(&owned, name, &current);
    prune_links(&owned);
    link(&current, &owned)
}

/// This process's own link to `path`, an output cargo built before the tests ran and no test of
/// this crate builds, taken while no builder holds the boundary.
pub fn held(path: &Path) -> PathBuf {
    held_in(&directory(&profile()), path)
}

/// [`held`] in the coordination directory `owned`.
pub fn held_in(owned: &Path, path: &Path) -> PathBuf {
    let _lock = shared(&owned.join("lock"));
    assert!(path.is_file(), "{} was not built", path.display());
    prune_links(owned);
    link(path, owned)
}

/// An exclusive advisory lock on `path`, held until the returned value is dropped.
pub fn exclusive(path: &Path) -> nix::fcntl::Flock<std::fs::File> {
    lock(path, nix::fcntl::FlockArg::LockExclusive)
}

/// A shared advisory lock on `path`, held until the returned value is dropped.
pub fn shared(path: &Path) -> nix::fcntl::Flock<std::fs::File> {
    lock(path, nix::fcntl::FlockArg::LockShared)
}

fn lock(path: &Path, kind: nix::fcntl::FlockArg) -> nix::fcntl::Flock<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .unwrap();
    nix::fcntl::Flock::lock(file, kind)
        .map_err(|(_, errno)| errno)
        .unwrap()
}

/// A copy of `built` in `directory`, named by its bytes. Only a builder writes there, by renaming
/// a verified copy into place, and a name that already holds exactly these bytes is used as it
/// is, so a later build never moves or rewrites a snapshot.
pub fn snapshot(built: &Path, directory: &Path) -> PathBuf {
    let bytes = std::fs::read(built).unwrap();
    let name = built.file_name().and_then(|name| name.to_str()).unwrap();
    let owned = directory.join(format!("{name}-{:016x}-{}", fnv1a(&bytes), bytes.len()));
    if std::fs::read(&owned).is_ok_and(|held| held == bytes) {
        return owned;
    }
    let staging = directory.join(format!(".staging-{name}-{}", std::process::id()));
    let _ = std::fs::remove_file(&staging);
    // A copy keeps the executable mode; on macOS it is the clone cargo itself makes.
    std::fs::copy(built, &staging).unwrap();
    assert!(
        std::fs::read(&staging).unwrap() == bytes,
        "{} changed while it was copied",
        built.display()
    );
    std::fs::rename(&staging, &owned).unwrap();
    owned
}

/// This process's hard link to `file` in `directory`: the same inode, so the same bytes, whatever
/// later replaces the name it came from. A snapshot's link is `<snapshot>.<pid>`, so other bytes
/// are another link; a held file's is `<name>-held-<device>-<inode>.<pid>`. A filesystem without
/// hard links gets a verified snapshot instead.
pub fn link(file: &Path, directory: &Path) -> PathBuf {
    let name = file.file_name().and_then(|name| name.to_str()).unwrap();
    let (device, inode) = identity(file).unwrap();
    let label = if file.parent() == Some(directory) {
        name.to_owned()
    } else {
        format!("{name}-held-{device:x}-{inode:x}")
    };
    let mine = directory.join(format!("{label}.{}", std::process::id()));
    if identity(&mine) == Some((device, inode)) {
        return mine;
    }
    let _ = std::fs::remove_file(&mine);
    if std::fs::hard_link(file, &mine).is_err() {
        return snapshot(file, directory);
    }
    assert_eq!(
        identity(&mine),
        Some((device, inode)),
        "{} is not the file it links",
        mine.display()
    );
    mine
}

/// Remove the links of processes that have exited. A link names its process; a live process,
/// or one whose number was reused, keeps it.
pub fn prune_links(directory: &Path) {
    for entry in std::fs::read_dir(directory).unwrap().flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|name| name.rsplit_once('.'))
            .and_then(|(_, pid)| pid.parse::<i32>().ok())
        else {
            continue;
        };
        if pid > 0 && !alive(pid) {
            // Another launcher may remove the same link first.
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Remove the snapshots of `name` other than `current`. No test starts a snapshot itself, only
/// its own link, which keeps the inode however its snapshot name is removed. Only a builder,
/// under the exclusive lock, calls this.
pub fn prune_snapshots(directory: &Path, name: &str, current: &Path) {
    let prefix = format!("{name}-");
    for entry in std::fs::read_dir(directory).unwrap().flatten() {
        let path = entry.path();
        let file = entry.file_name();
        let Some(file) = file.to_str() else { continue };
        let snapshot_of_name = file
            .strip_prefix(&prefix)
            .and_then(|rest| rest.split_once('-'))
            .is_some_and(|(hash, length)| {
                hash.len() == 16
                    && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                    && !length.is_empty()
                    && length.bytes().all(|byte| byte.is_ascii_digit())
            });
        if snapshot_of_name && path != current {
            std::fs::remove_file(&path).unwrap();
        }
    }
}

fn alive(pid: i32) -> bool {
    !matches!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
        Err(nix::errno::Errno::ESRCH)
    )
}

/// 64-bit FNV-1a: a snapshot's name, never its verification (the bytes are compared).
pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// A file's identity, its device and inode, if it exists.
pub fn identity(path: &Path) -> Option<(u64, u64)> {
    std::fs::metadata(path)
        .ok()
        .map(|metadata| (metadata.dev(), metadata.ino()))
}

/// The target directory and explicit triple of a profile directory, from the triple directory
/// cargo's own layout recorded at build time (`build.rs`): empty for `<target>/<profile>`, the
/// triple for `<target>/<triple>/<profile>`. No marker file or directory name decides it, so a
/// precreated, cache-restored or nested target directory resolves like a fresh one (ADR-0168).
pub fn layout<'a>(profile: &'a Path, triple: &'a str) -> (&'a Path, Option<&'a str>) {
    let above = profile.parent().unwrap();
    if triple.is_empty() {
        return (above, None);
    }
    assert_eq!(
        above.file_name().and_then(|name| name.to_str()),
        Some(triple),
        "{} is not below the {triple} layout cargo built",
        profile.display()
    );
    (above.parent().unwrap(), Some(triple))
}
