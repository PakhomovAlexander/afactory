//! Remote Checks (ADR-0140), credential-free and offline: a real `git` pushes to a local bare
//! repository named as the mapping's `push_url`, and a fake `gh` on the executor's PATH serves
//! the recorded GitHub API documents under `fixtures/remote-checks/github/`, simulating the
//! pull request, its merge ref and its jobs' logs in that bare repository.

mod executor;
mod task;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use review_core::Producer;
use review_core::task::remote_check::{
    RemoteCheckReasonV1, RemoteCheckStateV1, RemoteCheckV1, RemoteExecutorV1,
};
use review_pipeline::task::remote_check::github_pr::{self, RemotePhase};
use review_pipeline::task::remote_check::{
    GithubPrSettings, GithubPrTarget, RemoteCheckMapping, RemoteCheckOutcome, RemoteCheckRequest,
};
use review_source_git::task::{capture_snapshot, read_snapshot};
use review_source_git::{Entry, EntryKind, Manifest};
use review_store::Cas;
use serde_json::json;

pub const ROOT: &str = "5f1c0000000000000000000000000000000000aa";
pub const TASK: &str = "rc1-remote";
pub const OWNER: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000001";
pub const OTHER_STORE: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000002";
pub const LINT: &str = "validation / lint";
pub const CHECK: &str = "validation / check (ubuntu-latest)";

const FAKE_GH: &str = r#"#!/bin/sh
# A fake `gh` serving recorded GitHub API documents; it never contacts a network. It simulates
# GitHub's pull request and its `refs/pull/12/merge` test merge in the local bare repository.
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
  MODE=$(cat "$STATE/merge-mode" 2>/dev/null || echo good)
  case "$MODE" in
    good) T=$(git --git-dir="$BARE" rev-parse "$H^{tree}")
          M=$(echo merge | git --git-dir="$BARE" commit-tree "$T" -p "$B" -p "$H");;
    other-tree) T=$(git --git-dir="$BARE" rev-parse "$B^{tree}")
          M=$(echo merge | git --git-dir="$BARE" commit-tree "$T" -p "$B" -p "$H");;
    other-parents) T=$(git --git-dir="$BARE" rev-parse "$H^{tree}")
          M=$(echo merge | git --git-dir="$BARE" commit-tree "$T" -p "$H");;
    *) return 0;;
  esac
  git --git-dir="$BARE" update-ref refs/pull/12/merge "$M"
}
pull() {
  merge_ref
  PULL_STATE=$(cat "$STATE/pull-state" 2>/dev/null || echo open)
  printf '{"number":12,"html_url":"https://github.com/octo/gate/pull/12","state":"%s","draft":true,' "$PULL_STATE"
  printf '"head":{"ref":"af-gate/%s/head","sha":"%s","repo":{"full_name":"octo/gate"}},"base":{"ref":"af-gate/%s/base","sha":"%s","repo":{"full_name":"octo/gate"}}}' "$TASK" "$(head_sha)" "$TASK" "$(base_sha)"
}
serve() { sed -e "s/@HEAD@/$(head_sha)/g" -e "s/@TASK@/$TASK/g" "$1"; }
case "$1" in
  auth)
    if [ -e "$STATE/unauthenticated" ]; then
      echo "You are not logged into any GitHub hosts. To log in, run: gh auth login" >&2; exit 1
    fi
    echo "github.com: Logged in to github.com account octo (keyring)"; exit 0;;
  api) shift;;
  *) echo "fake gh: unsupported command $1" >&2; exit 2;;
esac
if [ "$1" = "--method" ] && [ "$2" = "POST" ]; then
  if [ -e "$STATE/refuse-pull" ]; then
    echo "gh: Validation Failed (HTTP 422): pushes to $BARE are not allowed" >&2; exit 1
  fi
  echo created >> "$STATE/pulls-created"
  touch "$STATE/pull-open"
  pull; exit 0
