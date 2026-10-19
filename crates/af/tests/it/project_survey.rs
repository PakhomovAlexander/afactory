//! ADR-0152 end to end: the Surveyor measures a Project into Surveys.
//!
//! A fixture repository built with fixed dates, names, emails and messages yields a Survey
//! byte-identical to `tests/fixtures/project-survey/expected.json` at a fixed cutoff; a second
//! refresh of the same head records nothing and prints `unchanged`; a new commit records a
//! Survey chained to the first. `max_commits = 1` and `max_wall_ms = 1` record partial Surveys
//! with the bound named, exit 0. No email reaches a record, an event, a document or an output
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
/// Every email the fixture's history holds, the Co-Authored-By trailer's included.
const EMAILS: [&str; 4] = [
    "ada@fixture.invalid",
    "bob@fixture.invalid",
    "bot@fixture.invalid",
    "noreply@anthropic.com",
];
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
    let mut command = Command::new("git");
    command
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("TZ", "UTC");
    let (name, email, days) = who.unwrap_or(("Fixture", "fixture@example.invalid", 0));
    let date = format!("@{} +0000", CUTOFF - days * DAY);
    command
        .env("GIT_AUTHOR_NAME", name)
        .env("GIT_AUTHOR_EMAIL", email)
        .env("GIT_COMMITTER_NAME", name)
        .env("GIT_COMMITTER_EMAIL", email)
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date);
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

fn commit(repo: &Path, who: (&str, &str), days: u64, message: &[&str]) {
    git(repo, &["add", "-A"]);
    let mut args = vec!["commit", "-q", "--allow-empty"];
    for paragraph in message {
        args.extend(["-m", paragraph]);
    }
    git_as(repo, Some((who.0, who.1, days)), &args);
}

/// The fixture repository of ADR-0152's test (a): six commits over 500 days by two people and
/// a bot, one of them with an agent trailer, a merge, two tags, several top-level directories,
/// a tests directory, a binary blob, a blob over `max_file_bytes = 512`, two ADRs, a CONTEXT.md,
/// a code policy with two checks, a review pipeline with one, and one workflow file.
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

#[test]
fn the_fixture_repository_yields_the_checked_in_survey_byte_for_byte() {
    let machine = Machine::new();
    let repo = fixture(&machine.root());
    let before = checkout_state(&repo);
    assert_eq!(before.0, "", "the fixture is clean");

    let (stdout, _) = machine.expect(&repo, &[], &["project", "refresh", "--json"], 0);
    assert_eq!(
        stdout, EXPECTED,
        "the Survey drifted from the checked-in record"
    );
    let (stdout, _) = machine.expect(&repo, &[], &["project", "survey", "--json"], 0);
    assert_eq!(stdout, EXPECTED);
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
        EXPECTED.trim_end()
    );

    // The counts the fixture was built to have, so the expected record is not merely recorded.
    let tree = &survey["tree"];
    assert_eq!(tree["files"], "14");
    assert_eq!(
        tree["files_without_lines"], "2",
        "the binary and the big blob"
    );
    assert_eq!(tree["directories"]["."]["files"], "4");
    assert_eq!(tree["tests"]["files"], "1");
    assert_eq!(tree["languages"]["rust"]["lines"], "9");
    let history = &survey["history"];
    assert_eq!(history["completeness"], "complete");
    assert_eq!(history["commits"], "6");
    assert_eq!(history["merges"], "1");
    assert_eq!(history["windows"]["30d"]["commits"], "2");
    assert_eq!(history["windows"]["90d"]["commits"], "4");
    assert_eq!(history["windows"]["365d"]["commits"], "5");
    assert_eq!(history["tags"]["count"], "2");
    assert_eq!(history["tags"]["release"], serde_json::json!(["v1.0.0"]));
    assert_eq!(history["local_branches"], "2");
    assert!(
        history["hot_paths"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hot| hot["path"] == "notes.txt" && hot["removed"] == true),
        "{history}"
    );
    let contributors = &survey["contributors"];
    assert_eq!(contributors["identities"], "3");
    assert_eq!(
        contributors["agent_commits"], "2",
        "the bot and the trailer"
    );
    assert_eq!(contributors["agent_share_bp"], "3333");
    assert_eq!(contributors["by_identity"]["Ada"]["commits"], "3");
    assert_eq!(survey["decisions"]["by_status"]["accepted"], "1");
    assert_eq!(survey["decisions"]["by_status"]["proposed"], "1");
    assert_eq!(
        survey["vocabulary"]["terms"],
        serde_json::json!(["Project", "Survey"])
    );
    let declared = survey["checks"]["declared"].as_array().unwrap();
    assert_eq!(declared.len(), 3);
    assert!(declared.iter().any(|check| check["program"] == "make"));
    assert_eq!(survey["checks"]["ci_workflows"], "1");
    assert_eq!(survey["identity"]["name"], "survey-fixture");
    assert_eq!(
        survey["identity"]["head"],
        git(&repo, &["rev-parse", "HEAD"])
    );
    assert_eq!(survey["identity"]["cutoff_unix_s"], CUTOFF.to_string());

    assert_eq!(
        checkout_state(&repo),
        before,
        "nothing was written in the checkout"
    );
}

