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
//! log, and a collection interrupted between its tombstones and its deletions, or after it moved
//! a record into its pass's container under `cas/removing`, is finished by the next sweep
//! through a container of its own, which removes the dead containers too. The adversarial
//! pattern `*a` thirty times then `b` neither hangs the Surveyor's `exclude` nor matches, its
//! matching form does, and a 257-byte pattern and a 65-entry list are refused naming the key.
//!
//! Every applied sweep here (but those a debug hook makes fail) is checked against the preview
//! just before it: one plan, so the totals, every removal with its bytes and the total after it,
//! and every entry left agree entry for entry. When a Survey's record was moved and a smaller file
//! took its CAS name, the plan deletes the moved record under `recovery` and lists the other file
//! `foreign` with its bytes; it stays in the Store's total, never the Survey's, and no sweep
//! deletes it, not even one with a one-byte budget. A leftover whose name is not UTF-8 is listed
//! `unreadable_name` by preview and apply and left (on a file system that allows such a name).
//!
//! A record is reached and deleted only through descriptors: a prefix directory replaced by a
//! link to a directory outside the Store refuses its Survey, touches nothing outside and adds no
//! tombstone. A corrupt record is collected `unreadable` and its bytes freed, while one the file
//! system will not let af read is refused and keeps its record and its place in the log, and one
//! a byte over the shared ceiling is left `oversize`. A record replaced between its identity read
//! and its rename is `refused (replaced during collection)`, its entry kept as `failed: …` with
//! the running total unchanged: nothing else is deleted and a rerun collects it. One replaced
//! between inventory and the plan's judgement is `refused (replaced since inventory)`: the plan
//! takes nothing of it, and the totals the sweep reports are what the directory holds. The
//! preview leaves the Surveys of a held Project Store `held`, exactly as the applied sweep does,
//! and a `project.lock` af cannot open for writing holds them in both with the same reason.
//!
//! What an interruption left is in the one inventory: a removal a dead process left claimed is a
//! `claim` entry under `recovery` that the preview and the applied sweep list alike, with the
//! same bytes, and a Survey ID file in a container whose name is not UTF-8 is `unreadable_name`
//! and left with its container.
//! A dead pass's container that held a moved record is its own `recovery` entry after it, with
//! its bytes. A registered Store a dead process renamed into its claim is recovered, and its
//! registration dropped, alike by preview and apply; a claim under a Project Store is counted
//! once; a campaign nested deeper than the removal walk is left `too_deep`; and an event whose
//! cutoff its record disproves is never the Survey's last use, the Survey refused and kept.
//! `?` is one byte of the canonical encoded spelling in the Surveyor's `exclude`, and the
//! superseded `af/storage@2` and `af/storage-prune@1` are refused.

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
/// The allocated bytes below `path` as the kernel counts them, directories' own blocks included.
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

/// The allocated bytes of the regular files below `path`, for a before/after comparison across
/// a collection pass: the pass leaves a container directory behind, whose own blocks differ by
/// filesystem (4096 on ext4, none on APFS) and are no part of what a removal reports.
fn allocated_files(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path).unwrap();
    if metadata.is_dir() {
        return std::fs::read_dir(path)
            .unwrap()
            .map(|entry| allocated_files(&entry.unwrap().path()))
            .sum();
    }
    metadata.blocks() * 512
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

/// `af storage prune --apply --json` under `env`, validated against `af/storage-prune@2`. Unless
/// a debug hook makes an entry of the applied sweep fail, the preview just before it is the same
/// plan: every removal, with its bytes and the total after it, and every entry left agree.
fn prune(machine: &Machine, repo: &Path, env: &[(&str, &str)]) -> Value {
    let hooked = env.iter().any(|(name, _)| name.starts_with("AF_TEST_"));
    let preview = (!hooked).then(|| preview(machine, repo, env));
    let pruned = machine.json(repo, env, &["storage", "prune", "--apply", "--json"]);
    schemas::valid(&schemas::validator("storage-prune-v2.json"), &pruned);
    if let Some(preview) = preview {
        assert_agree(&preview, &pruned);
    }
    pruned
}

/// `af storage prune --json` under `env`, validated against `af/storage-prune@2`.
fn preview(machine: &Machine, repo: &Path, env: &[(&str, &str)]) -> Value {
    let preview = machine.json(repo, env, &["storage", "prune", "--json"]);
    schemas::valid(&schemas::validator("storage-prune-v2.json"), &preview);
    assert_eq!(preview["applied"], false);
    preview
}

/// A preview and the applied sweep that ran its plan: entry for entry, unless an entry of the
/// applied one failed.
fn assert_agree(preview: &Value, applied: &Value) {
    let failed = applied["removals"]
        .as_array()
        .unwrap()
        .iter()
        .any(|removal| removal.get("outcome").is_some());
    if failed {
        return;
    }
    for key in ["total_before", "total_after", "stop", "removals", "left"] {
        assert_eq!(
            preview[key], applied[key],
            "`{key}` of the preview and the applied sweep\n{preview}\n{applied}"
        );
    }
}

/// The paths a sweep removed under `rule`, of kind `survey`, a removal that failed excluded.
fn removed(pruned: &Value, rule: &str) -> Vec<String> {
    pruned["removals"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|removal| {
            removal["kind"] == "survey"
                && removal["rule"] == rule
                && removal.get("outcome").is_none()
        })
        .map(|removal| removal["path"].as_str().unwrap().to_string())
        .collect()
}

/// What a sweep left with `reason`.
fn left(pruned: &Value, reason: &str) -> Vec<Value> {
    pruned["left"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["reason"] == reason)
        .cloned()
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
    assert!(removed(&pruned, "keep_surveys").is_empty(), "{pruned}");
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
    assert_eq!(removed(&preview, "keep_surveys").len(), 3, "{preview}");
    assert!(machine.collected().is_empty());

    let cas = machine.store().join("cas");
    let before = allocated_files(&cas);
    let pruned = prune(&machine, &repo, &keep);
    let oldest: Vec<String> = surveys[..3]
        .iter()
        .map(|id| path_of(&machine, id))
        .collect();
    assert_eq!(removed(&pruned, "keep_surveys"), oldest, "oldest first");
    let freed: u64 = pruned["removals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|removal| removal["bytes"].as_u64().unwrap())
        .sum();
    assert!(freed > 0);
    let after = allocated_files(&cas);
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
    schemas::valid(&schemas::validator("project-history-v2.json"), &listed);
    assert_eq!(listed["surveys"].as_array().unwrap().len(), 5);
    // The newest Survey is uncollected, so `af project survey` shows it.
    let newest = machine.json(&repo, &[], &["project", "survey", "--json"]);
    assert_eq!(newest["previous_survey_id"], surveys[3].as_str());
    let (_, entries) = survey_entries(&machine, &repo);
    assert_eq!(entries.len(), 2);
    // A second pass finds nothing more beyond the limit.
    assert!(removed(&prune(&machine, &repo, &keep), "keep_surveys").is_empty());
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
    assert_eq!(removed(&pruned, "keep_surveys"), taken);
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
    // The oldest was interrupted later still, after its rename into its pass's container, and
    // another dead pass left its container empty.
    let removing = machine.store().join("cas/removing");
    let dead = removing.join(DEAD_PASS);
    std::fs::create_dir_all(&dead).unwrap();
    std::fs::create_dir_all(removing.join(EMPTY_PASS)).unwrap();
    std::fs::rename(machine.object(&surveys[0]), dead.join(&surveys[0])).unwrap();
    let moved_bytes = allocated(&dead.join(&surveys[0]));

    // The preview lists every pending deletion under `recovery` with the bytes it frees.
    let finished: Vec<String> = surveys[..3]
        .iter()
        .map(|id| path_of(&machine, id))
        .collect();
    let preview = preview(&machine, &repo, &[keep]);
    assert_eq!(removed(&preview, "recovery"), finished, "{preview}");
    let recovery = |document: &Value| -> Vec<Value> {
        document["removals"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|removal| removal["kind"] == "survey")
            .cloned()
            .collect()
    };
    for removal in recovery(&preview) {
        assert!(removal["bytes"].as_u64().unwrap() > 0, "{removal}");
    }
    assert_eq!(
        recovery(&preview)[0]["bytes"].as_u64().unwrap(),
        moved_bytes
    );
    assert!(dead.join(&surveys[0]).is_file(), "a preview moves nothing");
    assert!(preview["left"].as_array().unwrap().is_empty(), "{preview}");

    // Each dead container is an entry of its own, after the record it holds: the one that held
    // the moved record, then the empty one, each with its allocated bytes subtracted.
    let containers = |document: &Value| -> Vec<Value> {
        document["removals"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|removal| removal["kind"] == "claim")
            .cloned()
            .collect()
    };
    let planned = containers(&preview);
    let paths: Vec<&str> = planned
        .iter()
        .map(|removal| removal["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        [
            dead.to_str().unwrap(),
            removing.join(EMPTY_PASS).to_str().unwrap()
        ],
        "{preview}"
    );
    let own_bytes = |path: &Path| {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(path).unwrap().blocks() * 512
    };
    assert_eq!(planned[0]["bytes"].as_u64().unwrap(), own_bytes(&dead));
    assert_eq!(
        planned[1]["bytes"].as_u64().unwrap(),
        own_bytes(&removing.join(EMPTY_PASS))
    );
    let removals = preview["removals"].as_array().unwrap();
    let at = |path: &str| {
        removals
            .iter()
            .position(|removal| removal["path"] == path)
            .unwrap()
    };
    assert!(
        at(&finished[0]) < at(dead.to_str().unwrap()),
        "after its record"
    );
    for (index, removal) in removals.iter().enumerate().skip(1) {
        assert_eq!(
            removal["total_after"].as_u64().unwrap(),
            removals[index - 1]["total_after"].as_u64().unwrap()
                - removal["bytes"].as_u64().unwrap(),
            "{removal}"
        );
    }

    let pruned = prune(&machine, &repo, &[keep]);
    assert_eq!(removed(&pruned, "recovery"), finished, "{pruned}");
    assert_eq!(recovery(&pruned), recovery(&preview), "entry for entry");
    assert_eq!(
        containers(&pruned),
        planned,
        "the same containers, the same bytes"
    );
    assert_eq!(pruned["left"], preview["left"]);
    // Settled through the next pass's own container: every container, the dead ones and its
    // own, is gone, and only through the entries the plan listed.
    assert!(
        std::fs::read_dir(&removing).unwrap().next().is_none(),
        "{:?}",
        std::fs::read_dir(&removing)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>()
    );
    assert!(removed(&pruned, "keep_surveys").is_empty());
    assert_eq!(machine.collected().len(), 3, "no tombstone twice");
    for survey_id in &surveys[..3] {
        assert!(!machine.held(survey_id));
    }
    assert!(machine.held(&surveys[3]));
    let (_, entries) = survey_entries(&machine, &repo);
    assert_eq!(entries.len(), 1);
    assert_consistent(&machine);
}

/// The name of the container a dead pass left, as a pass names its own: 32 hex digits.
const DEAD_PASS: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f0";
/// Another dead pass's container, left empty.
const EMPTY_PASS: &str = "00112233445566778899aabbccddeeff";

/// The total `af storage --json` measures.
fn storage_total(machine: &Machine, repo: &Path) -> u64 {
    survey_entries(machine, repo).0["total_bytes"]
        .as_u64()
        .unwrap()
}

/// Two files for one Survey: its record was moved into a dead pass's container and a smaller
/// file took its CAS name since. The moved record is the Survey's bytes, deleted under
/// `recovery`; the file at its name is `foreign`, listed with its bytes, counted in the Store's
/// total and never the Survey's, and never deleted, not even by a budget that wants every byte.
#[test]
fn a_file_at_a_moved_surveys_name_is_foreign_and_only_the_moved_record_is_deleted() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let surveys: Vec<String> = (0..4)
        .map(|n| refresh(&machine, &repo, &at(n), true))
        .collect();
    let keep = ("AF_STORAGE__KEEP_SURVEYS", "3");
    prune(
        &machine,
        &repo,
        &[keep, ("AF_TEST_SURVEY_STOP_AFTER_TOMBSTONES", "1")],
    );
    let target = &surveys[0];
    assert_eq!(machine.collected(), vec![target.clone()]);
    let object = machine.object(target);
    let dead = machine.store().join("cas/removing").join(DEAD_PASS);
    std::fs::create_dir_all(&dead).unwrap();
    std::fs::rename(&object, dead.join(target)).unwrap();
    std::fs::write(&object, b"not af's").unwrap();
    let moved_bytes = allocated(&dead.join(target));
    let container_bytes = {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(&dead).unwrap().blocks() * 512
    };
    let foreign_bytes = allocated(&object);
    assert!(foreign_bytes > 0);

    // Inventory counts the foreign file in the Project Store, never as the Survey.
    let (shown, entries) = survey_entries(&machine, &repo);
    assert!(!entries.contains_key(&path_of(&machine, target)), "{shown}");
    let before = storage_total(&machine, &repo);

    let previewed = preview(&machine, &repo, &[keep]);
    assert_eq!(previewed["total_before"].as_u64().unwrap(), before);
    let recovery: Vec<&Value> = previewed["removals"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|removal| removal["kind"] == "survey")
        .collect();
    assert_eq!(recovery.len(), 1, "{previewed}");
    assert_eq!(recovery[0]["rule"], "recovery");
    assert_eq!(recovery[0]["path"], path_of(&machine, target).as_str());
    assert_eq!(recovery[0]["bytes"].as_u64().unwrap(), moved_bytes);
    assert_eq!(
        recovery[0]["total_after"].as_u64().unwrap(),
        before - moved_bytes
    );
    let foreign = left(&previewed, "foreign");
    assert_eq!(foreign.len(), 1, "{previewed}");
    assert_eq!(foreign[0]["path"], path_of(&machine, target).as_str());
    assert_eq!(foreign[0]["bytes"].as_u64().unwrap(), foreign_bytes);
    assert_eq!(foreign[0]["kind"], "survey");
    let (text, _) = machine.expect(&repo, &[keep], &["storage", "prune"], 0);
    assert!(text.contains("left survey "), "{text}");
    assert!(text.contains(", foreign: "), "{text}");

    let applied = prune(&machine, &repo, &[keep]);
    assert_eq!(
        removed(&applied, "recovery"),
        vec![path_of(&machine, target)]
    );
    assert_eq!(applied["left"], previewed["left"]);
    assert!(
        !dead.exists(),
        "the moved record and its dead container are gone"
    );
    assert_eq!(
        std::fs::read(&object).unwrap(),
        b"not af's",
        "the foreign file is left"
    );
    // The pass recorded that the Survey's record left the CAS: `settled.json`, af's own record
    // beside the lock, written by the pass and measured by the next inventory.
    let settled = allocated(&machine.store().join("settled.json"));
    let after = storage_total(&machine, &repo);
    assert_eq!(applied["total_after"].as_u64().unwrap() + settled, after);
    assert_eq!(
        before + settled - after,
        moved_bytes + container_bytes,
        "the directory freed what was said: the moved record and its planned container"
    );

    // No pass ever deletes it: not the next collection, not a budget of one byte.
    let squeezed = prune(&machine, &repo, &[keep, ("AF_STORAGE__MAX_BYTES", "1")]);
    assert_eq!(left(&squeezed, "foreign").len(), 1, "{squeezed}");
    assert!(
        squeezed["removals"]
            .as_array()
            .unwrap()
            .iter()
            .all(|removal| removal["path"] != path_of(&machine, target).as_str()),
        "{squeezed}"
    );
    assert_eq!(std::fs::read(&object).unwrap(), b"not af's");
    assert_eq!(
        machine
            .collected()
            .iter()
            .filter(|id| *id == target)
            .count(),
        1,
        "no tombstone twice"
    );
}

