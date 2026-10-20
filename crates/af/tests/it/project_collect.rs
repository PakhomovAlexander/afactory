//! ADR-0152 clauses 13 and 14 end to end: Surveys in the Storage Budget, and bounded patterns.
//!
//! Every Survey is an `af storage` entry of kind `survey` with its CAS bytes and its cutoff as
//! its last use. `af project refresh` collects nothing: 51 forced refreshes within the hour leave
//! 51 records, even past an applied sweep. An explicit collection takes the Surveys beyond
//! `[storage] keep_surveys`, oldest first, appending `SurveyCollected@1` before it frees their
//! bytes, keeps the newest complete Survey whatever follows it, and `af project history` and
//! `af project survey` answer from the tombstones. Budget pressure below `keep_surveys` takes the
//! oldest Survey first and never the newest complete one, a Survey the Project Store's lock
//! holds or the Project's own files. A refresh and a collection at once leave one consistent
//! log, and a collection interrupted between its tombstones and its deletions is finished by the
//! next sweep. The adversarial pattern `*a` thirty times then `b` neither hangs the Surveyor's
//! `exclude` nor matches, its matching form does, and a 257-byte pattern and a 65-entry list are
//! refused naming the key.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::schemas;

const DAY: u64 = 86_400;

/// Thirty days before this test process started, in Unix seconds: a cutoff idle past
/// `keep_days` and past the hour, the same for every call.
fn old_cutoff() -> u64 {
    static OLD: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *OLD.get_or_init(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 30 * DAY
    })
}

/// One machine: every XDG directory and `HOME` its own, so a sweep it runs with a tiny budget
/// reaches nothing another test keeps.
struct Machine {
    root: tempfile::TempDir,
}

impl Machine {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        for directory in ["state", "cache", "data", "config", "bin", "home"] {
            std::fs::create_dir_all(root.path().join(directory)).unwrap();
        }
        Self { root }
    }

    fn root(&self) -> PathBuf {
        std::fs::canonicalize(self.root.path()).unwrap()
    }

    fn command(&self, directory: &Path, env: &[(&str, &str)], args: &[&str]) -> Command {
        let root = self.root();
        let mut command = crate::common::af();
        command
            .current_dir(directory)
            .env("HOME", root.join("home"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_BIN_HOME", root.join("bin"))
            .env("AF_SELF_OFFLINE", "1")
            .env("NO_COLOR", "1");
        for (name, value) in env {
            command.env(name, value);
        }
        command.args(args);
        command
    }

    fn af(&self, directory: &Path, env: &[(&str, &str)], args: &[&str]) -> Output {
        self.command(directory, env, args).output().unwrap()
    }

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
        let mut stores: Vec<PathBuf> = std::fs::read_dir(self.root().join("state/af/project"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(stores.len(), 1, "{stores:?}");
        stores.pop().unwrap()
    }

    /// Every event of the Project Store's log: its run, type and payload, in order per run.
    fn events(&self) -> Vec<(String, String, Value)> {
        let log = rusqlite::Connection::open(self.store().join("events.sqlite")).unwrap();
        log.prepare("SELECT run_id, type, payload FROM events ORDER BY run_id, sequence")
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .map(|(run, kind, payload)| (run, kind, serde_json::from_str(&payload).unwrap()))
            .collect()
    }

    /// The `survey_id` of every event of type `kind`, in order.
    fn ids(&self, kind: &str) -> Vec<String> {
        self.events()
            .into_iter()
            .filter(|(_, event, _)| event == kind)
            .map(|(_, _, payload)| payload["survey_id"].as_str().unwrap().to_string())
            .collect()
    }

    fn recorded(&self) -> Vec<String> {
        self.ids("SurveyRecorded@1")
    }

    fn collected(&self) -> Vec<String> {
        self.ids("SurveyCollected@1")
    }

    /// Where the CAS keeps a Survey's record.
    fn object(&self, survey_id: &str) -> PathBuf {
        let hex = survey_id.trim_start_matches("sha256:");
        self.store()
            .join("cas/objects")
            .join(&hex[..2])
            .join(&hex[2..])
    }

    fn held(&self, survey_id: &str) -> bool {
        self.object(survey_id).is_file()
    }
}

/// Allocated bytes below `path`, links not followed.
fn allocated(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path).unwrap();
    let mut bytes = metadata.blocks() * 512;
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path).unwrap() {
            bytes += allocated(&entry.unwrap().path());
        }
    }
    bytes
}

