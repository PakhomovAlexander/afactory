//! `af catalog digest` prints exactly the digest a catalog pin records, through the same recipe
//! and validation Task capture uses, and reads nothing but the package directory.

use crate::common::{Sandbox, Signer, VERSION, af, af_binary, err, out, write};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Output;

fn workspace() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn digest(cwd: &Path, package: &Path) -> Output {
    af().current_dir(cwd)
        .args(["catalog", "digest"])
        .arg(package)
        .output()
        .unwrap()
}

/// Every `[packages]` pin of one catalog file: name, digest and package path.
fn pins(catalog: &Path) -> Vec<(String, String, String)> {
    let text = std::fs::read_to_string(catalog).unwrap();
    let value: toml::Table = toml::from_str(&text).unwrap();
    value["packages"]
        .as_table()
        .unwrap()
        .iter()
        .map(|(name, pin)| {
            (
                name.clone(),
                pin["digest"].as_str().unwrap().to_owned(),
                pin["path"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn assert_reproduces(repo: &Path, catalog: &Path) -> usize {
    let pins = pins(catalog);
    for (name, pin, path) in &pins {
        let output = digest(repo, Path::new(path));
        assert!(output.status.success(), "{name}: {}", err(&output));
        assert_eq!(out(&output), format!("{pin}\n"), "{name} at {path}");
        assert!(output.stderr.is_empty(), "{name}: {}", err(&output));
    }
    pins.len()
}

/// Every file of a directory tree with its bytes and modification time.
fn tree(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
                files.insert(path.clone(), (std::fs::read(&path).unwrap(), modified));
            }
        }
    }
    files
}

/// A private, writable copy of a package: bytes only. `std::fs::copy` would carry over the
/// source's file modes, and the checkout under test may be read-only.
fn copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy(&entry.path(), &target);
        } else {
            std::fs::write(target, std::fs::read(entry.path()).unwrap()).unwrap();
        }
    }
}

/// AF's own catalog publishes the pins the issue's workaround matched (ten when it was filed;
/// the catalog has grown since). Every pin it holds now, and every pin of the runtime fixture
/// catalogs, reproduces byte for byte. `bound-inputs` and `continue` are excluded: their
/// catalogs carry placeholder digests their tests replace before planning.
#[test]
fn every_published_package_pin_reproduces() {
    let root = workspace();
    let own = assert_reproduces(&root, &root.join(".af/task-catalog.toml"));
    assert!(own >= 10, "AF's catalog publishes {own} pins");

    let fixtures = root.join("fixtures/task-runtime");
    let mut checked = 0;
    for entry in std::fs::read_dir(&fixtures).unwrap() {
        let repo = entry.unwrap().path();
        let catalog = repo.join(".af/task-catalog.toml");
        if repo.ends_with("bound-inputs") || repo.ends_with("continue") || !catalog.is_file() {
            continue;
        }
        checked += assert_reproduces(&repo, &catalog);
    }
    assert!(checked >= 28, "fixture catalogs held {checked} pins");
}

/// A copy of a read-only package is writable, so the mutations below never need the checkout
/// to be writable, and never touch it.
#[test]
fn private_copies_of_a_read_only_package_are_writable() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let sealed = dir.path().join("sealed");
    std::fs::create_dir_all(sealed.join("outputs")).unwrap();
    for file in ["worker.toml", "outputs/report.json"] {
        let path = sealed.join(file);
        std::fs::write(&path, "x").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    }
    let private = dir.path().join("private");
    copy(&sealed, &private);
    for file in ["worker.toml", "outputs/report.json"] {
        std::fs::write(private.join(file), "y").unwrap();
        assert_eq!(std::fs::read(sealed.join(file)).unwrap(), b"x");
    }
}