/// A leftover whose name is not UTF-8 is walked with its raw name: preview and apply both list
/// it `unreadable_name`, its path in the encoded spelling, and leave it in place. A file system
/// that refuses such names (APFS) has nothing to show.
#[test]
fn a_leftover_with_a_name_that_is_not_utf8_is_reported_and_left() {
    use std::os::unix::ffi::OsStrExt;
    let machine = Machine::new();
    let repo = repository(&machine, "");
    for n in 0..2 {
        refresh(&machine, &repo, &at(n), true);
    }
    let dead = machine.store().join("cas/removing").join(DEAD_PASS);
    std::fs::create_dir_all(&dead).unwrap();
    let name = std::ffi::OsStr::from_bytes(b"caf\xe9");
    let leftover = dead.join(name);
    match std::fs::write(&leftover, b"x") {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == Some(nix::libc::EILSEQ) => return,
        Err(error) => panic!("{error}"),
    }
    let expected = format!("{}/caf%E9", dead.display());
    for document in [preview(&machine, &repo, &[]), prune(&machine, &repo, &[])] {
        let unreadable = left(&document, "unreadable_name");
        assert_eq!(unreadable.len(), 1, "{document}");
        assert_eq!(unreadable[0]["path"], expected.as_str());
        assert_eq!(unreadable[0]["path_encoded"], true);
        assert!(leftover.is_file(), "left in place");
    }
    let (text, _) = machine.expect(&repo, &[], &["storage", "prune"], 0);
    assert!(text.contains("unreadable name"), "{text}");
    assert!(text.contains("caf\u{fffd}"), "{text}");
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

/// Every file below `root`, links not followed: its path and its bytes.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                pending.push(path);
            } else {
                files.insert(path.clone(), std::fs::read(&path).unwrap_or_default());
            }
        }
    }
    files
}

/// The failures of a sweep that name `object`.
fn failures_naming(pruned: &Value, object: &str) -> Vec<String> {
    pruned["failures"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .filter(|failure| failure.contains(object))
        .map(str::to_string)
        .collect()
}

#[test]
fn a_prefix_directory_replaced_by_a_link_is_never_followed() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let surveys: Vec<String> = (0..3)
        .map(|n| refresh(&machine, &repo, &at(n), true))
        .collect();
    // The oldest Survey's prefix directory moves outside the Store; a link takes its place.
    let target = &surveys[0];
    let object = machine.object(target);
    let prefix = object.parent().unwrap().to_path_buf();
    let outside = machine.root().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let moved = outside.join(prefix.file_name().unwrap());
    std::fs::rename(&prefix, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &prefix).unwrap();
    let before = snapshot(&outside);
    assert!(!before.is_empty());
    let behind_the_link: Vec<&String> = surveys
        .iter()
        .filter(|id| machine.object(id).parent() == Some(prefix.as_path()))
        .collect();

    // Inventory measures nothing through the link and lists the Survey in use.
    let (_, entries) = survey_entries(&machine, &repo);
    let entry = &entries[&path_of(&machine, target)];
    assert_eq!(entry["bytes"], 0, "{entry}");
    assert!(
        entry["in_use"]
            .as_str()
            .unwrap()
            .contains("prefix directory is refused"),
        "{entry}"
    );

    // The Survey is eligible for collection and the budget presses: it is refused, reported
    // with its reason, and nothing outside the Store changes.
    let pruned = prune(
        &machine,
        &repo,
        &[
            ("AF_STORAGE__KEEP_SURVEYS", "0"),
            ("AF_STORAGE__MAX_BYTES", "1"),
        ],
    );
    let refused = failures_naming(&pruned, &path_of(&machine, target));
    assert_eq!(refused.len(), 1, "{pruned}");
    assert!(refused[0].contains("is refused"), "{}", refused[0]);
    assert!(
        refused[0].contains("prefix directory is refused"),
        "{}",
        refused[0]
    );
    for survey_id in &behind_the_link {
        assert!(
            !removed(&pruned, "keep_surveys").contains(&path_of(&machine, survey_id))
                && !removed(&pruned, "budget").contains(&path_of(&machine, survey_id)),
            "{pruned}"
        );
        assert!(
            !machine.collected().contains(survey_id),
            "no tombstone for a refused Survey"
        );
    }
    assert_eq!(
        snapshot(&outside),
        before,
        "nothing outside the Store is touched"
    );
    assert!(
        std::fs::symlink_metadata(&prefix)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    // The other eligible Survey went, the newest complete one stays.
    assert!(machine.collected().contains(&surveys[1]) || behind_the_link.contains(&&surveys[1]));
    assert!(machine.held(&surveys[2]));
    let (text, _) = machine.expect(
        &repo,
        &[("AF_STORAGE__KEEP_SURVEYS", "0")],
        &["storage", "prune"],
        0,
    );
    assert!(text.contains("failed: collecting"), "{text}");
}

#[test]
fn a_corrupt_survey_is_collected_unreadable_and_an_unreadable_one_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let surveys: Vec<String> = (0..4)
        .map(|n| refresh(&machine, &repo, &at(n), true))
        .collect();
    // The oldest record's bytes are corrupt and large enough that freeing them shows.
    let corrupt = machine.object(&surveys[0]);
    std::fs::set_permissions(&corrupt, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::write(&corrupt, vec![b'x'; 256 * 1024]).unwrap();
    let corrupt_bytes = allocated(&corrupt);
    assert!(corrupt_bytes >= 256 * 1024);
    // The next one af is not allowed to read.
    let locked = machine.object(&surveys[1]);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let enforced = std::fs::read(&locked).is_err();

    let keep = [("AF_STORAGE__KEEP_SURVEYS", "2")];
    let cas = machine.store().join("cas");
    let before = allocated_files(&cas);
    let tombstones_before = machine.collected();
    let pruned = prune(&machine, &repo, &keep);
    let collected: Vec<&Value> = pruned["removals"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|removal| removal["kind"] == "survey" && removal["rule"] == "keep_surveys")
        .collect();
    let corrupt_removal = collected
        .iter()
        .find(|removal| removal["path"] == path_of(&machine, &surveys[0]).as_str())
        .unwrap_or_else(|| panic!("the corrupt Survey is collected: {pruned}"));
    assert_eq!(
        corrupt_removal["unreadable"], "its bytes do not match its digest",
        "{corrupt_removal}"
    );
    assert_eq!(corrupt_removal["bytes"].as_u64().unwrap(), corrupt_bytes);
    assert!(!corrupt.exists(), "its bytes are deleted");
    assert!(
        before - allocated_files(&cas) >= corrupt_bytes,
        "the corrupt bytes are freed"
    );
    let tombstone = machine
        .events()
        .into_iter()
        .find(|(_, kind, payload)| {
            kind == "SurveyCollected@1" && payload["survey_id"] == surveys[0].as_str()
        })
        .unwrap()
        .2;
    schemas::valid(
        &schemas::validator("project-survey-collected-v1.json"),
        &tombstone,
    );
    assert_eq!(tombstone["unreadable"], "its bytes do not match its digest");
    assert!(tombstone.get("tree").is_none() && tombstone.get("history").is_none());
    // An uncollected record af cannot read refuses replay (ADR-0152 clause 2) until it can.
    let (history, refused) =
        machine.expect(&repo, &[], &["project", "history"], i32::from(enforced));
    if enforced {
        assert!(refused.contains("whose record cannot be read"), "{refused}");
    } else {
        let line = history
            .lines()
            .find(|line| line.starts_with(surveys[0].as_str()))
            .unwrap();
        assert!(line.contains("  collected (unreadable) "), "{line}");
    }

    if enforced {
        // Refused and reported; its record and the log are as they were.
        let refused = failures_naming(&pruned, &path_of(&machine, &surveys[1]));
        assert_eq!(refused.len(), 1, "{pruned}");
        assert!(refused[0].contains("cannot be read"), "{}", refused[0]);
        assert!(
            !collected
                .iter()
                .any(|removal| removal["path"] == path_of(&machine, &surveys[1]).as_str())
        );
        assert!(
            std::fs::symlink_metadata(&locked).is_ok(),
            "the record stays"
        );
        let mut expected = tombstones_before.clone();
        expected.push(surveys[0].clone());
        assert_eq!(machine.collected(), expected, "no tombstone for it");
    }
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o444)).unwrap();
    // Readable again, the next pass collects it with its totals.
    let pruned = prune(&machine, &repo, &keep);
    if enforced {
        assert_eq!(
            removed(&pruned, "keep_surveys"),
            vec![path_of(&machine, &surveys[1])]
        );
    }
    let (text, _) = machine.expect(&repo, &[], &["project", "history"], 0);
    assert_eq!(
        text.matches("  collected (unreadable) ").count(),
        1,
        "{text}"
    );
    assert_consistent(&machine);
}

