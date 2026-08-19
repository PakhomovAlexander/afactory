// Each integration-test binary compiles this module separately, so a helper used only by the
// other binary reads as dead here. Both are used; see capture.rs and hostile_git_config.rs.
#![allow(dead_code)]

//! Fixture repositories.
//!
//! Built with ordinary git (the thing under test is *capture*, not construction), but with an
//! isolated HOME and explicit identity so a developer's global config cannot make a fixture
//! behave differently on their machine than in CI.

use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Fixture {
    pub dir: tempfile::TempDir,
}

impl Fixture {
    pub fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("repo")).unwrap();
        std::fs::create_dir_all(dir.path().join("home")).unwrap();
        std::fs::create_dir_all(dir.path().join("cas")).unwrap();
        let fixture = Fixture { dir };
        fixture.git(&["init", "-q", "-b", "main"]);
        fixture.git(&["config", "user.email", "fixture@example.invalid"]);
        fixture.git(&["config", "user.name", "Fixture"]);
        // Keep objects loose: a capture test that removes a blob's loose object to make it
        // unproducible must not race an auto-gc that packed it away first.
        fixture.git(&["config", "gc.auto", "0"]);
        fixture
    }

    pub fn repo_path(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    pub fn home_path(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    pub fn cas_path(&self) -> PathBuf {
        self.dir.path().join("cas")
    }

    pub fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(self.repo_path())
            .env("HOME", self.home_path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    pub fn write(&self, path: &str, contents: &[u8]) {
        let full = self.repo_path().join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, contents).unwrap();
    }

    #[cfg(unix)]
    pub fn write_executable(&self, path: &str, contents: &[u8]) {
        use std::os::unix::fs::PermissionsExt;
        self.write(path, contents);
        std::fs::set_permissions(
            self.repo_path().join(path),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }

    #[cfg(unix)]
    pub fn symlink(&self, target: &str, at: &str) {
        std::os::unix::fs::symlink(target, self.repo_path().join(at)).unwrap();
    }

    pub fn commit_all(&self, message: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
        self.git(&["rev-parse", "HEAD"]).trim().to_string()
    }

    /// A small repository exercising every entry kind the manifest knows.
    pub fn with_content(&self) {
        self.write("src/main.rs", b"fn main() { println!(\"hi\"); }\n");
        self.write("docs/readme.md", b"# readme\n");
        self.write_executable("scripts/run.sh", b"#!/bin/sh\necho hi\n");
        self.symlink("src/main.rs", "latest.rs");
        self.write(".gitignore", b"ignored/\n");
        self.write("ignored/secret.txt", b"never reviewed\n");
    }
}

pub fn repo_of(fixture: &Fixture) -> review_source_git::Repo {
    review_source_git::Repo::open(fixture.repo_path(), fixture.home_path())
}

pub fn cas_of(fixture: &Fixture) -> review_store::Cas {
    review_store::Cas::open(fixture.cas_path()).unwrap()
}

pub fn marker_path(dir: &Path) -> PathBuf {
    dir.join("HOST-MARKER")
}
