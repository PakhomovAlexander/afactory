//! The trusted CI Pipeline exception (ADR-0141) through `af task start --authority`: a Task
//! whose implementer rewrites the workflow that judges it reaches the repository's CI only when
//! the selected root Pipeline, pinned in the committed catalog, carries the exact tag `ci`. Real
//! `git` pushes to a local bare repository named as the mapping's push URL and a fake `gh`
//! serves the recorded GitHub documents under `fixtures/remote-checks/github/`, so nothing here
//! touches a network or a credential.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use review_config::lock::package_digest;
use serde_json::Value;

use crate::schemas;
use crate::task_cli;

const TASK: &str = "remote-ci";
const LINT: &str = "validation / lint";
const CHECK: &str = "validation / check (ubuntu-latest)";
const SOURCE_WORKFLOW: &str = "on: pull_request\njobs: {}\n";
const CHANGED_WORKFLOW: &str = "on: pull_request\njobs:\n  changed: {}\n";

/// A fake `gh` serving recorded GitHub API documents from its state directory; it simulates the
/// gate pull request and its `refs/pull/12/merge` test merge in the local bare repository.
const FAKE_GH: &str = r#"#!/bin/sh
STATE='@STATE@'
BARE='@BARE@'
TASK='@TASK@'
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null
export GIT_AUTHOR_NAME=github GIT_AUTHOR_EMAIL=github@example.invalid
export GIT_COMMITTER_NAME=github GIT_COMMITTER_EMAIL=github@example.invalid
printf '%s\n' "$*" >> "$STATE/calls.log"
head_sha() { git --git-dir="$BARE" rev-parse --verify -q "refs/heads/af-gate/$TASK/head"; }
base_sha() { git --git-dir="$BARE" rev-parse --verify -q "refs/heads/af-gate/$TASK/base"; }
merge_ref() {
  H=$(head_sha); B=$(base_sha)
  [ -n "$H" ] && [ -n "$B" ] || return 0
  T=$(git --git-dir="$BARE" rev-parse "$H^{tree}")
  M=$(echo merge | git --git-dir="$BARE" commit-tree "$T" -p "$B" -p "$H")
  git --git-dir="$BARE" update-ref refs/pull/12/merge "$M"
}
pull() {
  merge_ref
  printf '{"number":12,"html_url":"https://github.com/octo/gate/pull/12","state":"open","draft":true,'
  printf '"head":{"ref":"af-gate/%s/head","sha":"%s","repo":{"full_name":"octo/gate"}},"base":{"ref":"af-gate/%s/base","sha":"%s","repo":{"full_name":"octo/gate"}}}' "$TASK" "$(head_sha)" "$TASK" "$(base_sha)"
}
serve() { sed -e "s/@HEAD@/$(head_sha)/g" -e "s/@TASK@/$TASK/g" "$1"; }
case "$1" in
  auth) echo "github.com: Logged in to github.com account octo (keyring)"; exit 0;;
  api) shift;;
  *) echo "fake gh: unsupported command $1" >&2; exit 2;;
esac
if [ "$1" = "--method" ] && [ "$2" = "POST" ]; then
  touch "$STATE/pull-open"
  pull; exit 0