#[test]
fn the_preview_lists_held_surveys_as_held_and_agrees_with_the_sweep() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let surveys: Vec<String> = (0..4)
        .map(|n| refresh(&machine, &repo, &at(n), true))
        .collect();
    let keep = [("AF_STORAGE__KEEP_SURVEYS", "1")];
    let surveys_of = |document: &Value, list: &str| -> Vec<Value> {
        document[list]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["kind"] == "survey")
            .cloned()
            .collect()
    };
    let preview_json = |machine: &Machine| {
        let preview = machine.json(&repo, &keep, &["storage", "prune", "--json"]);
        schemas::valid(&schemas::validator("storage-prune-v2.json"), &preview);
        preview
    };

    let lock = std::fs::OpenOptions::new()
        .write(true)
        .open(machine.store().join("project.lock"))
        .unwrap();
    let lock = nix::fcntl::Flock::lock(lock, nix::fcntl::FlockArg::LockExclusive).unwrap();
    let preview = preview_json(&machine);
    let held = left(&preview, "held");
    let expected: Vec<String> = surveys[..3]
        .iter()
        .map(|id| path_of(&machine, id))
        .collect();
    assert_eq!(
        held.iter()
            .map(|entry| entry["path"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        expected,
        "{preview}"
    );
    for entry in &held {
        assert!(entry["bytes"].as_u64().unwrap() > 0, "{entry}");
        assert_eq!(entry["kind"], "survey");
        assert!(
            entry["detail"]
                .as_str()
                .unwrap()
                .contains("a refresh or a plan holds its Project Store"),
            "{entry}"
        );
    }
    assert!(surveys_of(&preview, "removals").is_empty(), "{preview}");
    let (text, _) = machine.expect(&repo, &keep, &["storage", "prune"], 0);
    assert_eq!(text.matches("held survey ").count(), 3, "{text}");
    let applied = prune(&machine, &repo, &keep);
    assert_eq!(left(&applied, "held"), held, "entry for entry");
    assert_eq!(
        surveys_of(&applied, "removals"),
        surveys_of(&preview, "removals")
    );
    assert!(machine.collected().is_empty());
    assert!(surveys.iter().all(|id| machine.held(id)));
    drop(lock);

    // Released, the preview and the sweep name the same removals, entry for entry.
    let preview = preview_json(&machine);
    assert!(left(&preview, "held").is_empty());
    let applied = prune(&machine, &repo, &keep);
    assert!(left(&applied, "held").is_empty());
    assert_eq!(
        surveys_of(&applied, "removals"),
        surveys_of(&preview, "removals")
    );
    assert_eq!(removed(&applied, "keep_surveys"), expected);
    assert_eq!(machine.collected(), surveys[..3].to_vec());
    assert_consistent(&machine);
}

/// `git` with raw path bytes: a blob of `content` at `path` in the index.
fn add_raw(repo: &Path, path: &[u8], content: &str) {
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    let mut hashing = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["hash-object", "-w", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    hashing
        .stdin
        .take()
        .unwrap()
        .write_all(content.as_bytes())
        .unwrap();
    let blob = String::from_utf8(hashing.wait_with_output().unwrap().stdout).unwrap();
    let mut info = format!("100644,{},", blob.trim()).into_bytes();
    info.extend_from_slice(path);
    let status = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["update-index", "--add", "--cacheinfo"])
        .arg(std::ffi::OsStr::from_bytes(&info))
        .status()
        .unwrap();
    assert!(status.success());
}

/// `?` consumes one byte of the canonical encoded spelling in the Surveyor's `exclude`, as in
/// the routes of `.af/af.toml` (`crate::project`'s tests run the same cases through routing):
/// `café.rs` spells `é` in two bytes, a non-UTF-8 `caf\xe9.rs` is `caf%E9.rs` and `50%.txt` is
/// `50%25.txt`.
#[test]
fn a_question_mark_is_one_byte_in_the_surveyors_exclude() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    add_raw(&repo, "café.rs".as_bytes(), "accent\n");
    add_raw(&repo, b"caf\xe9.rs", "latin-1\n");
    add_raw(&repo, b"50%.txt", "percent\n");
    git(&repo, &["commit", "-q", "-m", "Spellings"]);
    machine.expect(&repo, &[], &["project", "refresh"], 0);
    let all = tree_files(&machine, &repo);
    assert_eq!(all, 7);
    for (pattern, excluded) in [
        ("caf?.rs", 0),
        ("caf??.rs", 1),
        ("caf???.rs", 1),
        ("50?.txt", 0),
        ("50???.txt", 1),
        ("caf*.rs", 2),
    ] {
        write(
            &repo,
            ".af/af.toml",
            &format!(
                "version = 1\n\n[project]\nname = \"collect-fixture\"\nmin_af = \"0.1\"\n\n\
                 [defaults]\npipeline = \"review\"\n\n[survey]\nexclude = [\"{pattern}\"]\n"
            ),
        );
        // Not `-a`: the index holds paths the working tree does not.
        git(&repo, &["add", ".af/af.toml"]);
        git(&repo, &["commit", "-q", "-m", pattern]);
        machine.expect(&repo, &[], &["project", "refresh"], 0);
        assert_eq!(
            tree_files(&machine, &repo),
            all - excluded,
            "`{pattern}` excludes {excluded}"
        );
    }
}

/// ADR-0145 clause 2: the documents Survey collection changed are `af/storage@3` and
/// `af/storage-prune@2`; their superseded versions are refused and no schema of them ships.
#[test]
fn superseded_storage_documents_are_refused() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let mut shown = machine.json(&repo, &[], &["storage", "--json"]);
    let current = schemas::validator("storage-v3.json");
    assert!(current.is_valid(&shown));
    shown["schema"] = serde_json::json!("af/storage@2");
    assert!(!current.is_valid(&shown));
    let mut pruned = machine.json(&repo, &[], &["storage", "prune", "--json"]);
    let current = schemas::validator("storage-prune-v2.json");
    assert!(current.is_valid(&pruned));
    pruned["schema"] = serde_json::json!("af/storage-prune@1");
    assert!(!current.is_valid(&pruned));
    let schemas = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .join("schemas");
    for superseded in [
        "storage-v1.json",
        "storage-v2.json",
        "storage-prune-v1.json",
    ] {
        assert!(!schemas.join(superseded).exists(), "{superseded}");
    }
}

/// A Survey's record replaced between its identity read and its rename, by the debug build's
/// hook: a byte-identical copy, another inode, takes its name and the original moves aside.
#[test]
fn a_record_replaced_before_its_rename_is_refused_and_a_rerun_collects_it() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let surveys: Vec<String> = (0..4)
        .map(|n| refresh(&machine, &repo, &at(n), true))
        .collect();
    let target = &surveys[0];
    let object = machine.object(target);
    let original = std::fs::read(&object).unwrap();
    let aside = machine.root().join("aside");
    std::fs::create_dir_all(&aside).unwrap();
    let keep = ("AF_STORAGE__KEEP_SURVEYS", "3");
    let pruned = prune(
        &machine,
        &repo,
        &[
            keep,
            (
                "AF_TEST_SURVEY_REPLACE_BEFORE_RENAME",
                aside.to_str().unwrap(),
            ),
        ],
    );
    let refused = failures_naming(&pruned, &path_of(&machine, target));
    assert_eq!(refused.len(), 1, "{pruned}");
    assert!(
        refused[0].contains(&format!(
            "Survey {target} is refused (replaced during collection)"
        )),
        "{}",
        refused[0]
    );
    assert!(removed(&pruned, "keep_surveys").is_empty(), "{pruned}");
    // The applied sweep ran the plan: the entry keeps its place as failed, freed nothing and
    // left the running total as it was.
    let marked: Vec<&Value> = pruned["removals"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|removal| removal["path"] == path_of(&machine, target).as_str())
        .collect();
    assert_eq!(marked.len(), 1, "{pruned}");
    assert_eq!(marked[0]["rule"], "keep_surveys");
    assert_eq!(marked[0]["bytes"], 0);
    assert_eq!(
        marked[0]["outcome"].as_str(),
        Some(format!("failed: {}", refused[0]).as_str())
    );
    assert_eq!(marked[0]["total_after"], pruned["total_before"]);
    assert_eq!(pruned["total_after"], pruned["total_before"]);
    // Nothing is deleted: the original is where the replacement put it, and its name holds the
    // copy, the Survey's record still.
    let moved = aside.join(object.file_name().unwrap());
    assert_eq!(std::fs::read(&moved).unwrap(), original);
    assert_eq!(std::fs::read(&object).unwrap(), original);
    assert!(
        std::fs::read_dir(machine.store().join("cas/removing"))
            .unwrap()
            .next()
            .is_none()
    );
    // Its tombstone was appended before the rename, so the history answers from it.
    assert_eq!(machine.collected(), vec![target.clone()]);
    for survey_id in &surveys[1..] {
        assert!(machine.held(survey_id));
    }

    // A rerun deletes the record its name holds, and only that.
    let pruned = prune(&machine, &repo, &[keep]);
    assert_eq!(
        removed(&pruned, "recovery"),
        vec![path_of(&machine, target)],
        "{pruned}"
    );
    assert!(
        pruned["failures"].as_array().unwrap().is_empty(),
        "{pruned}"
    );
    assert!(!machine.held(target));
    assert_eq!(
        std::fs::read(&moved).unwrap(),
        original,
        "nothing unrelated"
    );
    assert_eq!(machine.collected(), vec![target.clone()]);
    assert_consistent(&machine);
}

/// The one ceiling the Surveyor writes within: `max_survey_bytes`, 1 MiB.
const MAX_SURVEY_BYTES: usize = 1_048_576;

