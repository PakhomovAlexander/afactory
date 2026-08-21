//! The adversarial case from `fixtures/adversarial/hostile-git-config.md`, made executable.
//!
//! The premise: the repository being reviewed is the attacker. Its `.git/config`, its
//! `.gitattributes` and its hooks are all candidate-controlled, and capture runs *before* any
//! sandbox exists, with the operator's privileges. Two properties have to hold:
//!
//! 1. **No candidate-controlled code runs.** A planted hook, filter, textconv or fsmonitor must
//!    never execute. The marker file is the proof: if anything ran, it exists.
//! 2. **Configuration cannot change identity.** The same content captured from a weaponized
//!    repository and a clean one must produce the same digest. If it could differ, two reviewers
//!    could agree they inspected snapshot X while holding different bytes.
//!
//! Both failures are silent in the resulting review, which is why they are tested rather than
//! argued about.

mod common;

use common::{Fixture, cas_of, marker_path, repo_of};
use review_source_git::{Capture, worktree_state};

/// Plant every content-transforming and code-executing lever a repository controls.
fn weaponize(fixture: &Fixture, marker: &std::path::Path) {
    let hooks = fixture.repo_path().join(".weaponized-hooks");
    std::fs::create_dir_all(&hooks).unwrap();

    // Hooks that could plausibly fire during inspection commands.
    for hook in [
        "post-index-change",
        "reference-transaction",
        "pre-auto-gc",
        "post-checkout",
        "fsmonitor-watchman",
        "proc-receive",
    ] {
        let path = hooks.join(hook);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\necho \"{hook} ran\" >> \"{}\"\nexit 0\n",
                marker.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    // A "filter" that rewrites content on the way out, and a textconv for diffs.
    let filter = fixture.repo_path().join("evil-filter.sh");
    std::fs::write(
        &filter,
        format!(
            "#!/bin/sh\necho \"filter ran\" >> \"{}\"\nsed 's/hi/PWNED/'\n",
            marker.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&filter, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fixture.git(&["config", "core.hooksPath", hooks.to_str().unwrap()]);
    fixture.git(&["config", "filter.evil.clean", filter.to_str().unwrap()]);
    fixture.git(&["config", "filter.evil.smudge", filter.to_str().unwrap()]);
    fixture.git(&["config", "diff.evil.textconv", filter.to_str().unwrap()]);
    fixture.git(&["config", "core.fsmonitor", filter.to_str().unwrap()]);
    // autocrlf would rewrite the bytes of any file with CRLF line endings.
    fixture.git(&["config", "core.autocrlf", "true"]);
    // An alias that shadows the plumbing capture calls.
    fixture.git(&["config", "alias.ls-tree", "!sh -c 'echo aliased'"]);

    // `.gitattributes` turns the filter on for the paths under review — and it lives in the
    // candidate tree, so it is exactly as untrusted as the code being reviewed.
    std::fs::write(
        fixture.repo_path().join(".gitattributes"),
        "* filter=evil diff=evil\n",
    )
    .unwrap();
    // A submodule pointing at a network URL: capture must never contact it.
    std::fs::write(
        fixture.repo_path().join(".gitmodules"),
        "[submodule \"evil\"]\n\tpath = evil\n\turl = https://127.0.0.1:1/evil.git\n",
    )
    .unwrap();
}

/// Content used by both repositories, including a CRLF file that `core.autocrlf` would rewrite.
fn plant_content(fixture: &Fixture) {
    fixture.write("src/main.rs", b"fn main() { println!(\"hi\"); }\n");
    fixture.write("crlf.txt", b"line one\r\nline two\r\n");
    fixture.write("docs/readme.md", b"# hi\n");
}

#[test]
fn a_weaponized_repository_cannot_execute_anything_or_change_identity() {
    // The clean control: same content, no hostile configuration.
    let clean = Fixture::new();
    plant_content(&clean);
    clean.commit_all("initial");
    let clean_repo = repo_of(&clean);
    let clean_cas = cas_of(&clean);
    let clean_snapshot = Capture::new(&clean_repo, &clean_cas)
        .committed("HEAD")
        .unwrap();

    // The hostile repository: identical content, every lever pulled. Content is committed
    // first, so the objects are the same and only the configuration differs.
    let hostile = Fixture::new();
    plant_content(&hostile);
    hostile.commit_all("initial");
    let marker = marker_path(hostile.dir.path());
    weaponize(&hostile, &marker);

    let repo = repo_of(&hostile);
    let cas = cas_of(&hostile);
    let capture = Capture::new(&repo, &cas);
    let before = worktree_state(&repo).unwrap();

    let committed = capture.committed("HEAD").unwrap();
    let dirty = capture.dirty().unwrap();

    assert!(
        !marker.exists(),
        "candidate-controlled code ran: {}",
        std::fs::read_to_string(&marker).unwrap_or_default()
    );

    assert_eq!(
        committed.content_digest, clean_snapshot.content_digest,
        "configuration changed the identity of identical content"
    );

    // The dirty capture sees the extra untracked files the weaponization added, so it must
    // differ — but the files it shares with the clean tree must be byte-identical.
    assert_eq!(
        dirty.manifest.get("crlf.txt").unwrap().content,
        clean_snapshot.manifest.get("crlf.txt").unwrap().content,
        "autocrlf altered worktree bytes"
    );
    assert_eq!(
        dirty.manifest.get("src/main.rs").unwrap().content,
        clean_snapshot.manifest.get("src/main.rs").unwrap().content,
        "a clean filter altered worktree bytes"
    );

    assert_eq!(
        before,
        worktree_state(&repo).unwrap(),
        "capture modified a hostile checkout"
    );
}

/// A hostile *global* config must be inert too. Capture runs with a private, empty HOME and
/// `GIT_CONFIG_GLOBAL=/dev/null`, so a `~/.gitconfig` — whether the developer's own or one an
/// attacker dropped there — cannot execute code or alter identity.
#[test]
fn a_hostile_global_config_is_inert() {
    let clean = Fixture::new();
    plant_content(&clean);
    clean.commit_all("initial");
    let clean_repo = repo_of(&clean);
    let clean_cas = cas_of(&clean);
    let expected = Capture::new(&clean_repo, &clean_cas)
        .committed("HEAD")
        .unwrap()
        .content_digest;

    let fixture = Fixture::new();
    plant_content(&fixture);
    fixture.commit_all("initial");
    let marker = marker_path(fixture.dir.path());

    let hooks = fixture.dir.path().join("global-hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    for hook in ["post-index-change", "reference-transaction"] {
        let path = hooks.join(hook);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\necho \"global {hook} ran\" >> \"{}\"\n",
                marker.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    std::fs::write(
        fixture.home_path().join(".gitconfig"),
        format!(
            "[core]\n\thooksPath = {}\n\tautocrlf = true\n\tfsmonitor = {}\n",
            hooks.display(),
            hooks.join("post-index-change").display()
        ),
    )
    .unwrap();

    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();

    assert!(!marker.exists(), "a global config ran code");
    assert_eq!(
        snapshot.content_digest, expected,
        "a global config changed identity"
    );
}

/// The environment git receives is rebuilt from a fixed allowlist after `env_clear`, which is
/// why an inherited `GIT_EXTERNAL_DIFF` or `GIT_CONFIG_COUNT` cannot reach it. This pins the
/// list so a future edit cannot quietly widen it.
#[test]
fn git_receives_only_the_allowlisted_environment() {
    let fixture = Fixture::new();
    plant_content(&fixture);
    fixture.commit_all("initial");
    let repo = repo_of(&fixture);

    let passed: Vec<&str> = repo.environment().iter().map(|(k, _)| *k).collect();
    assert_eq!(passed, review_source_git::Repo::ENV_ALLOWLIST);

    for dangerous in [
        "GIT_EXTERNAL_DIFF",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_KEY_0",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_PROXY_COMMAND",
        "GIT_SSH_COMMAND",
        "GIT_ASKPASS",
    ] {
        assert!(!passed.contains(&dangerous), "{dangerous} would reach git");
    }
}

/// A submodule entry is recorded content, never a fetch. Capture is offline by construction:
/// the URL points at a closed port, so any attempt to contact it would fail the capture.
#[test]
fn a_submodule_url_is_never_contacted() {
    let fixture = Fixture::new();
    plant_content(&fixture);
    std::fs::write(
        fixture.repo_path().join(".gitmodules"),
        "[submodule \"evil\"]\n\tpath = evil\n\turl = https://127.0.0.1:1/evil.git\n",
    )
    .unwrap();
    fixture.commit_all("with a submodule declaration");

    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    let snapshot = Capture::new(&repo, &cas).committed("HEAD").unwrap();

    // `.gitmodules` is just a file in the tree, and that is all it should ever be here.
    assert!(snapshot.manifest.get(".gitmodules").is_some());
    assert!(snapshot.manifest.get("evil").is_none());
}

/// The hole this file found on its first run: `git status` hashes worktree files, which runs the
/// candidate's own `clean` filter. Nothing in capture may call such a command, and the boundary
/// is enforced rather than remembered.
#[test]
fn filtering_subcommands_are_refused_outright() {
    let fixture = Fixture::new();
    plant_content(&fixture);
    fixture.commit_all("initial");
    let repo = repo_of(&fixture);

    for dangerous in ["status", "diff", "add", "checkout", "stash", "gc", "fetch"] {
        let err = repo.text(&[dangerous]).unwrap_err();
        assert!(
            matches!(
                err,
                review_source_git::GitError::UnsafeSubcommand { ref subcommand } if subcommand == dangerous
            ),
            "`git {dangerous}` was not refused: {err}"
        );
    }

    // And the plumbing capture actually uses still works.
    assert!(repo.text(&["rev-parse", "HEAD"]).is_ok());
}

/// Tree diff is a separate typed door: candidate attributes may select a configured textconv,
/// but the adapter neither executes it nor lets hostile diff settings alter the patch.
#[cfg(unix)]
#[test]
fn tree_diff_ignores_candidate_textconv_and_hostile_diff_configuration() {
    fn history(fixture: &Fixture) -> (String, String) {
        plant_content(fixture);
        fixture.write(".gitattributes", b"* diff=evil\n");
        let base = fixture.commit_all("base");
        fixture.write("src/main.rs", b"fn main() { println!(\"changed\"); }\n");
        let head = fixture.commit_all("head");
        (base, head)
    }

    let clean = Fixture::new();
    let (clean_base, clean_head) = history(&clean);
    let clean_repo = repo_of(&clean);
    let clean_diff = clean_repo
        .tree_diff(
            &clean_repo.resolve_tree(&clean_base).unwrap(),
            &clean_repo.resolve_tree(&clean_head).unwrap(),
        )
        .unwrap();

    let hostile = Fixture::new();
    let (hostile_base, hostile_head) = history(&hostile);
    let marker = marker_path(hostile.dir.path());
    let textconv = hostile.dir.path().join("evil-textconv.sh");
    std::fs::write(
        &textconv,
        format!(
            "#!/bin/sh\necho textconv-ran >> \"{}\"\ncat \"$1\"\n",
            marker.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&textconv, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    hostile.git(&["config", "diff.evil.textconv", textconv.to_str().unwrap()]);
    hostile.git(&["config", "diff.algorithm", "histogram"]);
    hostile.git(&["config", "diff.renames", "false"]);
    hostile.git(&["config", "diff.mnemonicPrefix", "true"]);
    hostile.git(&["config", "core.quotePath", "false"]);
    hostile.git(&["config", "color.ui", "always"]);

    // Prove the fixture is armed, then clear the marker before entering the typed adapter.
    hostile.git(&["diff", "--textconv", &hostile_base, &hostile_head, "--"]);
    assert!(
        marker.exists(),
        "ordinary git diff did not execute textconv"
    );
    std::fs::remove_file(&marker).unwrap();

    let hostile_repo = repo_of(&hostile);
    let hostile_diff = hostile_repo
        .tree_diff(
            &hostile_repo.resolve_tree(&hostile_base).unwrap(),
            &hostile_repo.resolve_tree(&hostile_head).unwrap(),
        )
        .unwrap();

    assert!(!marker.exists(), "typed tree diff executed textconv");
    assert_eq!(
        hostile_diff, clean_diff,
        "repository diff configuration changed the typed tree diff"
    );
}

/// A weaponized repository where the marker would fire on a *single* filtering command, proving
/// the boundary is what keeps it absent rather than luck about which commands git needs.
#[test]
fn one_filtering_command_would_have_been_enough() {
    let fixture = Fixture::new();
    plant_content(&fixture);
    fixture.commit_all("initial");
    let marker = marker_path(fixture.dir.path());
    weaponize(&fixture, &marker);
    // Modify a file so a status/diff would have to hash it.
    fixture.write("src/main.rs", b"fn main() { println!(\"hi there\"); }\n");

    // Ordinary git, no boundary: the filter runs. This is the control.
    fixture.git(&["status", "--porcelain"]);
    assert!(
        marker.exists(),
        "the fixture is not actually weaponized; the control proves nothing"
    );
    std::fs::remove_file(&marker).unwrap();

    // The capture path over the same repository: nothing runs.
    let repo = repo_of(&fixture);
    let cas = cas_of(&fixture);
    let capture = Capture::new(&repo, &cas);
    capture.committed("HEAD").unwrap();
    capture.dirty().unwrap();
    worktree_state(&repo).unwrap();

    assert!(!marker.exists(), "capture ran candidate-controlled code");
}
