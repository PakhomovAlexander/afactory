# ADR-0196: Skip pull request measurements only for an explicit `no-perf` label

**Status:** accepted (2026-10-22). Amends the pull request trigger of
[ADR-0177](0177-schedule-continuous-runs-from-trusted-control-and-move-builds-once-to-bounded-shards.md),
the stable check of
[ADR-0190](0190-integrate-performance-checks-images-and-evidence-collection.md) for a labeled
pull request, and the gate pull request authority of
[ADR-0140](0140-run-a-declared-check-through-a-gate-pull-request.md) and
[ADR-0144](0144-hold-afs-disk-use-to-a-machine-budget.md) by one label. Their other decisions
stand. No persisted contract changes.

## Context

Every pull request to `main` runs the continuous performance laboratory: a plan, a build per
planned run and a matrix of hosted sample shards, for hours of runner time. Most pull requests
here are made by af Tasks, and many change only prose — an ADR, a runbook, a change note — whose
bytes no build, test or benchmark reads. Measuring them spends that runner time to compare two
identical products. The owner asked for an opt-out: a `no-perf` label that skips the continuous
measurement of a pull request, applied by agents only to easy changes that obviously have no
performance impact, both on the ordinary pull requests agents open and on the draft gate pull
requests af opens for a Remote Check.

An opt-out is a way not to measure, so it must be narrow and visible. A skipped run must not
read as a measured pass; a missing plan or report must still fail an ordinary run; pushes to
`main`, the nightly schedule, release handoffs, calibration and manual experiments must measure
whatever labels exist; and the label must follow the pull request as it changes. "Docs only" is
not a property a file extension proves: Markdown is embedded into this binary
(`include_bytes!("README.md")` under `crates/af/src/task_execution/starter/`), `.af/` prompts
are af runtime inputs, and a small edit to code is not a neutral one.

## Considered options

- **`paths-ignore` on the workflow.** GitHub skips the whole workflow for a pull request whose
  files all match, so no `Performance check` is reported at all; a required check would then
  wait forever, and a glob over extensions is the heuristic above. Rejected.
- **Decide in each job with `contains(labels.*.name, 'no-perf')`.** Expression comparisons are
  case-insensitive, so `NO-PERF` would exempt too, and the skip would be repeated across every
  job. Rejected for one exact comparison in a first job that every other job follows.
- **Skip the stable check too.** A skipped job satisfies a required check on GitHub, and says
  nothing about why. Rejected: the stable check runs and reports the exemption.
- **Ignore label events for any label but `no-perf`.** The workflow cannot filter on a label's
  name before a run exists, and a run that skipped its jobs would leave a skipped
  `Performance check`, which a required check counts as passing. Rejected: every label event
  plans again, so the newest run always reflects the labels and the measurement it made.
- **Let the gate executor infer prose from extensions or diff size.** Rejected for the reasons
  above. A generic repository's runtime inputs cannot be inferred by af, so the classification
  is the operator's own list of prose-only paths, on the machine-local mapping that already
  authorizes the gate.
- **Let the executor create the label when the repository lacks it.** Adding an unknown label
  through GitHub's API creates it in the repository, an administrative change the gate never
  had authority for. Rejected: the executor checks that the exact label exists and otherwise
  leaves the pull request unlabeled, which only means it is measured.

## Decision

1. **The workflow.** `performance.yml` runs for `opened`, `synchronize`, `reopened`, `labeled`
   and `unlabeled` pull request events to `main`, in the one concurrency group per pull
   request that cancels the run in progress. A first job, `scope`, checks nothing out and runs
   no laboratory or candidate code: it reads the event's pull request labels and outputs
   `exempt=true` only for a `pull_request` event on run attempt 1 whose labels include one
   named exactly `no-perf`. Every other event, a release claim and every rerun output
   `exempt=false` (a rerun reads the payload as it was when the run began, so a label removed
   since then could otherwise exempt it); unreadable labels fail the job. `plan` needs `scope`
   and runs only when `exempt == 'false'`; build, shard and aggregate follow the plan as
   before, so an exempt run has no plan, build or matrix.