#[test]
fn an_oversize_survey_is_refused_and_a_corrupt_one_within_the_ceiling_is_unreadable() {
    use std::os::unix::fs::PermissionsExt;
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let surveys: Vec<String> = (0..4)
        .map(|n| refresh(&machine, &repo, &at(n), true))
        .collect();
    // Written by the test, not the Surveyor: one byte over the ceiling, and corrupt bytes at it.
    let oversize = machine.object(&surveys[0]);
    std::fs::set_permissions(&oversize, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::write(&oversize, vec![b'x'; MAX_SURVEY_BYTES + 1]).unwrap();
    let corrupt = machine.object(&surveys[1]);
    std::fs::set_permissions(&corrupt, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::write(&corrupt, vec![b'x'; MAX_SURVEY_BYTES]).unwrap();
    let events_before = machine.events();

    let (_, entries) = survey_entries(&machine, &repo);
    let entry = &entries[&path_of(&machine, &surveys[0])];
    assert!(
        entry["in_use"]
            .as_str()
            .unwrap()
            .starts_with("refused (oversize, 1048577 bytes)"),
        "{entry}"
    );

    let keep = [("AF_STORAGE__KEEP_SURVEYS", "2")];
    let preview = preview(&machine, &repo, &keep);
    let pruned = prune(&machine, &repo, &keep);
    for document in [&preview, &pruned] {
        let oversize = left(document, "oversize");
        assert_eq!(oversize.len(), 1, "{document}");
        assert_eq!(oversize[0]["path"], path_of(&machine, &surveys[0]).as_str());
        assert!(oversize[0]["bytes"].as_u64().unwrap() > MAX_SURVEY_BYTES as u64);
        assert!(
            oversize[0]["detail"].as_str().unwrap().contains(&format!(
                "Survey {} is refused (oversize, 1048577 bytes)",
                surveys[0]
            )),
            "{}",
            oversize[0]
        );
        assert!(
            failures_naming(document, &path_of(&machine, &surveys[0])).is_empty(),
            "left, not failed: {document}"
        );
        assert_eq!(
            removed(document, "keep_surveys"),
            vec![path_of(&machine, &surveys[1])],
            "{document}"
        );
    }
    assert_eq!(preview["removals"], pruned["removals"]);
    // Left in place, its place in the log unchanged.
    assert_eq!(
        std::fs::metadata(&oversize).unwrap().len(),
        MAX_SURVEY_BYTES as u64 + 1
    );
    assert_eq!(machine.collected(), vec![surveys[1].clone()]);
    let events_after: Vec<_> = machine
        .events()
        .into_iter()
        .filter(|(_, kind, _)| kind != "SurveyCollected@1")
        .collect();
    assert_eq!(events_after, events_before);
    // The corrupt one within the ceiling is collected `unreadable`.
    let tombstone = machine
        .events()
        .into_iter()
        .find(|(_, kind, _)| kind == "SurveyCollected@1")
        .unwrap()
        .2;
    assert_eq!(tombstone["unreadable"], "its bytes do not match its digest");
    assert!(!corrupt.exists());
}

#[test]
fn an_unwritable_project_lock_holds_every_survey_in_preview_and_apply_alike() {
    use std::os::unix::fs::PermissionsExt;
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let surveys: Vec<String> = (0..4)
        .map(|n| refresh(&machine, &repo, &at(n), true))
        .collect();
    let lock = machine.store().join("project.lock");
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o444)).unwrap();
    if std::fs::OpenOptions::new().write(true).open(&lock).is_ok() {
        // A user the file mode does not bind (root): nothing to see.
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
        return;
    }
    let keep = [("AF_STORAGE__KEEP_SURVEYS", "1")];
    let held_of = |document: &Value| -> Vec<Value> { left(document, "held") };
    let preview = preview(&machine, &repo, &keep);
    let held = held_of(&preview);
    let expected: Vec<String> = surveys[..3]
        .iter()
        .map(|id| path_of(&machine, id))
        .collect();
    assert_eq!(
        held.iter()
            .map(|entry| entry["path"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        expected,
        "{preview}"
    );
    let reason = held[0]["detail"].as_str().unwrap().to_string();
    assert!(reason.starts_with("lock unavailable: "), "{reason}");
    for entry in &held {
        assert_eq!(entry["detail"], reason.as_str());
        assert!(entry["bytes"].as_u64().unwrap() > 0, "{entry}");
    }
    assert!(removed(&preview, "keep_surveys").is_empty(), "{preview}");
    let (text, _) = machine.expect(&repo, &keep, &["storage", "prune"], 0);
    assert_eq!(text.matches("held survey ").count(), 3, "{text}");
    assert!(text.contains("lock unavailable: "), "{text}");

    let applied = prune(&machine, &repo, &keep);
    assert_eq!(held_of(&applied), held, "entry for entry, same reason");
    assert!(removed(&applied, "keep_surveys").is_empty(), "{applied}");
    assert!(machine.collected().is_empty());
    assert!(surveys.iter().all(|id| machine.held(id)));
    // Inventory names the same reason, so the budget takes none of them either.
    let (_, entries) = survey_entries(&machine, &repo);
    for survey_id in &surveys {
        assert!(
            entries[&path_of(&machine, survey_id)]["in_use"]
                .as_str()
                .unwrap()
                .starts_with("lock unavailable: ")
        );
    }
    let tiny = prune(&machine, &repo, &[keep[0], ("AF_STORAGE__MAX_BYTES", "1")]);
    assert!(removed(&tiny, "budget").is_empty(), "{tiny}");
    assert!(machine.collected().is_empty());

    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
    let applied = prune(&machine, &repo, &keep);
    assert_eq!(removed(&applied, "keep_surveys"), expected);
    assert_consistent(&machine);
}

/// A removal a process that died left claimed is part of the one inventory: `af storage prune
/// --json` and `--apply --json` list the same `claim` entry under `recovery` with the bytes the
/// inventory measured and agree entry for entry, and only the applied sweep removes it. A
/// container a dead Survey collection left empty is planned the same way.
#[test]
fn an_abandoned_claim_is_planned_and_listed_alike_by_preview_and_apply() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    refresh(&machine, &repo, &at(0), true);
    let claim = machine
        .root()
        .join("state/af/review/campaigns/.af-removing-999999999-7-0");
    std::fs::create_dir_all(claim.join("cas")).unwrap();
    std::fs::write(claim.join("cas/object"), vec![1_u8; 64 * 1024]).unwrap();
    let claim_bytes = allocated(&claim);
    let container = machine.store().join("cas/removing").join(EMPTY_PASS);
    std::fs::create_dir_all(&container).unwrap();

    let claims = |document: &Value| -> Vec<Value> {
        document["removals"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|removal| removal["kind"] == "claim")
            .cloned()
            .collect()
    };
    let previewed = preview(&machine, &repo, &[]);
    let planned = claims(&previewed);
    assert_eq!(planned.len(), 2, "{previewed}");
    assert_eq!(planned[0]["path"], claim.to_str().unwrap());
    assert_eq!(planned[0]["rule"], "recovery");
    assert_eq!(planned[0]["bytes"].as_u64().unwrap(), claim_bytes);
    let total_before = previewed["total_before"].as_u64().unwrap();
    assert_eq!(
        planned[0]["total_after"].as_u64().unwrap(),
        total_before - claim_bytes,
        "counted in the total it had"
    );
    assert_eq!(planned[1]["path"], container.to_str().unwrap());
    assert_eq!(planned[1]["rule"], "recovery");
    assert!(
        claim.join("cas/object").is_file(),
        "a preview removes nothing"
    );
    assert!(container.is_dir());
    let (text, _) = machine.expect(&repo, &[], &["storage", "prune"], 0);
    assert!(
        text.contains(&format!("would remove claim {}", claim.display())),
        "{text}"
    );

    // The applied sweep runs that plan: the same entries, the same bytes, entry for entry.
    let applied = prune(&machine, &repo, &[]);
    assert_eq!(claims(&applied), planned, "entry for entry");
    assert_eq!(applied["total_before"], previewed["total_before"]);
    assert!(!claim.exists(), "the claim is finished");
    assert!(!container.exists(), "the empty container goes");
    assert!(claims(&preview(&machine, &repo, &[])).is_empty());
}

/// A Survey's record replaced between inventory and the plan's judgement, by the debug build's
/// hook: a byte-identical copy, another inode, takes its name. The plan refuses the entry
/// (`replaced since inventory`) and never adopts the copy, the applied sweep deletes nothing of
/// either file and appends no tombstone, and its totals are what the directory holds. A rerun
/// measures the copy and collects it.
#[test]
fn a_record_replaced_between_inventory_and_judgement_is_refused_and_nothing_is_deleted() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let surveys: Vec<String> = (0..4)
        .map(|n| refresh(&machine, &repo, &at(n), true))
        .collect();
    let target = &surveys[0];
    let object = machine.object(target);
    let original = std::fs::read(&object).unwrap();
    let aside = machine.root().join("aside");
    std::fs::create_dir_all(&aside).unwrap();
    let keep = ("AF_STORAGE__KEEP_SURVEYS", "3");
    let pruned = prune(
        &machine,
        &repo,
        &[
            keep,
            (
                "AF_TEST_SURVEY_REPLACE_BEFORE_JUDGE",
                aside.to_str().unwrap(),
            ),
        ],
    );
    let refused = failures_naming(&pruned, &path_of(&machine, target));
    assert_eq!(refused.len(), 1, "{pruned}");
    assert!(
        refused[0].contains(&format!(
            "Survey {target} is refused (replaced since inventory)"
        )),
        "{}",
        refused[0]
    );
    assert!(
        pruned["removals"].as_array().unwrap().is_empty(),
        "the plan takes nothing of it: {pruned}"
    );
    assert!(machine.collected().is_empty(), "no tombstone");
    let moved = aside.join(object.file_name().unwrap());
    assert_eq!(
        std::fs::read(&moved).unwrap(),
        original,
        "the original is untouched"
    );
    assert_eq!(
        std::fs::read(&object).unwrap(),
        original,
        "the copy is untouched"
    );
    assert!(!machine.store().join("cas/removing").exists());
    assert_eq!(pruned["total_after"], pruned["total_before"]);
    assert_eq!(
        pruned["total_after"].as_u64().unwrap(),
        storage_total(&machine, &repo),
        "the totals are what the directory holds"
    );

    // The next pass measures the copy and collects it, and only it.
    let pruned = prune(&machine, &repo, &[keep]);
    assert_eq!(
        removed(&pruned, "keep_surveys"),
        vec![path_of(&machine, target)]
    );
    assert!(!machine.held(target));
    assert_eq!(
        std::fs::read(&moved).unwrap(),
        original,
        "nothing unrelated"
    );
    assert_consistent(&machine);
}

/// A tombstoned Survey's ID file inside a container whose own name is not UTF-8 is no dead
/// pass's record: preview and apply both list it `unreadable_name`, its path in the encoded
/// spelling, and leave it and its container in place. A file system that refuses such names
/// (APFS) has nothing to show.
#[test]
fn a_survey_file_in_a_container_with_a_name_that_is_not_utf8_is_reported_and_left() {
    use std::os::unix::ffi::OsStrExt;
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let surveys: Vec<String> = (0..3)
        .map(|n| refresh(&machine, &repo, &at(n), true))
        .collect();
    let keep = ("AF_STORAGE__KEEP_SURVEYS", "2");
    prune(
        &machine,
        &repo,
        &[keep, ("AF_TEST_SURVEY_STOP_AFTER_TOMBSTONES", "1")],
    );
    let target = &surveys[0];
    assert_eq!(machine.collected(), vec![target.clone()]);
    let container = machine
        .store()
        .join("cas/removing")
        .join(std::ffi::OsStr::from_bytes(
            b"0f1e2d3c4b5a69788796a5b4c3d2e1\xe9",
        ));
    match std::fs::create_dir_all(&container) {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == Some(nix::libc::EILSEQ) => return,
        Err(error) => panic!("{error}"),
    }
    let leftover = container.join(target);
    std::fs::rename(machine.object(target), &leftover).unwrap();
    let expected = review_core::encode_path(leftover.as_os_str().as_bytes());
    assert!(
        expected.ends_with(&format!(
            "/0f1e2d3c4b5a69788796a5b4c3d2e1%E9/sha256%3A{}",
            target.trim_start_matches("sha256:")
        )),
        "{expected}"
    );
    for document in [
        preview(&machine, &repo, &[keep]),
        prune(&machine, &repo, &[keep]),
    ] {
        let unreadable = left(&document, "unreadable_name");
        assert_eq!(unreadable.len(), 1, "{document}");
        assert_eq!(unreadable[0]["path"], expected.as_str());
        assert_eq!(unreadable[0]["path_encoded"], true);
        assert!(unreadable[0]["bytes"].as_u64().unwrap() > 0);
        assert!(
            removed(&document, "recovery").is_empty(),
            "never the Survey's record: {document}"
        );
        assert!(leftover.is_file(), "left in place");
    }
    assert_eq!(
        machine.collected(),
        vec![target.clone()],
        "no tombstone twice"
    );
}