#[test]
fn an_unchanged_head_records_nothing_and_a_new_commit_chains_a_survey() {
    let machine = Machine::new();
    let repo = fixture(&machine.root());
    let (first, _) = machine.expect(&repo, &[], &["project", "refresh"], 0);
    assert!(first.starts_with("recorded\nSurvey sha256:"), "{first}");
    let events = machine.events();
    assert_eq!(machine.surveys(), 1, "{events:?}");

    let (again, _) = machine.expect(&repo, &[], &["project", "refresh"], 0);
    assert!(again.starts_with("unchanged\n"), "{again}");
    assert_eq!(
        again.lines().nth(1),
        first.lines().nth(1),
        "the newest Survey"
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
fn max_commits_and_max_wall_ms_record_partial_surveys_that_name_the_bound() {
    let machine = Machine::new();
    let repo = fixture(&machine.root());
    // The generic environment form of `[survey] max_commits` (`af help environment`).
    let cut = [("AF_SURVEY__MAX_COMMITS", "1")];
    machine.expect(&repo, &cut, &["project", "refresh"], 0);
    let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
    schemas::valid(&schemas::validator("project-survey-v1.json"), &survey);
    assert_eq!(survey["completeness"], "partial");
    for section in ["history", "contributors"] {
        assert_eq!(survey[section]["completeness"], "partial", "{section}");
        assert_eq!(survey[section]["reason"], "max_commits", "{section}");
    }
    assert_eq!(survey["history"]["commits"], "1");
    assert_eq!(survey["history"]["reachable_commits"], "6");
    assert_eq!(
        survey["history"]["oldest_reached"],
        git(&repo, &["rev-parse", "HEAD"]),
        "the one commit reached is the head"
    );
    assert_eq!(survey["tree"]["completeness"], "complete");
    assert_eq!(machine.surveys(), 1);

    let (stdout, stderr) = machine.expect(
        &repo,
        &[("AF_SURVEY__MAX_WALL_MS", "1")],
        &["project", "refresh"],
        0,
    );
    assert!(stdout.starts_with("recorded\n"), "{stdout}\n{stderr}");
    assert_eq!(
        machine.surveys(),
        2,
        "a Survey cut by its deadline is recorded"
    );
    let survey = machine.json(&repo, &[], &["project", "survey", "--json"]);
    schemas::valid(&schemas::validator("project-survey-v1.json"), &survey);
    assert_eq!(survey["completeness"], "partial");
    assert_eq!(survey["identity"]["completeness"], "partial");
    assert_eq!(survey["identity"]["reason"], "max_wall_ms");
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
fn no_email_reaches_a_record_an_event_a_document_or_an_output_under_either_identity() {
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
        let kept = everything_kept(&machine);
        for email in EMAILS {
            assert!(!said.contains(email), "{identity}: {email} was printed");
            assert!(!kept.contains(email), "{identity}: {email} was recorded");
            assert!(!kept.contains(&email.to_uppercase()), "{identity}: {email}");
        }
        let survey = machine.json(&repo, &env, &["project", "survey", "--json"]);
        let identities: Vec<&String> = survey["contributors"]["by_identity"]
            .as_object()
            .unwrap()
            .keys()
            .collect();
        assert_eq!(survey["contributors"]["identity"], identity);
        if identity == "email_hash" {
            let rule = |email: &str| {
                review_core::hex::encode(&Sha256::digest(email.trim().to_lowercase()))[..16]
                    .to_string()
            };
            let mut expected: Vec<String> = EMAILS[..3].iter().map(|email| rule(email)).collect();
            expected.sort();
            assert_eq!(identities, expected.iter().collect::<Vec<_>>());
        } else {
            assert_eq!(identities, ["Ada", "Bob", "renovate[bot]"]);
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

    // A second --apply refuses the existing `.af/` with onboarding's own exit code and records
    // no second Survey of the unchanged head.
    let events = machine.events();
    let (stdout, stderr) = machine.expect(&repo, &[], &apply, 1);
    assert!(stderr.contains("`.af/` already exists"), "{stderr}");
    assert!(stdout.contains("af onboard: unchanged"), "{stdout}");
    assert_eq!(machine.events(), events, "nothing was recorded");
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
