//! ADR-0158 end to end: the Surveyor measures a Project into Surveys.
//!
//! A fixture repository built with fixed dates, names, emails and messages, a merge, a rename,
//! a binary change, a commit whose author date is a year before its committer date and agent
//! trailers, yields a Survey byte-identical to `tests/fixtures/project-survey/expected.json` at
//! a fixed cutoff, with the totals the measurement policy states. An unchanged survey key
//! records nothing and prints the key; a new commit, tag or branch, a partial newest Survey or
//! `--force` records a Survey chained to the one before. `max_commits`, `max_wall_ms`,
//! `max_paths`, `max_entries`, `max_file_bytes` and `max_survey_bytes` record what they bound
//! and why, exit 0, or `SurveyFailed@1` naming the bytes. A slow stream stops at the deadline
//! inside its section. No author email reaches a record, an event, a document or an output
//! under either identity policy, and `email_hash` is the 16-hex rule. `af onboard --apply`
//! records the first Survey once. Nothing is written inside the checkout.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::schemas;

/// The fixed cutoff of every Survey here: 2027-01-15T08:00:00Z.
const CUTOFF: u64 = 1_800_000_000;
const DAY: u64 = 86_400;
/// Every author email the fixture's history holds, the Co-Authored-By trailers' included; the
/// last is also written as an author name.
const EMAILS: [&str; 6] = [
    "ada@fixture.invalid",
    "bob@fixture.invalid",
    "bot@fixture.invalid",
    "noreply@anthropic.com",
    "codex@fixture.invalid",
    "carol@fixture.invalid",
];
/// The title of the fixture's third ADR: repository text, recorded as committed.
const CONTACT: &str = "Contact ada@fixture.invalid";
const EXPECTED: &str = include_str!("../fixtures/project-survey/expected.json");
/// The format of the Surveyor's one ref observation, which the survey key and `history`'s tags
/// and branches share.
const REF_FORMAT: &str =
    "--format=%(refname)%09%(objectname)%09%(objecttype)%09%(creatordate:unix)";

struct Machine {
    root: tempfile::TempDir,
}

impl Machine {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("state")).unwrap();
        Self { root }
    }

    fn root(&self) -> PathBuf {
        std::fs::canonicalize(self.root.path()).unwrap()
    }

    fn state_home(&self) -> PathBuf {
        self.root().join("state")
    }

    fn af(&self, directory: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
        let mut command = crate::common::af();
        command
            .current_dir(directory)
            .env("XDG_STATE_HOME", self.state_home())
            .env("AF_SELF_OFFLINE", "1")
            .env("NO_COLOR", "1")
            .env("AF_TEST_SURVEY_CUTOFF_UNIX_S", CUTOFF.to_string());
        for (name, value) in env {
            command.env(name, value);
        }
        command.args(args).output().unwrap()
    }

    /// Run `af`, expect `code`, and return stdout and stderr.
    fn expect(
        &self,
        directory: &Path,
        env: &[(&str, &str)],
        args: &[&str],
        code: i32,
    ) -> (String, String) {
        let output = self.af(directory, env, args);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert_eq!(
            output.status.code(),
            Some(code),
            "af {args:?}\n{stdout}\n{stderr}"
        );
        (stdout, stderr)
    }

    fn json(&self, directory: &Path, env: &[(&str, &str)], args: &[&str]) -> Value {
        let (stdout, _) = self.expect(directory, env, args, 0);
        serde_json::from_str(&stdout).unwrap_or_else(|_| panic!("not JSON: {stdout}"))
    }

    /// The one Project Store of this machine.
    fn store(&self) -> PathBuf {
        let mut stores: Vec<PathBuf> = std::fs::read_dir(self.state_home().join("af/project"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(stores.len(), 1, "{stores:?}");
        stores.pop().unwrap()
    }

    /// The type of every event of the Project Store's log, in order.
    fn events(&self) -> Vec<String> {
        let log = rusqlite::Connection::open(self.store().join("events.sqlite")).unwrap();
        log.prepare("SELECT type FROM events ORDER BY sequence")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn surveys(&self) -> usize {
        self.events()
            .iter()
            .filter(|event| *event == "SurveyRecorded@1")
            .count()
    }
}

/// git as the fixture's author `who` at `days` before the cutoff.
fn git_as(repo: &Path, who: Option<(&str, &str, u64)>, args: &[&str]) -> String {
    let (name, email, days) = who.unwrap_or(("Fixture", "fixture@example.invalid", 0));
    git_dated(repo, (name, email), days, days, args)
}

/// git as `who`, authoring `authored` days and committing `committed` days before the cutoff.
fn git_dated(
    repo: &Path,
    who: (&str, &str),
    authored: u64,
    committed: u64,
    args: &[&str],
) -> String {
    let mut command = Command::new("git");
    // No background maintenance: its transient `.git/objects/maintenance.lock` would show up in
    // a before/after listing of the checkout (seen on CI once).
    command.args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"]);
    command
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("TZ", "UTC");
    let (name, email) = who;
    let date = |days: u64| format!("@{} +0000", CUTOFF - days * DAY);
    command
        .env("GIT_AUTHOR_NAME", name)
        .env("GIT_AUTHOR_EMAIL", email)
        .env("GIT_COMMITTER_NAME", name)
        .env("GIT_COMMITTER_EMAIL", email)
        .env("GIT_AUTHOR_DATE", date(authored))
        .env("GIT_COMMITTER_DATE", date(committed));
    let output = command.args(args).output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn git(repo: &Path, args: &[&str]) -> String {
    git_as(repo, None, args)
}

/// The first 16 hex digits of SHA-256 of `bytes`.
fn hex16(bytes: &[u8]) -> String {
    review_core::hex::encode(&Sha256::digest(bytes))[..16].to_string()
}

/// A contributor's key under `identity = "name"`: the hash of the exact author-name bytes.
fn name_key(name: &str) -> String {
    hex16(name.as_bytes())
}

/// The `h:` identity of an author name that carries an email address, under either policy.
fn name_hash(name: &str) -> String {
    format!("h:{}", hex16(name.trim().to_lowercase().as_bytes()))
}

/// The contributor entry whose display name is `name`.
fn contributor<'a>(contributors: &'a Value, name: &str) -> &'a Value {
    contributors["by_identity"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == name)
        .unwrap_or_else(|| panic!("no contributor {name}: {contributors}"))
}

/// The display names of `contributors`, in ranked order.
fn names(contributors: &Value) -> Vec<String> {
    contributors["by_identity"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap().to_string())
        .collect()
}

/// The keys of `contributors`, in ranked order.
fn keys(contributors: &Value) -> Vec<String> {
    contributors["by_identity"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["key"].as_str().unwrap().to_string())
        .collect()
}

/// A repository named `name` below `root` holding `files`, committed by Ada a day before the
/// cutoff.
fn repository(root: &Path, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
    let repo = root.join(name);
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    for (path, content) in files {
        write(&repo, path, content);
    }
    commit(&repo, ADA, 1, &["Start"]);
    repo
}

fn write(repo: &Path, path: &str, content: &[u8]) {
    let path = repo.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

const ADA: (&str, &str) = ("Ada", "ada@fixture.invalid");
const BOB: (&str, &str) = ("Bob", "bob@fixture.invalid");
const BOT: (&str, &str) = ("renovate[bot]", "bot@fixture.invalid");
/// An author whose name is itself an email address.
const CAROL: (&str, &str) = ("carol@fixture.invalid", "carol@fixture.invalid");

fn commit(repo: &Path, who: (&str, &str), days: u64, message: &[&str]) {
    git(repo, &["add", "-A"]);
    let mut args = vec!["commit", "-q", "--allow-empty"];
    for paragraph in message {
        args.extend(["-m", paragraph]);
    }
    git_as(repo, Some((who.0, who.1, days)), &args);
}

/// The fixture repository of ADR-0158's test (a): eight commits over 500 days by two people, a
/// bot and an author named by an email address; a human commit with an agent trailer and one
/// with two; a merge; a rename and a binary change in a commit authored a year before it was
/// committed; two tags, two branches, several top-level directories, a tests directory, a
/// binary blob, a blob over `max_file_bytes = 512`, three ADRs and a README beside them, a
/// CONTEXT.md, a code policy with two checks, a review pipeline with one, and one workflow file.
/// It pins the walkthrough's disputed cases too: status words in prose before an ADR's status
/// line, a vocabulary written out of order, a toolchain file of the set beside one that is not,
/// and two configured language patterns that both match `src/core.rs`.
fn fixture(root: &Path) -> PathBuf {
    let repo = root.join("survey-fixture");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    write(
        &repo,
        ".af/af.toml",
        b"version = 1\n\n[project]\nname = \"survey-fixture\"\nmin_af = \"0.1\"\n\n[defaults]\n\
          pipeline = \"review\"\n\n[survey]\nmax_file_bytes = 512\n\n[survey.languages]\n\
          \"*.rs\" = \"rust\"\n\"src/**\" = \"source\"\n",
    );
    write(&repo, "README.md", b"# Fixture\n\nTwo more lines.\n");
    write(&repo, "src/lib.rs", b"pub fn one() -> u32 {\n    1\n}\n");
    write(&repo, "assets/logo.bin", b"PNG\0\x01\x02\nbinary\n");
    commit(&repo, ADA, 500, &["Start"]);

    write(
        &repo,
        "docs/adr/0001-first.md",
        b"# ADR-0001: First\n\nThis was proposed once and is superseded by nothing.\n\n\
          **Status:** accepted (2026-01-02)\n\nText.\n",
    );
    write(
        &repo,
        "CONTEXT.md",
        b"# Context\n\n**Survey**:\nOne record.\n\n**Project**:\nThe repository.\n",
    );
    write(
        &repo,
        "docs/big.txt",
        format!("{}\n", "x".repeat(599)).as_bytes(),
    );
    commit(&repo, BOB, 200, &["Docs"]);
    git(&repo, &["tag", "v1.0.0"]);

    git(&repo, &["checkout", "-q", "-b", "feature"]);
    write(&repo, "crates/x/tests/it.rs", b"#[test]\nfn it() {}\n");
    write(&repo, ".github/workflows/ci.yml", b"on: push\njobs: {}\n");
    commit(&repo, BOT, 60, &["Bump"]);

    git(&repo, &["checkout", "-q", "main"]);
    write(
        &repo,
        ".af/code-policy.toml",
        b"schema = \"af.code-task-policy/1\"\n\n[checks.test]\nname = \"test\"\n\n\
          [checks.test.command]\nprogram = \"cargo\"\n\n[[checks.test.command.args]]\n\
          value = \"test\"\nprovenance = \"literal\"\n\n[checks.lint]\nname = \"lint\"\n\n\
          [checks.lint.command]\nprogram = \"/usr/bin/make\"\n",
    );
    write(&repo, "Cargo.toml", b"[workspace]\n");
    write(
        &repo,
        "rust-toolchain.toml",
        b"[toolchain]\nchannel = \"1.88\"\n",
    );
    // A toolchain file of the set and one beside it that is not.
    write(&repo, ".tool-versions", b"rust 1.88\n");
    write(&repo, ".go-version", b"1.23\n");
    write(
        &repo,
        "src/lib.rs",
        b"pub fn one() -> u32 {\n    1\n}\n\npub fn two() -> u32 {\n    2\n}\n",
    );
    write(&repo, "notes.txt", b"temporary\n");
    commit(
        &repo,
        ADA,
        45,
        &[
            "Policy",
            "Co-Authored-By: Claude Opus <noreply@anthropic.com>",
        ],
    );

    git_as(
        &repo,
        Some((ADA.0, ADA.1, 10)),
        &["merge", "-q", "--no-ff", "-m", "Merge feature", "feature"],
    );

    write(
        &repo,
        "docs/adr/0002-second.md",
        b"# ADR-0002: Second\n\nStatus: Proposed on 2026-11-30.\n",
    );
    write(
        &repo,
        ".af/pipelines/review.toml",
        b"[[checks]]\nname = \"docs\"\nprogram = \"npx\"\nargs = [{ value = \"--yes\" }]\n",
    );
    std::fs::remove_file(repo.join("notes.txt")).unwrap();
    commit(&repo, BOB, 5, &["Second decision"]);
    git(&repo, &["tag", "nightly"]);

    // Authored a year before it was committed: counted by its committer date, 3 days ago. A
    // rename is a removal and an addition; a binary change is one file and no lines.
    git(&repo, &["mv", "src/lib.rs", "src/core.rs"]);
    write(
        &repo,
        "assets/logo.bin",
        b"PNG\0\x03\x04\nbinary, changed\n",
    );
    git(&repo, &["add", "-A"]);
    git_dated(
        &repo,
        ADA,
        368,
        3,
        &[
            "commit",
            "-q",
            "-m",
            "Rename and redraw",
            "-m",
            "Co-Authored-By: Claude Opus <noreply@anthropic.com>\n\
             Co-Authored-By: Codex <codex@fixture.invalid>",
        ],
    );

    write(
        &repo,
        "docs/adr/0003-contact.md",
        format!(
            "# {CONTACT}

Status: accepted on 2026-12-01.
"
        )
        .as_bytes(),
    );
    write(
        &repo,
        "docs/adr/README.md",
        b"# Decisions

An index.
",
    );
    commit(&repo, CAROL, 2, &["Contact"]);
    repo
}

/// Every byte af keeps on this machine, as text: the Project Store's log and CAS, the registry.
fn everything_kept(machine: &Machine) -> String {
    let mut out = String::new();
    let mut stack = vec![machine.state_home()];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push_str(&String::from_utf8_lossy(&std::fs::read(&path).unwrap()));
            }
        }
    }
    out
}

/// What `git status` and the checkout's own listing say: neither may change.
fn checkout_state(repo: &Path) -> (String, Vec<PathBuf>) {
    let mut names: Vec<PathBuf> = Vec::new();
    let mut stack = vec![repo.to_path_buf()];
    while let Some(directory) = stack.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path.clone());
            }
            names.push(path);
        }
    }
    names.sort();
    (git(repo, &["status", "--porcelain", "--ignored"]), names)
}

/// `refs_digest` as the Surveyor computes it: SHA-256 of the exact stdout of its one ref
/// observation.
fn refs_digest(repo: &Path) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["for-each-ref", REF_FORMAT, "refs/heads", "refs/tags"])
        .output()
        .unwrap();
    assert!(output.status.success());
    format!(
        "sha256:{}",
        review_core::hex::encode(&Sha256::digest(&output.stdout))
    )
}

/// The content digest of the head's committed capture, the Snapshot capture of every Task.
fn committed_digest(repo: &Path) -> String {
    let scratch = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(scratch.path().join("home")).unwrap();
    let source = review_source_git::Repo::open(repo, scratch.path().join("home"));
    let cas = review_store::Cas::open(scratch.path().join("cas")).unwrap();
    review_source_git::Capture::new(&source, &cas)
        .committed("HEAD")
        .unwrap()
        .content_digest
}