fi
case "$1" in
  repos/octo/gate/pulls\?*)
    if [ -e "$STATE/pull-open" ]; then printf '['; pull; printf ']'; else printf '[]'; fi;;
  repos/octo/gate/pulls/12) pull;;
  repos/octo/gate/actions/runs\?*) serve "$STATE/runs.json";;
  repos/octo/gate/actions/runs/77/attempts/1/jobs\?*) serve "$STATE/jobs.json";;
  repos/octo/gate/actions/jobs/*/logs) echo "gh: Not Found (HTTP 404)" >&2; exit 1;;
  *) echo "fake gh: unsupported api $1" >&2; exit 2;;
esac
"#;

struct Fixture {
    _root: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    state: PathBuf,
    home: PathBuf,
    bin: PathBuf,
    bare: PathBuf,
    gh_state: PathBuf,
}

fn workspace() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn git(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// The pagination fixture with a workflow, an implementer that also rewrites it, a `remote`
/// table on its one check, the implementation Pipeline's `tags` line (none when `None`), the
/// operator's mapping and a passing fake GitHub.
fn fixture(tags: Option<&str>) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let (repo, state) = task_cli::fixture_named(&root, "pagination");
    std::fs::create_dir_all(repo.join(".github/workflows")).unwrap();
    std::fs::write(repo.join(".github/workflows/ci.yml"), SOURCE_WORKFLOW).unwrap();
    let packages = repo.join(".af/task-packages");
    std::fs::write(
        packages.join("fixture/implementer/worker.py"),
        format!(
            "import json,os,sys\njson.load(sys.stdin)\n\
             open('pagination.py','w').write('def paginate(items, offset=0, limit=2):\\n    return items[offset:offset+limit]\\n')\n\
             os.makedirs('.github/workflows', exist_ok=True)\n\
             open('.github/workflows/ci.yml','w').write({CHANGED_WORKFLOW:?})\n\
             print(json.dumps({{'schema':'af.worker-reply/1','outputs':{{'report':[{{'summary':'Implemented pagination and changed CI'}}]}}}}))\n"
        ),
    )
    .unwrap();
    if let Some(tags) = tags {
        let path = packages.join("fixture/implementation/pipeline.toml");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("tags = {tags}\n{text}")).unwrap();
    }
    let catalog_path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
    for name in ["fixture/implementer", "fixture/implementation"] {
        catalog["packages"][name]["digest"] =
            toml::Value::String(package_digest(name, &packages.join(name)).unwrap());
    }
    std::fs::write(&catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    let policy_path = repo.join(".af/code-policy.toml");
    let policy = std::fs::read_to_string(&policy_path)
        .unwrap()
        .replace("check_wall_ms = 5000", "check_wall_ms = 120000");
    std::fs::write(
        &policy_path,
        format!(
            "{policy}\n[checks.pagination.remote]\nexecutor = \"github-pr\"\n\
             workflow = \".github/workflows/ci.yml\"\nrequired = [\"{LINT}\", \"{CHECK}\"]\n"
        ),
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "remote ci fixture"]);

    let home = root.join("home");
    let bin = root.join("bin");
    let gh_state = root.join("gh-state");
    let bare = root.join("remote.git");
    for path in [
        &home.join(".config/af"),
        &bin,
        &gh_state,
        &root.join("tasks"),
    ] {
        std::fs::create_dir_all(path).unwrap();
    }
    git(&root, &["init", "--bare", "-q", bare.to_str().unwrap()]);
    let gh = bin.join("gh");
    std::fs::write(
        &gh,
        FAKE_GH
            .replace("@STATE@", gh_state.to_str().unwrap())
            .replace("@BARE@", bare.to_str().unwrap())
            .replace("@TASK@", TASK),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let github = workspace().join("fixtures/remote-checks/github");
    for (document, name) in [
        ("runs-pull-request.json", "runs.json"),
        ("jobs-success.json", "jobs.json"),
    ] {
        std::fs::write(
            gh_state.join(name),
            std::fs::read(github.join(document)).unwrap(),
        )
        .unwrap();
    }
    let repository_id = git(&repo, &["rev-list", "--max-parents=0", "HEAD"]);
    std::fs::write(
        home.join(".config/af/remote-checks.toml"),
        format!(
            "version = 1\n\n[[github_pr]]\nrepository_id = \"{repository_id}\"\n\
             github = \"octo/gate\"\npush_url = \"{}\"\nchecks = [\"pagination\"]\n",
            bare.display()
        ),
    )
    .unwrap();
    Fixture {
        _root: directory,
        root,
        repo,
        state,
        home,
        bin,
        bare,
        gh_state,
    }
}

fn af(fixture: &Fixture, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(&fixture.repo)
        .env("HOME", &fixture.home)
        .env("XDG_CONFIG_HOME", fixture.home.join(".config"))
        .env("XDG_CACHE_HOME", fixture.root.join("cache"))
        .env("PATH", format!("{}:/usr/bin:/bin", fixture.bin.display()))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("AF_TASK_REMOTE_CHECK_POLICY_FILE")
        .env_remove("AF_CACHE_POLICY_FILE")
        .args(args)
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Start and execute the Task from the committed authority, then read it back through
/// `af task show --json`.
fn start(fixture: &Fixture) -> (i32, Value) {
    let file = fixture.root.join("tasks").join(format!("{TASK}.json"));
    std::fs::write(
        &file,
        serde_json::to_vec(&serde_json::json!({
            "schema": "af.task-file/1",
            "task_id": TASK,
            "kind": "implement",
            "goal": "Implement offset/limit pagination and the CI change it needs",
            "pipeline": {"name": "fixture/implementation", "fallback": "refuse"},
            "strategy": "small",
            "facts": {},
            "limits": {
                "tokens": 1000,
                "max_attempts": 3,
                "wall_ms": 600_000,
                "verification": {"tokens": 200, "attempts": 2, "wall_ms": 300_000}
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let (code, _, stderr) = af(
        fixture,
        &[
            "task",
            "start",
            "--execute",
            "--authority",
            "HEAD",
            "--file",
            file.to_str().unwrap(),
            "--state",
            fixture.state.to_str().unwrap(),
            "--json",
        ],
    );
    let (shown, stdout, show_error) = af(
        fixture,
        &[
            "task",
            "show",
            TASK,
            "--state",
            fixture.state.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(shown, 0, "{show_error}\n{stderr}");
    let inspection: Value = serde_json::from_str(stdout.trim()).unwrap();
    schemas::valid(&schemas::validator("task-inspection-v11.json"), &inspection);
    (code, inspection)
}

/// The evidence of every remote check the Task's current receipts hold: at least one, and every
/// one for `pagination`.
fn remote_records(inspection: &Value) -> Vec<Value> {
    let checks = inspection["remote_checks"]
        .as_array()
        .expect("remote checks");
    assert!(!checks.is_empty(), "{inspection:#}");
    checks
        .iter()
        .map(|check| {
            assert_eq!(check["check"], "pagination");
            check["record"].clone()
        })
        .collect()
}

#[test]
fn a_pinned_ci_tagged_root_lets_a_changed_workflow_hold_the_gate() {
    let fixture = fixture(Some("[\"ci\"]"));
    let (code, inspection) = start(&fixture);
    assert_eq!(code, 0, "{inspection:#}");
    let records = remote_records(&inspection);
    let [record] = records.as_slice() else {
        panic!("one remote check: {records:#?}");
    };
    assert_eq!(record["state"], "observed", "{record:#}");
    assert!(record.get("reason").is_none(), "{record:#}");
    let trusted = &record["trusted_ci"];
    assert_eq!(trusted["tag"], "ci");
    assert_eq!(trusted["pipeline"], "fixture/implementation");
    assert_eq!(trusted["plan_id"], inspection["plan_id"]);
    // The grant names the Task's whole captured run authority, not only its code policy.
    let cas = review_store::Cas::open_existing(fixture.state.join("cas")).unwrap();
    let plan = cas
        .get_json(inspection["plan_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        trusted["authority_id"],
        plan["payload"]["authority"]["policy_id"]
    );
    let authority = cas
        .get_json(trusted["authority_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(authority["schema"], "af.task-run-authority/2");
    assert_eq!(
        trusted["pipeline_id"],
        authority["packages"]["fixture/implementation"]["artifact_id"]
    );
    // GitHub ran the workflow the implementer wrote, on the exact candidate tree.
    let head = record["head_commit"].as_str().unwrap();
    assert_eq!(
        git(
            &fixture.bare,
            &["show", &format!("{head}:.github/workflows/ci.yml")]
        ),
        CHANGED_WORKFLOW.trim()
    );
    let base = record["base_commit"].as_str().unwrap();
    assert_eq!(
        git(
            &fixture.bare,
            &["show", &format!("{base}:.github/workflows/ci.yml")]
        ),
        SOURCE_WORKFLOW.trim()
    );
    // `af task show` names the exception under the check.
    let (shown, text, _) = af(
        &fixture,
        &[
            "task",
            "show",
            TASK,
            "--state",
            fixture.state.to_str().unwrap(),
        ],
    );
    assert_eq!(shown, 0);
    assert!(
        text.contains("changed .github/ sent under trusted CI Pipeline fixture/implementation"),
        "{text}"
    );
}

#[test]
fn an_untagged_or_lookalike_root_never_sends_a_changed_workflow() {
    for tags in [None, Some("[\"CI\"]"), Some("[\"cicd\"]")] {
        let fixture = fixture(tags);
        let (_, inspection) = start(&fixture);
        for record in remote_records(&inspection) {
            assert_eq!(record["state"], "refused", "{tags:?}: {record:#}");
            assert_eq!(record["reason"], "remote_candidate_changes_ci", "{tags:?}");
            assert!(record.get("trusted_ci").is_none(), "{tags:?}");
        }
        assert_ne!(inspection["result"]["acceptance"], "satisfied", "{tags:?}");
        assert_eq!(git(&fixture.bare, &["for-each-ref"]), "", "nothing pushed");
        assert!(
            !fixture.gh_state.join("calls.log").exists(),
            "{tags:?}: gh was never called"
        );
    }
}
