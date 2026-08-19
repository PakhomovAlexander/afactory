//! A small repository to capture and sandbox.

use review_source_git::Repo;
use review_store::Cas;

pub fn fixture_repo() -> (tempfile::TempDir, Repo, Cas) {
    let dir = tempfile::tempdir().unwrap();
    let repo_path = dir.path().join("repo");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&repo_path).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(&repo_path)
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };

    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "fixture@example.invalid"]);
    git(&["config", "user.name", "Fixture"]);
    std::fs::create_dir_all(repo_path.join("src")).unwrap();
    std::fs::write(repo_path.join("src/main.rs"), b"fn main() {}\n").unwrap();
    std::fs::write(repo_path.join("README.md"), b"# fixture\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "initial"]);

    let cas = Cas::open(dir.path().join("cas")).unwrap();
    let repo = Repo::open(&repo_path, &home);
    (dir, repo, cas)
}
