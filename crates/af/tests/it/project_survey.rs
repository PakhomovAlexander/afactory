//! ADR-0152 end to end: the Surveyor measures a Project into Surveys.
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

/// The fixture repository of ADR-0152's test (a): eight commits over 500 days by two people, a
/// bot and an author named by an email address; a human commit with an agent trailer and one
/// with two; a merge; a rename and a binary change in a commit authored a year before it was
/// committed; two tags, two branches, several top-level directories, a tests directory, a
/// binary blob, a blob over `max_file_bytes = 512`, three ADRs and a README beside them, a
/// CONTEXT.md, a code policy with two checks, a review pipeline with one, and one workflow file.
fn fixture(root: &Path) -> PathBuf {
    let repo = root.join("survey-fixture");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    write(
        &repo,
        ".af/af.toml",
        b"version = 1\n\n[project]\nname = \"survey-fixture\"\nmin_af = \"0.1\"\n\n[defaults]\n\
          pipeline = \"review\"\n\n[survey]\nmax_file_bytes = 512\n",
    );
    write(&repo, "README.md", b"# Fixture\n\nTwo more lines.\n");
    write(&repo, "src/lib.rs", b"pub fn one() -> u32 {\n    1\n}\n");
    write(&repo, "assets/logo.bin", b"PNG\0\x01\x02\nbinary\n");
    commit(&repo, ADA, 500, &["Start"]);

    write(
        &repo,
        "docs/adr/0001-first.md",
        b"# ADR-0001: First\n\n**Status:** accepted (2026-01-02)\n\nText.\n",
    );
    write(
        &repo,
        "CONTEXT.md",
        b"# Context\n\n**Project**:\nThe repository.\n\n**Survey**:\nOne record.\n",
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
    schemas::valid(&schemas::validator("project-survey-v1.json"), &survey);

    // The record in the CAS is those bytes, and its ID is what the log names.
    let history = machine.json(&repo, &[], &["project", "history", "--json"]);
    schemas::valid(&schemas::validator("project-history-v1.json"), &history);
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
    assert_eq!(tree["files"], "16");
    assert_eq!(
        tree["files_without_lines"], "2",
        "the binary and the big blob"
    );
    assert_eq!(tree["directories"]["."]["files"], "4");
    assert_eq!(tree["tests"]["files"], "1");
    assert_eq!(tree["languages"]["rust"]["lines"], "9");
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
    assert_eq!(contributors["by_identity"]["Ada"]["commits"], "4");
    assert_eq!(contributors["by_identity"]["Bob"]["commits"], "2");
    let decisions = &survey["decisions"];
    assert_eq!(
        decisions["count"], "3",
        "docs/adr/README.md is not a decision"
    );
    assert_eq!(decisions["by_status"]["accepted"], "2");
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
        serde_json::json!([{"name": "Project"}, {"name": "Survey"}])
    );
    let declared = survey["checks"]["declared"].as_array().unwrap();
    assert_eq!(declared.len(), 3);
    assert!(declared.iter().any(|check| check["program"] == "make"));
    assert_eq!(survey["checks"]["ci_workflows"], "1");
    let identity = &survey["identity"];
    assert_eq!(identity["name"], "survey-fixture");
    assert_eq!(identity["head"], git(&repo, &["rev-parse", "HEAD"]));
    assert_eq!(identity["cutoff_unix_s"], CUTOFF.to_string());
    assert_eq!(identity["surveyor_version"], "2");
    assert_eq!(identity["git_version"], git_version().as_str());
    let refs = git(
        &repo,
        &[
            "for-each-ref",
            "--format=%(refname)%09%(objectname)",
            "refs/heads",
            "refs/tags",
        ],
    );
    assert_eq!(
        identity["refs_digest"],
        format!(
            "sha256:{}",
            review_core::hex::encode(&Sha256::digest(format!("{refs}\n")))
        )
    );

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
    schemas::valid(&schemas::validator("project-survey-v1.json"), &cut);
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
        "unchanged: head {}, refs {}, policy {}, surveyor 2",
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
    let (stdout, stderr) = machine.expect(
        &repo,
        &[("AF_SURVEY__MAX_WALL_MS", "1")],
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
    schemas::valid(&schemas::validator("project-survey-v1.json"), &survey);
    assert_eq!(survey["completeness"], "partial");
    let identity = &survey["identity"];
    assert_eq!(identity["completeness"], "partial");
    assert_eq!(identity["reason"], "max_wall_ms");
    assert!(identity.get("tree_digest").is_none(), "{identity}");
    assert_eq!(identity["tree_digest_reason"], "max_wall_ms");
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
    schemas::valid(&schemas::validator("project-survey-v1.json"), &survey);
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
    schemas::valid(&schemas::validator("project-survey-v1.json"), &survey);
    assert_eq!(survey["tree"]["completeness"], "partial");
    assert_eq!(survey["tree"]["reason"], "max_paths");
    assert_eq!(survey["tree"]["files"], "3");
    assert_eq!(survey["tree"]["seen"]["paths"], "16");
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
    schemas::valid(&schemas::validator("project-survey-v1.json"), &survey);
    let history = &survey["history"];
    assert_eq!(history["reason"], "max_entries");
    assert_eq!(history["hot_paths"].as_array().unwrap().len(), 2);
    assert_eq!(history["churn_90d"].as_object().unwrap().len(), 2);
    assert_eq!(history["seen"]["churn_90d"], "7");
    assert_eq!(history["seen"]["hot_paths"], "13");
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
    write(&repo, "r0.txt", b"0\n");
    commit(
        &repo,
        (long.as_str(), "long@fixture.invalid"),
        9,
        &["Root 0"],
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
    schemas::valid(&schemas::validator("project-survey-v1.json"), &survey);
    assert_eq!(survey["identity"]["repository_id"], repository_id.as_str());
    let by_identity = survey["contributors"]["by_identity"].as_object().unwrap();
    let (name, entry) = by_identity
        .iter()
        .find(|(name, _)| name.starts_with("Ada x"))
        .unwrap();
    assert_eq!(name.len(), 256);
    assert!(long.starts_with(name.as_str()));
    assert_eq!(entry["truncated"], true);
    assert!(by_identity["Ada"].get("truncated").is_none());
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
    schemas::valid(&schemas::validator("project-survey-v1.json"), &survey);

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
        let identities: Vec<&String> = survey["contributors"]["by_identity"]
            .as_object()
            .unwrap()
            .keys()
            .collect();
        assert_eq!(survey["contributors"]["identity"], identity);
        let rule = |text: &str| {
            review_core::hex::encode(&Sha256::digest(text.trim().to_lowercase()))[..16].to_string()
        };
        if identity == "email_hash" {
            let mut expected: Vec<String> = [ADA.1, BOB.1, BOT.1, CAROL.1]
                .iter()
                .map(|email| rule(email))
                .collect();
            expected.sort();
            assert_eq!(identities, expected.iter().collect::<Vec<_>>());
        } else {
            // The author named by an address is `h:` and the hash of the name.
            let carol = format!("h:{}", rule(CAROL.0));
            assert_eq!(identities, ["Ada", "Bob", carol.as_str(), "renovate[bot]"]);
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