fn git(repo: &Path, args: &[&str]) -> String {
    let date = format!("@{} +0000", old_cutoff() - 10 * DAY);
    let output = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Ada")
        .env("GIT_AUTHOR_EMAIL", "ada@collect.invalid")
        .env("GIT_COMMITTER_NAME", "Ada")
        .env("GIT_COMMITTER_EMAIL", "ada@collect.invalid")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn write(repo: &Path, path: &str, content: &str) {
    let path = repo.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

/// A repository of three commits whose `.af/af.toml` carries `survey`, the `[survey]` table's
/// body.
fn repository(machine: &Machine, survey: &str) -> PathBuf {
    let repo = machine.root().join("collect-fixture");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    write(
        &repo,
        ".af/af.toml",
        &format!(
            "version = 1\n\n[project]\nname = \"collect-fixture\"\nmin_af = \"0.1\"\n\n\
             [defaults]\npipeline = \"review\"\n\n[survey]\n{survey}"
        ),
    );
    for (n, file) in ["src/a.rs", "src/b.rs", "README.md"].iter().enumerate() {
        write(&repo, file, &format!("line {n}\n"));
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", &format!("Commit {n}")]);
    }
    repo
}

/// The environment of a Survey taken `seconds` after the old cutoff.
fn at(seconds: u64) -> String {
    (old_cutoff() + seconds).to_string()
}

/// Record a complete Survey (`--force`) or a partial one (`max_commits = 1`) at `cutoff`.
fn refresh(machine: &Machine, repo: &Path, cutoff: &str, complete: bool) -> String {
    let mut env = vec![("AF_TEST_SURVEY_CUTOFF_UNIX_S", cutoff)];
    if !complete {
        env.push(("AF_SURVEY__MAX_COMMITS", "1"));
    }
    machine.expect(repo, &env, &["project", "refresh", "--force"], 0);
    machine.recorded().last().unwrap().clone()
}

/// The `survey` entries of `af storage --json`, by path, validated against `af/storage@3`.
fn survey_entries(machine: &Machine, repo: &Path) -> (Value, BTreeMap<String, Value>) {
    let shown = machine.json(repo, &[], &["storage", "--json"]);
    schemas::valid(&schemas::validator("storage-v3.json"), &shown);
    let entries = shown["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["kind"] == "survey")
        .map(|entry| (entry["path"].as_str().unwrap().to_string(), entry.clone()))
        .collect();
    (shown, entries)
}

/// `af storage prune --apply --json` under `env`, validated against `af/storage-prune@2`.
fn prune(machine: &Machine, repo: &Path, env: &[(&str, &str)]) -> Value {
    let pruned = machine.json(repo, env, &["storage", "prune", "--apply", "--json"]);
    schemas::valid(&schemas::validator("storage-prune-v2.json"), &pruned);
    pruned
}

/// The paths a sweep removed under `rule`, of kind `survey`.
fn removed(pruned: &Value, rule: &str) -> Vec<String> {
    pruned["removals"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|removal| removal["kind"] == "survey" && removal["rule"] == rule)
        .map(|removal| removal["path"].as_str().unwrap().to_string())
        .collect()
}

fn path_of(machine: &Machine, survey_id: &str) -> String {
    machine.object(survey_id).to_string_lossy().into_owned()
}

/// The log is one consistent story: every tombstone names a recorded Survey once, every
/// collected record is gone from the CAS and every other one is there, and each Survey names
/// the one recorded before it.
fn assert_consistent(machine: &Machine) {
    let recorded = machine.recorded();
    let collected = machine.collected();
    let unique: BTreeSet<&String> = collected.iter().collect();
    assert_eq!(unique.len(), collected.len(), "a Survey tombstoned twice");
    for survey_id in &collected {
        assert!(
            recorded.contains(survey_id),
            "a tombstone names {survey_id}"
        );
        assert!(
            !machine.held(survey_id),
            "{survey_id} was tombstoned and kept"
        );
    }
    for survey_id in recorded.iter().filter(|id| !collected.contains(id)) {
        assert!(
            machine.held(survey_id),
            "{survey_id} lost without a tombstone"
        );
    }
    let chain: Vec<(Option<String>, String)> = machine
        .events()
        .into_iter()
        .filter(|(_, kind, _)| kind == "SurveyRecorded@1")
        .map(|(run, _, payload)| {
            assert_eq!(run, "project");
            (
                payload["previous_survey_id"].as_str().map(str::to_string),
                payload["survey_id"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    for pair in chain.windows(2) {
        assert_eq!(pair[1].0.as_deref(), Some(pair[0].1.as_str()));
    }
    for (run, kind, _) in machine.events() {
        if kind == "SurveyCollected@1" {
            assert_eq!(run, "survey-collection");
        }
    }
}

#[test]
fn fifty_one_forced_refreshes_within_an_hour_delete_nothing() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    for _ in 0..51 {
        machine.expect(&repo, &[], &["project", "refresh", "--force"], 0);
    }
    let recorded = machine.recorded();
    assert_eq!(recorded.len(), 51);
    assert!(machine.collected().is_empty(), "a refresh collects nothing");
    assert!(recorded.iter().all(|id| machine.held(id)));
    let (_, entries) = survey_entries(&machine, &repo);
    assert_eq!(entries.len(), 51, "every Survey is a budget entry");
    // Even an explicit collection keeps the Survey beyond `keep_surveys = 50`: it was used within
    // the hour, as each of them was.
    let pruned = prune(&machine, &repo, &[]);
    assert!(removed(&pruned, "collection").is_empty(), "{pruned}");
    assert!(machine.collected().is_empty());
    assert!(recorded.iter().all(|id| machine.held(id)));
    let (history, _) = machine.expect(&repo, &[], &["project", "history"], 0);
    assert!(history.starts_with("51 Surveys\n"), "{history}");
    assert!(!history.contains("collected"), "{history}");
}

#[test]
fn an_explicit_collection_beyond_keep_surveys_tombstones_and_frees_the_bytes() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let surveys: Vec<String> = (0..5)
        .map(|n| refresh(&machine, &repo, &at(n), true))
        .collect();
    let (shown, entries) = survey_entries(&machine, &repo);
    assert_eq!(entries.len(), 5);
    let kinds: Vec<&str> = shown["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|kind| kind["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"survey"), "{kinds:?}");
    for (n, survey_id) in surveys.iter().enumerate() {
        let entry = &entries[&path_of(&machine, survey_id)];
        assert!(entry["bytes"].as_u64().unwrap() > 0, "{entry}");
        assert_eq!(
            entry["last_use_unix_ms"].as_u64().unwrap(),
            (old_cutoff() + n as u64) * 1000,
            "a Survey is last used at its cutoff"
        );
    }
    let (text, _) = machine.expect(&repo, &[], &["storage"], 0);
    assert!(text.contains("Surveys"), "{text}");

    // A preview names what would go and removes nothing.
    let keep = [("AF_STORAGE__KEEP_SURVEYS", "2")];
    let preview = machine.json(&repo, &keep, &["storage", "prune", "--json"]);
    schemas::valid(&schemas::validator("storage-prune-v2.json"), &preview);
    assert_eq!(removed(&preview, "collection").len(), 3, "{preview}");
    assert!(machine.collected().is_empty());

    let cas = machine.store().join("cas");
    let before = allocated(&cas);
    let pruned = prune(&machine, &repo, &keep);
    let oldest: Vec<String> = surveys[..3]
        .iter()
        .map(|id| path_of(&machine, id))
        .collect();
    assert_eq!(removed(&pruned, "collection"), oldest, "oldest first");
    let freed: u64 = pruned["removals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|removal| removal["bytes"].as_u64().unwrap())
        .sum();
    assert!(freed > 0);
    let after = allocated(&cas);
    assert!(
        before - after >= freed,
        "{before} - {after} freed less than the {freed} bytes reported"
    );
    assert_eq!(machine.collected(), surveys[..3].to_vec());
    for survey_id in &surveys[..3] {
        assert!(!machine.held(survey_id));
    }
    for survey_id in &surveys[3..] {
        assert!(machine.held(survey_id));
    }
    // The tombstone keeps the totals, and the history answers from it.
    let tombstone = machine
        .events()
        .into_iter()
        .find(|(_, kind, _)| kind == "SurveyCollected@1")
        .unwrap()
        .2;
    schemas::valid(
        &schemas::validator("project-survey-collected-v1.json"),
        &tombstone,
    );
    assert_eq!(tombstone["tree"]["files"], "4");
    assert_eq!(tombstone["history"]["commits"], "3");
    assert_eq!(tombstone["completeness"], "complete");
    let (history, _) = machine.expect(&repo, &[], &["project", "history"], 0);
    assert!(history.starts_with("5 Surveys\n"), "{history}");
    let lines: Vec<&str> = history.lines().skip(1).collect();
    for (line, survey_id) in lines.iter().zip(surveys.iter().rev()) {
        assert!(line.starts_with(survey_id.as_str()), "{line}");
        assert_eq!(
            line.contains("  collected "),
            surveys[..3].contains(survey_id),
            "{line}"
        );
    }
    assert!(lines[4].ends_with("(4 files, 3 commits)"), "{}", lines[4]);
    let listed = machine.json(&repo, &[], &["project", "history", "--json"]);
    schemas::valid(&schemas::validator("project-history-v1.json"), &listed);
    assert_eq!(listed["surveys"].as_array().unwrap().len(), 5);
    // The newest Survey is uncollected, so `af project survey` shows it.
    let newest = machine.json(&repo, &[], &["project", "survey", "--json"]);
    assert_eq!(newest["previous_survey_id"], surveys[3].as_str());
    let (_, entries) = survey_entries(&machine, &repo);
    assert_eq!(entries.len(), 2);
    // A second pass finds nothing more beyond the limit.
    assert!(removed(&prune(&machine, &repo, &keep), "collection").is_empty());
    assert_consistent(&machine);
}

#[test]
fn a_complete_survey_followed_by_partial_ones_survives_and_a_collected_newest_is_said() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let complete = refresh(&machine, &repo, &at(0), true);
    let partial: Vec<String> = (1..4)
        .map(|n| refresh(&machine, &repo, &at(n), false))
        .collect();
    let (_, entries) = survey_entries(&machine, &repo);
    assert_eq!(
        entries[&path_of(&machine, &complete)]["in_use"],
        "the newest complete Survey of its Project"
    );
    let pruned = prune(&machine, &repo, &[("AF_STORAGE__KEEP_SURVEYS", "0")]);
    let taken: Vec<String> = partial.iter().map(|id| path_of(&machine, id)).collect();
    assert_eq!(removed(&pruned, "collection"), taken);
    assert!(
        machine.held(&complete),
        "the newest complete Survey survives"
    );
    assert_eq!(machine.collected(), partial);

    // The newest Survey is collected: `af project survey` says so and names the refresh.
    let (_, stderr) = machine.expect(&repo, &[], &["project", "survey"], 1);
    assert!(stderr.contains("was collected"), "{stderr}");
    assert!(stderr.contains(&partial[2]), "{stderr}");
    assert!(stderr.contains("af project refresh"), "{stderr}");
    let (_, stderr) = machine.expect(&repo, &[], &["project", "survey", "--json"], 1);
    assert!(stderr.contains("was collected"), "{stderr}");
    // `af project show` still answers, with no uncollected newest Survey to summarize.
    let (shown, _) = machine.expect(&repo, &[], &["project", "show"], 0);
    assert!(shown.contains("No Survey yet"), "{shown}");
    // A refresh records a new one, chained by name to the collected one.
    machine.expect(
        &repo,
        &[("AF_TEST_SURVEY_CUTOFF_UNIX_S", &at(10))],
        &["project", "refresh"],
        0,
    );
    let newest = machine.json(&repo, &[], &["project", "survey", "--json"]);
    assert_eq!(newest["previous_survey_id"], partial[2].as_str());
    assert_consistent(&machine);
}

#[test]
fn budget_pressure_below_keep_surveys_reclaims_the_oldest_and_never_the_protected() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let first = refresh(&machine, &repo, &at(0), true);
    let complete = refresh(&machine, &repo, &at(1), true);
    let partial = refresh(&machine, &repo, &at(2), false);
    let (shown, entries) = survey_entries(&machine, &repo);
    assert_eq!(entries.len(), 3, "below keep_surveys = 50");
    let total = shown["total_bytes"].as_u64().unwrap();

    // One byte over: the oldest Survey goes, and nothing else.
    let over = (total - 1).to_string();
    let pruned = prune(&machine, &repo, &[("AF_STORAGE__MAX_BYTES", &over)]);
    assert_eq!(removed(&pruned, "budget"), vec![path_of(&machine, &first)]);
    assert_eq!(pruned["stop"], "fits");
    assert_eq!(machine.collected(), vec![first.clone()]);

    // Nothing fits: every unprotected Survey goes, the newest complete one stays.
    let tiny = [("AF_STORAGE__MAX_BYTES", "1")];
    let pruned = prune(&machine, &repo, &tiny);
    assert_eq!(
        removed(&pruned, "budget"),
        vec![path_of(&machine, &partial)]
    );
    assert_eq!(pruned["stop"], "nothing_evictable");
    assert!(machine.held(&complete));
    // The Project's own files are never reclaimed.
    let store = machine.store();
    for file in ["events.sqlite", "workspaces.toml", "project.lock"] {
        assert!(store.join(file).is_file(), "{file}");
    }
    let (shown, _) = survey_entries(&machine, &repo);
    assert!(
        shown["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["kind"] == "project_store"),
        "{shown}"
    );
    machine.expect(&repo, &[], &["project", "show"], 0);

    // A Survey the Project Store's lock holds stays, whatever the budget.
    let held = refresh(&machine, &repo, &at(3), false);
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .open(store.join("project.lock"))
        .unwrap();
    let lock = nix::fcntl::Flock::lock(lock, nix::fcntl::FlockArg::LockExclusive).unwrap();
    let (_, entries) = survey_entries(&machine, &repo);
    assert_eq!(
        entries[&path_of(&machine, &held)]["in_use"],
        "a refresh or a plan holds its Project Store"
    );
    let pruned = prune(&machine, &repo, &tiny);
    assert!(removed(&pruned, "budget").is_empty(), "{pruned}");
    assert!(machine.held(&held));
    assert_eq!(machine.collected().len(), 2);
    drop(lock);
    let pruned = prune(&machine, &repo, &tiny);
    assert_eq!(removed(&pruned, "budget"), vec![path_of(&machine, &held)]);
    assert!(machine.held(&complete));
    assert_consistent(&machine);
}

#[test]
fn a_refresh_and_a_collection_at_once_leave_one_consistent_log() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    for n in 0..4 {
        refresh(&machine, &repo, &at(n), true);
    }
    let keep = [("AF_STORAGE__KEEP_SURVEYS", "1")];
    for round in 0..4_u64 {
        let cutoff = at(10 + round);
        let mut refreshing = machine
            .command(
                &repo,
                &[("AF_TEST_SURVEY_CUTOFF_UNIX_S", &cutoff)],
                &["project", "refresh", "--force"],
            )
            .spawn()
            .unwrap();
        let collecting = machine.af(&repo, &keep, &["storage", "prune", "--apply", "--json"]);
        assert_eq!(collecting.status.code(), Some(0));
        assert_eq!(refreshing.wait().unwrap().code(), Some(0));
        assert_consistent(&machine);
    }
    let pruned = prune(&machine, &repo, &keep);
    assert!(
        pruned["failures"].as_array().unwrap().is_empty(),
        "{pruned}"
    );
    assert_consistent(&machine);
    let recorded = machine.recorded();
    assert_eq!(recorded.len(), 8);
    assert_eq!(machine.collected().len(), 7, "all but the newest");
    machine.expect(&repo, &[], &["project", "survey"], 0);
    let (history, _) = machine.expect(&repo, &[], &["project", "history"], 0);
    assert_eq!(history.matches("  collected ").count(), 7, "{history}");
}

#[test]
fn a_collection_interrupted_after_its_tombstones_is_finished_by_the_next_sweep() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let surveys: Vec<String> = (0..4)
        .map(|n| refresh(&machine, &repo, &at(n), true))
        .collect();
    let keep = ("AF_STORAGE__KEEP_SURVEYS", "1");
    prune(
        &machine,
        &repo,
        &[keep, ("AF_TEST_SURVEY_STOP_AFTER_TOMBSTONES", "1")],
    );
    assert_eq!(machine.collected(), surveys[..3].to_vec());
    for survey_id in &surveys[..3] {
        assert!(machine.held(survey_id), "stopped before the deletion");
    }
    // The history already answers from the tombstones; the bytes are still counted.
    let (history, _) = machine.expect(&repo, &[], &["project", "history"], 0);
    assert_eq!(history.matches("  collected ").count(), 3, "{history}");
    let (_, entries) = survey_entries(&machine, &repo);
    assert_eq!(entries.len(), 4);

    let pruned = prune(&machine, &repo, &[keep]);
    let finished: Vec<String> = surveys[..3]
        .iter()
        .map(|id| path_of(&machine, id))
        .collect();
    assert_eq!(removed(&pruned, "recovery"), finished, "{pruned}");
    assert!(removed(&pruned, "collection").is_empty());
    assert_eq!(machine.collected().len(), 3, "no tombstone twice");
    for survey_id in &surveys[..3] {
        assert!(!machine.held(survey_id));
    }
    assert!(machine.held(&surveys[3]));
    let (_, entries) = survey_entries(&machine, &repo);
    assert_eq!(entries.len(), 1);
    assert_consistent(&machine);
}

/// `*a` thirty times then `b`: the failing match the recursive matcher took billions of steps to
/// refuse.
fn adversarial() -> String {
    format!("{}b", "*a".repeat(30))
}

/// The files the newest Survey's `tree` counts.
fn tree_files(machine: &Machine, repo: &Path) -> u64 {
    machine.json(repo, &[], &["project", "survey", "--json"])["tree"]["files"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn the_adversarial_pattern_neither_hangs_nor_matches_the_surveyors_exclude() {
    let thirty = "a".repeat(30);
    let machine = Machine::new();
    let repo = repository(&machine, &format!("exclude = [\"{}\"]\n", adversarial()));
    write(&repo, &thirty, "thirty\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "Thirty"]);
    let started = Instant::now();
    machine.expect(&repo, &[], &["project", "refresh"], 0);
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        tree_files(&machine, &repo),
        5,
        "the failing pattern excludes nothing"
    );

    // Its matching form excludes the file.
    write(
        &repo,
        ".af/af.toml",
        &format!(
            "version = 1\n\n[project]\nname = \"collect-fixture\"\nmin_af = \"0.1\"\n\n\
             [defaults]\npipeline = \"review\"\n\n[survey]\nexclude = [\"{}\"]\n",
            "*a".repeat(30)
        ),
    );
    git(&repo, &["commit", "-q", "-am", "Matching"]);
    machine.expect(&repo, &[], &["project", "refresh"], 0);
    assert_eq!(tree_files(&machine, &repo), 4);
}

#[test]
fn a_pattern_over_256_bytes_and_a_list_over_64_are_refused_naming_the_key() {
    let machine = Machine::new();
    let long = format!("{}*", "a".repeat(256));
    assert_eq!(long.len(), 257);
    let repo = repository(&machine, &format!("exclude = [\"{long}\"]\n"));
    let (_, stderr) = machine.expect(&repo, &[], &["project", "refresh"], 1);
    assert!(stderr.contains("exclude"), "{stderr}");
    assert!(stderr.contains("256"), "{stderr}");
    assert!(machine.recorded().is_empty());

    let list: Vec<String> = (0..65).map(|n| format!("\"*Bot{n}*\"")).collect();
    write(
        &repo,
        ".af/af.toml",
        &format!(
            "version = 1\n\n[project]\nname = \"collect-fixture\"\nmin_af = \"0.1\"\n\n\
             [defaults]\npipeline = \"review\"\n\n[survey]\nagent_trailers = [{}]\n",
            list.join(", ")
        ),
    );
    let (_, stderr) = machine.expect(&repo, &[], &["project", "refresh"], 1);
    assert!(
        stderr.contains("agent_trailers holds 65 patterns, more than 64"),
        "{stderr}"
    );
    assert!(machine.recorded().is_empty());
    let failed = machine
        .events()
        .into_iter()
        .filter(|(_, kind, _)| kind == "SurveyFailed@1")
        .count();
    assert_eq!(failed, 2, "each refusal is a SurveyFailed@1");

    // Sixty-four patterns load.
    let list: Vec<String> = (0..64).map(|n| format!("\"*Bot{n}*\"")).collect();
    write(
        &repo,
        ".af/af.toml",
        &format!(
            "version = 1\n\n[project]\nname = \"collect-fixture\"\nmin_af = \"0.1\"\n\n\
             [defaults]\npipeline = \"review\"\n\n[survey]\nagent_trailers = [{}]\n",
            list.join(", ")
        ),
    );
    machine.expect(&repo, &[], &["project", "refresh"], 0);
    assert_eq!(machine.recorded().len(), 1);
}