/// The running git's `git --version`, trimmed: what `identity.git_version` records.
fn git_version() -> String {
    let output = Command::new("git").arg("--version").output().unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// The checked-in record with this machine's git version in its one place.
fn expected() -> String {
    let version = serde_json::to_string(&git_version()).unwrap();
    let filled = EXPECTED.replace("\"GIT_VERSION\"", &version);
    assert_ne!(filled, EXPECTED, "the fixture names the git version once");
    filled
}

#[test]
fn the_fixture_repository_yields_the_checked_in_survey_byte_for_byte() {
    let machine = Machine::new();
    let repo = fixture(&machine.root());
    let before = checkout_state(&repo);
    assert_eq!(before.0, "", "the fixture is clean");
    let expected = expected();

    let (stdout, _) = machine.expect(&repo, &[], &["project", "refresh", "--json"], 0);
    assert_eq!(
        stdout, expected,
        "the Survey drifted from the checked-in record"
    );
    let (stdout, _) = machine.expect(&repo, &[], &["project", "survey", "--json"], 0);
    assert_eq!(stdout, expected);
    let survey: Value = serde_json::from_str(&stdout).unwrap();
    schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);

    // The record in the CAS is those bytes, and its ID is what the log names.
    let history = machine.json(&repo, &[], &["project", "history", "--json"]);
    schemas::valid(&schemas::validator("project-history-v2.json"), &history);
    let survey_id = history["surveys"][0]["survey_id"].as_str().unwrap();
    let hex = survey_id.trim_start_matches("sha256:");
    let object = machine
        .store()
        .join("cas/objects")
        .join(&hex[..2])
        .join(&hex[2..]);
    assert_eq!(
        std::fs::read_to_string(object).unwrap(),
        expected.trim_end()
    );

    // The totals the measurement policy states, so the expected record is not merely recorded.
    let tree = &survey["tree"];
    assert_eq!(tree["files"], "18");
    assert_eq!(
        tree["files_without_lines"], "2",
        "the binary and the big blob"
    );
    assert_eq!(tree["directories"]["."]["files"], "6");
    assert_eq!(tree["tests"]["files"], "1");
    // `*.rs` and `src/**` both match `src/core.rs`; the bytewise-first, `*.rs`, names it.
    assert_eq!(tree["languages"]["rust"]["lines"], "9");
    assert_eq!(tree["languages"]["rust"]["files"], "2");
    assert!(tree["languages"].get("source").is_none(), "{tree}");
    let history = &survey["history"];
    assert_eq!(history["completeness"], "complete");
    assert_eq!(history["commits"], "8", "the merge counts as a commit");
    assert_eq!(history["merges"], "1");
    // By committer date: the commit authored 368 days ago and committed 3 days ago is in the
    // 30-day window, and the 365-day window holds every commit but the first.
    assert_eq!(history["windows"]["30d"]["commits"], "4");
    assert_eq!(history["windows"]["30d"]["merges"], "1");
    assert_eq!(history["windows"]["90d"]["commits"], "6");
    assert_eq!(history["windows"]["365d"]["commits"], "7");
    assert_eq!(
        history["last_commit_unix_s"],
        (CUTOFF - 2 * DAY).to_string()
    );
    assert_eq!(history["tags"]["count"], "2");
    assert_eq!(
        history["tags"]["release"],
        serde_json::json!([{"name": "v1.0.0"}])
    );
    assert_eq!(history["local_branches"], "2");
    let hot = |path: &str| {
        history["hot_paths"]
            .as_array()
            .unwrap()
            .iter()
            .find(|hot| hot["path"] == path)
            .cloned()
            .unwrap_or_else(|| panic!("{path} is not a hot path: {history}"))
    };
    // A rename is a removal and an addition; the head no longer holds the old path.
    assert_eq!(hot("src/lib.rs")["commits"], "2");
    assert_eq!(hot("src/lib.rs")["removed"], true);
    assert_eq!(hot("src/core.rs")["commits"], "1");
    assert_eq!(hot("notes.txt")["removed"], true);
    // A binary change is one file touched and no lines.
    assert_eq!(hot("assets/logo.bin")["commits"], "1");
    assert_eq!(
        history["churn_90d"]["assets"],
        serde_json::json!({"commits": "1", "added": "0", "removed": "0"})
    );
    assert_eq!(
        history["churn_90d"]["src"],
        serde_json::json!({"commits": "2", "added": "11", "removed": "7"})
    );
    let contributors = &survey["contributors"];
    assert_eq!(contributors["identities"], "4");
    // The bot, the human commit with an agent trailer, and the one with two trailers, once.
    assert_eq!(contributors["agent_commits"], "3");
    assert_eq!(contributors["human_commits"], "5");
    assert_eq!(contributors["agent_share_bp"], "3750");
    assert_eq!(contributors["human_share_bp"], "6250");
    assert_eq!(contributors["top_share_bp"], "5000");
    // Ada authored both agent-assisted human commits and the merge: her count holds them.
    assert_eq!(contributor(contributors, "Ada")["commits"], "4");
    assert_eq!(contributor(contributors, "Ada")["key"], name_key("Ada"));
    assert_eq!(contributor(contributors, "Bob")["commits"], "2");
    let decisions = &survey["decisions"];
    assert_eq!(
        decisions["count"], "3",
        "docs/adr/README.md is not a decision"
    );
    // The status line decides; the status words of the prose before it name nothing.
    assert_eq!(decisions["by_status"]["accepted"], "2");
    assert_eq!(decisions["records"][0]["status"], "accepted");
    assert_eq!(decisions["by_status"]["proposed"], "1");
    assert!(decisions.get("newest").is_none());
    let numbers: Vec<&str> = decisions["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| record["number"].as_str().unwrap())
        .collect();
    assert_eq!(numbers, ["0001", "0002", "0003"]);
    assert_eq!(decisions["records"][2]["title"], CONTACT);
    assert_eq!(
        survey["vocabulary"]["terms"],
        serde_json::json!([{"name": "Project"}, {"name": "Survey"}]),
        "by name, as written out of order"
    );
    let declared = survey["checks"]["declared"].as_array().unwrap();
    assert_eq!(declared.len(), 3);
    assert!(declared.iter().any(|check| check["program"] == "make"));
    assert_eq!(declared[0]["source"], "code-policy");
    assert_eq!(declared[2]["source"], "review.toml");
    assert_eq!(survey["checks"]["ci_workflows"], "1");
    assert_eq!(
        survey["checks"]["toolchain"],
        serde_json::json!([".tool-versions", "rust-toolchain.toml"]),
        "Cargo.toml and .go-version are no toolchain file of the set"
    );
    let identity = &survey["identity"];
    assert_eq!(identity["name"], "survey-fixture");
    assert_eq!(identity["head"], git(&repo, &["rev-parse", "HEAD"]));
    assert_eq!(identity["cutoff_unix_s"], CUTOFF.to_string());
    assert_eq!(identity["surveyor_version"], "8");
    assert_eq!(identity["git_version"], git_version().as_str());
    assert_eq!(identity["refs_digest"], refs_digest(&repo));
    // The Tree Digest is the committed capture's, from the digest-only path: the blob over
    // max_file_bytes included.
    assert_eq!(identity["tree_digest"], committed_digest(&repo));

    assert_eq!(
        checkout_state(&repo),
        before,
        "nothing was written in the checkout"
    );
}

#[test]
fn the_max_commits_boundary_cuts_the_walk_in_date_order() {
    let machine = Machine::new();
    let repo = fixture(&machine.root());
    machine.expect(
        &repo,
        &[("AF_SURVEY__MAX_COMMITS", "8")],
        &["project", "refresh"],
        0,
    );
    let whole = machine.json(&repo, &[], &["project", "survey", "--json"]);
    assert_eq!(whole["history"]["completeness"], "complete");
    assert!(whole["history"].get("oldest_reached").is_none());

    machine.expect(
        &repo,
        &[("AF_SURVEY__MAX_COMMITS", "7")],
        &["project", "refresh"],
        0,
    );
    let cut = machine.json(&repo, &[], &["project", "survey", "--json"]);
    schemas::valid(&schemas::validator("project-survey-v2.json"), &cut);
    for section in ["history", "contributors"] {
        assert_eq!(cut[section]["completeness"], "partial", "{section}");
        assert_eq!(cut[section]["reason"], "max_commits", "{section}");
    }
    assert_eq!(cut["history"]["commits"], "7");
    assert_eq!(cut["history"]["reachable_commits"], "8");
    // Newest first by committer date: the seventh is the commit v1.0.0 tags.
    assert_eq!(
        cut["history"]["oldest_reached"],
        git(&repo, &["rev-parse", "v1.0.0^{commit}"])
    );
    assert_eq!(cut["history"]["windows"], whole["history"]["windows"]);
    assert_eq!(cut["contributors"]["agent_commits"], "3");
    assert_eq!(cut["contributors"]["human_commits"], "4");
}

#[test]
fn an_unchanged_survey_key_records_nothing_and_prints_the_key() {
    let machine = Machine::new();
    let repo = fixture(&machine.root());
    let (first, _) = machine.expect(&repo, &[], &["project", "refresh"], 0);
    assert!(first.starts_with("recorded\nSurvey sha256:"), "{first}");
    let events = machine.events();
    assert_eq!(machine.surveys(), 1, "{events:?}");

    let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
    let identity = &survey["identity"];
    let hex16 = |digest: &Value| digest.as_str().unwrap()["sha256:".len()..][..16].to_string();
    let key = format!(
        "unchanged: head {}, refs {}, policy {}, surveyor 8",
        &identity["head"].as_str().unwrap()[..12],
        hex16(&identity["refs_digest"]),
        hex16(&identity["policy_digest"])
    );
    let (again, _) = machine.expect(&repo, &[], &["project", "refresh"], 0);
    assert_eq!(again.lines().next(), Some(key.as_str()), "{again}");
    assert_eq!(
        again.lines().nth(1),
        first.lines().nth(1),
        "the newest Survey"
    );
    let (json, stderr) = machine.expect(&repo, &[], &["project", "refresh", "--json"], 0);
    assert_eq!(stderr.trim(), key);
    assert_eq!(
        json,
        machine
            .expect(&repo, &[], &["project", "survey", "--json"], 0)
            .0
    );
    assert_eq!(machine.events(), events, "no event was recorded");

    let history = machine.json(&repo, &[], &["project", "history", "--json"]);
    let first_id = history["surveys"][0]["survey_id"]
        .as_str()
        .unwrap()
        .to_string();

    write(&repo, "src/more.rs", b"pub fn three() {}\n");
    commit(&repo, ADA, 1, &["More"]);
    let (second, _) = machine.expect(&repo, &[], &["project", "refresh"], 0);
    assert!(second.starts_with("recorded\n"), "{second}");
    assert_eq!(machine.surveys(), 2);
    let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
    assert_eq!(survey["previous_survey_id"], first_id.as_str());
    let history = machine.json(&repo, &[], &["project", "history", "--json"]);
    let chain: Vec<&str> = history["surveys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["survey_id"].as_str().unwrap())
        .collect();
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[1], first_id, "newest first");
    let (text, _) = machine.expect(&repo, &[], &["project", "history"], 0);
    assert!(text.starts_with("2 Surveys\n"), "{text}");
}

#[test]
fn a_new_tag_a_new_branch_a_partial_survey_and_force_each_record_a_survey() {
    let machine = Machine::new();
    let repo = fixture(&machine.root());
    let head = git(&repo, &["rev-parse", "HEAD"]);
    machine.expect(&repo, &[], &["project", "refresh"], 0);
    assert_eq!(machine.surveys(), 1);
    let newest = |machine: &Machine| machine.json(&repo, &[], &["project", "survey", "--json"]);
    let first = newest(&machine);

    // A new tag at the same head.
    git(&repo, &["tag", "v2.0.0"]);
    let (stdout, _) = machine.expect(&repo, &[], &["project", "refresh"], 0);
    assert!(stdout.starts_with("recorded\n"), "{stdout}");
    assert_eq!(machine.surveys(), 2);
    let tagged = newest(&machine);
    assert_eq!(tagged["identity"]["head"], head.as_str());
    assert_ne!(
        tagged["identity"]["refs_digest"],
        first["identity"]["refs_digest"]
    );
    assert_eq!(tagged["history"]["tags"]["count"], "3");

    // A new local branch at the same head.
    git(&repo, &["branch", "extra"]);
    let (stdout, _) = machine.expect(&repo, &[], &["project", "refresh"], 0);
    assert!(stdout.starts_with("recorded\n"), "{stdout}");
    assert_eq!(machine.surveys(), 3);
    assert_eq!(newest(&machine)["history"]["local_branches"], "3");

    // A partial newest Survey is measured again under the same key.
    let cut = [("AF_SURVEY__MAX_COMMITS", "1")];
    machine.expect(&repo, &cut, &["project", "refresh"], 0);
    assert_eq!(machine.surveys(), 4);
    assert_eq!(newest(&machine)["completeness"], "partial");
    let (stdout, _) = machine.expect(&repo, &cut, &["project", "refresh"], 0);
    assert!(stdout.starts_with("recorded\n"), "{stdout}");
    assert_eq!(machine.surveys(), 5);

    // A complete one is reused, unless --force asks; the forced one chains to it.
    machine.expect(&repo, &[], &["project", "refresh"], 0);
    assert_eq!(machine.surveys(), 6);
    let (stdout, _) = machine.expect(&repo, &[], &["project", "refresh"], 0);
    assert!(stdout.starts_with("unchanged: "), "{stdout}");
    assert_eq!(machine.surveys(), 6);
    let history = machine.json(&repo, &[], &["project", "history", "--json"]);
    let reused = history["surveys"][0]["survey_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (stdout, _) = machine.expect(&repo, &[], &["project", "refresh", "--force"], 0);
    assert!(stdout.starts_with("recorded\n"), "{stdout}");
    assert_eq!(machine.surveys(), 7);
    assert_eq!(newest(&machine)["previous_survey_id"], reused.as_str());
}

#[test]
fn max_wall_ms_records_a_partial_survey_without_a_partial_tree_digest() {
    let machine = Machine::new();
    let repo = fixture(&machine.root());
    // The deadline runs from before `rev-parse`: one millisecond computes no survey key.
    let (_, stderr) = machine.expect(
        &repo,
        &[("AF_SURVEY__MAX_WALL_MS", "1")],
        &["project", "refresh"],
        1,
    );
    assert!(
        stderr.contains("the survey key could not be computed within max_wall_ms"),
        "{stderr}"
    );
    assert_eq!(machine.surveys(), 0);
    // A deadline that passes inside `identity`, its one slowed section, once the key exists.
    let (stdout, stderr) = machine.expect(
        &repo,
        &[
            ("AF_SURVEY__MAX_WALL_MS", "1500"),
            ("AF_TEST_SURVEY_SLOW_ITEM_US", "2000000"),
            ("AF_TEST_SURVEY_SLOW_SECTION", "identity"),
        ],
        &["project", "refresh"],
        0,
    );
    assert!(stdout.starts_with("recorded\n"), "{stdout}\n{stderr}");
    assert_eq!(
        machine.surveys(),
        1,
        "a Survey cut by its deadline is recorded"
    );
    let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
    schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);
    assert_eq!(survey["completeness"], "partial");
    let identity = &survey["identity"];
    assert_eq!(identity["completeness"], "partial");
    assert_eq!(identity["reason"], "max_wall_ms");
    assert!(identity.get("tree_digest").is_none(), "{identity}");
    assert_eq!(identity["tree_digest_reason"], "deadline");
    assert_eq!(identity["head"], git(&repo, &["rev-parse", "HEAD"]));
    for section in [
        "tree",
        "history",
        "contributors",
        "decisions",
        "vocabulary",
        "checks",
    ] {
        assert_eq!(
            survey[section],
            serde_json::json!({"completeness": "unavailable", "reason": "max_wall_ms"}),
            "{section}"
        );
    }
}

#[test]
fn a_slow_tree_stops_at_the_deadline_inside_its_section() {
    let machine = Machine::new();
    let repo = machine.root().join("wide");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    for index in 0..2000 {
        write(&repo, &format!("f/{index:04}.txt"), b"x\n");
    }
    commit(&repo, ADA, 1, &["Wide"]);
    // Register the Project first, so the refresh below times only the Surveyor.
    machine.expect(&repo, &[], &["project", "show"], 0);
    let max_wall_ms: u64 = 1500;
    let started = std::time::Instant::now();
    machine.expect(
        &repo,
        &[
            ("AF_SURVEY__MAX_WALL_MS", &max_wall_ms.to_string()),
            ("AF_TEST_SURVEY_SLOW_ITEM_US", "1000"),
        ],
        &["project", "refresh"],
        0,
    );
    let took = started.elapsed();
    assert!(
        took < std::time::Duration::from_millis(max_wall_ms + 2000),
        "the refresh took {took:?}"
    );
    let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
    schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);
    let tree = &survey["tree"];
    assert_eq!(tree["completeness"], "partial", "{tree}");
    assert_eq!(tree["reason"], "max_wall_ms");
    let reached: u64 = tree["files"].as_str().unwrap().parse().unwrap();
    assert!(
        reached > 0 && reached < 2000,
        "the listing stopped at {reached}"
    );
    for section in [
        "history",
        "contributors",
        "decisions",
        "vocabulary",
        "checks",
    ] {
        assert_eq!(survey[section]["completeness"], "unavailable", "{section}");
        assert_eq!(survey[section]["reason"], "max_wall_ms", "{section}");
    }
}

#[test]
fn the_fixed_maxima_cut_with_their_reasons_and_identity_strings_stay_exact() {
    let machine = Machine::new();
    let repo = fixture(&machine.root());

    // More tree entries than max_paths.
    machine.expect(
        &repo,
        &[("AF_SURVEY__MAX_PATHS", "3")],
        &["project", "refresh"],
        0,
    );
    let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
    schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);
    assert_eq!(survey["tree"]["completeness"], "partial");
    assert_eq!(survey["tree"]["reason"], "max_paths");
    assert_eq!(survey["tree"]["files"], "3");
    assert_eq!(survey["tree"]["seen"]["paths"], "18");
    assert_eq!(
        survey["decisions"]["count"], "3",
        "the decisions beyond max_paths are still read"
    );

    // A commit touching more paths than max_entries, and every other list.
    machine.expect(
        &repo,
        &[("AF_SURVEY__MAX_ENTRIES", "2")],
        &["project", "refresh"],
        0,
    );
    let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
    schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);
    let history = &survey["history"];
    assert_eq!(history["reason"], "max_entries");
    assert_eq!(history["hot_paths"].as_array().unwrap().len(), 2);
    assert_eq!(history["churn_90d"].as_object().unwrap().len(), 2);
    assert_eq!(history["seen"]["churn_90d"], "7");
    assert_eq!(history["seen"]["hot_paths"], "15");
    assert_eq!(survey["tree"]["reason"], "max_entries");
    assert_eq!(survey["tree"]["seen"]["directories"], "7");
    assert_eq!(survey["contributors"]["seen"]["by_identity"], "4");
    assert_eq!(survey["contributors"]["identities"], "4");
    assert_eq!(survey["decisions"]["seen"]["records"], "3");
    assert_eq!(survey["decisions"]["records"].as_array().unwrap().len(), 2);
    assert_eq!(survey["checks"]["seen"]["declared"], "3");
    for section in ["tree", "history", "contributors", "decisions", "checks"] {
        assert_eq!(survey[section]["completeness"], "partial", "{section}");
    }

    // A record over max_survey_bytes records SurveyFailed@1 with the bytes it needed, and
    // nothing else.
    let surveys = machine.surveys();
    let (_, stderr) = machine.expect(
        &repo,
        &[("AF_SURVEY__MAX_SURVEY_BYTES", "1024")],
        &["project", "refresh", "--force"],
        1,
    );
    assert!(
        stderr.contains("more than [survey] max_survey_bytes = 1024"),
        "{stderr}"
    );
    assert_eq!(machine.events().last().unwrap(), "SurveyFailed@1");
    assert_eq!(machine.surveys(), surveys);
    let (shown, _) = machine.expect(&repo, &[], &["project", "show"], 0);
    assert!(shown.contains("the Survey needs "), "{shown}");
}

#[test]
fn a_long_author_name_is_cut_and_marked_and_seven_roots_stay_exact() {
    let machine = Machine::new();
    let repo = machine.root().join("roots");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    let long = format!("Ada {}", "x".repeat(296));
    assert_eq!(long.len(), 300);
    // A second author whose name shares the first 256 bytes of the first one's.
    let twin = format!("Ada {}y", "x".repeat(295));
    assert_eq!(&twin[..256], &long[..256]);
    write(&repo, "r0.txt", b"0\n");
    commit(
        &repo,
        (long.as_str(), "long@fixture.invalid"),
        9,
        &["Root 0"],
    );
    write(&repo, "r0b.txt", b"0\n");
    commit(
        &repo,
        (twin.as_str(), "twin@fixture.invalid"),
        9,
        &["Root 0b"],
    );
    for root in 1..7 {
        git(
            &repo,
            &["checkout", "-q", "--orphan", &format!("root-{root}")],
        );
        git(&repo, &["rm", "-rq", "--cached", "."]);
        for stale in std::fs::read_dir(&repo).unwrap() {
            let path = stale.unwrap().path();
            if path.file_name().unwrap() != ".git" {
                std::fs::remove_file(path).unwrap();
            }
        }
        write(
            &repo,
            &format!("r{root}.txt"),
            format!("{root}\n").as_bytes(),
        );
        commit(&repo, BOB, 9 - root, &[&format!("Root {root}")]);
    }
    git(&repo, &["checkout", "-q", "main"]);
    for root in 1..7 {
        git_as(
            &repo,
            Some((ADA.0, ADA.1, 1)),
            &[
                "merge",
                "-q",
                "--allow-unrelated-histories",
                "-m",
                &format!("Join root {root}"),
                &format!("root-{root}"),
            ],
        );
    }
    let mut roots: Vec<String> = git(&repo, &["rev-list", "--max-parents=0", "HEAD"])
        .lines()
        .map(str::to_string)
        .collect();
    roots.sort();
    let repository_id = roots.join(",");
    assert_eq!(roots.len(), 7);
    assert_eq!(repository_id.len(), 286);

    machine.expect(&repo, &[], &["project", "refresh"], 0);
    let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
    schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);
    assert_eq!(survey["identity"]["repository_id"], repository_id.as_str());
    let contributors = &survey["contributors"];
    let cut: Vec<&Value> = contributors["by_identity"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["name"].as_str().unwrap().starts_with("Ada x"))
        .collect();
    // Two identities, by key, with equal display text.
    assert_eq!(cut.len(), 2, "{contributors}");
    assert_eq!(cut[0]["name"], cut[1]["name"]);
    let name = cut[0]["name"].as_str().unwrap();
    assert_eq!(name.len(), 256);
    assert!(long.starts_with(name));
    let mut cut_keys: Vec<&str> = cut
        .iter()
        .map(|entry| entry["key"].as_str().unwrap())
        .collect();
    cut_keys.sort();
    let mut expected = vec![name_key(&long), name_key(&twin)];
    expected.sort();
    assert_eq!(cut_keys, expected);
    for entry in &cut {
        assert_eq!(entry["truncated"], true);
    }
    assert!(contributor(contributors, "Ada").get("truncated").is_none());
    assert_eq!(contributors["identities"], "4");
}

#[test]
fn an_oversized_vocabulary_decision_or_declaration_leaves_its_section_partial() {
    let machine = Machine::new();
    let repo = machine.root().join("oversized");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    let big = format!("{}\n", "y".repeat(200));
    write(
        &repo,
        "CONTEXT.md",
        format!("**Term**:\nA definition.\n{big}").as_bytes(),
    );
    write(
        &repo,
        "docs/adr/0001-big.md",
        format!("# ADR-0001: Big\n\nStatus: accepted\n{big}").as_bytes(),
    );
    write(&repo, "docs/adr/0002-binary.md", b"# ADR-0002\n\0binary\n");
    write(
        &repo,
        "docs/adr/0003-small.md",
        b"# ADR-0003: Small\n\nAccepted.\n",
    );
    write(
        &repo,
        ".af/pipelines/review.toml",
        format!("[[checks]]\nname = \"docs\"\nprogram = \"npx\"\n# {big}").as_bytes(),
    );
    write(
        &repo,
        ".af/pipelines/other.toml",
        b"[[checks]]\nname = \"x\"\nprogram = \"y\"\n",
    );
    commit(&repo, ADA, 1, &["Oversized"]);
    let env = [("AF_SURVEY__MAX_FILE_BYTES", "128")];
    machine.expect(&repo, &env, &["project", "refresh"], 0);
    let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
    schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);

    let vocabulary = &survey["vocabulary"];
    assert_eq!(vocabulary["completeness"], "partial", "{vocabulary}");
    assert_eq!(vocabulary["reason"], "max_file_bytes");
    assert_eq!(vocabulary["present"], true);
    assert!(vocabulary.get("terms").is_none() && vocabulary.get("count").is_none());

    let decisions = &survey["decisions"];
    assert_eq!(decisions["completeness"], "partial", "{decisions}");
    assert_eq!(decisions["reason"], "max_file_bytes");
    assert_eq!(decisions["count"], "3");
    assert_eq!(decisions["by_status"]["unreadable"], "2");
    for unread in [0, 1] {
        let record = &decisions["records"][unread];
        assert_eq!(record["status"], "unreadable", "{record}");
        assert!(record.get("title").is_none(), "{record}");
    }
    assert_eq!(decisions["records"][2]["title"], "ADR-0003: Small");

    let checks = &survey["checks"];
    assert_eq!(checks["completeness"], "partial", "{checks}");
    assert_eq!(checks["reason"], "max_file_bytes");
    assert_eq!(
        checks["unreadable"],
        serde_json::json!([".af/pipelines/review.toml"])
    );
    assert_eq!(checks["declared"].as_array().unwrap().len(), 1);
}