/// Append a registration of `kind` at `path` to the machine's Store registry.
fn register(machine: &Machine, path: &Path, kind: &str) {
    let registry = machine.root().join("state/af/stores.toml");
    let mut text = std::fs::read_to_string(&registry).unwrap_or_else(|_| "version = 2\n".into());
    text.push_str(&format!(
        "\n[[store]]\npath = \"{}\"\nkind = \"{kind}\"\nfirst_use_unix_ms = 1\nlast_use_unix_ms = 1\n",
        path.display()
    ));
    std::fs::write(&registry, text).unwrap();
}

/// The removals of `kind` in a sweep's document.
fn removals_of(document: &Value, kind: &str) -> Vec<Value> {
    document["removals"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|removal| removal["kind"] == kind)
        .cloned()
        .collect()
}

/// A dead process's claim holding `bytes` at `claim`.
fn dead_claim(claim: &Path, bytes: usize) -> u64 {
    std::fs::create_dir_all(claim.join("cas")).unwrap();
    std::fs::write(claim.join("cas/object"), vec![1_u8; bytes]).unwrap();
    allocated(claim)
}

/// A registered Store at a custom path that a dead process renamed into its claim: apply drops
/// no registration before the inventory, so the inventory reaches the claim through the
/// registered path's parent. Preview and apply list the same `recovery` entry and the same
/// registration's removal, and after apply the claim and the registration are gone.
#[test]
fn a_registered_store_renamed_into_a_dead_claim_is_recovered_and_unregistered_alike() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    refresh(&machine, &repo, &at(0), true);
    let parent = machine.root().join("custom/stores");
    let registered = parent.join("state");
    let claim = parent.join(".af-removing-999999999-3-0");
    let claim_bytes = dead_claim(&claim, 64 * 1024);
    register(&machine, &registered, "task");

    let previewed = preview(&machine, &repo, &[]);
    let planned = removals_of(&previewed, "claim");
    assert_eq!(planned.len(), 1, "{previewed}");
    assert_eq!(planned[0]["path"], claim.to_str().unwrap());
    assert_eq!(planned[0]["rule"], "recovery");
    assert_eq!(planned[0]["bytes"].as_u64().unwrap(), claim_bytes);
    assert_eq!(
        previewed["registry_dropped"],
        serde_json::json!([registered.to_str().unwrap()]),
        "{previewed}"
    );
    assert!(claim.is_dir(), "a preview removes nothing");

    let applied = prune(&machine, &repo, &[]);
    assert_eq!(removals_of(&applied, "claim"), planned, "entry for entry");
    assert_eq!(applied["registry_dropped"], previewed["registry_dropped"]);
    assert!(!claim.exists(), "the claim is finished");
    let registry = std::fs::read_to_string(machine.root().join("state/af/stores.toml")).unwrap();
    assert!(
        !registry.contains(registered.to_str().unwrap()),
        "{registry}"
    );
    assert!(registry.contains("kind = \"project\""), "{registry}");
    let after = preview(&machine, &repo, &[]);
    assert!(removals_of(&after, "claim").is_empty(), "{after}");
    assert_eq!(after["registry_dropped"], serde_json::json!([]));
}

