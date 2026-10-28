use std::path::{Path, PathBuf};
use std::process::Command;

/// Command stand-ins have package independence, but no native principal, kind or model.
/// Change only copied fixture authority; the production catalog keeps model diversity.
pub fn command_fixture_independence(repo: &Path) {
    let workspace = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let production_path = workspace.join(".af/task-catalog.toml");
    let production = std::fs::read(&production_path).unwrap();
    let trusted: toml::Value = toml::from_str(std::str::from_utf8(&production).unwrap()).unwrap();
    assert_eq!(
        trusted["independence"]["distinct_models"].as_bool(),
        Some(true)
    );
    assert_eq!(
        trusted["independence"]["distinct_principals"].as_bool(),
        Some(false)
    );
    let path = repo.join(".af/task-catalog.toml");
    assert_ne!(
        path.canonicalize().unwrap(),
        production_path.canonicalize().unwrap()
    );
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    catalog["independence"] = toml::from_str(
        "command_workers_by_package = true\ndistinct_principals = false\n\
         distinct_providers = false\ndistinct_models = false\n",
    )
    .unwrap();
    std::fs::write(path, toml::to_string(&catalog).unwrap()).unwrap();
    assert_eq!(std::fs::read(production_path).unwrap(), production);
}

#[test]
fn command_fixture_policy_is_scoped_to_a_disposable_catalog() {
    let workspace = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("command-fixture");
    copy_tree(&workspace.join(".af"), &repo.join(".af"));
    crate::planning_models::assert_repository_policy(&repo);
    let path = repo.join(".af/task-catalog.toml");
    let mut expected: toml::Value =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

    command_fixture_independence(&repo);

    let actual: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        actual["independence"]["command_workers_by_package"].as_bool(),
        Some(true)
    );
    for field in [
        "distinct_principals",
        "distinct_providers",
        "distinct_models",
    ] {
        assert_eq!(actual["independence"][field].as_bool(), Some(false));
    }
    expected["independence"] = actual["independence"].clone();
    assert_eq!(actual, expected, "all other catalog authority is preserved");
    crate::planning_models::assert_repository_policy(&workspace);
}

pub fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
            // Gates supply read-only source files. These are independent disposable
            // fixtures whose tests intentionally edit them; keep the source immutable.
            let mut permissions = std::fs::metadata(&target).unwrap().permissions();
            {
                use std::os::unix::fs::PermissionsExt;
                permissions.set_mode(permissions.mode() | 0o200);
            }
            std::fs::set_permissions(&target, permissions).unwrap();
        }
    }
}

/// The one migration ADR-0255 asks of a project whose packages predate it, applied to a copy
/// under `repo`: every package file below `trees` (repository-relative directories) that names
/// `af/TaskCheckReceipt@1` names `af/TaskCheckReceipt@2`, and every package each of `catalogs`
/// (repository-relative catalog files, package paths relative to `repo`) pins is pinned again.
/// This repository's own `.af/`, and the fixtures staged to install into it, take it when the
/// release pin moves to the release that carries ADR-0255; until then a test that copies them
/// to run them under this `af` takes it on its copy.
pub fn migrate_check_receipt(repo: &Path, trees: &[&str], catalogs: &[&str]) {
    // A gate's sealed candidate supplies read-only files, and a copy made with `cp -R` keeps
    // their modes: every file this migration rewrites takes its owner-write bit first.
    fn writable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(permissions.mode() | 0o200);
        std::fs::set_permissions(path, permissions).unwrap();
    }
    let mut stack: Vec<PathBuf> = trees.iter().map(|tree| repo.join(tree)).collect();
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            for entry in std::fs::read_dir(&path).unwrap() {
                stack.push(entry.unwrap().path());
            }
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if text.contains("af/TaskCheckReceipt@1") {
            writable(&path);
            std::fs::write(
                &path,
                text.replace("af/TaskCheckReceipt@1", "af/TaskCheckReceipt@2"),
            )
            .unwrap();
        }
    }
    for catalog in catalogs {
        let path = repo.join(catalog);
        let mut value: toml::Value =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let mut changed = false;
        for (name, pin) in value["packages"].as_table_mut().unwrap() {
            let package = repo.join(pin["path"].as_str().unwrap());
            if !package.is_dir() {
                continue;
            }
            let digest = review_config::lock::package_digest(name, &package).unwrap();
            if pin["digest"].as_str() != Some(digest.as_str()) {
                pin["digest"] = toml::Value::String(digest);
                changed = true;
            }
        }
        if changed {
            writable(&path);
            std::fs::write(&path, toml::to_string(&value).unwrap()).unwrap();
        }
    }
}

pub fn fixture_named(root: &Path, name: &str) -> (PathBuf, PathBuf) {
    let repo = root.join("repo");
    let workspace = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    copy_tree(&workspace.join("fixtures/task-runtime").join(name), &repo);
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.name", "Fixture"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["add", "-A"],
        vec!["commit", "-qm", "fixture"],
    ] {
        let output = Command::new("git")
            .current_dir(&repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    (repo, root.join("state"))
}

/// ADR-0114's load-safe wall, two minutes, for a fixture process whose subject is not a
/// deadline: one that answers ends when it answers, and only a hung one waits this long.
pub const LOAD_SAFE_WALL_MS: i64 = 120_000;

/// Give every Worker Attempt of the fixture at `repo` (each package's `[signature.attempt]`)
/// and its code policy's checks the load-safe wall. A loaded gate host left a 5 s reviewer 377
/// ms of its wall after its preparation and killed it, so the Task ended on a lost Attempt
/// instead of its review (ADR-0124). The caller re-pins the packages, gives its Task file a wall
/// that holds them, and commits.
pub fn load_safe_walls(repo: &Path) {
    let catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(repo.join(".af/task-catalog.toml")).unwrap())
            .unwrap();
    for pin in catalog["packages"].as_table().unwrap().values() {
        let path = repo.join(pin["path"].as_str().unwrap()).join("worker.toml");
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut worker: toml::Value = toml::from_str(&text).unwrap();
        if let Some(attempt) = worker
            .get_mut("signature")
            .and_then(|signature| signature.get_mut("attempt"))
            .and_then(toml::Value::as_table_mut)
        {
            attempt.insert("wall_ms".into(), toml::Value::Integer(LOAD_SAFE_WALL_MS));
            std::fs::write(&path, toml::to_string(&worker).unwrap()).unwrap();
        }
    }
    let policy = repo.join(".af/code-policy.toml");
    let mut value: toml::Value =
        toml::from_str(&std::fs::read_to_string(&policy).unwrap()).unwrap();
    value["check_wall_ms"] = toml::Value::Integer(LOAD_SAFE_WALL_MS);
    std::fs::write(&policy, toml::to_string(&value).unwrap()).unwrap();
}

/// A Task wall of an hour and a verification wall of half of it: room for every Attempt of a
/// test's Task file at [`LOAD_SAFE_WALL_MS`].
pub fn load_safe_limits(limits: &mut serde_json::Value) {
    limits["wall_ms"] = serde_json::json!(3_600_000);
    limits["verification"]["wall_ms"] = serde_json::json!(1_800_000);
}