#[test]
fn no_author_email_reaches_a_record_an_event_a_document_or_an_output_under_either_identity() {
    for identity in ["name", "email_hash"] {
        let machine = Machine::new();
        let repo = fixture(&machine.root());
        let env = [("AF_SURVEY__IDENTITY", identity)];
        let mut said = String::new();
        for args in [
            &["project", "refresh"][..],
            &["project", "refresh", "--json"],
            &["project", "survey"],
            &["project", "survey", "--json"],
            &["project", "history"],
            &["project", "history", "--json"],
            &["project", "show"],
            &["project", "show", "--json"],
        ] {
            let (stdout, stderr) = machine.expect(&repo, &env, args, 0);
            said.push_str(&stdout);
            said.push_str(&stderr);
        }
        let survey = machine.json(&repo, &env, &["project", "survey", "--json"]);
        // Repository text is recorded as committed, an address in an ADR's title included;
        // that is the only place the fixture's addresses may appear.
        assert_eq!(survey["decisions"]["records"][2]["title"], CONTACT);
        let said = said.replace(CONTACT, "");
        let kept = everything_kept(&machine).replace(CONTACT, "");
        for email in EMAILS {
            assert!(!said.contains(email), "{identity}: {email} was printed");
            assert!(!kept.contains(email), "{identity}: {email} was recorded");
            assert!(!kept.contains(&email.to_uppercase()), "{identity}: {email}");
        }
        let contributors = &survey["contributors"];
        assert_eq!(contributors["identity"], identity);
        let rule = |text: &str| hex16(text.trim().to_lowercase().as_bytes());
        // Ranked by commits, then lines added: Ada 4, Bob 2, the address 1 with 6 lines, the
        // bot 1 with 4. The author named by an address is `h:` and the hash of the name under
        // both policies, never of the email.
        let carol = name_hash(CAROL.0);
        if identity == "email_hash" {
            assert_eq!(
                keys(contributors),
                [rule(ADA.1), rule(BOB.1), carol.clone(), rule(BOT.1)]
            );
            assert!(
                contributors["by_identity"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|entry| entry.get("name").is_none()),
                "email_hash names no one"
            );
        } else {
            assert_eq!(
                names(contributors),
                ["Ada", "Bob", carol.as_str(), "renovate[bot]"]
            );
            assert_eq!(
                keys(contributors),
                [
                    name_key("Ada"),
                    name_key("Bob"),
                    carol.clone(),
                    name_key("renovate[bot]")
                ]
            );
        }
    }
}

#[test]
fn onboarding_records_the_first_survey_once_and_its_exit_code_is_its_own() {
    let machine = Machine::new();
    let repo = machine.root().join("fresh");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    write(&repo, "README.md", b"# Fresh\n");
    commit(&repo, ADA, 3, &["Start"]);
    let repo_arg = repo.to_str().unwrap().to_string();
    let onboard = [
        "onboard",
        "--repo",
        repo_arg.as_str(),
        "--gate",
        "check=make check",
    ];
    // Without --apply nothing is recorded, not even the Project.
    machine.expect(&repo, &[], &onboard, 0);
    assert!(!machine.state_home().join("af/project").exists());

    let mut apply = onboard.to_vec();
    apply.push("--apply");
    let (stdout, stderr) = machine.expect(&repo, &[], &apply, 0);
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("af onboard: recorded Survey sha256:")),
        "{stdout}\n{stderr}"
    );
    assert_eq!(machine.surveys(), 1);
    let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
    assert_eq!(
        survey["identity"]["head"],
        git(&repo, &["rev-parse", "HEAD"])
    );

    // A second --apply refuses the existing `.af/` with onboarding's own exit code and, with an
    // unchanged survey key, records no second Survey.
    let events = machine.events();
    let (stdout, stderr) = machine.expect(&repo, &[], &apply, 1);
    assert!(stderr.contains("`.af/` already exists"), "{stderr}");
    assert!(stdout.contains("af onboard: unchanged: head "), "{stdout}");
    assert_eq!(machine.events(), events, "nothing was recorded");
    assert_eq!(machine.surveys(), 1);
}

#[test]
fn survey_without_a_recorded_one_fails_and_an_unknown_policy_key_records_the_failure() {
    let machine = Machine::new();
    let repo = fixture(&machine.root());
    let (_, stderr) = machine.expect(&repo, &[], &["project", "survey"], 1);
    assert!(stderr.contains("af project refresh"), "{stderr}");
    assert_eq!(stderr.trim().lines().count(), 1, "{stderr}");

    // `refresh` is not a key of this release's [survey] table.
    let (_, stderr) = machine.expect(
        &repo,
        &[("AF_SURVEY__REFRESH", "manual")],
        &["project", "refresh"],
        1,
    );
    assert!(stderr.contains("unknown field `refresh`"), "{stderr}");
    assert_eq!(machine.events().last().unwrap(), "SurveyFailed@1");
    let (shown, _) = machine.expect(&repo, &[], &["project", "show"], 0);
    assert!(shown.contains("Survey failed at "), "{shown}");
    assert!(shown.contains("unknown field `refresh`"), "{shown}");
    let document = machine.json(&repo, &[], &["project", "show", "--json"]);
    schemas::valid(&schemas::validator("project-v1.json"), &document);
    assert!(
        document.get("survey").is_none(),
        "af/project@1 is unchanged"
    );

    machine.expect(&repo, &[], &["project", "refresh"], 0);
    let (shown, _) = machine.expect(&repo, &[], &["project", "show"], 0);
    assert!(!shown.contains("Survey failed"), "{shown}");
    assert!(shown.contains("\nSurvey sha256:"), "{shown}");
}

/// `af` as `Machine::af` with the fake git of `bin` first on `PATH`.
fn af_with_git(
    machine: &Machine,
    repo: &Path,
    bin: &Path,
    env: &[(&str, &str)],
    args: &[&str],
) -> Output {
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut command = crate::common::af();
    command
        .current_dir(repo)
        .env("XDG_STATE_HOME", machine.state_home())
        .env("AF_SELF_OFFLINE", "1")
        .env("NO_COLOR", "1")
        .env("AF_TEST_SURVEY_CUTOFF_UNIX_S", CUTOFF.to_string())
        .env("PATH", path);
    for (name, value) in env {
        command.env(name, value);
    }
    command.args(args).output().unwrap()
}

#[test]
fn a_stalled_for_each_ref_fails_the_refresh_within_max_wall_ms() {
    use std::os::unix::fs::PermissionsExt;
    let machine = Machine::new();
    let repo = repository(&machine.root(), "stalled", &[("README.md", b"# Stalled\n")]);
    machine.expect(&repo, &[], &["project", "show"], 0);
    // A git that passes every command to the real one but the survey key's `for-each-ref`,
    // which never answers.
    let real = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let real = String::from_utf8(real.stdout).unwrap().trim().to_string();
    let bin = machine.root().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let fake = bin.join("git");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\n[ \"$1\" = warm ] && exit 0\nfor argument in \"$@\"; do\n  [ \
             \"$argument\" = '{REF_FORMAT}' ] && exec sleep 120\ndone\n\
             exec '{real}' \"$@\"\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    // The first exec of a script just written can be slow (macOS scans it): start it once.
    assert!(Command::new(&fake).arg("warm").status().unwrap().success());
    let max_wall_ms: u64 = 1500;
    let started = std::time::Instant::now();
    let output = af_with_git(
        &machine,
        &repo,
        &bin,
        &[("AF_SURVEY__MAX_WALL_MS", &max_wall_ms.to_string())],
        &["project", "refresh"],
    );
    let took = started.elapsed();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        took < std::time::Duration::from_millis(max_wall_ms + 2000),
        "the refresh took {took:?}"
    );
    let reason = "the survey key could not be computed within max_wall_ms";
    assert!(stderr.contains(reason), "{stderr}");
    assert_eq!(machine.events().last().unwrap(), "SurveyFailed@1");
    assert_eq!(machine.surveys(), 0);
    let (shown, _) = machine.expect(&repo, &[], &["project", "show"], 0);
    assert!(shown.contains(reason), "{shown}");
}