/// A claimed Workspace `state` directory under a Project Store is in the Store's measured
/// bytes: counted once, so `total_before` is what `af storage` measures, and the recovery takes
/// the claim's bytes out of the Store's, leaving `total_after` the measured total less them.
/// `af storage` lists the nested claim all the same, a `claim` entry `abandoned` with its
/// measured bytes and `counted_in` the Project Store, whose bytes hold them: its entries are the
/// claims the preview recovers, and `total_bytes` and `kinds` count its bytes once.
#[test]
fn a_claim_under_a_project_store_is_counted_once() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    refresh(&machine, &repo, &at(0), true);
    let workspace = machine.store().join("workspaces/main");
    let claim = workspace.join(".af-removing-999999999-4-0");
    let claim_bytes = dead_claim(&claim, 128 * 1024);
    register(&machine, &workspace.join("state"), "task");

    let measured = storage_total(&machine, &repo);
    let (shown, _) = survey_entries(&machine, &repo);
    let listed = shown["entries"].as_array().unwrap();
    let of_store: u64 = listed
        .iter()
        .filter(|entry| {
            entry.get("counted_in").is_none()
                && entry["path"]
                    .as_str()
                    .unwrap()
                    .starts_with(machine.store().to_str().unwrap())
        })
        .map(|entry| entry["bytes"].as_u64().unwrap())
        .sum();
    assert_eq!(of_store, allocated(&machine.store()), "the directory, once");
    let claims: Vec<&Value> = listed
        .iter()
        .filter(|entry| entry["kind"] == "claim")
        .collect();
    assert_eq!(claims.len(), 1, "{shown}");
    assert_eq!(claims[0]["path"], claim.to_str().unwrap());
    assert_eq!(claims[0]["state"], "abandoned");
    assert_eq!(claims[0]["bytes"].as_u64().unwrap(), claim_bytes);
    assert_eq!(claims[0]["counted_in"], machine.store().to_str().unwrap());
    let counted: u64 = listed
        .iter()
        .filter(|entry| entry.get("counted_in").is_none())
        .map(|entry| entry["bytes"].as_u64().unwrap())
        .sum();
    assert_eq!(shown["total_bytes"].as_u64().unwrap(), counted);
    assert_eq!(counted, measured);
    let kind_bytes: u64 = shown["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|kind| kind["bytes"].as_u64().unwrap())
        .sum();
    assert_eq!(kind_bytes, measured, "every kind counts its bytes once");
    let claim_kind = shown["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|kind| kind["kind"] == "claim")
        .unwrap();
    assert_eq!(claim_kind["entries"], 1);
    assert_eq!(claim_kind["bytes"], 0, "counted in the Project Store");
    let (text, _) = machine.expect(&repo, &[], &["storage"], 0);
    let line = text
        .lines()
        .find(|line| line.starts_with(&format!("abandoned removal {} (", claim.display())))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(
        line.ends_with(&format!(") counted in {}", machine.store().display())),
        "{line}"
    );

    let previewed = preview(&machine, &repo, &[]);
    assert_eq!(previewed["total_before"].as_u64().unwrap(), measured);
    let planned = removals_of(&previewed, "claim");
    assert_eq!(planned.len(), 1, "{previewed}");
    assert_eq!(planned[0]["path"], claims[0]["path"], "the listed claims");
    assert_eq!(planned[0]["bytes"], claims[0]["bytes"]);
    assert_eq!(
        planned[0]["total_after"].as_u64().unwrap(),
        measured - claim_bytes
    );

    let applied = prune(&machine, &repo, &[]);
    assert_eq!(applied["total_before"].as_u64().unwrap(), measured);
    assert_eq!(
        applied["total_after"].as_u64().unwrap(),
        measured - claim_bytes
    );
    assert!(!claim.exists());
    assert_eq!(storage_total(&machine, &repo), measured - claim_bytes);
}

/// A campaign directory nested deeper than the removal walk's 128 levels is left, and said, by
/// the preview and the applied sweep alike: never planned, so never partly removed, and listed
/// in use by `af storage`.
#[test]
fn a_campaign_deeper_than_the_removal_walk_is_left_and_reported() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    refresh(&machine, &repo, &at(0), true);
    let campaign = machine
        .root()
        .join("state/af/review/campaigns")
        .join(format!("c-{}", "d".repeat(64)));
    std::fs::create_dir_all(&campaign).unwrap();
    review_store::EventStore::open(campaign.join("events.sqlite")).unwrap();
    let mut deep = campaign.join("deep");
    for _ in 0..130 {
        deep = deep.join("d");
    }
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join("file"), b"x").unwrap();
    let old = SystemTime::now() - Duration::from_secs(60 * DAY);
    std::fs::File::open(campaign.join("events.sqlite"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    let keep = [("AF_STORAGE__KEEP_CAMPAIGNS", "0")];

    let (shown, _) = survey_entries(&machine, &repo);
    let entry = shown["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == campaign.to_str().unwrap())
        .unwrap_or_else(|| panic!("{shown}"))
        .clone();
    assert!(
        entry["in_use"]
            .as_str()
            .unwrap()
            .starts_with("left: deeper than the removal walk"),
        "{entry}"
    );
    for document in [
        preview(&machine, &repo, &keep),
        prune(&machine, &repo, &keep),
    ] {
        assert!(removals_of(&document, "campaign").is_empty(), "{document}");
        let deep = left(&document, "too_deep");
        assert_eq!(deep.len(), 1, "{document}");
        assert_eq!(deep[0]["kind"], "campaign");
        assert_eq!(deep[0]["path"], campaign.to_str().unwrap());
        assert_eq!(deep[0]["bytes"], entry["bytes"]);
        assert!(
            deep[0]["detail"]
                .as_str()
                .unwrap()
                .starts_with("deeper than the removal walk"),
            "{}",
            deep[0]
        );
        assert!(deep_exists(&campaign), "left whole");
    }
    let (text, _) = machine.expect(&repo, &keep, &["storage", "prune"], 0);
    assert!(text.contains("left campaign "), "{text}");
    assert!(
        text.contains(", too deep: deeper than the removal walk"),
        "{text}"
    );
}

fn deep_exists(campaign: &Path) -> bool {
    let mut deep = campaign.join("deep");
    for _ in 0..130 {
        deep = deep.join("d");
    }
    deep.join("file").is_file()
}

/// The keep_surveys = 1 forged-cutoff case: the oldest Survey's `SurveyRecorded@1` names a
/// recent cutoff its record does not record. Its event is verified once, in the Store's one
/// read: the inventory uses its record's cutoff, never the forged one, so the collection pass
/// picks it as the old Survey it is, and preview and apply both refuse and report it; nothing is
/// collected for it, and the other Survey beyond the limit is.
#[test]
fn a_forged_cutoff_is_never_used_and_its_survey_is_refused_and_kept() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let surveys: Vec<String> = (0..3)
        .map(|n| refresh(&machine, &repo, &at(n), true))
        .collect();
    let target = &surveys[0];
    let log = rusqlite::Connection::open(machine.store().join("events.sqlite")).unwrap();
    let (sequence, payload): (i64, String) = log
        .query_row(
            "SELECT sequence, payload FROM events WHERE type = 'SurveyRecorded@1' AND \
             json_extract(payload, '$.survey_id') = ?1",
            [target.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let mut payload: Value = serde_json::from_str(&payload).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    payload["cutoff_unix_s"] = if payload["cutoff_unix_s"].is_string() {
        Value::String(now.to_string())
    } else {
        Value::from(now)
    };
    log.execute(
        "UPDATE events SET payload = ?1 WHERE sequence = ?2 AND type = 'SurveyRecorded@1'",
        rusqlite::params![payload.to_string(), sequence],
    )
    .unwrap();
    drop(log);

    // The inventory's last use is the record's cutoff, and the Survey is in use for its refusal.
    let (_, entries) = survey_entries(&machine, &repo);
    let entry = &entries[&path_of(&machine, target)];
    let recorded_ms = (old_cutoff()) * 1000;
    assert_eq!(
        entry["last_use_unix_ms"].as_u64().unwrap(),
        recorded_ms,
        "{entry}"
    );
    assert!(
        entry["in_use"]
            .as_str()
            .unwrap()
            .contains("names a cutoff_unix_s its record does not record"),
        "{entry}"
    );
    let refused = format!(
        "Survey {target} is refused: its SurveyRecorded@1 names a cutoff_unix_s its record does \
         not record, so it is kept"
    );
    let keep = [("AF_STORAGE__KEEP_SURVEYS", "1")];
    for document in [
        preview(&machine, &repo, &keep),
        prune(&machine, &repo, &keep),
    ] {
        let said: Vec<&str> = document["failures"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .filter(|failure| failure.contains(&refused))
            .collect();
        assert_eq!(said.len(), 1, "{document}");
        assert_eq!(
            removed(&document, "keep_surveys"),
            vec![path_of(&machine, &surveys[1])],
            "{document}"
        );
    }
    assert_eq!(
        machine.collected(),
        vec![surveys[1].clone()],
        "no tombstone for it"
    );
    assert!(machine.held(target), "its record is kept");
}

/// Design review 16's blocker: a collected Survey whose record a dead pass moved into its
/// container, and a byte-identical replacement that took its CAS name since. The first sweep
/// settles the moved record and lists the replacement `foreign`; it records that the Survey's
/// record left the CAS, so the next sweep, which finds no moved record and a file whose bytes
/// hash to the Survey's ID, lists it `foreign` again and deletes nothing of it. Every sweep
/// after that does the same.
#[test]
fn a_byte_identical_replacement_at_a_settled_surveys_name_survives_two_sweeps_as_foreign() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let surveys: Vec<String> = (0..4)
        .map(|n| refresh(&machine, &repo, &at(n), true))
        .collect();
    let keep = ("AF_STORAGE__KEEP_SURVEYS", "3");
    prune(
        &machine,
        &repo,
        &[keep, ("AF_TEST_SURVEY_STOP_AFTER_TOMBSTONES", "1")],
    );
    let target = &surveys[0];
    assert_eq!(machine.collected(), vec![target.clone()]);
    let object = machine.object(target);
    let record = std::fs::read(&object).unwrap();
    let dead = machine.store().join("cas/removing").join(DEAD_PASS);
    std::fs::create_dir_all(&dead).unwrap();
    std::fs::rename(&object, dead.join(target)).unwrap();
    std::fs::write(&object, &record).unwrap();
    let replacement = {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(&object).unwrap().ino()
    };

    for sweep in [
        "the sweep that settles the moved record",
        "the sweep after it",
    ] {
        let applied = prune(&machine, &repo, &[keep, ("AF_STORAGE__MAX_BYTES", "1")]);
        let foreign = left(&applied, "foreign");
        assert_eq!(foreign.len(), 1, "{sweep}: {applied}");
        assert_eq!(foreign[0]["path"], path_of(&machine, target).as_str());
        assert_eq!(foreign[0]["kind"], "survey");
        assert_eq!(foreign[0]["bytes"].as_u64().unwrap(), allocated(&object));
        assert!(
            applied["removals"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|removal| removal["path"] == path_of(&machine, target).as_str())
                .all(|removal| removal["kind"] == "survey" && removal["rule"] == "recovery"),
            "{sweep}: only the moved record is ever planned: {applied}"
        );
        assert_eq!(
            std::fs::read(&object).unwrap(),
            record,
            "{sweep}: the replacement is left"
        );
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            std::fs::symlink_metadata(&object).unwrap().ino(),
            replacement,
            "{sweep}: the same file"
        );
        assert!(!dead.exists(), "{sweep}: the moved record is settled");
    }
    let settled: Value =
        serde_json::from_slice(&std::fs::read(machine.store().join("settled.json")).unwrap())
            .unwrap();
    assert_eq!(settled["schema"], "af/ProjectSurveysSettled@1");
    assert!(
        settled["survey_ids"]
            .as_array()
            .unwrap()
            .contains(&Value::String(target.clone())),
        "{settled}"
    );
    schemas::valid(
        &schemas::validator("project-surveys-settled-v1.json"),
        &settled,
    );
    assert_eq!(
        machine
            .collected()
            .iter()
            .filter(|id| *id == target)
            .count(),
        1,
        "no tombstone twice"
    );
}

/// The digest af names the bytes `bytes` by in a CAS.
fn digest_of(bytes: &[u8]) -> String {
    review_store::canonical::blob_content_id(bytes)
}

/// Put `bytes` into the Project Store's CAS at their digest, as a writer that crashed before its
/// `SurveyRecorded@1` committed leaves them, and return that digest.
fn unrecorded_object(machine: &Machine, bytes: &[u8]) -> String {
    let survey_id = digest_of(bytes);
    let object = machine.object(&survey_id);
    std::fs::create_dir_all(object.parent().unwrap()).unwrap();
    std::fs::write(&object, bytes).unwrap();
    survey_id
}

/// The pending-publication entry a writer keeps for `survey_id` (ADR-0152 clause 2,
/// `af/survey-pending@1` at `pending-surveys/<64 hex>.json`), naming the record's length and
/// `object` by device and inode when given. Returns its path.
fn pending_entry(
    machine: &Machine,
    survey_id: &str,
    length: usize,
    object: Option<&Path>,
) -> PathBuf {
    let directory = machine.store().join("pending-surveys");
    std::fs::create_dir_all(&directory).unwrap();
    let mut entry = serde_json::json!({
        "schema": "af/survey-pending@1",
        "survey_id": survey_id,
        "bytes": length.to_string(),
    });
    if let Some(object) = object {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(object).unwrap();
        entry["object"] = serde_json::json!({
            "device": metadata.dev().to_string(),
            "inode": metadata.ino().to_string(),
        });
    }
    schemas::valid(
        &schemas::validator("project-survey-pending-v1.json"),
        &entry,
    );
    let path = directory.join(format!("{}.json", survey_id.trim_start_matches("sha256:")));
    std::fs::write(&path, entry.to_string()).unwrap();
    path
}

/// A refresh at `cutoff` that the writer's own debug-build stop ends after the record's bytes
/// and before `SurveyRecorded@1`, as a crash would: the Survey it was publishing and the one
/// pending entry it left, valid as the writer's `af/survey-pending@1`.
fn stopped_refresh(machine: &Machine, repo: &Path, cutoff: &str) -> (String, PathBuf) {
    machine.expect(
        repo,
        &[
            ("AF_TEST_SURVEY_CUTOFF_UNIX_S", cutoff),
            ("AF_TEST_SURVEY_STOP_BEFORE_EVENT", "1"),
        ],
        &["project", "refresh", "--force"],
        86,
    );
    let mut entries: Vec<PathBuf> = std::fs::read_dir(machine.store().join("pending-surveys"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(entries.len(), 1, "{entries:?}");
    let path = entries.pop().unwrap();
    let entry: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    schemas::valid(
        &schemas::validator("project-survey-pending-v1.json"),
        &entry,
    );
    let survey_id = entry["survey_id"].as_str().unwrap().to_string();
    assert!(
        !machine.recorded().contains(&survey_id),
        "no event names it"
    );
    assert!(machine.held(&survey_id), "its bytes are in the CAS");
    (survey_id, path)
}

/// Writer and reader meet: a refresh stopped by the writer's own hook after its record's bytes
/// and before its event leaves its `af/survey-pending@1` entry; `af storage` counts the object
/// as a Survey `pending`, the preview plans exactly that object's recovery and then the entry's,
/// and the applied sweep, the same plan, removes both and nothing else.
#[test]
fn a_survey_the_writer_left_before_its_event_is_recovered_through_its_own_entry() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let kept = refresh(&machine, &repo, &at(0), true);
    let (survey_id, entry) = stopped_refresh(&machine, &repo, &at(60));
    let object = machine.object(&survey_id);

    let (shown, entries) = survey_entries(&machine, &repo);
    let pending = &entries[&path_of(&machine, &survey_id)];
    assert_eq!(pending["state"], "pending", "{shown}");
    assert_eq!(pending["bytes"].as_u64().unwrap(), allocated(&object));

    let previewed = preview(&machine, &repo, &[]);
    let planned = removals_of(&previewed, "survey");
    assert_eq!(planned.len(), 2, "{previewed}");
    assert_eq!(planned[0]["path"], path_of(&machine, &survey_id).as_str());
    assert_eq!(planned[0]["rule"], "recovery");
    assert_eq!(planned[0]["bytes"].as_u64().unwrap(), allocated(&object));
    assert_eq!(planned[1]["path"], entry.to_str().unwrap());
    assert_eq!(planned[1]["rule"], "recovery");
    assert_eq!(previewed["removals"].as_array().unwrap().len(), 2);
    assert!(left(&previewed, "foreign").is_empty(), "{previewed}");

    let applied = prune(&machine, &repo, &[]);
    assert_eq!(removals_of(&applied, "survey"), planned, "entry for entry");
    assert!(!object.exists() && !entry.exists());
    assert!(machine.held(&kept), "the recorded Survey is kept");
    assert!(machine.collected().is_empty(), "no tombstone");
    assert_eq!(machine.recorded(), [kept]);
    let again = preview(&machine, &repo, &[]);
    assert!(again["removals"].as_array().unwrap().is_empty(), "{again}");
}

/// The writer's own entry outliving its committed event (a crash between the commit and the
/// clear, restored here from the bytes the stopped refresh wrote) is cleared as a planned
/// `recovery` entry, and nothing is deleted: the Survey stays recorded and held.
#[test]
fn the_writers_entry_of_a_committed_survey_is_cleared_without_deletion() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let (survey_id, entry) = stopped_refresh(&machine, &repo, &at(0));
    let written = std::fs::read(&entry).unwrap();
    // The same bytes again: the writer finishes that publication and clears its entry.
    assert_eq!(refresh(&machine, &repo, &at(0), true), survey_id);
    assert!(!entry.exists());
    std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
    std::fs::write(&entry, &written).unwrap();

    let previewed = preview(&machine, &repo, &[]);
    let planned = removals_of(&previewed, "survey");
    assert_eq!(planned.len(), 1, "{previewed}");
    assert_eq!(planned[0]["path"], entry.to_str().unwrap());
    assert_eq!(planned[0]["rule"], "recovery");
    let applied = prune(&machine, &repo, &[]);
    assert_eq!(removals_of(&applied, "survey"), planned);
    assert!(!entry.exists(), "the entry is cleared");
    assert!(machine.held(&survey_id), "its Survey is kept");
    assert!(machine.collected().is_empty());
}

/// A recovery of a pending object stopped after its rename into its pass's container and
/// before its unlink keeps its recovery path: the entry is not cleared, the next preview plans
/// that object (never `foreign`), then the dead container, then the entry, and the applied
/// sweep, the same plan, finishes all three.
#[test]
fn a_pending_recovery_stopped_before_its_unlink_is_finished_by_the_next_sweep() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let kept = refresh(&machine, &repo, &at(0), true);
    let (survey_id, entry) = stopped_refresh(&machine, &repo, &at(60));
    let object = machine.object(&survey_id);
    let bytes = allocated(&object);
    machine.expect(
        &repo,
        &[("AF_TEST_SURVEY_STOP_BEFORE_UNLINK", "1")],
        &["storage", "prune", "--apply", "--json"],
        86,
    );
    assert!(!object.exists(), "moved off its CAS name");
    assert!(entry.is_file(), "the entry outlives no object it describes");
    let removing = machine.store().join("cas/removing");
    let containers: Vec<PathBuf> = std::fs::read_dir(&removing)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(containers.len(), 1, "{containers:?}");
    assert!(containers[0].join(&survey_id).is_file());

    let previewed = preview(&machine, &repo, &[]);
    assert!(left(&previewed, "foreign").is_empty(), "{previewed}");
    let planned = removals_of(&previewed, "survey");
    assert_eq!(planned.len(), 2, "{previewed}");
    assert_eq!(planned[0]["path"], path_of(&machine, &survey_id).as_str());
    assert_eq!(planned[0]["rule"], "recovery");
    assert_eq!(planned[0]["bytes"].as_u64().unwrap(), bytes);
    assert_eq!(planned[1]["path"], entry.to_str().unwrap());
    let container = removals_of(&previewed, "claim");
    assert_eq!(container.len(), 1, "{previewed}");
    assert_eq!(container[0]["path"], containers[0].to_str().unwrap());

    let applied = prune(&machine, &repo, &[]);
    assert_eq!(removals_of(&applied, "survey"), planned, "entry for entry");
    assert!(!containers[0].exists() && !entry.exists());
    assert!(!object.exists());
    assert!(machine.held(&kept));
    assert!(machine.collected().is_empty());
    assert!(
        !machine.store().join("settled.json").exists(),
        "nothing settled"
    );
}

/// A settlement whose directory sync fails after the rename of `settled.json` fails its
/// collection: the record is put back at its CAS name and nothing is deleted, the earlier
/// settled record stays, and the failure is reported; the tombstone stays, and the next sweep
/// deletes the record as `recovery` and settles it.
#[test]
fn a_failed_settlement_sync_deletes_nothing_and_the_next_sweep_retries() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let surveys: Vec<String> = (0..2)
        .map(|n| refresh(&machine, &repo, &at(n), true))
        .collect();
    let oldest = &surveys[0];
    let settled = machine.store().join("settled.json");
    let pruned = prune(
        &machine,
        &repo,
        &[
            ("AF_STORAGE__KEEP_SURVEYS", "1"),
            ("AF_TEST_SURVEY_FAIL_SETTLED_SYNC", "1"),
        ],
    );
    let removals = removals_of(&pruned, "survey");
    assert_eq!(removals.len(), 1, "{pruned}");
    let outcome = removals[0]["outcome"].as_str().unwrap();
    assert!(
        outcome.starts_with("failed: ")
            && outcome.contains("syncing `settled.json`")
            && outcome.contains("the earlier record is put back and synced")
            && outcome.contains("its record is put back and nothing is deleted"),
        "{outcome}"
    );
    assert_eq!(removals[0]["bytes"], 0, "nothing freed");
    assert!(machine.held(oldest), "the original record is not unlinked");
    assert!(!settled.exists(), "the Survey stays unsettled");
    assert_eq!(machine.collected(), [oldest.clone()], "its tombstone stays");

    let retried = prune(&machine, &repo, &[]);
    assert_eq!(
        removed(&retried, "recovery"),
        [path_of(&machine, oldest)],
        "{retried}"
    );
    assert!(!machine.held(oldest));
    let record: Value = serde_json::from_slice(&std::fs::read(&settled).unwrap()).unwrap();
    assert_eq!(record["survey_ids"], serde_json::json!([oldest]));
    assert_consistent(&machine);
}

