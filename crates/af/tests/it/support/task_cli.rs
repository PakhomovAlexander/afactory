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