/// A repository of `size` decision records, vocabulary terms, code-policy checks and commits
/// by distinct authors, written with one `git fast-import`.
fn wide_repository(root: &Path, size: usize) -> PathBuf {
    let repo = root.join("wide-sections");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    let mut stream = Vec::new();
    let blob = |stream: &mut Vec<u8>, path: &str, content: &str| {
        stream.extend(format!("M 644 inline {path}\ndata {}\n{content}\n", content.len()).bytes());
    };
    for index in 0..size {
        let time = CUTOFF - (2 * size - index) as u64 * 3600;
        let who = format!("Author {index:03} <author{index}@fixture.invalid> {time} +0000");
        let message = format!("Commit {index}");
        stream.extend(
            format!(
                "commit refs/heads/main\nmark :{}\nauthor {who}\ncommitter {who}\ndata {}\n{message}\n",
                index + 1,
                message.len()
            )
            .bytes(),
        );
        if index > 0 {
            stream.extend(format!("from :{index}\n").bytes());
        } else {
            for adr in 0..size {
                blob(
                    &mut stream,
                    &format!("docs/adr/{:04}-x.md", adr + 1),
                    &format!("# ADR {adr}\n\nStatus: accepted\n"),
                );
            }
            let terms: String = (0..size)
                .map(|term| format!("**Term {term:03}**:\nA definition.\n"))
                .collect();
            blob(&mut stream, "CONTEXT.md", &terms);
            let checks: String = (0..size)
                .map(|check| format!("[checks.c{check:03}.command]\nprogram = \"make\"\n"))
                .collect();
            blob(&mut stream, ".af/code-policy.toml", &checks);
        }
        blob(&mut stream, "log.txt", &format!("{index}\n"));
    }
    let mut child = Command::new("git")
        .current_dir(&repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["fast-import", "--quiet"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write as _;
    child.stdin.take().unwrap().write_all(&stream).unwrap();
    assert!(child.wait().unwrap().success());
    repo
}

#[test]
fn a_deadline_inside_any_sections_post_processing_leaves_it_partial_and_the_rest_unavailable() {
    const SECTIONS: [&str; 7] = [
        "identity",
        "tree",
        "history",
        "contributors",
        "decisions",
        "vocabulary",
        "checks",
    ];
    let size = 120;
    let root = tempfile::tempdir().unwrap();
    let repo = wide_repository(root.path(), size);
    let max_wall_ms: u64 = 4000;
    // Each slowed section on its own machine, at once: every item of that section alone, an
    // aggregated entry, a ranked identity, a declaration, a line of terms or a declared check,
    // costs a tenth of a second, so the deadline falls inside it.
    let runs: Vec<_> = ["tree", "contributors", "decisions", "vocabulary", "checks"]
        .into_iter()
        .map(|slowed| {
            let repo = repo.clone();
            std::thread::spawn(move || {
                let machine = Machine::new();
                machine.expect(&repo, &[], &["project", "show"], 0);
                let started = std::time::Instant::now();
                machine.expect(
                    &repo,
                    &[
                        ("AF_SURVEY__MAX_WALL_MS", &max_wall_ms.to_string()),
                        ("AF_TEST_SURVEY_SLOW_ITEM_US", "100000"),
                        ("AF_TEST_SURVEY_SLOW_SECTION", slowed),
                    ],
                    &["project", "refresh"],
                    0,
                );
                let took = started.elapsed();
                let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
                (slowed, took, survey)
            })
        })
        .collect();
    for run in runs {
        let (slowed, took, survey) = run.join().unwrap();
        assert!(
            took < std::time::Duration::from_millis(max_wall_ms + 2000),
            "{slowed}: the refresh took {took:?}"
        );
        schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);
        let at = SECTIONS
            .iter()
            .position(|section| *section == slowed)
            .unwrap();
        for (index, section) in SECTIONS.iter().enumerate() {
            let measured = &survey[*section];
            let expected = match index.cmp(&at) {
                std::cmp::Ordering::Less => "complete",
                std::cmp::Ordering::Equal => "partial",
                std::cmp::Ordering::Greater => "unavailable",
            };
            assert_eq!(
                measured["completeness"], expected,
                "{slowed}: {section}: {measured}"
            );
            if index >= at {
                assert_eq!(measured["reason"], "max_wall_ms", "{slowed}: {section}");
            }
        }
        let count = |value: &Value| value.as_str().unwrap().parse::<usize>().unwrap();
        let section = &survey[slowed];
        let reached = match slowed {
            "tree" => count(&section["files"]),
            "contributors" => {
                assert_eq!(count(&section["seen"]["by_identity"]), size);
                assert_eq!(count(&section["identities"]), size, "the totals stay whole");
                section["by_identity"].as_array().unwrap().len()
            }
            "decisions" => count(&section["count"]),
            "vocabulary" => count(&section["count"]),
            _ => section["declared"].as_array().unwrap().len(),
        };
        assert!(reached < size, "{slowed} reached {reached} of {size}");
    }
}

#[test]
fn the_status_grammar_shares_ranking_checks_discovery_and_languages_are_stated() {
    let machine = Machine::new();
    let repo = machine.root().join("rules");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    let files: &[(&str, &[u8])] = &[
        (
            ".af/af.toml",
            b"version = 1\n\n[survey.languages]\n\"**/*.rs\" = \"rust-any\"\n\
              \"generated/**\" = \"generated\"\n\"*.md\" = \"docs\"\n",
        ),
        (
            "docs/adr/0001-two-dates.md",
            b"# Two dates\n\n**Status:** accepted 2026-01-02, reviewed 2026-03-04\n",
        ),
        (
            "docs/adr/0002-partial.md",
            b"# Partial\n\nStatus: accepted; superseded in part by ADR-0003, amended 2026-05-06\n",
        ),
        (
            "docs/adr/0003-undated.md",
            b"# Undated\n\nStatus: proposed\n",
        ),
        (
            "docs/adr/0004-none.md",
            b"# No status line\n\nThis was accepted on 2026-01-02.\n",
        ),
        (
            ".af/code-policy.toml",
            b"[checks.test.command]\nprogram = \"cargo\"\n",
        ),
        (
            ".af/pipelines/review.toml",
            b"[[checks]]\nname = \"test\"\nprogram = \"cargo\"\n",
        ),
        // A pipeline nothing routes to, declaring a check of the same name.
        (
            ".af/pipelines/unused.toml",
            b"[[checks]]\nname = \"test\"\nprogram = \"make\"\n\n[[checks]]\nname = \"lint\"\n\
              program = \"npx\"\n",
        ),
        ("rust-toolchain", b"stable\n"),
        (".nvmrc", b"22\n"),
        ("Cargo.toml", b"[workspace]\n"),
        ("sub/.python-version", b"3.12\n"),
        ("generated/x.rs", b"fn x() {}\n"),
        ("generated/notes.txt", b"notes\n"),
        ("src/lib.rs", b"pub fn lib() {}\n"),
        ("README.md", b"# Rules\n"),
        ("data.xyz", b"data\n"),
    ];
    for (path, content) in files {
        write(&repo, path, content);
    }
    commit(&repo, BOB, 30, &["Start"]);
    // Ada: more commits and fewer lines than Bob; one of the three commits is agent-assisted.
    write(&repo, "a.txt", b"a\n");
    commit(&repo, ADA, 20, &["One line"]);
    write(&repo, "b.txt", b"b\n");
    commit(
        &repo,
        ADA,
        10,
        &[
            "One more",
            "Co-Authored-By: Claude Opus <noreply@anthropic.com>",
        ],
    );
    machine.expect(&repo, &[], &["project", "refresh"], 0);
    let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
    schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);

    // Status and date from the first status line; `amended` a flag; unknown when absent.
    let decisions = &survey["decisions"];
    assert_eq!(decisions["completeness"], "complete");
    let records = decisions["records"].as_array().unwrap();
    let stated: Vec<(&str, Option<&str>, Option<&str>, bool)> = records
        .iter()
        .map(|record| {
            (
                record["number"].as_str().unwrap(),
                record["status"].as_str(),
                record["date"].as_str(),
                record["amended"] == true,
            )
        })
        .collect();
    assert_eq!(
        stated,
        [
            ("0001", Some("accepted"), Some("2026-01-02"), false),
            ("0002", Some("accepted"), Some("2026-05-06"), true),
            ("0003", Some("proposed"), None, false),
            ("0004", None, None, false),
        ]
    );
    assert_eq!(
        decisions["by_status"],
        serde_json::json!({"accepted": "2", "proposed": "1", "unknown": "1"})
    );

    // One third of three commits is 3333 basis points; ranking by commits, then lines added.
    let contributors = &survey["contributors"];
    assert_eq!(contributors["agent_share_bp"], "3333");
    assert_eq!(contributors["human_share_bp"], "6666");
    assert_eq!(contributors["top_share_bp"], "6666");
    assert_eq!(names(contributors), ["Ada", "Bob"]);
    let added = |name: &str| {
        contributor(contributors, name)["added"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
    };
    assert_eq!(added("Ada"), 2);
    assert!(
        added("Bob") > added("Ada"),
        "Bob added more lines in fewer commits"
    );

    // Every pipeline file, routed or not; identity `<source>:<name>`; the listed toolchain
    // files at the root only.
    let checks = &survey["checks"];
    let declared: Vec<String> = checks["declared"]
        .as_array()
        .unwrap()
        .iter()
        .map(|check| {
            format!(
                "{}:{} {}",
                check["source"].as_str().unwrap(),
                check["name"].as_str().unwrap(),
                check["program"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(
        declared,
        [
            "code-policy:test cargo",
            "review.toml:test cargo",
            "unused.toml:lint npx",
            "unused.toml:test make",
        ]
    );
    assert_eq!(
        checks["toolchain"],
        serde_json::json!([".nvmrc", "rust-toolchain"])
    );

    // Each file one language: configured over built-in, bytewise-first pattern, else other.
    let languages = &survey["tree"]["languages"];
    assert_eq!(languages["rust-any"]["files"], "2", "{languages}");
    assert_eq!(languages["generated"]["files"], "1", "{languages}");
    assert_eq!(languages["docs"]["files"], "5", "{languages}");
    assert!(languages.get("rust").is_none() && languages.get("markdown").is_none());
    assert_eq!(languages["toml"]["files"], "5", "{languages}");
    let total: u64 = languages
        .as_object()
        .unwrap()
        .values()
        .map(|language| language["files"].as_str().unwrap().parse::<u64>().unwrap())
        .sum();
    assert_eq!(total.to_string(), survey["tree"]["files"].as_str().unwrap());

    // A list over max_entries keeps its first ranked entries and says how many it saw.
    machine.expect(
        &repo,
        &[("AF_SURVEY__MAX_ENTRIES", "1")],
        &["project", "refresh"],
        0,
    );
    let cut = machine.json(&repo, &[], &["project", "survey", "--json"]);
    schemas::valid(&schemas::validator("project-survey-v2.json"), &cut);
    let contributors = &cut["contributors"];
    assert_eq!(names(contributors), ["Ada"], "more commits, fewer lines");
    assert_eq!(contributors["seen"]["by_identity"], "2");
    assert_eq!(
        contributors["agent_share_bp"], "3333",
        "the totals cover all"
    );
    assert_eq!(cut["decisions"]["records"][0]["number"], "0001");
    assert_eq!(cut["decisions"]["seen"]["records"], "4");
    assert_eq!(cut["checks"]["declared"][0]["source"], "code-policy");
    assert_eq!(cut["checks"]["seen"]["declared"], "4");
    assert_eq!(cut["checks"]["toolchain"], serde_json::json!([".nvmrc"]));
    assert_eq!(cut["checks"]["seen"]["toolchain"], "2");
    assert_eq!(cut["checks"]["reason"], "max_entries");
}

#[test]
fn a_historical_commit_over_max_paths_leaves_history_partial_and_the_survey_recorded() {
    let machine = Machine::new();
    let repo = machine.root().join("wide-history");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    for index in 0..10 {
        write(&repo, &format!("f{index}.txt"), b"x\n");
    }
    commit(&repo, ADA, 3, &["Ten files"]);
    for index in 1..10 {
        std::fs::remove_file(repo.join(format!("f{index}.txt"))).unwrap();
    }
    commit(&repo, ADA, 2, &["Nine removed"]);
    write(&repo, "f0.txt", b"y\n");
    commit(&repo, BOB, 1, &["One changed"]);
    let (stdout, _) = machine.expect(
        &repo,
        &[("AF_SURVEY__MAX_PATHS", "5")],
        &["project", "refresh"],
        0,
    );
    assert!(stdout.starts_with("recorded\n"), "{stdout}");
    let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
    schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);
    assert_eq!(survey["tree"]["completeness"], "complete", "a small head");
    for section in ["history", "contributors"] {
        assert_eq!(survey[section]["completeness"], "partial", "{section}");
        assert_eq!(survey[section]["reason"], "max_paths", "{section}");
    }
    // The totals and rankings cover the commits observed before the walk stopped.
    assert_eq!(survey["history"]["commits"], "1");
    assert_eq!(survey["history"]["reachable_commits"], "3");
    assert_eq!(names(&survey["contributors"]), ["Bob"]);
}

#[test]
fn every_list_of_checks_is_bounded_and_a_non_utf8_declaration_is_unreadable() {
    let machine = Machine::new();
    let big = format!("# {}\n", "z".repeat(200));
    let repo = repository(
        &machine.root(),
        "unreadable",
        &[
            (".af/pipelines/a.toml", big.as_bytes()),
            (".af/pipelines/b.toml", big.as_bytes()),
        ],
    );
    let env = [
        ("AF_SURVEY__MAX_FILE_BYTES", "128"),
        ("AF_SURVEY__MAX_ENTRIES", "1"),
    ];
    machine.expect(&repo, &env, &["project", "refresh"], 0);
    let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
    schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);
    let checks = &survey["checks"];
    assert_eq!(
        checks["unreadable"],
        serde_json::json!([".af/pipelines/a.toml"])
    );
    assert_eq!(checks["seen"]["unreadable"], "2");
    assert_eq!(checks["completeness"], "partial");
    assert_eq!(checks["reason"], "max_entries");
    let surveys = machine.events();
    assert_eq!(
        surveys.last().unwrap(),
        "SurveyRecorded@1",
        "under max_survey_bytes"
    );

    // A decision, the vocabulary and a declaration that are not valid UTF-8 are not text.
    let repo = repository(
        &machine.root(),
        "latin1",
        &[
            ("docs/adr/0001-cafe.md", b"# Caf\xe9\n\nStatus: accepted\n"),
            ("docs/adr/0002-fine.md", b"# Fine\n\nStatus: accepted\n"),
            ("CONTEXT.md", b"**Caf\xe9**:\nA definition.\n"),
            (
                ".af/pipelines/latin.toml",
                b"[[checks]]\nname = \"caf\xe9\"\nprogram = \"make\"\n",
            ),
        ],
    );
    machine.expect(&repo, &[], &["project", "refresh"], 0);
    let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
    schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);
    for section in ["decisions", "vocabulary", "checks"] {
        assert_eq!(survey[section]["completeness"], "partial", "{section}");
        assert_eq!(survey[section]["reason"], "max_file_bytes", "{section}");
    }
    let decisions = &survey["decisions"];
    assert_eq!(decisions["records"][0]["status"], "unreadable");
    assert!(decisions["records"][0].get("title").is_none());
    assert_eq!(decisions["records"][1]["title"], "Fine");
    let vocabulary = &survey["vocabulary"];
    assert_eq!(vocabulary["present"], true);
    assert!(vocabulary.get("terms").is_none() && vocabulary.get("count").is_none());
    assert_eq!(
        survey["checks"]["unreadable"],
        serde_json::json!([".af/pipelines/latin.toml"])
    );
    assert_eq!(survey["checks"]["declared"], serde_json::json!([]));
}

#[test]
fn an_author_name_carrying_an_email_is_its_hash_under_both_identity_policies() {
    const ALICE: (&str, &str) = ("Alice alice@example.org", "alice@real.invalid");
    for identity in ["name", "email_hash"] {
        let machine = Machine::new();
        let repo = machine.root().join("emails");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        write(&repo, "a.txt", b"a\nb\n");
        commit(&repo, ALICE, 2, &["Alice"]);
        write(&repo, "c.txt", b"c\n");
        commit(&repo, CAROL, 1, &["Carol"]);
        let env = [("AF_SURVEY__IDENTITY", identity)];
        let (said, _) = machine.expect(&repo, &env, &["project", "refresh", "--json"], 0);
        let survey: Value = serde_json::from_str(&said).unwrap();
        schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);
        let contributors = &survey["contributors"];
        // The email field is never used for such a name: both keys are the names' hashes.
        assert_eq!(
            keys(contributors),
            [name_hash(ALICE.0), name_hash(CAROL.0)],
            "{identity}"
        );
        if identity == "name" {
            assert_eq!(
                names(contributors),
                [name_hash(ALICE.0), name_hash(CAROL.0)]
            );
        }
        let kept = everything_kept(&machine);
        for email in ["alice@example.org", ALICE.1, CAROL.1] {
            assert!(!kept.contains(email), "{identity}: {email} was recorded");
            assert!(!said.contains(email), "{identity}: {email} was printed");
        }
    }
}

#[test]
fn replay_refuses_a_survey_event_that_does_not_describe_its_record() {
    let machine = Machine::new();
    let repo = fixture(&machine.root());
    machine.expect(&repo, &[], &["project", "refresh"], 0);
    // Hand-write the event: the same Survey, another refs digest.
    let log = rusqlite::Connection::open(machine.store().join("events.sqlite")).unwrap();
    let (sequence, payload): (i64, String) = log
        .query_row(
            "SELECT sequence, payload FROM events WHERE type = 'SurveyRecorded@1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let mut payload: Value = serde_json::from_str(&payload).unwrap();
    payload["refs_digest"] = Value::String(format!("sha256:{}", "0".repeat(64)));
    log.execute(
        "UPDATE events SET payload = ?1 WHERE sequence = ?2 AND type = 'SurveyRecorded@1'",
        rusqlite::params![payload.to_string(), sequence],
    )
    .unwrap();
    drop(log);
    for args in [&["project", "survey"][..], &["project", "refresh"]] {
        let (_, stderr) = machine.expect(&repo, &[], args, 1);
        assert!(
            stderr.contains(&format!(
                "SurveyRecorded@1 at sequence {sequence} names a refs_digest"
            )),
            "{args:?}: {stderr}"
        );
    }
}

#[test]
fn the_ceilings_are_only_lowered_from_the_project_layer_and_the_environment() {
    let machine = Machine::new();
    let repo = repository(
        &machine.root(),
        "ceilings",
        &[("README.md", b"# Ceilings\n")],
    );
    for (key, ceiling) in [
        ("max_paths", 200_000_u64),
        ("max_entries", 1_000),
        ("max_survey_bytes", 1_048_576),
    ] {
        let variable = format!("AF_SURVEY__{}", key.to_uppercase());
        let refused = format!("{key} is a positive integer at most its ceiling {ceiling}");
        // The environment form: zero, above the ceiling, and a value that fits no integer.
        for value in [
            "0",
            &(ceiling + 1).to_string(),
            "99999999999999999999",
            "-1",
        ] {
            let (_, stderr) = machine.expect(
                &repo,
                &[(variable.as_str(), value)],
                &["project", "refresh"],
                1,
            );
            assert!(stderr.contains(&refused), "{variable}={value}: {stderr}");
            assert_eq!(machine.events().last().unwrap(), "SurveyFailed@1");
        }
        // The project layer alike.
        for value in [
            "0",
            &(ceiling + 1).to_string(),
            "9223372036854775807",
            "\"1\"",
        ] {
            write(
                &repo,
                ".af/af.toml",
                format!("version = 1\n\n[survey]\n{key} = {value}\n").as_bytes(),
            );
            let (_, stderr) = machine.expect(&repo, &[], &["project", "refresh"], 1);
            assert!(stderr.contains(&refused), "{key} = {value}: {stderr}");
        }
        // An integer TOML itself cannot hold is refused with the file.
        write(
            &repo,
            ".af/af.toml",
            format!("version = 1\n\n[survey]\n{key} = 18446744073709551616\n").as_bytes(),
        );
        let (_, stderr) = machine.expect(&repo, &[], &["project", "refresh"], 1);
        assert!(
            stderr.contains("the configuration does not load"),
            "{stderr}"
        );
        assert_eq!(machine.events().last().unwrap(), "SurveyFailed@1");
        // The ceiling itself and anything below it load.
        write(
            &repo,
            ".af/af.toml",
            format!("version = 1\n\n[survey]\n{key} = {ceiling}\n").as_bytes(),
        );
        machine.expect(&repo, &[], &["project", "refresh", "--force"], 0);
        let lower = (ceiling / 2).to_string();
        machine.expect(
            &repo,
            &[(variable.as_str(), lower.as_str())],
            &["project", "refresh", "--force"],
            0,
        );
    }
}

/// The real git's path, which a fake git on `PATH` hands every command it does not stop.
fn real_git() -> String {
    let real = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    String::from_utf8(real.stdout).unwrap().trim().to_string()
}

/// A directory holding a fake `git` whose script runs `body` before it hands the command to the
/// real git; `body` sees the arguments as `$@` and the real git as `$REAL`.
fn fake_git(machine: &Machine, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = machine.root().join(name);
    std::fs::create_dir_all(&bin).unwrap();
    let fake = bin.join("git");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\n[ \"$1\" = warm ] && exit 0\nREAL='{}'\n{body}\nexec \"$REAL\" \"$@\"\n",
            real_git()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    // The first exec of a script just written can be slow (macOS scans it): start it once.
    assert!(Command::new(&fake).arg("warm").status().unwrap().success());
    bin
}

/// The sequence of every `SurveyRecorded@1` of the Project Store, oldest first.
fn recorded_sequences(machine: &Machine) -> Vec<i64> {
    let log = rusqlite::Connection::open(machine.store().join("events.sqlite")).unwrap();
    log.prepare("SELECT sequence FROM events WHERE type = 'SurveyRecorded@1' ORDER BY sequence")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Rewrite the `refs_digest` of the `SurveyRecorded@1` at `sequence`: an otherwise valid event
/// that no longer describes its record.
fn mismatch_refs_digest(machine: &Machine, sequence: i64) {
    let log = rusqlite::Connection::open(machine.store().join("events.sqlite")).unwrap();
    let payload: String = log
        .query_row(
            "SELECT payload FROM events WHERE sequence = ?1 AND type = 'SurveyRecorded@1'",
            [sequence],
            |row| row.get(0),
        )
        .unwrap();
    let mut payload: Value = serde_json::from_str(&payload).unwrap();
    payload["refs_digest"] = Value::String(format!("sha256:{}", "0".repeat(64)));
    log.execute(
        "UPDATE events SET payload = ?1 WHERE sequence = ?2 AND type = 'SurveyRecorded@1'",
        rusqlite::params![payload.to_string(), sequence],
    )
    .unwrap();
}

#[test]
fn history_survey_and_refresh_refuse_a_mismatched_event_and_history_an_older_one() {
    let machine = Machine::new();
    let repo = fixture(&machine.root());
    machine.expect(&repo, &[], &["project", "refresh"], 0);
    machine.expect(&repo, &[], &["project", "refresh", "--force"], 0);
    let sequences = recorded_sequences(&machine);
    assert_eq!(sequences.len(), 2);
    let named =
        |sequence: i64| format!("SurveyRecorded@1 at sequence {sequence} names a refs_digest");

    // An older event no longer describing its record: the history refuses it, in text and in
    // JSON, so no mismatched event is printed; the newest Survey is still read.
    mismatch_refs_digest(&machine, sequences[0]);
    // Under --json the refusal is the `af/error@1` document, never a history.
    let refused = |args: &[&str], sequence: i64| {
        let (stdout, stderr) = machine.expect(&repo, &[], args, 1);
        let said = if args.contains(&"--json") {
            let error: Value = serde_json::from_str(&stdout).unwrap();
            assert_eq!(error["schema"], "af/error@1", "{args:?}: {stdout}");
            error["error"].as_str().unwrap().to_string()
        } else {
            assert!(stdout.is_empty(), "{args:?}: {stdout}");
            stderr
        };
        assert!(said.contains(&named(sequence)), "{args:?}: {said}");
    };
    for args in [
        &["project", "history"][..],
        &["project", "history", "--json"],
    ] {
        refused(args, sequences[0]);
    }
    machine.expect(&repo, &[], &["project", "survey", "--json"], 0);

    // The newest one: history, survey and refresh refuse it, naming its sequence and field.
    mismatch_refs_digest(&machine, sequences[1]);
    for args in [
        &["project", "history", "--json"][..],
        &["project", "survey"],
        &["project", "survey", "--json"],
        &["project", "refresh"],
    ] {
        refused(args, sequences[1]);
    }
}

#[test]
fn a_deadline_in_any_sections_finishing_step_leaves_it_partial_and_the_rest_unavailable() {
    const SECTIONS: [&str; 7] = [
        "identity",
        "tree",
        "history",
        "contributors",
        "decisions",
        "vocabulary",
        "checks",
    ];
    let root = tempfile::tempdir().unwrap();
    let repo = fixture(root.path());
    let max_wall_ms: u64 = 4000;
    // Each section's finishing step, after its last item and before it is recorded, lasts
    // past the deadline on its own machine, all at once.
    let runs: Vec<_> = SECTIONS
        .into_iter()
        .map(|slowed| {
            let repo = repo.clone();
            std::thread::spawn(move || {
                let machine = Machine::new();
                machine.expect(&repo, &[], &["project", "show"], 0);
                let started = std::time::Instant::now();
                machine.expect(
                    &repo,
                    &[
                        ("AF_SURVEY__MAX_WALL_MS", &max_wall_ms.to_string()),
                        ("AF_TEST_SURVEY_SLOW_FINISH", slowed),
                    ],
                    &["project", "refresh"],
                    0,
                );
                let took = started.elapsed();
                (
                    slowed,
                    took,
                    machine.json(&repo, &[], &["project", "survey", "--json"]),
                )
            })
        })
        .collect();
    for run in runs {
        let (slowed, took, survey) = run.join().unwrap();
        assert!(
            took < std::time::Duration::from_millis(max_wall_ms + 2000),
            "{slowed}: the refresh took {took:?}"
        );
        schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);
        let at = SECTIONS
            .iter()
            .position(|section| *section == slowed)
            .unwrap();
        for (index, section) in SECTIONS.iter().enumerate() {
            let measured = &survey[*section];
            let expected = match index.cmp(&at) {
                std::cmp::Ordering::Less => "complete",
                std::cmp::Ordering::Equal => "partial",
                std::cmp::Ordering::Greater => "unavailable",
            };
            assert_eq!(
                measured["completeness"], expected,
                "{slowed}: {section}: {measured}"
            );
            if index >= at {
                assert_eq!(measured["reason"], "max_wall_ms", "{slowed}: {section}");
            }
        }
        // What the section finished is kept.
        let kept = match slowed {
            "identity" => "head",
            "tree" => "files",
            "history" => "commits",
            "contributors" => "by_identity",
            "decisions" => "records",
            "vocabulary" => "terms",
            _ => "declared",
        };
        assert!(
            survey[slowed].get(kept).is_some(),
            "{slowed}: {}",
            survey[slowed]
        );
    }
}

#[test]
fn every_declaration_charges_the_clock_oversized_ones_included() {
    let machine = Machine::new();
    let repo = machine.root().join("oversized-first");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    let big = format!("# Big\n\nStatus: accepted\n{}\n", "z".repeat(200));
    for number in 1..=40 {
        write(
            &repo,
            &format!("docs/adr/{number:04}-big.md"),
            big.as_bytes(),
        );
    }
    write(
        &repo,
        "docs/adr/0099-small.md",
        b"# Small\n\nStatus: accepted\n",
    );
    commit(&repo, ADA, 1, &["Decisions"]);
    machine.expect(&repo, &[], &["project", "show"], 0);
    let max_wall_ms: u64 = 2500;
    let started = std::time::Instant::now();
    machine.expect(
        &repo,
        &[
            ("AF_SURVEY__MAX_WALL_MS", &max_wall_ms.to_string()),
            ("AF_SURVEY__MAX_FILE_BYTES", "128"),
            ("AF_TEST_SURVEY_SLOW_ITEM_US", "100000"),
            ("AF_TEST_SURVEY_SLOW_SECTION", "decisions"),
        ],
        &["project", "refresh"],
        0,
    );
    let took = started.elapsed();
    assert!(
        took < std::time::Duration::from_millis(max_wall_ms + 2000),
        "the refresh took {took:?}"
    );
    let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
    schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);
    let decisions = &survey["decisions"];
    assert_eq!(decisions["completeness"], "partial", "{decisions}");
    assert_eq!(decisions["reason"], "max_wall_ms");
    // Each oversized declaration, never read, was one item: the deadline stopped among them,
    // before the readable one behind them.
    let reached: usize = decisions["count"].as_str().unwrap().parse().unwrap();
    assert!(reached > 0 && reached < 40, "reached {reached}");
    assert_eq!(
        decisions["by_status"],
        serde_json::json!({"unreadable": reached.to_string()})
    );
    for section in ["vocabulary", "checks"] {
        assert_eq!(survey[section]["completeness"], "unavailable", "{section}");
    }
}

#[test]
fn an_impossible_calendar_date_is_unknown() {
    let machine = Machine::new();
    let repo = repository(
        &machine.root(),
        "calendar",
        &[
            (
                "docs/adr/0001-february.md",
                b"# Thirty-first\n\nStatus: accepted 2026-02-31\n",
            ),
            (
                "docs/adr/0002-common.md",
                b"# Common year\n\nStatus: accepted 2025-02-29\n",
            ),
            (
                "docs/adr/0003-leap.md",
                b"# Leap year\n\nStatus: accepted 2024-02-29\n",
            ),
        ],
    );
    machine.expect(&repo, &[], &["project", "refresh"], 0);
    let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
    schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);
    let dates: Vec<(&str, Option<&str>)> = survey["decisions"]["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| (record["number"].as_str().unwrap(), record["date"].as_str()))
        .collect();
    assert_eq!(
        dates,
        [("0001", None), ("0002", None), ("0003", Some("2024-02-29"))]
    );
    assert_eq!(survey["decisions"]["by_status"]["accepted"], "3");
}

#[test]
fn a_blank_or_control_character_name_is_its_hash_under_both_identity_policies() {
    // Names git's porcelain refuses, written with fast-import: blank twice, by two emails, one
    // holding a bell, and 256 printable bytes followed by one, which the check over the whole
    // name finds past the display bound.
    let long = format!("{}\u{7}", "n".repeat(256));
    let authors = ["", " ", "Bell\u{7}Name", long.as_str()];
    for identity in ["name", "email_hash"] {
        let machine = Machine::new();
        let repo = machine.root().join("blank");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        let mut stream = Vec::new();
        for (index, (name, email)) in [
            (authors[0], "first@fixture.invalid"),
            (authors[1], "second@fixture.invalid"),
            (authors[2], "bell@fixture.invalid"),
            (authors[3], "long@fixture.invalid"),
        ]
        .into_iter()
        .enumerate()
        {
            let time = CUTOFF - (4 - index as u64) * DAY;
            let message = format!("Commit {index}");
            let file = format!("f{index}.txt");
            let author = if name.is_empty() {
                format!("<{email}>")
            } else {
                format!("{name} <{email}>")
            };
            stream.extend(
                format!(
                    "commit refs/heads/main\nmark :{}\nauthor {author} {time} +0000\n\
                     committer Fixture <fixture@example.invalid> {time} +0000\ndata {}\n{message}\n",
                    index + 1,
                    message.len()
                )
                .bytes(),
            );
            if index > 0 {
                stream.extend(format!("from :{index}\n").bytes());
            }
            stream.extend(format!("M 644 inline {file}\ndata 2\nx\n\n").bytes());
        }
        let mut child = Command::new("git")
            .current_dir(&repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(["fast-import", "--quiet"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write as _;
        child.stdin.take().unwrap().write_all(&stream).unwrap();
        assert!(child.wait().unwrap().success());
        git(&repo, &["reset", "-q", "--hard", "main"]);

        let env = [("AF_SURVEY__IDENTITY", identity)];
        let (said, _) = machine.expect(&repo, &env, &["project", "refresh", "--json"], 0);
        let survey: Value = serde_json::from_str(&said).unwrap();
        schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);
        let contributors = &survey["contributors"];
        // Both blank names are one identity, by the hash of the name, never by either email.
        assert_eq!(
            contributors["identities"], "3",
            "{identity}: {contributors}"
        );
        let blank = name_hash("");
        assert_eq!(name_hash(" "), blank);
        // One commit and one line each: ranked by key.
        let mut singles = [name_hash(authors[2]), name_hash(authors[3])];
        singles.sort();
        let expected = [blank.clone(), singles[0].clone(), singles[1].clone()];
        assert_eq!(keys(contributors), expected, "{identity}");
        assert_eq!(contributors["by_identity"][0]["commits"], "2", "{identity}");
        if identity == "name" {
            assert_eq!(names(contributors), expected);
            assert!(!said.contains(&"n".repeat(256)), "{identity}");
        } else {
            assert!(contributors["by_identity"][0].get("name").is_none());
        }
        let kept = everything_kept(&machine);
        for email in [
            "first@fixture.invalid",
            "second@fixture.invalid",
            "bell@fixture.invalid",
            "long@fixture.invalid",
        ] {
            assert!(
                !kept.contains(email) && !said.contains(email),
                "{identity}: {email}"
            );
        }
    }
}

#[test]
fn a_silent_git_ends_with_its_process_group_at_the_survey_deadline() {
    let machine = Machine::new();
    let repo = repository(&machine.root(), "silent", &[("README.md", b"# Silent\n")]);
    machine.expect(&repo, &[], &["project", "show"], 0);
    let pid_file = machine.root().join("silent.pid");
    // The tree listing's `ls-tree -l` sleeps and writes nothing; every other command is git's.
    let bin = fake_git(
        &machine,
        "silent-bin",
        &format!(
            "for argument in \"$@\"; do\n  if [ \"$argument\" = -l ]; then\n    echo $$ > '{}'\n    \
             exec sleep 120\n  fi\ndone",
            pid_file.display()
        ),
    );
    let max_wall_ms: u64 = 2000;
    let started = std::time::Instant::now();
    let output = af_with_git(
        &machine,
        &repo,
        &bin,
        &[("AF_SURVEY__MAX_WALL_MS", &max_wall_ms.to_string())],
        &["project", "refresh"],
    );
    let took = started.elapsed();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    // The grace: the process group's end and the record's write after the deadline.
    assert!(
        took < std::time::Duration::from_millis(max_wall_ms + 2000),
        "the refresh took {took:?}"
    );
    let pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .to_string();
    let alive = Command::new("kill")
        .args(["-0", &pid])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(!alive, "the silent git {pid} still runs");
    let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
    schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);
    assert_eq!(survey["completeness"], "partial");
    assert_eq!(survey["identity"]["completeness"], "complete");
    for section in [
        "tree",
        "history",
        "contributors",
        "decisions",
        "vocabulary",
        "checks",
    ] {
        assert_eq!(survey[section]["completeness"], "unavailable", "{section}");
        assert_eq!(survey[section]["reason"], "max_wall_ms", "{section}");
    }
}

#[test]
fn the_tree_digest_is_the_committed_captures_and_a_ceiling_leaves_it_absent_and_clean() {
    let machine = Machine::new();
    let repo = fixture(&machine.root());
    let committed = committed_digest(&repo);
    let scratch = machine.root().join("tmp");
    std::fs::create_dir_all(&scratch).unwrap();
    let tmpdir = scratch.to_str().unwrap();
    let digests_left = || {
        std::fs::read_dir(&scratch)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("af-tree-digest-"))
            .count()
    };
    // A blob above max_file_bytes and a tree beyond max_paths are digested whole.
    for env in [
        &[("AF_SURVEY__MAX_FILE_BYTES", "16"), ("TMPDIR", tmpdir)][..],
        &[("AF_SURVEY__MAX_PATHS", "3"), ("TMPDIR", tmpdir)],
        // Spilled runs merged: the same digest.
        &[
            ("AF_TEST_SURVEY_DIGEST_MANIFEST_BYTES", "300"),
            ("TMPDIR", tmpdir),
        ],
    ] {
        machine.expect(&repo, env, &["project", "refresh", "--force"], 0);
        let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
        assert_eq!(
            survey["identity"]["tree_digest"],
            committed.as_str(),
            "{env:?}"
        );
        assert_eq!(survey["identity"]["completeness"], "complete", "{env:?}");
        assert_eq!(digests_left(), 0, "{env:?}");
    }
    // A ceiling that stops the digest: absent with its reason, never a part, nothing left.
    for (env, reason) in [
        (
            &[
                ("AF_TEST_SURVEY_DIGEST_MANIFEST_BYTES", "20"),
                ("TMPDIR", tmpdir),
            ][..],
            "manifest_bytes",
        ),
        (
            &[
                ("AF_TEST_SURVEY_DIGEST_MANIFEST_BYTES", "300"),
                ("AF_TEST_SURVEY_DIGEST_TEMP_BYTES", "400"),
                ("TMPDIR", tmpdir),
            ],
            "temp_bytes",
        ),
    ] {
        machine.expect(&repo, env, &["project", "refresh", "--force"], 0);
        let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
        schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);
        let identity = &survey["identity"];
        assert!(identity.get("tree_digest").is_none(), "{identity}");
        assert_eq!(identity["tree_digest_reason"], reason);
        assert_eq!(identity["completeness"], "partial");
        assert_eq!(identity["reason"], reason);
        assert_eq!(
            survey["tree"]["completeness"], "complete",
            "a ceiling is no deadline"
        );
        assert_eq!(digests_left(), 0, "{reason}");
    }
}

#[test]
fn section_is_text_only_and_json_prints_the_complete_record() {
    let machine = Machine::new();
    let repo = fixture(&machine.root());
    let (record, _) = machine.expect(&repo, &[], &["project", "refresh", "--json"], 0);
    let (stdout, stderr) = machine.expect(
        &repo,
        &[],
        &["project", "survey", "--section", "tree", "--json"],
        2,
    );
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("cannot be used with"), "{stderr}");
    let (json, _) = machine.expect(&repo, &[], &["project", "survey", "--json"], 0);
    assert_eq!(json, record, "the complete record, unchanged");
    let (whole, _) = machine.expect(&repo, &[], &["project", "survey"], 0);
    let (picked, _) = machine.expect(
        &repo,
        &[],
        &["project", "survey", "--section", "history,tree"],
        0,
    );
    let line = |text: &str, name: &str| {
        text.lines()
            .find(|line| line.starts_with(&format!("  {name} ")))
            .map(str::to_string)
    };
    let picked_lines: Vec<&str> = picked.lines().collect();
    assert_eq!(picked_lines[..4], whole.lines().collect::<Vec<_>>()[..4]);
    assert_eq!(picked_lines.len(), 6, "{picked}");
    assert_eq!(line(&picked, "tree"), line(&whole, "tree"));
    assert_eq!(line(&picked, "history"), line(&whole, "history"));
    assert!(line(&picked, "checks").is_none(), "{picked}");
    assert_eq!(
        picked_lines[4..],
        [
            line(&whole, "tree").unwrap(),
            line(&whole, "history").unwrap()
        ],
        "in record order"
    );
    machine.expect(&repo, &[], &["project", "survey", "--section", "bogus"], 2);
}