/// A Survey published to the CAS whose `SurveyRecorded@1` never committed, its pending entry
/// naming the object: `af storage` counts it as a Survey `pending`, in use; preview and apply
/// plan, alike, its reclamation and then its entry's under `recovery`, and only the applied
/// sweep removes them. Nothing else is touched.
#[test]
fn an_unrecorded_object_its_pending_entry_describes_is_reclaimed_through_the_entry() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let kept = refresh(&machine, &repo, &at(0), true);
    let bytes = vec![b'x'; 8 * 1024];
    let survey_id = unrecorded_object(&machine, &bytes);
    let object = machine.object(&survey_id);
    let entry = pending_entry(&machine, &survey_id, bytes.len(), Some(&object));

    let (shown, entries) = survey_entries(&machine, &repo);
    let pending = &entries[&path_of(&machine, &survey_id)];
    assert_eq!(pending["state"], "pending", "{shown}");
    assert_eq!(pending["bytes"].as_u64().unwrap(), allocated(&object));
    assert!(
        pending["in_use"].as_str().unwrap().starts_with("pending: "),
        "{pending}"
    );

    let previewed = preview(&machine, &repo, &[]);
    let planned = removals_of(&previewed, "survey");
    assert_eq!(planned.len(), 2, "{previewed}");
    assert_eq!(planned[0]["path"], path_of(&machine, &survey_id).as_str());
    assert_eq!(planned[0]["rule"], "recovery");
    assert_eq!(planned[0]["bytes"].as_u64().unwrap(), allocated(&object));
    assert_eq!(planned[1]["path"], entry.to_str().unwrap());
    assert_eq!(planned[1]["rule"], "recovery");
    assert_eq!(planned[1]["bytes"].as_u64().unwrap(), allocated(&entry));
    assert!(
        object.is_file() && entry.is_file(),
        "a preview removes nothing"
    );

    let applied = prune(&machine, &repo, &[]);
    assert_eq!(removals_of(&applied, "survey"), planned, "entry for entry");
    assert!(
        !object.exists(),
        "the object the entry describes is reclaimed"
    );
    assert!(!entry.exists(), "and then its entry");
    assert!(machine.held(&kept), "the recorded Survey is kept");
    assert!(machine.collected().is_empty(), "no tombstone");
    let (_, entries) = survey_entries(&machine, &repo);
    assert_eq!(entries.len(), 1);
}

/// An object no event and no pending-publication entry names is `foreign`: listed with its
/// bytes by preview and apply alike, never planned, never deleted, not even by a budget that
/// wants every byte. So is one at the CAS name a pending entry names but whose identity the
/// entry does not record; that entry is cleared, the object left.
#[test]
fn an_unreferenced_object_without_an_entry_describing_it_is_foreign_and_kept() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    refresh(&machine, &repo, &at(0), true);
    let alone = unrecorded_object(&machine, b"no entry names these bytes");
    let undescribed = unrecorded_object(&machine, b"an entry names these, not their inode");
    let entry = pending_entry(&machine, &undescribed, 37, None);
    let squeeze = [("AF_STORAGE__MAX_BYTES", "1")];
    for document in [
        preview(&machine, &repo, &squeeze),
        prune(&machine, &repo, &squeeze),
    ] {
        let foreign: BTreeMap<String, u64> = left(&document, "foreign")
            .iter()
            .map(|left| {
                (
                    left["path"].as_str().unwrap().to_string(),
                    left["bytes"].as_u64().unwrap(),
                )
            })
            .collect();
        for survey_id in [&alone, &undescribed] {
            assert_eq!(
                foreign.get(&path_of(&machine, survey_id)),
                Some(&allocated(&machine.object(survey_id))),
                "{document}"
            );
            assert!(
                document["removals"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|removal| removal["path"] != path_of(&machine, survey_id).as_str()),
                "{document}"
            );
            assert!(machine.object(survey_id).is_file(), "never deleted");
        }
    }
    assert!(
        !entry.exists(),
        "the entry that describes no object is cleared"
    );
    // Without the entry, the next sweep finds the object foreign all the same.
    let again = prune(&machine, &repo, &squeeze);
    assert_eq!(left(&again, "foreign").len(), 2, "{again}");
    assert!(machine.object(&undescribed).is_file());
}

/// A dead claim nested deeper than the 256-level measurement walk cannot be measured: the
/// inventory records it with no bytes, preview and apply list it `too_deep` alike and never plan
/// it, and nothing inside it is deleted.
#[test]
fn an_unmeasurable_dead_claim_is_left_and_reported_by_preview_and_apply() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    refresh(&machine, &repo, &at(0), true);
    let claim = machine
        .root()
        .join("state/af/review/campaigns/.af-removing-999999999-8-0");
    let mut deep = claim.clone();
    for _ in 0..300 {
        deep = deep.join("d");
    }
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join("file"), b"x").unwrap();
    std::fs::write(claim.join("top"), vec![1_u8; 64 * 1024]).unwrap();

    let (shown, _) = survey_entries(&machine, &repo);
    let listed: Vec<&Value> = shown["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["kind"] == "claim")
        .collect();
    assert_eq!(listed.len(), 1, "{shown}");
    assert_eq!(listed[0]["bytes"], 0, "no bytes it could not measure");
    assert_eq!(listed[0]["state"], "abandoned");
    assert!(
        listed[0]["in_use"].as_str().unwrap().starts_with("left: "),
        "{}",
        listed[0]
    );
    for document in [preview(&machine, &repo, &[]), prune(&machine, &repo, &[])] {
        assert!(removals_of(&document, "claim").is_empty(), "{document}");
        let deep_left = left(&document, "too_deep");
        assert_eq!(deep_left.len(), 1, "{document}");
        assert_eq!(deep_left[0]["kind"], "claim");
        assert_eq!(deep_left[0]["path"], claim.to_str().unwrap());
        assert_eq!(deep_left[0]["bytes"], 0);
        assert!(deep.join("file").is_file(), "nothing inside it is deleted");
        assert!(claim.join("top").is_file(), "nothing inside it is deleted");
    }
}

/// `af storage` counts an abandoned claim no entry holds: one `claim` entry, `abandoned`, with
/// its measured bytes, in `kinds` and `total_bytes`, equal to what the prune preview counts and
/// plans, so the listing and the sweep agree on what the directory holds.
#[test]
fn af_storage_counts_an_abandoned_claim_as_the_preview_does() {
    let machine = Machine::new();
    let directory = machine.root().join("home");
    let claim = machine
        .root()
        .join("state/af/review/campaigns/.af-removing-999999999-9-0");
    let claim_bytes = dead_claim(&claim, 96 * 1024);
    let shown = machine.json(&directory, &[], &["storage", "--json"]);
    schemas::valid(&schemas::validator("storage-v3.json"), &shown);
    let entries = shown["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{shown}");
    assert_eq!(entries[0]["kind"], "claim");
    assert_eq!(entries[0]["state"], "abandoned");
    assert_eq!(entries[0]["path"], claim.to_str().unwrap());
    assert_eq!(entries[0]["bytes"].as_u64().unwrap(), claim_bytes);
    let kind = shown["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|kind| kind["kind"] == "claim")
        .unwrap();
    assert_eq!(kind["entries"], 1);
    assert_eq!(kind["bytes"].as_u64().unwrap(), claim_bytes);
    assert_eq!(shown["total_bytes"].as_u64().unwrap(), claim_bytes);
    let (text, _) = machine.expect(&directory, &[], &["storage"], 0);
    assert!(text.contains("abandoned removals"), "{text}");

    let previewed = preview(&machine, &directory, &[]);
    assert_eq!(previewed["total_before"], shown["total_bytes"]);
    let planned = removals_of(&previewed, "claim");
    assert_eq!(planned.len(), 1, "{previewed}");
    assert_eq!(planned[0]["bytes"], entries[0]["bytes"]);
    assert_eq!(planned[0]["total_after"], 0);
}

/// A settlement whose install sync fails and whose put-back's sync fails too (the debug
/// build's `AF_TEST_SURVEY_FAIL_SETTLED_SYNC=2`) reports both failures and leaves the Survey an
/// `unverified settlement`: nothing is deleted, its tombstone stays, and the next sweep, which
/// reads a consistent `settled.json`, deletes the record as `recovery` and settles it.
#[test]
fn a_settlement_whose_rollback_sync_fails_too_is_left_unverified_and_deletes_nothing() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let surveys: Vec<String> = (0..2)
        .map(|n| refresh(&machine, &repo, &at(n), true))
        .collect();
    let oldest = &surveys[0];
    let settled = machine.store().join("settled.json");
    let pruned = prune(
        &machine,
        &repo,
        &[
            ("AF_STORAGE__KEEP_SURVEYS", "1"),
            ("AF_TEST_SURVEY_FAIL_SETTLED_SYNC", "2"),
        ],
    );
    let removals = removals_of(&pruned, "survey");
    assert_eq!(removals.len(), 1, "{pruned}");
    let outcome = removals[0]["outcome"].as_str().unwrap();
    assert!(
        outcome.starts_with("failed: ")
            && outcome.contains("syncing `settled.json`: an injected failure")
            && outcome.contains("syncing the put-back: an injected failure")
            && outcome.contains("the Survey is left unverified settlement")
            && outcome.contains("its record is put back and nothing is deleted"),
        "{outcome}"
    );
    assert_eq!(removals[0]["bytes"], 0, "nothing freed");
    assert!(machine.held(oldest), "the record is not deleted");
    assert_eq!(machine.collected(), [oldest.clone()], "its tombstone stays");

    let retried = prune(&machine, &repo, &[]);
    assert_eq!(
        removed(&retried, "recovery"),
        [path_of(&machine, oldest)],
        "{retried}"
    );
    assert!(!machine.held(oldest));
    let record: Value = serde_json::from_slice(&std::fs::read(&settled).unwrap()).unwrap();
    assert_eq!(record["survey_ids"], serde_json::json!([oldest]));
    assert_consistent(&machine);
}