fi
case "$1" in
  repos/octo/gate/actions/jobs/*/logs)
    P=${1#repos/octo/gate/actions/jobs/}; JOB=${P%%/*}
    if [ -e "$STATE/log-$JOB.txt" ]; then cat "$STATE/log-$JOB.txt"; exit 0; fi
    echo "gh: Not Found (HTTP 404)" >&2; exit 1;;
  repos/octo/gate/pulls\?*)
    if [ -e "$STATE/pull-open" ]; then printf '['; pull; printf ']'; else printf '[]'; fi;;
  repos/octo/gate/pulls/12) pull;;
  repos/octo/gate/actions/runs\?*)
    if [ -e "$STATE/runs-hang" ]; then echo $$ > "$STATE/hang.pid"; exec sleep 60; fi
    if [ -e "$STATE/runs.json" ]; then serve "$STATE/runs.json"
    else printf '{"total_count":0,"workflow_runs":[]}'; fi;;
  repos/octo/gate/actions/runs/*/attempts/*/jobs\?*)
    P=${1#repos/octo/gate/actions/runs/}; RUN=${P%%/*}; P=${P#*/attempts/}; ATTEMPT=${P%%/*}
    if [ -e "$STATE/jobs-$RUN-$ATTEMPT.json" ]; then serve "$STATE/jobs-$RUN-$ATTEMPT.json"
    else printf '{"total_count":0,"jobs":[]}'; fi;;
  *) echo "fake gh: unsupported api $1" >&2; exit 2;;
esac
"#;

fn workspace_root() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

pub fn git(directory: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "someone")
        .env("GIT_AUTHOR_EMAIL", "someone@example.invalid")
        .env("GIT_COMMITTER_NAME", "someone")
        .env("GIT_COMMITTER_EMAIL", "someone@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// One isolated remote: a bare repository as the push URL, a fake `gh` and its state.
pub struct Remote {
    pub directory: tempfile::TempDir,
    pub bare: PathBuf,
    pub state: PathBuf,
    pub bin: PathBuf,
}

impl Remote {
    pub fn new() -> Remote {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let bare = root.join("remote.git");
        let state = root.join("gh-state");
        let bin = root.join("bin");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        git(
            &root,
            &["init", "--bare", "--quiet", bare.to_str().unwrap()],
        );
        let script = FAKE_GH
            .replace("@STATE@", state.to_str().unwrap())
            .replace("@BARE@", bare.to_str().unwrap())
            .replace("@TASK@", TASK);
        let gh = bin.join("gh");
        std::fs::write(&gh, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        Remote {
            directory,
            bare,
            state,
            bin,
        }
    }

    pub fn push_url(&self) -> String {
        self.bare.display().to_string()
    }

    /// Serve a recorded document as the runs list, or as one run attempt's jobs.
    pub fn serve_runs(&self, document: &str) {
        self.serve(document, "runs.json");
    }

    pub fn serve_jobs(&self, run: u64, attempt: u64, document: &str) {
        self.serve(document, &format!("jobs-{run}-{attempt}.json"));
    }

    fn serve(&self, document: &str, as_name: &str) {
        // Written, not copied: a copy would keep the read-only mode a Task check's source tree
        // gives the fixture, and a test could then never serve another document in its place.
        let bytes = std::fs::read(
            workspace_root()
                .join("fixtures/remote-checks/github")
                .join(document),
        )
        .unwrap();
        self.replace(as_name, &bytes);
    }

    /// Put `bytes` at `name` in one step, so the fake never reads a half-written document.
    pub fn replace(&self, name: &str, bytes: &[u8]) {
        Self::replace_in(&self.state, name, bytes);
    }

    pub fn replace_in(state: &Path, name: &str, bytes: &[u8]) {
        let staged = state.join(format!("{name}.staged"));
        std::fs::write(&staged, bytes).unwrap();
        std::fs::rename(&staged, state.join(name)).unwrap();
    }

    /// Serve `text` as one job's log; a job without one answers 404.
    pub fn serve_log(&self, job: u64, text: &[u8]) {
        std::fs::write(self.state.join(format!("log-{job}.txt")), text).unwrap();
    }

    /// What the fake reports as the gate pull request's state: `open` or `closed`.
    pub fn pull_state(&self, state: &str) {
        std::fs::write(self.state.join("pull-state"), state).unwrap();
    }

    pub fn flag(&self, name: &str) {
        std::fs::write(self.state.join(name), b"").unwrap();
    }

    pub fn merge_mode(&self, mode: &str) {
        std::fs::write(self.state.join("merge-mode"), mode).unwrap();
    }

    pub fn calls(&self) -> String {
        std::fs::read_to_string(self.state.join("calls.log")).unwrap_or_default()
    }

    pub fn pulls_created(&self) -> usize {
        std::fs::read_to_string(self.state.join("pulls-created"))
            .unwrap_or_default()
            .lines()
            .count()
    }

    /// Every branch of the bare repository with its commit. GitHub's own merge ref, which
    /// the fake recomputes on every read as GitHub does, is not a branch.
    pub fn refs(&self) -> String {
        git(
            &self.bare,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/heads/",
            ],
        )
    }

    pub fn branch(&self, role: &str) -> Option<String> {
        let output = std::process::Command::new("git")
            .args([
                "--git-dir",
                self.bare.to_str().unwrap(),
                "rev-parse",
                "--verify",
                "-q",
                &format!("refs/heads/af-gate/{TASK}/{role}"),
            ])
            .output()
            .unwrap();
        output
            .status
            .success()
            .then(|| String::from_utf8(output.stdout).unwrap().trim().to_owned())
    }

    pub fn settings(&self) -> GithubPrSettings {
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(self.bin.clone()).chain(std::env::split_paths(&inherited)),
        )
        .unwrap();
        GithubPrSettings {
            path: Some(path),
            poll_interval: Duration::from_millis(100),
            missing_after: Duration::from_millis(400),
        }
    }

    pub fn target(&self) -> GithubPrTarget {
        RemoteCheckMapping::parse(&self.mapping_text(&["kernel"]))
            .unwrap()
            .select_any(ROOT)
    }

    pub fn mapping_text(&self, checks: &[&str]) -> String {
        format!(
            "version = 1\n\n[[github_pr]]\nrepository_id = \"{ROOT}\"\ngithub = \"octo/gate\"\n\
             push_url = \"{}\"\nchecks = [{}]\n",
            self.push_url(),
            checks
                .iter()
                .map(|check| format!("\"{check}\""))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// Test-only access to a parsed mapping's entry without a policy.
trait SelectAny {
    fn select_any(&self, repository_id: &str) -> GithubPrTarget;
}

impl SelectAny for RemoteCheckMapping {
    fn select_any(&self, repository_id: &str) -> GithubPrTarget {
        let mut policy = task::policy(false, true);
        policy.checks.retain(|name, _| name == "kernel");
        self.select(repository_id, &policy)
            .unwrap()
            .unwrap()
            .clone()
    }
}

/// A source Snapshot (with a workflow, an executable and a symbolic link) and candidates
/// derived from it.
pub struct Snapshots {
    pub source_id: String,
    pub source: Manifest,
    origin: String,
}

pub fn producer() -> Producer {
    Producer::KernelOperation {
        run_id: "remote-checks-test".into(),
        node_id: None,
        operation_id: "capture@1".into(),
    }
}

fn entry(cas: &Cas, path: &str, kind: EntryKind, bytes: &[u8]) -> Entry {
    Entry {
        path: path.into(),
        kind,
        content: cas.put(bytes).unwrap(),
        size: bytes.len() as u64,
    }
}

impl Snapshots {
    pub fn new(cas: &Cas) -> Snapshots {
        let source = Manifest::new(vec![
            entry(
                cas,
                ".github/workflows/ci.yml",
                EntryKind::File,
                b"on: pull_request\njobs: {}\n",
            ),
            entry(cas, "src/lib.txt", EntryKind::File, b"version 1\n"),
            entry(
                cas,
                "scripts/check.sh",
                EntryKind::Executable,
                b"#!/bin/sh\nexit 0\n",
            ),
            entry(cas, "docs/lib link", EntryKind::Symlink, b"../src/lib.txt"),
        ])
        .unwrap();
        let origin = cas
            .put_json(
                &json!({"schema": "af.task-source-origin/1", "repository_id": ROOT,
                "source_revision": null, "content_digest": source.content_digest()}),
            )
            .unwrap();
        let source_id = capture_snapshot(cas, &source, &origin, None).unwrap();
        Snapshots {
            source_id,
            source,
            origin,
        }
    }

    /// A candidate whose `src/lib.txt` holds `text`, derived from the source.
    pub fn candidate(&self, cas: &Cas, text: &str) -> (String, Manifest) {
        self.derived(cas, "src/lib.txt", EntryKind::File, text.as_bytes())
    }

    pub fn derived(
        &self,
        cas: &Cas,
        path: &str,
        kind: EntryKind,
        bytes: &[u8],
    ) -> (String, Manifest) {
        let mut entries: Vec<Entry> = self
            .source
            .entries
            .iter()
            .filter(|e| e.path != path)
            .cloned()
            .collect();
        entries.push(entry(cas, path, kind, bytes));
        let manifest = Manifest::new(entries).unwrap();
        let id = capture_snapshot(cas, &manifest, &self.origin, Some(&self.source_id)).unwrap();
        assert_eq!(read_snapshot(cas, &id).unwrap().1, manifest);
        (id, manifest)
    }
}

pub fn kernel_request(workflow: &str) -> RemoteCheckRequest {
    RemoteCheckRequest {
        name: "kernel".into(),
        declaration: RemoteCheckV1 {
            executor: RemoteExecutorV1::GithubPr,
            workflow: workflow.into(),
            required: vec![LINT.into(), CHECK.into()],
        },
    }
}

/// Run the remote phase directly, the way the check operator calls it.
#[allow(clippy::too_many_arguments)]
pub fn phase(
    cas: &Cas,
    remote: &Remote,
    snapshots: &Snapshots,
    candidate: &(String, Manifest),
    owner: &str,
    task_id: &str,
    checks: &[RemoteCheckRequest],
    limit: Duration,
    cancellation: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Vec<RemoteCheckOutcome>, String> {
    let target = remote.target();
    github_pr::run(
        &RemotePhase {
            cas,
            task_id,
            owner,
            candidate_id: &candidate.0,
            candidate: &candidate.1,
            source_id: &snapshots.source_id,
            source: &snapshots.source,
            target: &target,
            mapping: None,
            checks,
            deadline: Instant::now() + limit,
            cancellation,
        },
        &remote.settings(),
    )
}

/// Assert one outcome's state and reason, and that its evidence validates.
pub fn expect(
    outcome: &RemoteCheckOutcome,
    state: RemoteCheckStateV1,
    reason: Option<RemoteCheckReasonV1>,
) {
    outcome.evidence.validate().unwrap();
    assert_eq!(
        (outcome.evidence.state, outcome.evidence.reason),
        (state, reason),
        "{outcome:#?}"
    );
}

/// Every byte the Store holds — CAS objects and the event database — as one haystack.
pub fn store_bytes(directory: &Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut stack = vec![directory.to_path_buf()];
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            for entry in std::fs::read_dir(&path).unwrap() {
                stack.push(entry.unwrap().path());
            }
        } else if let Ok(read) = std::fs::read(&path) {
            bytes.extend(read);
        }
    }
    bytes
}

pub fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

pub fn names(set: &[&str]) -> BTreeSet<String> {
    set.iter().map(|s| s.to_string()).collect()
}