#[test]
fn a_language_map_of_sixty_five_patterns_is_refused() {
    let machine = Machine::new();
    let languages: String = (0..65)
        .map(|n| format!("\"*.x{n}\" = \"lang{n}\"\n"))
        .collect();
    let repo = repository(
        &machine.root(),
        "languages",
        &[(
            ".af/af.toml",
            format!("version = 1\n\n[survey.languages]\n{languages}").as_bytes(),
        )],
    );
    let (_, stderr) = machine.expect(&repo, &[], &["project", "refresh"], 1);
    assert!(
        stderr.contains("[survey.languages] holds 65 patterns, more than 64"),
        "{stderr}"
    );
    assert_eq!(machine.events().last().unwrap(), "SurveyFailed@1");
    assert_eq!(machine.surveys(), 0);
    // Sixty-four load.
    let languages: String = (0..64)
        .map(|n| format!("\"*.x{n}\" = \"lang{n}\"\n"))
        .collect();
    write(
        &repo,
        ".af/af.toml",
        format!("version = 1\n\n[survey.languages]\n{languages}").as_bytes(),
    );
    machine.expect(&repo, &[], &["project", "refresh"], 0);
}

#[test]
fn checkout_attributes_and_diff_drivers_leave_history_counts_unchanged() {
    let machine = Machine::new();
    let repo = repository(&machine.root(), "attributes", &[("a.txt", b"one\ntwo\n")]);
    let measured = |machine: &Machine| {
        machine.expect(&repo, &[], &["project", "refresh", "--force"], 0);
        let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
        (survey["history"].clone(), survey["contributors"].clone())
    };
    let baseline = measured(&machine);
    assert_eq!(
        baseline.0["churn_90d"]["."],
        serde_json::json!({"commits": "1", "added": "2", "removed": "0"})
    );
    let numstat = || git(&repo, &["log", "--numstat", "--format="]);
    assert_eq!(numstat(), "2\t0\ta.txt");
    // An untracked `.gitattributes`: the checkout's own log counts no lines any more.
    write(&repo, ".gitattributes", b"*.txt -diff\n");
    assert_eq!(numstat(), "-\t-\ta.txt");
    assert_eq!(measured(&machine), baseline, "an untracked .gitattributes");
    std::fs::remove_file(repo.join(".gitattributes")).unwrap();
    // `.git/info/attributes`.
    write(&repo, ".git/info/attributes", b"*.txt -diff\n");
    assert_eq!(numstat(), "-\t-\ta.txt");
    assert_eq!(measured(&machine), baseline, ".git/info/attributes");
    // A local diff driver the attributes select.
    write(&repo, ".git/info/attributes", b"*.txt diff=opaque\n");
    git(&repo, &["config", "diff.opaque.binary", "true"]);
    assert_eq!(numstat(), "-\t-\ta.txt");
    assert_eq!(measured(&machine), baseline, "a local diff driver");
}

#[test]
fn a_tag_created_between_the_key_and_history_is_in_neither() {
    let machine = Machine::new();
    let repo = fixture(&machine.root());
    machine.expect(&repo, &[], &["project", "show"], 0);
    let digest = refs_digest(&repo);
    let tagged = machine.root().join("tagged");
    // The history's `log`, after the key's observation, first tags the head in the checkout.
    let bin = fake_git(
        &machine,
        "tagging-bin",
        &format!(
            "for argument in \"$@\"; do\n  if [ \"$argument\" = log ] && [ ! -e '{tagged}' ]; then\n    \
             : > '{tagged}'\n    env -u GIT_OBJECT_DIRECTORY -u GIT_NO_REPLACE_OBJECTS \"$REAL\" \
             -C '{repo}' tag v9.9.9 || exit 97\n  fi\ndone",
            tagged = tagged.display(),
            repo = repo.display()
        ),
    );
    let output = af_with_git(
        &machine,
        &repo,
        &bin,
        &[],
        &["project", "refresh", "--json"],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        tagged.exists(),
        "the tag was created during the measurement"
    );
    assert!(git(&repo, &["tag", "--list"]).contains("v9.9.9"));
    let survey: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        survey["identity"]["refs_digest"],
        digest.as_str(),
        "not in the key"
    );
    assert_eq!(
        survey["history"]["tags"]["count"], "2",
        "nor in the statistics"
    );
    assert_eq!(
        survey["history"]["tags"]["release"],
        serde_json::json!([{"name": "v1.0.0"}])
    );
    // The next refresh observes it in both.
    machine.expect(&repo, &[], &["project", "refresh"], 0);
    let next = machine.json(&repo, &[], &["project", "survey", "--json"]);
    assert_eq!(next["identity"]["refs_digest"], refs_digest(&repo).as_str());
    assert_eq!(next["history"]["tags"]["count"], "3");
}

/// Every `SurveyRecorded@1` of the Project Store: its sequence and payload, oldest first.
fn recorded_events(machine: &Machine) -> Vec<(i64, Value)> {
    let log = rusqlite::Connection::open(machine.store().join("events.sqlite")).unwrap();
    log.prepare(
        "SELECT sequence, payload FROM events WHERE type = 'SurveyRecorded@1' ORDER BY sequence",
    )
    .unwrap()
    .query_map([], |row| Ok((row.get(0)?, row.get::<_, String>(1)?)))
    .unwrap()
    .map(|row| {
        let (sequence, payload) = row.unwrap();
        (sequence, serde_json::from_str(&payload).unwrap())
    })
    .collect()
}

/// Every `SurveyCollected@1` payload of the Project Store, in order.
fn tombstones(machine: &Machine) -> Vec<Value> {
    let log = rusqlite::Connection::open(machine.store().join("events.sqlite")).unwrap();
    log.prepare("SELECT payload FROM events WHERE type = 'SurveyCollected@1' ORDER BY sequence")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|payload| serde_json::from_str(&payload.unwrap()).unwrap())
        .collect()
}

/// Set `field` of the `SurveyRecorded@1` at `sequence` to `value`: an otherwise valid event.
fn rewrite_event(machine: &Machine, sequence: i64, field: &str, value: &Value) {
    let log = rusqlite::Connection::open(machine.store().join("events.sqlite")).unwrap();
    let payload: String = log
        .query_row(
            "SELECT payload FROM events WHERE sequence = ?1 AND type = 'SurveyRecorded@1'",
            [sequence],
            |row| row.get(0),
        )
        .unwrap();
    let mut payload: Value = serde_json::from_str(&payload).unwrap();
    payload[field] = value.clone();
    log.execute(
        "UPDATE events SET payload = ?1 WHERE sequence = ?2 AND type = 'SurveyRecorded@1'",
        rusqlite::params![payload.to_string(), sequence],
    )
    .unwrap();
}

/// The CAS file of `survey_id`, made writable.
fn survey_object(machine: &Machine, survey_id: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let hex = survey_id.trim_start_matches("sha256:");
    let object = machine
        .store()
        .join("cas/objects")
        .join(&hex[..2])
        .join(&hex[2..]);
    std::fs::set_permissions(&object, std::fs::Permissions::from_mode(0o644)).unwrap();
    object
}

/// What `af` said refusing `args` with exit 1: stderr, or under `--json` the `af/error@1`
/// document's error, the only thing on stdout.
fn refusal(machine: &Machine, repo: &Path, env: &[(&str, &str)], args: &[&str]) -> String {
    let (stdout, stderr) = machine.expect(repo, env, args, 1);
    if args.contains(&"--json") {
        let error: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(error["schema"], "af/error@1", "{args:?}: {stdout}");
        error["error"].as_str().unwrap().to_string()
    } else {
        assert!(stdout.is_empty(), "{args:?}: {stdout}");
        stderr
    }
}

/// Thirty days before now, in Unix seconds: a cutoff the Storage Budget may collect.
fn collectable_cutoff() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 30 * DAY
}

/// `af storage prune` in `machine`'s own home and XDG directories, keeping `keep` Surveys:
/// applied, or previewed. The JSON document it prints.
fn prune_surveys(machine: &Machine, repo: &Path, keep: &str, apply: bool) -> Value {
    let root = machine.root();
    let mut env: Vec<(&str, String)> = ["home", "cache", "data", "config", "bin"]
        .into_iter()
        .map(|directory| {
            std::fs::create_dir_all(root.join(directory)).unwrap();
            (
                directory,
                root.join(directory).to_string_lossy().into_owned(),
            )
        })
        .map(|(directory, path)| {
            let name = match directory {
                "home" => "HOME",
                "cache" => "XDG_CACHE_HOME",
                "data" => "XDG_DATA_HOME",
                "config" => "XDG_CONFIG_HOME",
                _ => "XDG_BIN_HOME",
            };
            (name, path)
        })
        .collect();
    env.push(("AF_STORAGE__KEEP_SURVEYS", keep.to_string()));
    let env: Vec<(&str, &str)> = env
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();
    let mut args = vec!["storage", "prune", "--json"];
    if apply {
        args.push("--apply");
    }
    machine.json(repo, &env, &args)
}

#[test]
fn an_unreadable_or_corrupt_record_refuses_replay_naming_the_sequence_and_the_error() {
    let machine = Machine::new();
    let repo = fixture(&machine.root());
    machine.expect(&repo, &[], &["project", "refresh"], 0);
    machine.expect(&repo, &[], &["project", "refresh", "--force"], 0);
    let events = recorded_events(&machine);
    let [(older_at, older), (newest_at, newest)] = &events[..] else {
        panic!("two Surveys: {events:?}");
    };
    let id = |event: &Value| event["survey_id"].as_str().unwrap().to_string();
    let head = newest["head"].as_str().unwrap()[..12].to_string();

    // Neither collected: the newest record's bytes corrupt, the older one's gone.
    let corrupt = survey_object(&machine, &id(newest));
    let bytes = std::fs::read(&corrupt).unwrap();
    std::fs::write(&corrupt, b"{\"schema\":\"af/ProjectSurvey@2\"}").unwrap();
    std::fs::remove_file(survey_object(&machine, &id(older))).unwrap();
    let named = format!(
        "SurveyRecorded@1 at sequence {newest_at} names Survey {}, whose record cannot be read: \
         its bytes do not match its digest",
        id(newest)
    );
    for args in [
        &["project", "history"][..],
        &["project", "history", "--json"],
        &["project", "survey"],
        &["project", "survey", "--json"],
        &["project", "survey", "--section", "tree"],
        &["project", "refresh"],
    ] {
        let said = refusal(&machine, &repo, &[], args);
        assert!(said.contains(&named), "{args:?}: {said}");
        assert!(
            !said.contains(&head),
            "no event field is printed: {args:?}: {said}"
        );
    }

    // The newest readable again: the history refuses the older one, gone from the CAS, and the
    // newest Survey is read.
    std::fs::write(&corrupt, &bytes).unwrap();
    for args in [
        &["project", "history"][..],
        &["project", "history", "--json"],
    ] {
        let said = refusal(&machine, &repo, &[], args);
        assert!(
            said.contains(&format!(
                "SurveyRecorded@1 at sequence {older_at} names Survey {}, whose record cannot \
                 be read: it is not in the CAS",
                id(older)
            )),
            "{args:?}: {said}"
        );
    }
    machine.expect(&repo, &[], &["project", "survey"], 0);
}

