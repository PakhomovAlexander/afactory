//! Git metadata dependencies must work in both normal checkouts and linked worktrees.
use std::{path::Path, process::Command};

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn build_metadata_watches_existing_git_paths_and_current_branch() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let script = temp.path().join("build-metadata");
    let output = Command::new("rustc")
        .current_dir(&workspace)
        .arg("--edition=2024")
        .arg(workspace.join("crates/reviewctl/build.rs"))
        .arg("-o")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "fixture@example.invalid"]);
    git(&repo, &["config", "user.name", "fixture"]);
    git(&repo, &["commit", "--allow-empty", "-qm", "initial"]);
    let worktree = temp.path().join("worktree");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-qb",
            "linked",
            worktree.to_str().unwrap(),
        ],
    );
    for checkout in [&repo, &worktree] {
        for packed in [false, true] {
            if packed {
                git(checkout, &["pack-refs", "--all", "--prune"]);
            }
            let output = Command::new(&script)
                .current_dir(checkout)
                .env("CARGO_MANIFEST_DIR", checkout)
                .output()
                .unwrap();
            assert!(output.status.success());
            let output = String::from_utf8(output.stdout).unwrap();
            let expected = git(checkout, &["rev-parse", "--short=12", "HEAD"]);
            assert!(output.contains(&format!("cargo:rustc-env=AF_GIT_COMMIT={expected}")));
            let watches: Vec<_> = output
                .lines()
                .filter_map(|line| line.strip_prefix("cargo:rerun-if-changed="))
                .filter(|path| Path::new(path).is_absolute())
                .collect();
            assert!(!watches.is_empty());
            assert!(
                watches.iter().all(|path| Path::new(path).exists()),
                "{watches:?}"
            );
            let branch = git(checkout, &["symbolic-ref", "HEAD"]);
            let branch_path = git(
                checkout,
                &["rev-parse", "--path-format=absolute", "--git-path", &branch],
            );
            assert!(
                watches
                    .iter()
                    .any(|watch| Path::new(&branch_path).starts_with(watch)),
                "new loose refs must invalidate metadata: {watches:?}"
            );
            if packed {
                let packed_path = git(
                    checkout,
                    &[
                        "rev-parse",
                        "--path-format=absolute",
                        "--git-path",
                        "packed-refs",
                    ],
                );
                assert!(watches.contains(&packed_path.as_str()));
            }
            git(checkout, &["commit", "--allow-empty", "-qm", "advance"]);
            let advanced = Command::new(&script)
                .current_dir(checkout)
                .output()
                .unwrap();
            let next = git(checkout, &["rev-parse", "--short=12", "HEAD"]);
            assert_ne!(expected, next);
            assert!(
                String::from_utf8(advanced.stdout)
                    .unwrap()
                    .contains(&format!("AF_GIT_COMMIT={next}"))
            );
        }
    }
}