2. **The stable check.** `Performance check` still always runs. A first guard requires a
   successful `scope` job and an explicit boolean output; a cancelled or failed scope cannot
   pass even if it emitted an exemption before it stopped. The exemption step, only when
   `scope` succeeded with `exempt == 'true'`, writes `Performance check: exempt (label no-perf),
   not measured` and that no performance claim is made, and succeeds. Every other step is the
   measured path and runs only when the run is not exempt, unchanged: a run without a plan has
   no control and fails, and only an eligible measured primary qualifies. Adding the label to a
   measuring pull request cancels it and reports the exemption; removing it measures again.
3. **The agents' rule.** An agent opening or updating an ordinary pull request inspects the
   whole diff and applies `no-perf` only when every change is prose or a similarly clear
   non-executable edit that no build, test, benchmark, runtime or af input reads, and says why
   in the description. Code of any size, dependencies, build settings, the harness, fixtures,
   benchmarks, CI and performance policy, and anything mixed or uncertain keep the measurement.
   It re-evaluates on every update and removes a label that no longer holds (`AGENTS.md`,
   `docs/performance-sdlc.md`). This change itself changes performance CI and is measured.
4. **The gate executor.** A `[[github_pr]]` mapping entry may list `no_perf_paths`: at most 64
   repository paths, each whose first component is literal and whose later components may use
   `*` within that component; `**`, `.`, `..`, `.af`, `.cargo`, any `.git*` name, a root action
   and Cargo's build-closure names are refused. Every remote phase classifies the whole
   source-to-candidate change afresh: it qualifies only when it is not empty, has at most 1000
   changed paths, and every changed path is a regular file on each existing side that a listed path
   names. Without a list nothing qualifies.
5. **The one label it touches.** On the open draft gate pull request whose head and base are
   exactly this Task's `af-gate/` branches in the target repository, at the gate base commit
   and at the remote's verified head or the pushed head, the executor adds `no-perf` for a
   qualifying candidate only after its head is pushed — only when the repository already has a label spelled exactly
   `no-perf`, and a failed addition only leaves the pull request measured — and removes it for
   one that does not, reading the pull request back to confirm. Before a head update it reads
   the open gate pull request and removes a stale label first; an unreadable pull request or
   label list refuses the update before any push. Thus the update's run is never
   skipped for an earlier candidate; when that removal fails nothing is pushed and every check
   is refused `remote_push_refused`. After the pull request is resolved, a stale label that
   cannot be removed, or that sits on a pull request not provably this Task's, fails every
   check `remote_pr_refused`. No other label, pull request or repository setting is ever
   changed. This repository's gate pull requests target `af-gate/**` bases, which its
   performance workflow never measures, so the label changes nothing for them here.

## Consequences

- A prose-only pull request costs one short job and an exemption summary instead of a build
  and a shard matrix, and a reviewer sees why. A wrong label skips a measurement the change
  needed; the agents' rule, the description's justification and review are the guard. While
  the check is advisory the label changes no merge rule; if the check becomes required, anyone
  who may label a pull request may exempt it, and the owners decide then whether to keep that.
- Any label change re-plans the pull request and cancels its running measurement. That costs a
  restarted measurement for an unrelated label, and is the price of never leaving a skipped or
  stale `Performance check` as the newest result.
- A rerun of an exempt run measures. Push, schedule, dispatch, calibration and release runs
  never read the label.
- An operator who wants labeled gate pull requests creates the label once
  (`gh label create no-perf`) and lists prose-only paths in the mapping; af never creates it.
  For this repository the documented list is `docs/*.md`, `docs/adr/*.md`, `docs/design/*.md`,
  `docs/task-execution/*.md` and `changelog.d/*.md`: none of them is embedded or read by a
  build, test fixture or af at runtime.
- The executor's label decisions are not recorded in Remote Check evidence: a successful label
  change claims nothing and a refused one is the check's recorded reason and message, so
  `af/RemoteCheckEvidence@1` and every other persisted shape stay as they were.
- The workflow decisions were exercised by running the workflow's own steps against stand-in
  events (`scripts/performance/tests/test_performance_workflow.py`) and the executor's against
  the fake `gh` (`crates/review-pipeline/tests/it/remote_checks/no_perf.rs`); neither is a
  hosted run.