#[test]
fn a_collected_event_is_bound_to_every_identity_field_its_tombstone_copied_from_the_record() {
    let machine = Machine::new();
    let repo = fixture(&machine.root());
    let old = collectable_cutoff();
    let at = |seconds: u64| (old + seconds).to_string();
    let (first, second) = (at(0), at(1));
    machine.expect(
        &repo,
        &[("AF_TEST_SURVEY_CUTOFF_UNIX_S", first.as_str())],
        &["project", "refresh"],
        0,
    );
    // The newest Survey partial, so the newest complete one is the older and the newest goes.
    machine.expect(
        &repo,
        &[
            ("AF_TEST_SURVEY_CUTOFF_UNIX_S", second.as_str()),
            ("AF_SURVEY__MAX_COMMITS", "1"),
        ],
        &["project", "refresh", "--force"],
        0,
    );
    let events = recorded_events(&machine);
    let [(_, older), (newest_at, newest)] = &events[..] else {
        panic!("two Surveys: {events:?}");
    };
    assert_eq!(newest["completeness"], "partial");
    let newest_id = newest["survey_id"].as_str().unwrap().to_string();
    let record: Value =
        serde_json::from_slice(&std::fs::read(survey_object(&machine, &newest_id)).unwrap())
            .unwrap();

    let pruned = prune_surveys(&machine, &repo, "0", true);
    let collected = tombstones(&machine);
    assert_eq!(collected.len(), 1, "{pruned}");
    let tombstone = &collected[0];
    schemas::valid(
        &schemas::validator("project-survey-collected-v1.json"),
        tombstone,
    );
    // Every identity field is the record's.
    let identity = &record["identity"];
    for (field, expected) in [
        ("survey_id", &newest["survey_id"]),
        ("head", &identity["head"]),
        ("cutoff_unix_s", &identity["cutoff_unix_s"]),
        ("completeness", &record["completeness"]),
        ("refs_digest", &identity["refs_digest"]),
        ("policy_digest", &identity["policy_digest"]),
        ("surveyor_version", &identity["surveyor_version"]),
        ("previous_survey_id", &older["survey_id"]),
    ] {
        assert_eq!(&tombstone[field], expected, "{field}: {tombstone}");
    }
    assert_eq!(
        tombstone["previous_survey_id"],
        record["previous_survey_id"]
    );
    assert!(tombstone.get("unreadable").is_none());
    machine.expect(&repo, &[], &["project", "history"], 0);

    // The collected newest event's head changed to another commit of the repository: no command
    // uses it, in an error message included, before checking it against the tombstone.
    let other = git(&repo, &["rev-list", "--max-parents=0", "HEAD"]);
    let original = newest["head"].clone();
    assert_ne!(Value::String(other.clone()), original);
    rewrite_event(&machine, *newest_at, "head", &Value::String(other.clone()));
    let named = |field: &str| {
        format!(
            "SurveyRecorded@1 at sequence {newest_at} names a {field} its tombstone SurveyCollected@1 at sequence"
        )
    };
    for args in [
        &["project", "survey", "--section", "tree"][..],
        &["project", "survey"],
        &["project", "survey", "--json"],
        &["project", "refresh"],
        &["project", "history"],
        &["project", "history", "--json"],
    ] {
        let said = refusal(&machine, &repo, &[], args);
        assert!(said.contains(&named("head")), "{args:?}: {said}");
        assert!(!said.contains(&other[..12]), "{args:?}: {said}");
    }
    rewrite_event(&machine, *newest_at, "head", &original);
    machine.expect(&repo, &[], &["project", "survey"], 1);

    // Its refs digest changed: the history refuses it, naming the field.
    let original = newest["refs_digest"].clone();
    rewrite_event(
        &machine,
        *newest_at,
        "refs_digest",
        &Value::String(format!("sha256:{}", "0".repeat(64))),
    );
    for args in [
        &["project", "history"][..],
        &["project", "history", "--json"],
    ] {
        let said = refusal(&machine, &repo, &[], args);
        assert!(said.contains(&named("refs_digest")), "{args:?}: {said}");
    }
    rewrite_event(&machine, *newest_at, "refs_digest", &original);
    machine.expect(&repo, &[], &["project", "history", "--json"], 0);
}

#[test]
fn collection_refuses_and_reports_a_survey_whose_event_disagrees_with_its_record() {
    let machine = Machine::new();
    let repo = fixture(&machine.root());
    let old = collectable_cutoff();
    for seconds in 0..3_u64 {
        let cutoff = (old + seconds).to_string();
        machine.expect(
            &repo,
            &[("AF_TEST_SURVEY_CUTOFF_UNIX_S", cutoff.as_str())],
            &["project", "refresh", "--force"],
            0,
        );
    }
    let events = recorded_events(&machine);
    let id = |index: usize| events[index].1["survey_id"].as_str().unwrap().to_string();
    let object = survey_object(&machine, &id(0));
    rewrite_event(
        &machine,
        events[0].0,
        "refs_digest",
        &Value::String(format!("sha256:{}", "0".repeat(64))),
    );
    let refused = format!(
        "Survey {} is refused: its SurveyRecorded@1 names a refs_digest its record does not \
         record, so it is kept",
        id(0)
    );
    // Preview and pass alike report it, and the pass takes the other Survey beyond the limit.
    for apply in [false, true] {
        let pruned = prune_surveys(&machine, &repo, "1", apply);
        let failures: Vec<&str> = pruned["failures"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .filter(|failure| failure.contains(&refused))
            .collect();
        assert_eq!(failures.len(), 1, "apply {apply}: {pruned}");
    }
    let collected: Vec<Value> = tombstones(&machine)
        .into_iter()
        .map(|tombstone| tombstone["survey_id"].clone())
        .collect();
    assert_eq!(collected, [Value::String(id(1))], "no tombstone for it");
    assert!(object.is_file(), "its record is left in place");
    // Uncollected and unbound, it is refused at replay too.
    let said = refusal(&machine, &repo, &[], &["project", "history"]);
    assert!(
        said.contains(&format!(
            "SurveyRecorded@1 at sequence {} names a refs_digest its Survey {} does not record",
            events[0].0,
            id(0)
        )),
        "{said}"
    );
}

/// Commits written with `git fast-import`, which takes an author line verbatim where git's
/// porcelain would strip or refuse it: each `(author, email, days before the cutoff, message,
/// file)` on `main`, oldest first.
fn imported(repo: &Path, commits: &[(&str, &str, u64, &str, &str)]) {
    std::fs::create_dir_all(repo).unwrap();
    git(repo, &["init", "-q", "-b", "main"]);
    let mut stream = Vec::new();
    for (index, (name, email, days, message, file)) in commits.iter().enumerate() {
        let time = CUTOFF - days * DAY;
        stream.extend(
            format!(
                "commit refs/heads/main\nmark :{}\nauthor {name} <{email}> {time} +0000\n\
                 committer Fixture <fixture@example.invalid> {time} +0000\ndata {}\n{message}\n",
                index + 1,
                message.len()
            )
            .bytes(),
        );
        if index > 0 {
            stream.extend(format!("from :{index}\n").bytes());
        }
        stream.extend(format!("M 644 inline {file}\ndata 2\nx\n\n").bytes());
    }
    let mut child = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["fast-import", "--quiet"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write as _;
    child.stdin.take().unwrap().write_all(&stream).unwrap();
    assert!(child.wait().unwrap().success());
    git(repo, &["reset", "-q", "--hard", "main"]);
}

#[test]
fn a_0x1f_in_a_name_or_a_subject_never_splits_a_log_record() {
    // 256 printable bytes and then 0x1f, the byte the log's fields were once split at, as the
    // newest author; a subject holding 0x1f and a newline, with an agent trailer, before it.
    let long = format!("{}\u{1f}", "n".repeat(256));
    let subject = "Sub\u{1f}ject\nacross two lines\n\nCo-authored-by: Claude <c@fixture.invalid>\n";
    let commits = [
        ("Ada", "ada@fixture.invalid", 3, "Start", "a.txt"),
        ("Bob", "bob@fixture.invalid", 2, subject, "b.txt"),
        (long.as_str(), "long@fixture.invalid", 1, "Long", "c.txt"),
    ];
    for identity in ["name", "email_hash"] {
        let machine = Machine::new();
        let repo = machine.root().join("unit-separator");
        imported(&repo, &commits);
        let env = [("AF_SURVEY__IDENTITY", identity)];
        let (said, _) = machine.expect(&repo, &env, &["project", "refresh", "--json"], 0);
        let survey: Value = serde_json::from_str(&said).unwrap();
        schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);
        assert_eq!(survey["completeness"], "complete", "{identity}: {said}");
        assert!(survey.get("stopped").is_none(), "{identity}");
        let history = &survey["history"];
        assert_eq!(history["commits"], "3", "{identity}");
        assert_eq!(history["merges"], "0", "{identity}");
        let contributors = &survey["contributors"];
        assert_eq!(contributors["identities"], "3", "{identity}");
        // The whole name reached the identity rule: its `h:` key under both policies.
        let hashed = name_hash(&long);
        assert!(
            keys(contributors).contains(&hashed),
            "{identity}: {contributors}"
        );
        let entry = contributors["by_identity"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["key"] == hashed.as_str())
            .unwrap();
        assert_eq!(entry["commits"], "1", "{identity}");
        assert_eq!(entry["added"], "1", "{identity}");
        if identity == "name" {
            assert_eq!(entry["name"], hashed.as_str());
            assert_eq!(contributor(contributors, "Bob")["commits"], "1");
            assert_eq!(contributor(contributors, "Ada")["commits"], "1");
        } else {
            assert!(entry.get("name").is_none());
            assert!(keys(contributors).contains(&hex16(b"bob@fixture.invalid")));
        }
        // The subject's commit is one commit, its trailer read: agent-assisted.
        assert_eq!(contributors["agent_commits"], "1", "{identity}");
        assert_eq!(contributors["human_commits"], "2", "{identity}");
        assert!(!said.contains(&"n".repeat(256)), "{identity}");
    }
}

#[test]
fn a_merge_at_the_max_commits_boundary_counts_as_a_commit_and_a_merge_without_churn() {
    let machine = Machine::new();
    let repo = machine.root().join("merges");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    write(&repo, "a.txt", b"a\n");
    commit(&repo, ADA, 5, &["Root"]);
    git(&repo, &["checkout", "-q", "-b", "side"]);
    write(&repo, "b.txt", b"b\n");
    commit(&repo, BOB, 3, &["Side"]);
    git(&repo, &["checkout", "-q", "main"]);
    write(&repo, "a.txt", b"a\nb\n");
    commit(&repo, ADA, 4, &["Main"]);
    let main = git(&repo, &["rev-parse", "HEAD"]);
    git_as(
        &repo,
        Some(("Dee", "dee@fixture.invalid", 1)),
        &["merge", "-q", "--no-ff", "side", "-m", "Merge side"],
    );
    let merge = git(&repo, &["rev-parse", "HEAD"]);
    assert_eq!(git(&repo, &["rev-list", "--count", "HEAD"]), "4");

    // max_commits 4: the four commits, the merge among them, three authors.
    let refresh = |max_commits: &str| {
        machine.expect(
            &repo,
            &[("AF_SURVEY__MAX_COMMITS", max_commits)],
            &["project", "refresh", "--force"],
            0,
        );
        let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
        schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);
        survey
    };
    let whole = refresh("4");
    let history = &whole["history"];
    assert_eq!(history["completeness"], "complete", "{history}");
    assert_eq!(
        (&history["commits"], &history["merges"]),
        (&Value::from("4"), &Value::from("1"))
    );
    assert_eq!(history["windows"]["30d"]["merges"], "1");
    // The merge diffs nothing: only the three commits' lines are churn.
    assert_eq!(history["churn_90d"]["."]["commits"], "3");
    assert_eq!(history["churn_90d"]["."]["added"], "3");
    let contributors = &whole["contributors"];
    assert_eq!(contributors["identities"], "3");
    assert_eq!(contributor(contributors, "Ada")["commits"], "2");
    let dee = contributor(contributors, "Dee");
    assert_eq!((&dee["commits"], &dee["added"]), (&"1".into(), &"0".into()));

    // max_commits 3: newest first, the merge is reached and the root is not.
    let cut = refresh("3");
    let history = &cut["history"];
    assert_eq!(history["completeness"], "partial");
    assert_eq!(history["reason"], "max_commits");
    assert_eq!(
        (&history["commits"], &history["merges"]),
        (&Value::from("3"), &Value::from("1"))
    );
    assert_eq!(history["reachable_commits"], "4");
    assert_eq!(history["oldest_reached"], main.as_str());
    assert_eq!(cut["contributors"]["identities"], "3");
    assert_eq!(contributor(&cut["contributors"], "Ada")["commits"], "1");
    assert_eq!(cut["stopped"]["section"], "history");
    assert_eq!(cut["stopped"]["reason"], "max_commits");
    assert_eq!(cut["stopped"]["commit"], main.as_str());

    // max_commits 1: the merge alone, a commit and a merge by one author with no lines.
    let merge_only = refresh("1");
    let history = &merge_only["history"];
    assert_eq!(
        (&history["commits"], &history["merges"]),
        (&Value::from("1"), &Value::from("1"))
    );
    assert_eq!(history["oldest_reached"], merge.as_str());
    assert_eq!(merge_only["contributors"]["identities"], "1");
    assert_eq!(
        contributor(&merge_only["contributors"], "Dee")["added"],
        "0"
    );
}

/// `af project survey --json` of the newest Survey: the recorded bytes, unchanged.
fn newest_record(machine: &Machine, repo: &Path) -> String {
    machine
        .expect(repo, &[], &["project", "survey", "--json"], 0)
        .0
}

/// `record` with its `previous_survey_id` taken out.
fn without_previous(record: &str) -> Value {
    let mut value: Value = serde_json::from_str(record).unwrap();
    value.as_object_mut().unwrap().remove("previous_survey_id");
    value
}

#[test]
fn forced_refreshes_at_one_cutoff_differ_only_in_the_chain_and_a_partial_one_in_where_it_stopped() {
    let machine = Machine::new();
    let repo = fixture(&machine.root());
    // Complete Surveys of the same head, refs, policy, Surveyor, git and cutoff: byte for byte
    // the same but for previous_survey_id.
    machine.expect(&repo, &[], &["project", "refresh"], 0);
    let first = newest_record(&machine, &repo);
    machine.expect(&repo, &[], &["project", "refresh", "--force"], 0);
    let second = newest_record(&machine, &repo);
    machine.expect(&repo, &[], &["project", "refresh", "--force"], 0);
    let third = newest_record(&machine, &repo);
    let previous = |record: &str| {
        serde_json::from_str::<Value>(record).unwrap()["previous_survey_id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_ne!(previous(&second), previous(&third));
    assert_eq!(
        third.replace(&previous(&third), &previous(&second)),
        second,
        "two forced complete refreshes differ only in previous_survey_id"
    );
    assert_eq!(without_previous(&first), without_previous(&second));
    assert!(serde_json::from_str::<Value>(&first).unwrap()["stopped"].is_null());

    // A partial Survey records where it stopped, and records it again the same way.
    let cut = [("AF_SURVEY__MAX_COMMITS", "7")];
    machine.expect(&repo, &cut, &["project", "refresh", "--force"], 0);
    let partial = newest_record(&machine, &repo);
    machine.expect(&repo, &cut, &["project", "refresh", "--force"], 0);
    let again = newest_record(&machine, &repo);
    assert_eq!(
        again.replace(&previous(&again), &previous(&partial)),
        partial,
        "a partial Survey replays to the same stopping point"
    );
    let survey: Value = serde_json::from_str(&partial).unwrap();
    schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);
    let stopped = &survey["stopped"];
    let oldest = git(&repo, &["rev-parse", "v1.0.0^{commit}"]);
    assert_eq!(stopped["section"], "history", "{stopped}");
    assert_eq!(stopped["reason"], "max_commits");
    assert_eq!(stopped["commit"], oldest.as_str());
    let item: u64 = stopped["item"].as_str().unwrap().parse().unwrap();
    assert!(
        item >= 7,
        "the seven commits are items of history: {stopped}"
    );
    // Replay reads it back through the verified record, in text too.
    let (text, _) = machine.expect(&repo, &[], &["project", "survey"], 0);
    assert!(
        text.contains(&format!(
            "  stopped       history at item {item}, commit {} (max_commits)\n",
            &oldest[..12]
        )),
        "{text}"
    );
    machine.expect(&repo, &[], &["project", "history", "--json"], 0);

    // A deadline in a section's finishing step names that section and how far it got.
    let machine = Machine::new();
    machine.expect(
        &repo,
        &[
            ("AF_SURVEY__MAX_WALL_MS", "3000"),
            ("AF_TEST_SURVEY_SLOW_FINISH", "decisions"),
        ],
        &["project", "refresh"],
        0,
    );
    let survey: Value = serde_json::from_str(&newest_record(&machine, &repo)).unwrap();
    schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);
    let stopped = &survey["stopped"];
    assert_eq!(stopped["section"], "decisions", "{stopped}");
    assert_eq!(stopped["reason"], "max_wall_ms");
    // Three decisions read and the finishing step's check.
    assert_eq!(stopped["item"], "4", "{stopped}");
    assert!(stopped.get("commit").is_none() && stopped.get("path").is_none());
}

/// Kills the processes it holds when the test ends, a panicking one included.
struct Reaped {
    children: Vec<std::process::Child>,
    pids: Vec<String>,
}

impl Drop for Reaped {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
        for pid in &self.pids {
            let _ = Command::new("kill").args(["-9", pid]).status();
        }
    }
}