/// The digest follows every digest-relevant byte and path, and only those; the command changes
/// no file of the package it reads.
#[test]
fn digest_relevant_mutations_change_the_digest_and_the_package_is_left_unchanged() {
    let root = workspace();
    let (_, pin, path) = pins(&root.join(".af/task-catalog.toml"))
        .into_iter()
        .find(|(name, ..)| name == "kernel/bugs")
        .unwrap();
    let source = root.join(path);
    let original_source = tree(&source);
    let dir = tempfile::tempdir().unwrap();
    let package = dir.path().join("bugs");
    copy(&source, &package);
    let printed = |package: &Path| {
        let output = digest(dir.path(), package);
        assert!(output.status.success(), "{}", err(&output));
        out(&output).trim_end().to_owned()
    };

    let before = tree(&package);
    assert_eq!(printed(&package), pin, "a copy elsewhere keeps its pin");
    assert_eq!(tree(&package), before, "the command wrote to the package");

    let instructions = package.join("instructions.md");
    let original = std::fs::read(&instructions).unwrap();
    std::fs::write(&instructions, [original.as_slice(), b"\n"].concat()).unwrap();
    assert_ne!(printed(&package), pin, "instructions bytes are digested");
    std::fs::write(&instructions, &original).unwrap();
    assert_eq!(printed(&package), pin);

    std::fs::write(package.join("outputs/extra.json"), "{}").unwrap();
    assert_ne!(printed(&package), pin, "an added nested file is digested");
    std::fs::remove_file(package.join("outputs/extra.json")).unwrap();

    let schema = package.join("input.schema.json");
    std::fs::rename(&schema, package.join("input.json")).unwrap();
    assert_ne!(printed(&package), pin, "file paths are digested");
    std::fs::rename(package.join("input.json"), &schema).unwrap();

    let manifest = package.join("worker.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, text.replacen("\n", "\n# reviewed\n", 1)).unwrap();
    assert_ne!(printed(&package), pin, "manifest bytes are digested");
    std::fs::write(&manifest, &text).unwrap();
    assert_eq!(printed(&package), pin);

    assert_eq!(tree(&source), original_source, "the checkout was changed");
}

/// An invalid or incomplete package prints nothing on stdout, names the reason and exits 1.
#[test]
fn invalid_or_incomplete_packages_fail_closed() {
    let root = workspace();
    let dir = tempfile::tempdir().unwrap();
    let source = root.join(".af/task-packages/kernel/bugs");
    let refuses = |package: &Path, needle: &str| {
        let before = tree(dir.path());
        let output = digest(dir.path(), package);
        assert_eq!(output.status.code(), Some(1), "{needle}: {}", err(&output));
        assert!(output.stdout.is_empty(), "{needle}: {}", out(&output));
        let message = err(&output);
        assert!(message.starts_with("af catalog digest: "), "{message}");
        assert!(message.contains(needle), "`{needle}` missing: {message}");
        assert_eq!(tree(dir.path()), before, "{needle}: a refusal wrote a file");
    };

    refuses(&dir.path().join("absent"), "No Task package directory");
    let empty = dir.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    refuses(&empty, "has no files");

    let headless = dir.path().join("headless");
    copy(&source, &headless);
    std::fs::remove_file(headless.join("worker.toml")).unwrap();
    refuses(&headless, "found none");

    let twice = dir.path().join("twice");
    copy(&source, &twice);
    std::fs::write(
        twice.join("pipeline.toml"),
        std::fs::read(root.join(".af/task-packages/kernel/review-light/pipeline.toml")).unwrap(),
    )
    .unwrap();
    refuses(&twice, "found pipeline.toml, worker.toml");

    let broken = dir.path().join("broken");
    copy(&source, &broken);
    let manifest = broken.join("worker.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, text.replace("[signature]", "[signature")).unwrap();
    refuses(&broken, "worker.toml");

    let floating = dir.path().join("floating");
    copy(&source, &floating);
    let manifest = floating.join("worker.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    // Whatever release the package is at, its version cut to `major.minor`.
    let version = text
        .lines()
        .find_map(|line| line.strip_prefix("version = \""))
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap();
    let floating_version = version.rsplit_once('.').unwrap().0;
    std::fs::write(
        &manifest,
        text.replacen(
            &format!("version = \"{version}\""),
            &format!("version = \"{floating_version}\""),
            1,
        ),
    )
    .unwrap();
    refuses(&floating, "is not one exact major.minor.patch release");

    let invalid = dir.path().join("invalid");
    copy(&source, &invalid);
    let manifest = invalid.join("worker.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        text.replace("schema = \"af.worker/1\"", "schema = \"x\""),
    )
    .unwrap();
    refuses(
        &invalid,
        "Worker manifest has incompatible identity or protocol",
    );

    let linked = dir.path().join("linked");
    copy(&source, &linked);
    std::os::unix::fs::symlink(linked.join("instructions.md"), linked.join("alias.md")).unwrap();
    refuses(&linked, "contains a symlink at");

    let file = dir.path().join("file");
    std::fs::write(&file, "").unwrap();
    refuses(&file, "is not a directory");

    // A symlinked root is refused however its last component is spelled; the same spellings of
    // the package directory itself print its digest.
    let package = dir.path().join("package");
    copy(&source, &package);
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&package, &link).unwrap();
    let expected = out(&digest(dir.path(), &package));
    for suffix in ["", "/", "/.", "//", "/./", "/.//./."] {
        let mut spelled = link.clone().into_os_string();
        spelled.push(suffix);
        refuses(Path::new(&spelled), "is a symlink");
        let relative = format!("link{suffix}");
        refuses(Path::new(&relative), "is a symlink");
        let mut directory = package.clone().into_os_string();
        directory.push(suffix);
        let output = digest(dir.path(), Path::new(&directory));
        assert_eq!(
            output.status.code(),
            Some(0),
            "package{suffix}: {}",
            err(&output)
        );
        assert_eq!(out(&output), expected, "package{suffix}");
    }

    let missing = af().args(["catalog", "digest"]).output().unwrap();
    assert_eq!(
        missing.status.code(),
        Some(2),
        "a package directory is required"
    );
}

/// `--help` states the output, what contributes to the digest and what the command never does.
#[test]
fn help_describes_the_output_the_contributing_files_and_the_boundary() {
    for help in [
        af().args(["catalog", "digest", "--help"]).output().unwrap(),
        af().args(["help", "catalog", "digest"]).output().unwrap(),
    ] {
        assert!(help.status.success(), "{}", err(&help));
        let text = out(&help);
        for needle in [
            "Usage: af catalog digest <PACKAGE_DIR>",
            "sha256:",
            "contributes its relative path and its bytes",
            "exits 1",
            "Never:",
            "af catalog digest .af/task-packages/team/implementer",
        ] {
            assert!(text.contains(needle), "`{needle}` missing:\n{text}");
        }
    }
}

/// Every file under `root`, or none when it is absent: what af's own machine state holds.
fn state_files(root: &Path) -> Vec<PathBuf> {
    if root.exists() {
        tree(root).into_keys().collect()
    } else {
        Vec::new()
    }
}

/// The command is read-only for af itself too: in a project pinned to a release this machine
/// has not installed, with an update check due and `auto_update = "always"`, it neither
/// dispatches to nor installs the pinned release, schedules no check, activates nothing and
/// writes no self-management state; a malformed invocation of it does none of that either.
#[test]
fn the_digest_never_dispatches_installs_or_schedules_an_update_check() {
    let current = semver::Version::parse(VERSION).unwrap();
    let newer = semver::Version::new(current.major, current.minor + 1, 0).to_string();
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    let digest = sandbox.publish("0.8.1", false);
    sandbox.publish(&newer, false);
    let real = sandbox.adopt_real_binary();
    write(
        &sandbox.path("config/af/config.toml"),
        "[self]\nauto_update = \"always\"\n",
    );
    let repo = sandbox.pinned_repo("repo", &Sandbox::lock("0.8.1", Some(&digest)));
    let package = workspace().join(".af/task-packages/kernel/implementer");
    let expected = out(&digest_of(&package));
    let log = sandbox.path("checks.log");
    let before = (
        state_files(&sandbox.path("state")),
        state_files(&sandbox.path("cache")),
        state_files(&sandbox.versions()),
    );

    for binary in [af_binary(), real.as_path()] {
        let run = |args: &[&std::ffi::OsStr]| {
            sandbox
                .command(binary)
                .args(args)
                .current_dir(&repo)
                .env("AF_TEST_SELF_CHECK_LOG", &log)
                .output()
                .unwrap()
        };
        let output = run(&["catalog".as_ref(), "digest".as_ref(), package.as_os_str()]);
        assert!(output.status.success(), "{}", err(&output));
        assert_eq!(out(&output), expected, "the running af printed the digest");
        assert!(output.stderr.is_empty(), "{}", err(&output));

        let malformed = run(&["catalog".as_ref(), "digest".as_ref()]);
        assert_eq!(malformed.status.code(), Some(2), "{}", err(&malformed));
        assert!(!out(&malformed).contains("fake af"), "{}", out(&malformed));
    }
    assert!(!log.exists(), "no update check was scheduled");
    assert!(
        !sandbox.versions().join("0.8.1").exists(),
        "the pinned release was not installed"
    );
    assert!(!sandbox.versions().join(&newer).exists());
    assert!(sandbox.default_target().is_none(), "nothing was activated");
    assert_eq!(
        (
            state_files(&sandbox.path("state")),
            state_files(&sandbox.path("cache")),
            state_files(&sandbox.versions()),
        ),
        before,
        "af's own state is unchanged"
    );
}

/// The digest of `package` from the test's own af, with self-management off.
fn digest_of(package: &Path) -> Output {
    let output = af()
        .args(["catalog", "digest"])
        .arg(package)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", err(&output));
    output
}