/// The device and inode of `path`, not following it.
fn identity_of(path: &Path) -> (String, String) {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path).unwrap();
    (metadata.dev().to_string(), metadata.ino().to_string())
}

/// A retry never discards a pending object's identity (ADR-0152 clause 2): a refresh that
/// stopped after its record's bytes and their identity leaves the entry naming the object; a
/// retry of the same bytes whose CAS write fails (the debug build's `AF_TEST_SURVEY_FAIL_PUT`)
/// keeps that entry byte for byte, so it still names the first object, and collection reclaims
/// that object and then clears the entry.
#[test]
fn a_retry_whose_put_fails_keeps_the_first_objects_identity_for_collection() {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let kept = refresh(&machine, &repo, &at(0), true);
    let (survey_id, entry) = stopped_refresh(&machine, &repo, &at(60));
    let object = machine.object(&survey_id);
    let first = std::fs::read(&entry).unwrap();
    let named: Value = serde_json::from_slice(&first).unwrap();
    let (device, inode) = identity_of(&object);
    assert_eq!(
        (
            named["object"]["device"].as_str(),
            named["object"]["inode"].as_str()
        ),
        (Some(device.as_str()), Some(inode.as_str()))
    );

    let (_, stderr) = machine.expect(
        &repo,
        &[
            ("AF_TEST_SURVEY_CUTOFF_UNIX_S", &at(60)),
            ("AF_TEST_SURVEY_FAIL_PUT", "1"),
        ],
        &["project", "refresh", "--force"],
        1,
    );
    assert!(stderr.contains("AF_TEST_SURVEY_FAIL_PUT"), "{stderr}");
    assert_eq!(
        std::fs::read(&entry).unwrap(),
        first,
        "the entry still names the first object"
    );
    assert!(!machine.recorded().contains(&survey_id));

    let previewed = preview(&machine, &repo, &[]);
    let planned = removals_of(&previewed, "survey");
    assert_eq!(planned.len(), 2, "{previewed}");
    assert_eq!(planned[0]["path"], path_of(&machine, &survey_id).as_str());
    assert_eq!(planned[0]["rule"], "recovery");
    assert_eq!(planned[1]["path"], entry.to_str().unwrap());
    let applied = prune(&machine, &repo, &[]);
    assert_eq!(removals_of(&applied, "survey"), planned, "entry for entry");
    assert!(!object.exists() && !entry.exists());
    assert!(machine.held(&kept));
    assert!(machine.collected().is_empty());
}

/// A pending entry is cleared only once no file with its inode remains (ADR-0152 clause 13):
/// the object truncated in place, grown in place and then moved into a pass's container at its
/// new length is still the entry's inode, so the applied sweep keeps the entry, its outcome
/// `kept: object changed, still present`, and lists the object `foreign`; once the operator
/// deletes it, the next apply clears the entry.
#[test]
fn a_pending_entry_stays_while_its_changed_object_is_present_and_goes_once_it_is_deleted() {
    use std::os::unix::fs::PermissionsExt;
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let kept = refresh(&machine, &repo, &at(0), true);
    let (survey_id, entry) = stopped_refresh(&machine, &repo, &at(60));
    let object = machine.object(&survey_id);
    std::fs::set_permissions(&object, std::fs::Permissions::from_mode(0o644)).unwrap();
    let length = std::fs::metadata(&object).unwrap().len();
    let container = machine
        .store()
        .join("cas/removing")
        .join("0123456789abcdef0123456789abcdef");
    let moved = container.join(&survey_id);
    let resize = |at: &Path, to: u64| {
        std::fs::OpenOptions::new()
            .write(true)
            .open(at)
            .unwrap()
            .set_len(to)
            .unwrap();
    };
    for (case, at) in [
        ("truncated", object.clone()),
        ("grown", object.clone()),
        ("moved", moved.clone()),
    ] {
        match case {
            "truncated" => resize(&object, length / 2),
            "grown" => resize(&object, length + 7),
            _ => {
                std::fs::create_dir_all(&container).unwrap();
                std::fs::rename(&object, &moved).unwrap();
            }
        }
        let applied = prune(&machine, &repo, &[]);
        let foreign: Vec<String> = left(&applied, "foreign")
            .iter()
            .map(|left| left["path"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(foreign, [at.to_str().unwrap()], "{case}: {applied}");
        let clear: Vec<Value> = removals_of(&applied, "survey")
            .into_iter()
            .filter(|removal| removal["path"] == entry.to_str().unwrap())
            .collect();
        assert_eq!(clear.len(), 1, "{case}: {applied}");
        let outcome = clear[0]["outcome"].as_str().unwrap();
        assert!(
            outcome.starts_with("failed: ")
                && outcome.contains("kept: object changed, still present"),
            "{case}: {outcome}"
        );
        assert!(
            entry.is_file() && at.is_file(),
            "{case}: nothing is removed"
        );
    }

    // The operator deletes it: no file with its inode is left, so the next apply clears the
    // entry, and the dead pass's container, empty now, goes with it.
    std::fs::remove_file(&moved).unwrap();
    let applied = prune(&machine, &repo, &[]);
    assert!(
        removals_of(&applied, "survey")
            .iter()
            .any(|removal| removal["path"] == entry.to_str().unwrap()
                && removal.get("outcome").is_none()),
        "{applied}"
    );
    assert!(!entry.exists() && !container.exists());
    assert!(machine.held(&kept));
    assert!(machine.collected().is_empty());
}

/// The `reason` of every problem `af storage --json` reports under `env`, by path.
fn storage_problems(machine: &Machine, repo: &Path, env: &[(&str, &str)]) -> Vec<(String, String)> {
    let shown = machine.json(repo, env, &["storage", "--json"]);
    schemas::valid(&schemas::validator("storage-v3.json"), &shown);
    shown["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|problem| {
            (
                problem["path"].as_str().unwrap().to_string(),
                problem["reason"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// A Project Store whose log one of the row ceilings stops (a debug build lowers it, as it may
/// only be lowered): `af storage` reports the Store with that ceiling's name and lists none of
/// its Surveys, which are kept, and the applied sweep plans none of them, even under a budget
/// that wants every byte.
fn a_ceiling_keeps_the_stores_surveys(variable: &str, value: &str, ceiling: &str) {
    let machine = Machine::new();
    let repo = repository(&machine, "");
    let surveys: Vec<String> = (0..2)
        .map(|n| refresh(&machine, &repo, &at(n), true))
        .collect();
    let env = [(variable, value)];
    let problems = storage_problems(&machine, &repo, &env);
    let store = machine.store();
    assert!(
        problems
            .iter()
            .any(|(path, reason)| path == store.to_str().unwrap()
                && reason.contains("its log is beyond a Store discovery ceiling")
                && reason.contains(ceiling)),
        "{problems:?}"
    );
    let shown = machine.json(&repo, &env, &["storage", "--json"]);
    let entries: Vec<&Value> = shown["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["kind"] == "survey")
        .collect();
    assert!(entries.is_empty(), "{entries:?}");
    let squeeze = [(variable, value), ("AF_STORAGE__MAX_BYTES", "1")];
    let applied = machine.json(&repo, &squeeze, &["storage", "prune", "--apply", "--json"]);
    assert!(removals_of(&applied, "survey").is_empty(), "{applied}");
    for survey_id in &surveys {
        assert!(machine.held(survey_id), "{survey_id} is kept");
    }
    // Within the ceilings again, the Store is read as before.
    assert!(
        storage_problems(&machine, &repo, &[])
            .iter()
            .all(|(path, _)| path != store.to_str().unwrap())
    );
    assert_eq!(survey_entries(&machine, &repo).1.len(), 2);
}

/// The Store discovery ceiling of rows per Store (100,000, design note §3.3), lowered to two.
#[test]
fn a_project_store_log_beyond_the_row_ceiling_keeps_its_surveys() {
    a_ceiling_keeps_the_stores_surveys("AF_TEST_SURVEY_DISCOVERY_ROWS", "2", "max_rows");
}

/// The Store discovery ceiling of bytes per row (1 MiB), lowered below one event's.
#[test]
fn a_project_store_row_beyond_the_row_bytes_ceiling_keeps_its_surveys() {
    a_ceiling_keeps_the_stores_surveys("AF_TEST_SURVEY_DISCOVERY_ROW_BYTES", "64", "max_row_bytes");
}

/// The Store discovery ceiling of buffered bytes over a Store's rows (16 MiB), lowered below
/// what its events hold together while each row fits.
#[test]
fn a_project_store_log_beyond_the_buffered_bytes_ceiling_keeps_its_surveys() {
    a_ceiling_keeps_the_stores_surveys(
        "AF_TEST_SURVEY_DISCOVERY_BUFFERED_BYTES",
        "1200",
        "max_buffered_bytes",
    );
}

/// The Store discovery ceiling of Project Stores whose Surveys the inventory reads (1,000),
/// lowered to one: of two Project Stores, in path order, the first's Surveys are listed and the
/// second is reported with the ceiling, still measured as a Project Store, its Surveys kept.
#[test]
fn project_stores_beyond_the_store_discovery_ceiling_are_reported_and_their_surveys_kept() {
    let machine = Machine::new();
    let first = repository(&machine, "");
    refresh(&machine, &first, &at(0), true);
    let second = machine.root().join("collect-second");
    std::fs::create_dir_all(&second).unwrap();
    git(&second, &["init", "-q", "-b", "main"]);
    write(&second, "README.md", "another history\n");
    git(&second, &["add", "-A"]);
    git(&second, &["commit", "-q", "-m", "Another root"]);
    machine.expect(
        &second,
        &[("AF_TEST_SURVEY_CUTOFF_UNIX_S", &at(0))],
        &["project", "refresh", "--force"],
        0,
    );
    let mut stores: Vec<PathBuf> = std::fs::read_dir(machine.root().join("state/af/project"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    stores.sort();
    assert_eq!(stores.len(), 2);

    let env = [("AF_TEST_SURVEY_DISCOVERY_STORES", "1")];
    let shown = machine.json(&first, &env, &["storage", "--json"]);
    schemas::valid(&schemas::validator("storage-v3.json"), &shown);
    let problems: Vec<&Value> = shown["problems"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|problem| problem["reason"].as_str().unwrap().contains("max_stores"))
        .collect();
    assert_eq!(problems.len(), 1, "{shown}");
    assert_eq!(problems[0]["path"], stores[1].to_str().unwrap());
    assert!(
        problems[0]["reason"]
            .as_str()
            .unwrap()
            .contains("the inventory reads the Surveys of at most 1 Project Stores"),
        "{shown}"
    );
    let entries = shown["entries"].as_array().unwrap();
    let in_store = |store: &Path, kind: &str| {
        entries
            .iter()
            .filter(|entry| {
                entry["kind"] == kind
                    && Path::new(entry["path"].as_str().unwrap()).starts_with(store)
            })
            .count()
    };
    assert_eq!(in_store(&stores[0], "survey"), 1, "{shown}");
    assert_eq!(in_store(&stores[1], "survey"), 0, "{shown}");
    assert_eq!(
        in_store(&stores[1], "project_store"),
        1,
        "still measured: {shown}"
    );
    let (_, all) = survey_entries(&machine, &first);
    assert_eq!(all.len(), 2, "both listed within the ceiling");
}