#[test]
fn a_spill_a_killed_refresh_left_is_swept_by_the_next_while_a_live_refreshs_is_kept() {
    let machine = Machine::new();
    let repo = fixture(&machine.root());
    machine.expect(&repo, &[], &["project", "show"], 0);
    let scratch = machine.root().join("tmp");
    let parked = machine.root().join("parked");
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::create_dir_all(&parked).unwrap();
    let tmpdir = scratch.to_str().unwrap();
    // A complete Survey of this head, which every ordinary refresh below reuses.
    machine.expect(&repo, &[("TMPDIR", tmpdir)], &["project", "refresh"], 0);
    let complete = machine.json(&repo, &[], &["project", "survey", "--json"]);
    assert_eq!(complete["completeness"], "complete");
    // A refresh whose manifest runs are spilled parks its merge's `cat-file --batch`: the git
    // writes its pid under the af that started it and sleeps.
    let bin = fake_git(
        &machine,
        "spill-bin",
        &format!(
            "for argument in \"$@\"; do\n  if [ \"$argument\" = --batch ] && \
             ls -d '{}'/af-tree-digest-$PPID-* >/dev/null 2>&1; then\n    \
             echo $$ > '{}'/$PPID\n    exec sleep 120\n  fi\ndone",
            scratch.display(),
            parked.display()
        ),
    );
    let spills = |pid: u32| -> Vec<PathBuf> {
        std::fs::read_dir(&scratch)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&format!("af-tree-digest-{pid}-"))
            })
            .collect()
    };
    let mut reaped = Reaped {
        children: Vec::new(),
        pids: Vec::new(),
    };
    // Start a refresh that spills and parks; its af's pid and its parked git's.
    let mut park = || -> (u32, String) {
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let child = crate::common::af()
            .current_dir(&repo)
            .env("XDG_STATE_HOME", machine.state_home())
            .env("AF_SELF_OFFLINE", "1")
            .env("AF_TEST_SURVEY_CUTOFF_UNIX_S", CUTOFF.to_string())
            .env("AF_TEST_SURVEY_DIGEST_MANIFEST_BYTES", "300")
            .env("AF_SURVEY__MAX_WALL_MS", "60000")
            .env("TMPDIR", tmpdir)
            .env("PATH", path)
            .args(["project", "refresh", "--force"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        reaped.children.push(child);
        let marker = parked.join(pid.to_string());
        let started = std::time::Instant::now();
        while !marker.exists() {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(30),
                "the refresh {pid} never spilled"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        let git = std::fs::read_to_string(&marker).unwrap().trim().to_string();
        reaped.pids.push(git.clone());
        (pid, git)
    };
    let (live, _) = park();
    let (killed, killed_git) = park();
    assert_eq!(spills(live).len(), 1);
    assert_eq!(spills(killed).len(), 1);
    // af killed after its spill: no drop runs, so its runs stay.
    let mut child = reaped.children.pop().unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    Command::new("kill")
        .args(["-9", &killed_git])
        .status()
        .unwrap();
    assert_eq!(spills(killed).len(), 1, "a killed refresh leaves its spill");

    // The next refresh sweeps it before it decides to reuse the complete Survey, which it
    // does, and keeps the live refresh's.
    let surveys = machine.surveys();
    let (stdout, _) = machine.expect(&repo, &[("TMPDIR", tmpdir)], &["project", "refresh"], 0);
    assert!(stdout.starts_with("unchanged: "), "{stdout}");
    assert_eq!(machine.surveys(), surveys, "the Survey is reused");
    assert!(
        spills(killed).is_empty(),
        "the dead refresh's spill is swept"
    );
    assert_eq!(spills(live).len(), 1, "a live refresh's spill is kept");
    drop(reaped);
    let (stdout, _) = machine.expect(&repo, &[("TMPDIR", tmpdir)], &["project", "refresh"], 0);
    assert!(stdout.starts_with("unchanged: "), "{stdout}");
    assert!(spills(live).is_empty(), "swept once its refresh is gone");
}

#[test]
fn a_log_record_that_does_not_parse_leaves_history_partial_naming_its_commit() {
    let machine = Machine::new();
    let repo = repository(&machine.root(), "malformed", &[("a.txt", b"a\n")]);
    write(&repo, "b.txt", b"b\n");
    commit(&repo, BOB, 1, &["Second"]);
    write(&repo, "c.txt", b"c\n");
    commit(&repo, ADA, 0, &["Third"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let second = git(&repo, &["rev-parse", "HEAD~1"]);
    machine.expect(&repo, &[], &["project", "show"], 0);
    // The history's log, as a git that wrote the `broken`th commit's time as no number would:
    // the second, after the newest was folded, or the newest itself, before anything was.
    for (broken, named, commits, identities, record) in
        [(2, &second, "1", "1", "2"), (1, &head, "0", "0", "1")]
    {
        let bin = fake_git(
            &machine,
            &format!("malformed-bin-{broken}"),
            &format!(
                "for argument in \"$@\"; do\n  if [ \"$argument\" = --numstat ]; then\n    \
                 \"$REAL\" \"$@\" | perl -0 -pe 'if (/^\\d{{9,}}\\0$/) {{ $n++; s/^\\d+/soon/ if $n == \
                 {broken} }}'\n    exit 0\n  fi\ndone"
            ),
        );
        for identity in ["name", "email_hash"] {
            let why = format!("commit {broken}, {identity}");
            let output = af_with_git(
                &machine,
                &repo,
                &bin,
                &[("AF_SURVEY__IDENTITY", identity)],
                &["project", "refresh", "--force"],
            );
            assert_eq!(
                output.status.code(),
                Some(0),
                "{why}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
            schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);
            // Partial, never unavailable: the walk names the commit it stopped in.
            for section in ["history", "contributors"] {
                assert_eq!(survey[section]["completeness"], "partial", "{why}");
                assert_eq!(survey[section]["reason"], "malformed_record", "{why}");
            }
            let history = &survey["history"];
            assert_eq!(history["commits"], commits, "{why}: the commits before it");
            assert_eq!(history["reachable_commits"], "3");
            assert_eq!(history["stopped_in"], named.as_str(), "{why}");
            // Both sections name the record by its ordinal in the log.
            assert_eq!(history["stopped_record"], record, "{why}");
            assert_eq!(survey["contributors"]["stopped_record"], record, "{why}");
            // No identity was taken from it: Ada's newest commit alone, or nobody.
            let contributors = &survey["contributors"];
            assert_eq!(contributors["identities"], identities, "{why}");
            assert_eq!(contributors["identity"], identity, "{why}");
            let stopped = &survey["stopped"];
            assert_eq!(stopped["section"], "history", "{why}");
            assert_eq!(stopped["reason"], "malformed_record");
            assert_eq!(stopped["commit"], named.as_str());
            for section in ["identity", "tree", "decisions", "vocabulary", "checks"] {
                assert_eq!(
                    survey[section]["completeness"], "complete",
                    "{why}: {section}"
                );
            }
        }
    }
    // Nothing folded: zero counts, every share zero.
    let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
    assert_eq!(survey["history"]["merges"], "0");
    assert_eq!(survey["history"]["windows"]["365d"]["commits"], "0");
    assert_eq!(survey["contributors"]["by_identity"], serde_json::json!([]));
    assert_eq!(survey["contributors"]["top_share_bp"], "0");
    let (stdout, _) = machine.expect(&repo, &[], &["project", "survey"], 0);
    let stop = stdout
        .lines()
        .find(|line| line.starts_with("  stopped       history at item "))
        .unwrap_or_else(|| panic!("{stdout}"));
    assert!(
        stop.ends_with(&format!(", commit {} (malformed_record)", &head[..12])),
        "{stop}"
    );
    assert!(
        stdout.contains(&format!(
            "  history       0 commits (0 merges), 0 in 30 days, 0 in 90 days, stopped in record \
             1 (commit {}) (partial: malformed_record)",
            &head[..12]
        )),
        "{stdout}"
    );
}

/// A fake git whose history `log` is the real one passed through the perl substitution `cut`,
/// run over the whole stream at once.
fn cutting_git(machine: &Machine, name: &str, cut: &str) -> PathBuf {
    fake_git(
        machine,
        name,
        &format!(
            "for argument in \"$@\"; do\n  if [ \"$argument\" = --numstat ]; then\n    \
             \"$REAL\" \"$@\" | perl -0777 -pe '{cut}'\n    exit 0\n  fi\ndone"
        ),
    )
}

#[test]
fn a_log_stream_cut_before_its_final_nul_is_partial_history_naming_the_cut_commit() {
    let machine = Machine::new();
    let repo = repository(&machine.root(), "cut", &[("a.txt", b"a\n")]);
    write(&repo, "b.txt", b"b\n");
    commit(&repo, BOB, 1, &["Second"]);
    write(&repo, "c.txt", b"c\n");
    commit(&repo, ADA, 0, &["Third"]);
    let root = git(&repo, &["rev-list", "--max-parents=0", "HEAD"]);
    let second = git(&repo, &["rev-parse", "HEAD~1"]);
    machine.expect(&repo, &[], &["project", "show"], 0);
    // Where each git stops writing, the commit whose record it cut, and the records that ended
    // before it. The newest record ends with its numstat line `c.txt`; the second begins after.
    for (name, cut, named, commits, identities, record) in [
        ("after-numstat", r"s/\0\z//", &root, "2", "2", "3"),
        (
            "after-id",
            r"s/(c\.txt\0[0-9a-f]{40}).*/$1/s",
            &second,
            "1",
            "1",
            "2",
        ),
        (
            "in-header",
            r"s/(c\.txt\0[0-9a-f]{40}\0[0-9a-f]{40}\0\d{4}).*/$1/s",
            &second,
            "1",
            "1",
            "2",
        ),
    ] {
        let bin = cutting_git(&machine, &format!("cut-{name}"), cut);
        let output = af_with_git(
            &machine,
            &repo,
            &bin,
            &[],
            &["project", "refresh", "--force"],
        );
        assert_eq!(
            output.status.code(),
            Some(0),
            "{name}: never SurveyFailed@1: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
        schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);
        assert_eq!(survey["completeness"], "partial", "{name}: never complete");
        for section in ["history", "contributors"] {
            assert_eq!(survey[section]["completeness"], "partial", "{name}");
            assert_eq!(survey[section]["reason"], "malformed_record", "{name}");
        }
        let history = &survey["history"];
        assert_eq!(history["stopped_in"], named.as_str(), "{name}");
        assert_eq!(
            history["commits"], commits,
            "{name}: the records that ended"
        );
        assert_eq!(survey["contributors"]["identities"], identities, "{name}");
        assert_eq!(history["stopped_record"], record, "{name}: the cut record");
        assert_eq!(survey["contributors"]["stopped_record"], record, "{name}");
        assert_eq!(survey["stopped"]["commit"], named.as_str(), "{name}");
    }
    assert!(
        !machine
            .events()
            .iter()
            .any(|event| event == "SurveyFailed@1"),
        "{:?}",
        machine.events()
    );
}

#[test]
fn a_gitlink_is_one_file_of_no_bytes_or_lines_never_fetched_and_digests_as_the_manifest() {
    let machine = Machine::new();
    // A submodule declared at a URL that names nothing on this machine and needs no credential.
    let remote = machine.root().join("absent-remote");
    let gitmodules = format!(
        "[submodule \"vendor/sub\"]\n\tpath = vendor/sub\n\turl = {}\n",
        remote.display()
    );
    let repo = repository(
        &machine.root(),
        "gitlink",
        &[
            ("README.md", b"# x\n"),
            ("src/lib.rs", b"fn x() {}\n"),
            (".gitmodules", gitmodules.as_bytes()),
        ],
    );
    machine.expect(&repo, &[], &["project", "show"], 0);
    machine.expect(&repo, &[], &["project", "refresh"], 0);
    let before = machine.json(&repo, &[], &["project", "survey", "--json"]);
    // A gitlink to a commit this machine does not hold, committed without its checkout.
    let absent = "1234567890abcdef1234567890abcdef12345678";
    git(
        &repo,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{absent},vendor/sub"),
        ],
    );
    git_as(
        &repo,
        Some((ADA.0, ADA.1, 0)),
        &["commit", "-q", "-m", "Add a submodule"],
    );
    assert!(
        git(&repo, &["ls-tree", "-r", "-l", "HEAD"])
            .contains(&format!("160000 commit {absent}       -\tvendor/sub")),
        "the head holds the gitlink"
    );
    // Every git the refresh starts is logged, and every object name a `cat-file --batch` is
    // asked for: none fetches, clones or asks for a submodule, and the gitlink's target is
    // never opened.
    let calls = machine.root().join("git-calls");
    let batches = machine.root().join("git-batches");
    let bin = fake_git(
        &machine,
        "gitlink-bin",
        &format!(
            "echo \"$*\" >> '{}'\nfor argument in \"$@\"; do\n  if [ \"$argument\" = --batch ]; \
             then\n    tee -a '{}' | \"$REAL\" \"$@\"\n    exit $?\n  fi\ndone",
            calls.display(),
            batches.display()
        ),
    );
    let output = af_with_git(&machine, &repo, &bin, &[], &["project", "refresh"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let logged = std::fs::read_to_string(&calls).unwrap();
    for word in ["fetch", "clone", "submodule", absent] {
        assert!(
            !logged.split_whitespace().any(|arg| arg == word),
            "{word}: {logged}"
        );
    }
    assert!(!remote.exists(), "nothing reached the submodule's URL");
    let asked = std::fs::read_to_string(&batches).unwrap();
    assert!(!asked.is_empty(), "the blobs were read through cat-file");
    assert!(
        !asked.contains(absent),
        "the gitlink's target is never asked for: {asked}"
    );

    let after = machine.json(&repo, &[], &["project", "survey", "--json"]);
    schemas::valid(&schemas::validator("project-survey-v2.json"), &after);
    for section in ["identity", "tree"] {
        assert_eq!(after[section]["completeness"], "complete", "{section}");
    }
    let number = |survey: &Value, pointer: &str| -> u64 {
        survey
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or("0")
            .parse()
            .unwrap()
    };
    // One file more, of no bytes and no lines, counted among the files without lines, its
    // language `other`, under its top-level directory.
    for (pointer, more) in [
        ("/tree/files", 1),
        ("/tree/bytes", 0),
        ("/tree/lines", 0),
        ("/tree/files_without_lines", 1),
        ("/tree/languages/other/files", 1),
        ("/tree/languages/other/lines", 0),
        ("/tree/tests/other_files", 1),
    ] {
        assert_eq!(
            number(&after, pointer),
            number(&before, pointer) + more,
            "{pointer}"
        );
    }
    assert_eq!(after["tree"]["files"], "4");
    assert_eq!(after["tree"]["files_without_lines"], "1");
    assert_eq!(
        after["tree"]["directories"]["vendor"],
        serde_json::json!({"files": "1", "lines": "0", "bytes": "0"})
    );
    // The Tree Digest is the committed Manifest's, which leaves a gitlink out: the gitlink
    // adds a file to `tree` and nothing to the digest.
    assert_eq!(after["identity"]["tree_digest"], committed_digest(&repo));
    assert_eq!(
        after["identity"]["tree_digest"],
        before["identity"]["tree_digest"]
    );
}

/// The pending-publication entries of the Project Store, by file: its `<64 hex>.json` files,
/// never the objects the writer stages beside them.
fn pending_entries(machine: &Machine) -> Vec<(PathBuf, Value)> {
    let directory = machine.store().join("pending-surveys");
    let Ok(listing) = std::fs::read_dir(&directory) else {
        return Vec::new();
    };
    let mut entries: Vec<(PathBuf, Value)> = listing
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .map(|path| {
            let value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            (path, value)
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

#[test]
fn a_crash_between_a_surveys_bytes_and_its_event_leaves_its_pending_entry() {
    let machine = Machine::new();
    let repo = repository(&machine.root(), "pending", &[("a.txt", b"a\n")]);
    machine.expect(&repo, &[], &["project", "show"], 0);
    let stop = [("AF_TEST_SURVEY_STOP_BEFORE_EVENT", "1")];

    // Stopped after the bytes and before the event, as a crash would.
    machine.expect(&repo, &stop, &["project", "refresh"], 86);
    assert_eq!(machine.surveys(), 0, "no event names the record");
    let entries = pending_entries(&machine);
    assert_eq!(entries.len(), 1, "{entries:?}");
    let (path, entry) = &entries[0];
    schemas::valid(&schemas::validator("project-survey-pending-v2.json"), entry);
    let survey_id = entry["survey_id"].as_str().unwrap().to_string();
    let hex = survey_id.trim_start_matches("sha256:");
    assert_eq!(
        path.file_name().unwrap().to_str().unwrap(),
        format!("{hex}.json")
    );
    assert_eq!(path.parent().unwrap().parent().unwrap(), machine.store());
    // The entry names the object the crash left in the CAS, by its identity and length.
    let object = machine
        .store()
        .join("cas/objects")
        .join(&hex[..2])
        .join(&hex[2..]);
    let stated = std::fs::symlink_metadata(&object).unwrap();
    assert_eq!(entry["object"], object_identity(&stated));
    assert_eq!(entry["bytes"], stated.len().to_string());

    // The next refresh measures the same bytes, finishes that publication and clears it.
    let (stdout, _) = machine.expect(&repo, &[], &["project", "refresh"], 0);
    assert!(stdout.starts_with("recorded\n"), "{stdout}");
    let recorded = recorded_events(&machine);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].1["survey_id"], survey_id.as_str());
    assert!(pending_entries(&machine).is_empty());
    assert!(
        !machine.store().join("pending-surveys").exists(),
        "a completed refresh leaves none"
    );

    // An entry for another Survey is not this refresh's to clear: a crash at another cutoff
    // leaves one, and the refresh after it records its own Survey and leaves the other.
    let later = (CUTOFF + 60).to_string();
    machine.expect(
        &repo,
        &[
            ("AF_TEST_SURVEY_STOP_BEFORE_EVENT", "1"),
            ("AF_TEST_SURVEY_CUTOFF_UNIX_S", later.as_str()),
        ],
        &["project", "refresh", "--force"],
        86,
    );
    let left = pending_entries(&machine);
    assert_eq!(left.len(), 1);
    machine.expect(&repo, &[], &["project", "refresh", "--force"], 0);
    assert_eq!(machine.surveys(), 2);
    assert_eq!(
        pending_entries(&machine),
        left,
        "another Survey's entry stays"
    );
}

/// Every `SurveyFailed@1` payload of the Project Store, oldest first.
fn failed_events(machine: &Machine) -> Vec<Value> {
    let log = rusqlite::Connection::open(machine.store().join("events.sqlite")).unwrap();
    log.prepare("SELECT payload FROM events WHERE type = 'SurveyFailed@1' ORDER BY sequence")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|payload| serde_json::from_str(&payload.unwrap()).unwrap())
        .collect()
}

/// An object's identity as a pending-publication entry records it: its device, inode and birth
/// time in nanoseconds.
fn object_identity(stated: &std::fs::Metadata) -> Value {
    use std::os::unix::fs::MetadataExt;
    let born = stated
        .created()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    serde_json::json!({
        "device": stated.dev().to_string(),
        "inode": stated.ino().to_string(),
        "born_unix_ns": born.to_string(),
    })
}

/// The object the Surveyor's writer stages for `survey_id` before it takes its CAS name.
fn staged_name(machine: &Machine, survey_id: &str) -> PathBuf {
    machine.store().join("pending-surveys").join(format!(
        "{}.object",
        survey_id.trim_start_matches("sha256:")
    ))
}

/// The CAS file at `survey_id`'s name, as the Surveyor's writer links it.
fn cas_name(machine: &Machine, survey_id: &str) -> PathBuf {
    let hex = survey_id.trim_start_matches("sha256:");
    machine
        .store()
        .join("cas/objects")
        .join(&hex[..2])
        .join(&hex[2..])
}

/// Writer and reader of the pending-publication entry, one fixture for both halves at every
/// crash point: a refresh stopped after its bytes are staged and before their identity leaves
/// the entry without `object` and nothing at the CAS name, and the next refresh of the same
/// bytes finishes that publication; one stopped after the identity and before the staged object
/// takes its CAS name leaves the entry naming the staged object, and the next refresh finishes
/// from it; one stopped after the CAS name and before its event leaves the entry naming the
/// object at the Survey's CAS name by device, inode, birth time and length, which `af storage`
/// counts as a Survey `pending` and `af storage prune --apply` reclaims through that entry, as
/// collection's reader consumes the shape, preview and apply alike.
#[test]
fn the_writers_pending_entry_is_what_collection_reads_at_either_crash_point() {
    let machine = Machine::new();
    let repo = repository(&machine.root(), "pending-both", &[("a.txt", b"a\n")]);
    machine.expect(&repo, &[], &["project", "show"], 0);
    let validator = schemas::validator("project-survey-pending-v2.json");

    // Crash point one: the bytes staged, no identity yet, nothing at the CAS name.
    machine.expect(
        &repo,
        &[("AF_TEST_SURVEY_STOP_BEFORE_IDENTITY", "1")],
        &["project", "refresh"],
        86,
    );
    assert_eq!(machine.surveys(), 0, "no event names the record");
    let entries = pending_entries(&machine);
    assert_eq!(entries.len(), 1, "{entries:?}");
    let (path, entry) = &entries[0];
    schemas::valid(&validator, entry);
    assert!(entry.get("object").is_none(), "no identity yet: {entry}");
    let survey_id = entry["survey_id"].as_str().unwrap().to_string();
    let object = cas_name(&machine, &survey_id);
    assert!(
        std::fs::symlink_metadata(&object).is_err(),
        "nothing is at the CAS name before the entry names it"
    );
    let staged = std::fs::symlink_metadata(staged_name(&machine, &survey_id)).unwrap();
    assert!(staged.file_type().is_file(), "the bytes are staged");
    assert_eq!(entry["bytes"], staged.len().to_string());
    assert!(std::fs::read(path).unwrap().len() <= 4096);
    // The same bytes again: the publication finishes and the entry goes.
    let (stdout, _) = machine.expect(&repo, &[], &["project", "refresh"], 0);
    assert!(stdout.starts_with("recorded\n"), "{stdout}");
    let recorded = recorded_events(&machine);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].1["survey_id"], survey_id.as_str());
    assert!(object.is_file(), "the record took its CAS name");
    assert!(
        pending_entries(&machine).is_empty(),
        "a finished publication"
    );
    assert!(!staged_name(&machine, &survey_id).exists());

    // Crash point between: the staged object's identity recorded, not yet at its CAS name. The
    // next refresh of the same bytes finishes from that object, never staging another.
    let between = (CUTOFF + 30).to_string();
    let at_between = [("AF_TEST_SURVEY_CUTOFF_UNIX_S", between.as_str())];
    machine.expect(
        &repo,
        &[
            ("AF_TEST_SURVEY_STOP_BEFORE_LINK", "1"),
            ("AF_TEST_SURVEY_CUTOFF_UNIX_S", between.as_str()),
        ],
        &["project", "refresh", "--force"],
        86,
    );
    let entries = pending_entries(&machine);
    assert_eq!(entries.len(), 1, "{entries:?}");
    let (_, entry) = entries[0].clone();
    schemas::valid(&validator, &entry);
    let linked_later = entry["survey_id"].as_str().unwrap().to_string();
    let staged = staged_name(&machine, &linked_later);
    let stated = std::fs::symlink_metadata(&staged).unwrap();
    assert_eq!(
        entry["object"],
        object_identity(&stated),
        "the staged object"
    );
    assert!(!cas_name(&machine, &linked_later).exists());
    machine.expect(&repo, &at_between, &["project", "refresh", "--force"], 0);
    let finished = std::fs::symlink_metadata(cas_name(&machine, &linked_later)).unwrap();
    assert_eq!(
        object_identity(&finished),
        object_identity(&stated),
        "the very inode the entry named took the CAS name"
    );
    assert!(pending_entries(&machine).is_empty() && !staged.exists());

    // Crash point two: the identity recorded, no event. A later cutoff gives other bytes.
    let later = (CUTOFF + 60).to_string();
    machine.expect(
        &repo,
        &[
            ("AF_TEST_SURVEY_STOP_BEFORE_EVENT", "1"),
            ("AF_TEST_SURVEY_CUTOFF_UNIX_S", later.as_str()),
        ],
        &["project", "refresh", "--force"],
        86,
    );
    let entries = pending_entries(&machine);
    assert_eq!(entries.len(), 1, "{entries:?}");
    let (entry_path, entry) = entries[0].clone();
    schemas::valid(&validator, &entry);
    let unrecorded = entry["survey_id"].as_str().unwrap().to_string();
    assert_ne!(unrecorded, survey_id);
    let object = cas_name(&machine, &unrecorded);
    let stated = std::fs::symlink_metadata(&object).unwrap();
    assert_eq!(
        entry["object"],
        object_identity(&stated),
        "the identity is the file's at the Survey's CAS name"
    );
    assert_eq!(entry["bytes"], stated.len().to_string());
    assert_eq!(machine.surveys(), 2, "no event names it");

    // The reader: the object is a Survey `pending`, and recovery reclaims it and then the
    // entry, preview and apply alike; nothing else moves.
    let shown = machine.json(&repo, &[], &["storage", "--json"]);
    let pending = shown["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|listed| listed["path"] == object.to_str().unwrap())
        .unwrap_or_else(|| panic!("the object is inventoried: {shown}"));
    assert_eq!(pending["kind"], "survey");
    assert_eq!(pending["state"], "pending", "{pending}");
    let recoveries = |document: &Value| -> Vec<String> {
        document["removals"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|removal| removal["rule"] == "recovery")
            .map(|removal| removal["path"].as_str().unwrap().to_string())
            .collect()
    };
    let planned = recoveries(&prune_surveys(&machine, &repo, "50", false));
    let expected = vec![
        object.to_string_lossy().into_owned(),
        entry_path.to_string_lossy().into_owned(),
    ];
    assert_eq!(planned, expected, "the object, then its entry");
    let applied = prune_surveys(&machine, &repo, "50", true);
    assert_eq!(recoveries(&applied), expected, "{applied}");
    assert!(!object.exists() && !entry_path.exists());
    assert!(pending_entries(&machine).is_empty());
    assert!(
        cas_name(&machine, &survey_id).is_file(),
        "the recorded Survey stays"
    );
    assert_eq!(machine.surveys(), 2);
    assert!(
        tombstones(&machine).is_empty(),
        "recovery appends no tombstone"
    );
}

/// Every refresh of a registered Project that stops without a Survey leaves a record of the
/// attempt: a deadline before `rev-parse` answered, a newest Survey that does not replay, and a
/// record the CAS cannot take each record `SurveyFailed@1` naming the head, which `af project
/// show` prints, and the command still fails.
#[test]
fn every_refresh_that_stops_without_a_survey_records_its_failure() {
    use std::os::unix::fs::PermissionsExt;
    let machine = Machine::new();
    let repo = repository(&machine.root(), "failures", &[("a.txt", b"a\n")]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    machine.expect(&repo, &[], &["project", "show"], 0);

    // The deadline before `rev-parse` answered: the head is still named.
    let (_, stderr) = machine.expect(
        &repo,
        &[("AF_SURVEY__MAX_WALL_MS", "1")],
        &["project", "refresh"],
        1,
    );
    assert!(
        stderr.contains("could not be computed within max_wall_ms"),
        "{stderr}"
    );
    let failed = failed_events(&machine);
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0]["head"], head.as_str());
    assert_eq!(
        failed[0]["reason"],
        "the survey key could not be computed within max_wall_ms"
    );
    schemas::valid(
        &schemas::validator("project-survey-failed-v1.json"),
        &failed[0],
    );
    let (stdout, _) = machine.expect(&repo, &[], &["project", "show"], 0);
    assert!(
        stdout.contains(&format!("Survey failed at {}", &head[..12])),
        "{stdout}"
    );

    // A record the CAS cannot take: the entry begun stays for collection, the failure names
    // the head, and no path or `@` enters the event.
    let objects = machine.store().join("cas/objects");
    std::fs::create_dir_all(&objects).unwrap();
    std::fs::set_permissions(&objects, std::fs::Permissions::from_mode(0o500)).unwrap();
    let (_, stderr) = machine.expect(&repo, &[], &["project", "refresh"], 1);
    std::fs::set_permissions(&objects, std::fs::Permissions::from_mode(0o700)).unwrap();
    let failed = failed_events(&machine);
    assert_eq!(failed.len(), 2, "{stderr}");
    assert_eq!(failed[1]["head"], head.as_str());
    assert_eq!(
        failed[1]["reason"],
        "the Survey could not be published in the Project Store; run `af project refresh` again"
    );
    assert_eq!(machine.surveys(), 0);
    assert_eq!(pending_entries(&machine).len(), 1, "the entry begun stays");

    // A newest Survey that does not replay: refused as before, and recorded.
    machine.expect(&repo, &[], &["project", "refresh"], 0);
    assert!(
        pending_entries(&machine).is_empty(),
        "the same bytes finished it"
    );
    let sequence = recorded_sequences(&machine)[0];
    mismatch_refs_digest(&machine, sequence);
    let said = refusal(&machine, &repo, &[], &["project", "refresh"]);
    assert!(
        said.contains(&format!(
            "SurveyRecorded@1 at sequence {sequence} names a refs_digest"
        )),
        "{said}"
    );
    let failed = failed_events(&machine);
    assert_eq!(failed.len(), 3);
    assert_eq!(failed[2]["head"], head.as_str());
    let reason = failed[2]["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("the Project Store's newest Survey does not replay"),
        "{reason}"
    );
    assert!(!reason.contains('@') && !reason.contains('/'), "{reason}");
}

/// The spill sweep runs on every refresh before and after the Tree Digest, on every way out of
/// the digest: a spill a dead Surveyor left after this refresh's first sweep (planted while its
/// key is observed) is gone once the refresh ends, after a complete digest and after one a
/// ceiling stopped, while the directory of a process that still runs is kept.
#[test]
fn the_spill_sweep_runs_after_the_tree_digest_on_every_way_out() {
    let machine = Machine::new();
    let repo = fixture(&machine.root());
    machine.expect(&repo, &[], &["project", "show"], 0);
    let scratch = machine.root().join("sweep-tmp");
    std::fs::create_dir_all(&scratch).unwrap();
    let dead = scratch.join("af-tree-digest-2000000000-planted");
    let live = scratch.join(format!("af-tree-digest-{}-live", std::process::id()));
    // The key's one ref observation runs after the refresh's first sweep and before the digest.
    let bin = fake_git(
        &machine,
        "plant-bin",
        &format!(
            "for argument in \"$@\"; do\n  if [ \"$argument\" = for-each-ref ]; then\n    \
             mkdir -p '{}/runs' '{}'\n  fi\ndone",
            dead.display(),
            live.display()
        ),
    );
    let tmpdir = scratch.to_str().unwrap();
    for (env, completeness, why) in [
        (
            vec![("TMPDIR", tmpdir)],
            "complete",
            "a digest that finished",
        ),
        (
            vec![
                ("TMPDIR", tmpdir),
                ("AF_TEST_SURVEY_DIGEST_MANIFEST_BYTES", "300"),
                ("AF_TEST_SURVEY_DIGEST_TEMP_BYTES", "1"),
            ],
            "partial",
            "a digest a ceiling stopped",
        ),
    ] {
        let output = af_with_git(
            &machine,
            &repo,
            &bin,
            &env,
            &["project", "refresh", "--force"],
        );
        assert_eq!(
            output.status.code(),
            Some(0),
            "{why}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
        assert_eq!(survey["completeness"], completeness, "{why}");
        assert!(!dead.exists(), "{why}: the dead Surveyor's spill is swept");
        assert!(live.is_dir(), "{why}: a live process's directory is kept");
        let left: Vec<String> = std::fs::read_dir(&scratch)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("af-tree-digest-") && !live.ends_with(name))
            .collect();
        assert!(left.is_empty(), "{why}: nothing else is left: {left:?}");
    }
}

/// Every streamed record is read up to 64 KiB and no further, each with its own fixture: a log
/// field (history and contributors partial with `max_record_bytes`, naming the record by its
/// ordinal), a tree listing record (`tree` partial, and the declarations' sections with it,
/// since they did not see the whole listing) and a ref of the survey key (`SurveyFailed@1`, since
/// the key cannot be complete).
#[test]
fn every_streamed_record_over_64_kib_stops_at_max_record_bytes() {
    let machine = Machine::new();
    let repo = repository(&machine.root(), "records", &[("a.txt", b"a\n")]);
    write(&repo, "b.txt", b"b\n");
    commit(&repo, BOB, 1, &["Second"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let root = git(&repo, &["rev-parse", "HEAD~1"]);
    machine.expect(&repo, &[], &["project", "show"], 0);
    let long = 64 * 1024 + 1;

    // Ada's author name, in the log's second record (the root commit's), over 64 KiB.
    let bin = fake_git(
        &machine,
        "long-log-bin",
        &format!(
            "for argument in \"$@\"; do\n  if [ \"$argument\" = --numstat ]; then\n    \
             \"$REAL\" \"$@\" | perl -0 -pe '$_ = (\"n\" x {long}) . \"\\0\" if $_ eq \
             \"Ada\\0\"'\n    exit 0\n  fi\ndone"
        ),
    );
    let output = af_with_git(
        &machine,
        &repo,
        &bin,
        &[],
        &["project", "refresh", "--force"],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
    schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);
    for section in ["history", "contributors"] {
        assert_eq!(survey[section]["completeness"], "partial", "{section}");
        assert_eq!(survey[section]["reason"], "max_record_bytes", "{section}");
        assert_eq!(survey[section]["stopped_record"], "2", "{section}");
    }
    assert_eq!(survey["history"]["stopped_in"], root.as_str());
    assert_eq!(survey["history"]["commits"], "1");
    assert_eq!(survey["tree"]["completeness"], "complete");

    // One record of the tree's listing over 64 KiB, after every real one.
    let bin = fake_git(
        &machine,
        "long-listing-bin",
        &format!(
            "listing=0\nfor argument in \"$@\"; do\n  [ \"$argument\" = ls-tree ] && \
             listing=$((listing + 1))\n  [ \"$argument\" = -l ] && listing=$((listing + 1))\n\
             done\nif [ $listing = 2 ]; then\n  \"$REAL\" \"$@\"\n  perl -e 'print \"100644 \
             blob \" . (\"0\" x 40) . \" 1\\t\" . (\"x\" x {long}) . \"\\0\"'\n  exit 0\nfi"
        ),
    );
    let output = af_with_git(
        &machine,
        &repo,
        &bin,
        &[],
        &["project", "refresh", "--force"],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
    schemas::valid(&schemas::validator("project-survey-v2.json"), &survey);
    assert_eq!(survey["tree"]["completeness"], "partial");
    assert_eq!(survey["tree"]["reason"], "max_record_bytes");
    assert_eq!(
        survey["tree"]["files"], "2",
        "the entries before it counted"
    );
    for section in ["decisions", "vocabulary", "checks"] {
        assert_eq!(survey[section]["reason"], "max_record_bytes", "{section}");
    }
    assert_eq!(survey["history"]["completeness"], "complete");

    // A ref of the survey key over 64 KiB: no key, so no Survey, and the failure recorded.
    let surveys = machine.surveys();
    let bin = fake_git(
        &machine,
        "long-ref-bin",
        &format!(
            "for argument in \"$@\"; do\n  if [ \"$argument\" = for-each-ref ]; then\n    \
             \"$REAL\" \"$@\"\n    perl -e 'print \"refs/tags/\" . (\"t\" x {long}) . \"\\n\"'\n    \
             exit 0\n  fi\ndone"
        ),
    );
    let output = af_with_git(
        &machine,
        &repo,
        &bin,
        &[],
        &["project", "refresh", "--force"],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(machine.surveys(), surveys, "no Survey without its key");
    let failed = failed_events(&machine);
    let last = failed.last().unwrap();
    assert_eq!(last["head"], head.as_str());
    assert_eq!(
        last["reason"],
        "the survey key could not be computed: a ref is longer than 65536 bytes \
         (max_record_bytes)"
    );
}

/// The `reason` of every `SurveyFailed@1` of the Project Store, in order.
fn failure_reasons(machine: &Machine) -> Vec<String> {
    let log = rusqlite::Connection::open(machine.store().join("events.sqlite")).unwrap();
    log.prepare("SELECT payload FROM events WHERE type = 'SurveyFailed@1' ORDER BY sequence")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|payload| {
            let payload: Value = serde_json::from_str(&payload.unwrap()).unwrap();
            payload["reason"].as_str().unwrap().to_string()
        })
        .collect()
}

/// The Project Store's lock honours the Survey deadline (ADR-0158 clause 2): while another
/// process holds `project.lock`, a refresh tries it until its `max_wall_ms` deadline and then
/// stops with the bounded failure `store_locked`, recorded in the log while the holder still
/// holds the lock, and never hangs. Released, the next refresh records a Survey.
#[test]
fn a_held_project_store_lock_stops_a_refresh_with_store_locked_within_its_deadline() {
    let machine = Machine::new();
    let repo = repository(&machine.root(), "locked", &[("README.md", b"# Locked\n")]);
    machine.expect(&repo, &[], &["project", "show"], 0);
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .open(machine.store().join("project.lock"))
        .unwrap();
    let held = nix::fcntl::Flock::lock(lock, nix::fcntl::FlockArg::LockExclusiveNonblock)
        .map_err(|(_, errno)| errno)
        .unwrap();
    let max_wall_ms: u64 = 1500;
    for force in [false, true] {
        let mut args = vec!["project", "refresh"];
        if force {
            args.push("--force");
        }
        let started = std::time::Instant::now();
        let output = machine.af(
            &repo,
            &[("AF_SURVEY__MAX_WALL_MS", &max_wall_ms.to_string())],
            &args,
        );
        let took = started.elapsed();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{stderr}");
        assert!(stderr.contains("(store_locked)"), "{stderr}");
        assert!(
            took >= std::time::Duration::from_millis(max_wall_ms)
                && took < std::time::Duration::from_millis(max_wall_ms + 2000),
            "the refresh took {took:?}"
        );
        // Recorded while the holder still holds the lock.
        assert_eq!(machine.events().last().unwrap(), "SurveyFailed@1");
        assert!(
            failure_reasons(&machine)
                .last()
                .unwrap()
                .starts_with("the Project Store is locked (store_locked)"),
            "{:?}",
            failure_reasons(&machine)
        );
    }
    assert_eq!(machine.surveys(), 0);
    drop(held);
    machine.expect(&repo, &[], &["project", "refresh"], 0);
    assert_eq!(machine.surveys(), 1);
}

/// The writer opens `pending-surveys` as collection's reader does, never through a link: one
/// replaced by a symbolic link to a writable directory outside the Store refuses the refresh,
/// naming the path, records `SurveyFailed@1`, and nothing is written where the link points.
#[test]
fn a_pending_directory_replaced_by_a_link_refuses_the_refresh_and_nothing_is_written_outside() {
    let machine = Machine::new();
    let repo = repository(&machine.root(), "linked", &[("README.md", b"# Linked\n")]);
    machine.expect(&repo, &[], &["project", "refresh"], 0);
    let pending = machine.store().join("pending-surveys");
    assert!(!pending.exists(), "a published Survey leaves no entry");
    let outside = machine.root().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, &pending).unwrap();

    let (_, stderr) = machine.expect(&repo, &[], &["project", "refresh", "--force"], 1);
    assert!(
        stderr.contains(&format!(
            "{} is refused: it is a symbolic link or not a directory",
            pending.display()
        )),
        "{stderr}"
    );
    assert_eq!(
        std::fs::read_dir(&outside).unwrap().count(),
        0,
        "nothing outside"
    );
    assert!(
        std::fs::symlink_metadata(&pending)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(machine.surveys(), 1);
    assert_eq!(machine.events().last().unwrap(), "SurveyFailed@1");
}

/// A Survey record of the superseded `af/ProjectSurvey@1` (ADR-0145) in a Project Store, as an
/// earlier build wrote it: `af project history` lists it, text and `--json`
/// (`af/project-history@2`), by its ID and time as `unreadable: superseded version` and none of
/// its event's fields; `af project survey` refuses it naming the version, `af project show`
/// says it, a refresh never reuses it and records a Survey of this release after it, and its
/// record is never deleted.
#[test]
fn a_superseded_survey_record_is_listed_unreadable_kept_and_never_reused() {
    let machine = Machine::new();
    let repo = repository(&machine.root(), "superseded", &[("README.md", b"# Old\n")]);
    machine.expect(&repo, &[], &["project", "refresh"], 0);
    let (sequence, event) = recorded_events(&machine).pop().unwrap();
    let current = event["survey_id"].as_str().unwrap().to_string();
    let bytes = std::fs::read(survey_object(&machine, &current)).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("\"schema\":\"af/ProjectSurvey@2\""), "{text}");
    let old = text.replace("af/ProjectSurvey@2", "af/ProjectSurvey@1");
    let old_id = review_store::canonical::blob_content_id(old.as_bytes());
    let hex = old_id.trim_start_matches("sha256:");
    let object = machine
        .store()
        .join("cas/objects")
        .join(&hex[..2])
        .join(&hex[2..]);
    std::fs::create_dir_all(object.parent().unwrap()).unwrap();
    std::fs::write(&object, &old).unwrap();
    std::fs::remove_file(survey_object(&machine, &current)).unwrap();
    rewrite_event(
        &machine,
        sequence,
        "survey_id",
        &Value::from(old_id.clone()),
    );
    let log = rusqlite::Connection::open(machine.store().join("events.sqlite")).unwrap();
    log.execute(
        "UPDATE events SET artifact_refs = ?1 WHERE sequence = ?2 AND type = 'SurveyRecorded@1'",
        rusqlite::params![serde_json::json!([old_id]).to_string(), sequence],
    )
    .unwrap();

    let (listed, _) = machine.expect(&repo, &[], &["project", "history"], 0);
    assert!(
        listed.lines().any(|line| line.starts_with(old_id.as_str())
            && line.ends_with("unreadable: superseded version")
            && !line.contains("head")),
        "{listed}"
    );
    let history = machine.json(&repo, &[], &["project", "history", "--json"]);
    schemas::valid(&schemas::validator("project-history-v2.json"), &history);
    assert_eq!(
        history["surveys"][0],
        serde_json::json!({
            "survey_id": old_id,
            "recorded_unix_ms": event["recorded_unix_ms"],
            "unreadable": "superseded version",
        })
    );
    let refused = refusal(&machine, &repo, &[], &["project", "survey"]);
    assert!(
        refused.contains("it is af/ProjectSurvey@1, a superseded version another af release wrote"),
        "{refused}"
    );
    let (shown, _) = machine.expect(&repo, &[], &["project", "show"], 0);
    assert!(
        shown.contains(&format!(
            "The newest Survey {old_id} is unreadable: superseded version"
        )),
        "{shown}"
    );

    // Never reused: the unchanged key records a Survey of this release, chained after it.
    let (refreshed, _) = machine.expect(&repo, &[], &["project", "refresh"], 0);
    assert!(refreshed.starts_with("recorded"), "{refreshed}");
    let newest = machine.json(&repo, &[], &["project", "survey", "--json"]);
    assert_eq!(newest["schema"], "af/ProjectSurvey@2");
    assert_eq!(newest["previous_survey_id"], old_id.as_str());
    let history = machine.json(&repo, &[], &["project", "history", "--json"]);
    schemas::valid(&schemas::validator("project-history-v2.json"), &history);
    assert_eq!(history["surveys"].as_array().unwrap().len(), 2);
    assert_eq!(history["surveys"][1]["unreadable"], "superseded version");
    assert_eq!(
        std::fs::read_to_string(&object).unwrap(),
        old,
        "never deleted"
    );
}
