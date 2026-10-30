# Issue capture and asynchronous triage

Status: accepted by the owner on 2026-10-30, revision 3. Decisions 1 to 8 in §2 are the
owner's; revision 2 folded the 19 findings of the af review `triage-design-1` (Codex
gpt-6-astra, medium); revision 3 closes the last two open questions.

## 1. Problem

The repository has 54 open issues on 2026-10-30. Only 32 carry a label, and the labels that
exist (`bug`, `enhancement`, `Agent UX Improvement`, the legacy `test-perf`) say nothing about
how hard an issue is or which pipeline should take it. A hand triage of all 57 open issues on
2026-10-30 took one session and found three issues fixed on `main` and never closed (#196,
#357, #436), and three more partly fixed (#425, #393, #284) with the shipped items unmarked.

Two costs follow. The owner, or an agent working an af Task, finds a defect mid-work and must
either stop to classify it or lose it. And nobody can answer "what can the light pipeline take
tonight" without reading every issue again.

The fix has two halves. Filing an issue costs one minute and no judgement: a title, evidence,
an optional repro. A separate process, running on its own schedule, classifies the issue,
estimates it, names the pipeline and model that should take it, and records all of that as
labels for people and as a typed document for agents.

## 2. Decisions taken by the owner

1. The triage process runs on the Claw executor (`claw-3`) as a cron job, beside the nightly,
   using the `codex-personal` and `claude-personal` logins registered there. GitHub Actions is
   not an option: the repository's Providers are OAuth CLI logins, not API keys.
2. Two models triage every issue independently, and the comparison runs through an af
   pipeline, not an ad hoc script.
3. `perf` is a class of its own, not an area under `bug` or `feature`.
4. Complexity is two labels, `size/` and `pipeline/`, not one combined tier.
5. A new raw template is added, and the existing bug template's required fields are loosened
   as well, so outside reporters are not turned away by a form.
6. There is no `area/` namespace; where an issue lives is derived from the author's repository
   citations at read time (§4).
7. When an implementer finds the size estimate wrong, it posts a comment on the issue saying
   so, and that comment is a re-triage trigger (§6.6). The implementer never sets labels.
8. The verifier's own class and size travel in the text of its `af/ReportEvaluation@1`, as a
   second `Triage` block the check parses. No typed kernel field is added for triage.

## 3. Capture

### 3.1 The raw template

`.github/ISSUE_TEMPLATE/raw.yml`, "Raw note (triaged automatically)". Three fields:

- **What I saw** (required). One paragraph. The title may be a sentence.
- **Evidence** (optional). Anything pasteable: af Task ids, an `af task report` block, a commit
  sha, a command with its stderr, a gate log path, a screenshot.
- **Repro** (optional). Steps, or "seen in Task X" when that is all that is known.

The template applies exactly one label, `triage/pending`. It applies no class and no title
prefix. The bug template keeps its fields but requires only `What happened`, `af version` and
`Evidence`; the policy and command fields become optional. The feature template is unchanged.
Both existing templates also apply `triage/pending`, so every new issue enters the same queue.
The `bug` and `enhancement` labels those templates apply are queue hints that the runner is
authorized to replace (§6.5).

### 3.2 Filing from a session

An agent or the owner files from the shell with the same shape:

```sh
gh issue create -R PakhomovAlexander/afactory -l triage/pending -t "<one sentence>" -F body.md
```

`body.md` has the three headings of the raw template. The rule, added to `AGENTS.md` and the
session `CLAUDE.md`: when you meet a defect or a gap during work, file it raw with the evidence
you have and continue; never classify, estimate or search for duplicates yourself. The triage
process does that, and it is the only process that does.

### 3.3 Evidence conventions

Evidence is more useful to the triager when it is one of these, in this order of preference:

1. An af Task id plus its workstream, so the triager can read `af task show --checks`.
2. A commit sha or PR number on `main` the behaviour was observed on.
3. The exact command and the stderr line, in a fenced block.
4. A repository-relative file path with a line number, as `crates/af/src/main.rs:3485`.

Prose alone is accepted and triaged with lower confidence.

## 4. Labels

Every triage label is namespaced. The namespace is for parsing; the value is for reading. The
triage runner and the owner set these; nobody else does.

| Namespace | Values | Cardinality | Meaning |
|---|---|---|---|
| `class/` | `bug`, `perf`, `feature`, `aix`, `uix`, `sdlc`, `security`, `docs`, `research`, `decision` | exactly one | What kind of work it is |
| `size/` | `S`, `M`, `L`, `XL` | exactly one | Effort by the rubric in §5 |
| `pipeline/` | `light`, `default`, `design`, `hand` | exactly one | Which af pipeline, hence which model, takes it |
| `triage/` | `pending`, `needs-info`, `disputed`, `maybe-fixed`, `duplicate`, `held` | zero or one | Process state |

Class definitions:

- `bug`: af does something other than its documentation, contract or ADR says.
- `perf`: a measured cost (time, tokens, disk, memory) that is higher than it should be, with
  the number in the issue. A wrong number is a `bug`.
- `feature`: a capability or contract that does not exist.
- `aix`: agent experience, the ergonomics of af for an agent that drives it: output shapes,
  next-action lines, JSON documents, prompts.
- `uix`: human experience: the TUI, help text, first screens, onboarding.
- `sdlc`: tests, CI, gates, release, repository process, test speed and flakes.
- `security`: a trust-boundary defect. The runner never labels or comments on one in public;
  see §7.
- `docs`: documentation alone.
- `research`: a study whose output is a report, not code.
- `decision`: nothing moves until the owner chooses; the comment names the choice.

**Precedence.** The definitions overlap (#432 is a contract bug, an agent-facing JSON shape
and a TUI screen at once; #211 is a measured gate latency and a gate process change). A triager
takes the first matching class in this order: `security`; `decision` when the issue is
explicitly blocked on an owner choice; `research` for a report-only deliverable; `docs` for a
documentation-only change; `bug` for violated documented behaviour, except that measured
excessive resource consumption is `perf` unless the measurement itself is wrong; `perf`; `aix`;
`uix`; `sdlc`; `feature`. When several experience or process classes match, the class is the
one named by the issue's primary acceptance criterion, ties broken in the listed order; the
others are named in the rationale. There is no `area/` namespace (owner decision, 2026-10-30):
where an issue lives is derived at read time from the author's repository citations, whose first
path component names the crate or script directory.

`triage/` states:

- `pending`: not yet triaged, or re-triage requested.
- `needs-info`: the triager could not reach a confident class or size; the comment asks one
  concrete question. A reply from anyone re-enters the queue (§6.6).
- `disputed`: the two triagers disagree on class or size; the comment carries both rationales.
  An owner label on the disputed namespace resolves it (§6.5).
- `maybe-fixed`: the weekly sweep found an explicit completed-fix reference on `main`; the
  comment carries the evidence.
- `duplicate`: both triagers named the same open issue as the subject; the comment names it.
- `held`: the issue is under private owner review (§7) and no runner touches it.

Existing labels migrate once, by script: `Agent UX Improvement` becomes `class/aix`; `test-perf`
becomes `class/sdlc`; `bug` and `enhancement` stay as the labels the outside
templates apply, and triage replaces them with `class/bug` and `class/feature`. The PR labels
(`test:*`, `release:*`, `no-*`) are unchanged: they select validation classes on pull requests
and are not issue labels.

## 5. The size rubric

The rubric is what makes two models and the owner reach the same estimate. Sizes are evaluated
in descending order, `XL`, `L`, `M`, `S`, and the first match wins. Insufficient evidence
produces `needs-info`, never an invented size.

| Size | Test, in order | Default pipeline | Model and reviewers |
|---|---|---|---|
| `XL` | Needs a design document and owner decisions before any code. | `design` | Design rounds with the owner; no implementation Task until the design is accepted |
| `L` | Changes a contract, schema version, budget or sandbox boundary, or needs several Tasks. | `design` | A workstream: one design review by Astra xhigh before the first implementing Task, then `default` per Task |
| `M` | Everything understood well enough to implement that is not `L` or `S`: two or three crates, a new fixture or test class, an additive field under an existing document version, a new or amended ADR that records a bounded change. | `default` (`default/implementation`, ADR-0253) | Opus 5.5 high implementer; Terra bugs and Sol correctness reviewers (Sol required); Astra medium gate-quality reviewer; Astra high evaluator. Findings from every bound reviewer count toward convergence |
| `S` | All of: one module; repro present or derivable from the evidence; no schema, contract or budget change; no new fixture or test class; no ADR. | `light` (`default-light/implementation`, ADR-0262) | Opus 5.5 medium implementer, correctness reviewer, evaluator |

Two rules the check enforces mechanically (§6.3):

- A proposed contract, schema-version, budget or sandbox-boundary change forces at least `L`.
  Citing an existing ADR as evidence of expected behaviour does not affect size; a needed new
  or amended ADR is recorded in the document's `adr` field and makes `S` impossible, so the
  Task that writes it runs on `default` at least.
- `pipeline/` follows size (`S` to `light`, `M` to `default`, `L` and `XL` to `design`) unless
  the owner has set `pipeline/hand`, for a configuration edit, a documentation line or a GitHub
  action where a model Task would cost more than the change. The triager may propose `hand` in
  its rationale; only the owner sets it.

The hand triage of 2026-10-30 (`review-context/hand-triage-2026-10-30.json`) is a baseline for
calibration, not expected output under this rubric. Its "easy" set (#474, #432, #430, #355,
#211, #207, #199, #139) is re-evaluated by the rules above before any count is published: #474
amends ADR-0170's deadline semantics, so it is at least `M` with `adr` set; #432 adds a group to
the `af/task-attention@1` document, an additive field under the existing version, so `M`; the
rest stay candidates for `S` until a triager confirms the repro.

## 6. The triage pipeline

### 6.1 Shape: a report Task, not a new profile

The kernel's installed profiles are implementation, verification, document and report
(docs/tasks.md; ADR-0133). None is an evaluator-free Task with an arbitrary output, and adding
a package named `kernel/triage` would not create one. Triage therefore runs as the **report
profile**, which already has the shape decision 2 asks for: one author, a protected token-free
check, and one independent verifier reading the same Snapshot (ADR-0133, decisions 1 and 5).

- The business kind is `triage`, an `af.task-kind/1` package with `profile = "report"`, so Task
  files and `af task list` say what the Task is while the kernel runs the installed operators.
- The **author** slot is the first triager: Codex `gpt-6-astra`, `model_reasoning_effort =
  "medium"`, Provider `codex-personal`, `read-source` only (no `execute-checks`, so no shell).
  It returns an `af/DocumentDraft@2` with two required sections, `Triage` (the
  `af/issue-triage@1` document in one fenced `json` block, §6.3) and `Rationale`, and
  `repository_citations` for every file it relied on.
- `report_seal` and `report_check` are the kernel's: the Document is bound to the exact source
  Snapshot of `main`, and every citation is resolved against that Manifest, so a path the issue
  invented, a symlink or a binary fails the check and suppresses the verifier (ADR-0133,
  decision 5).
- The **verifier** slot is the second triager: Claude Opus 5.5, effort medium, Provider
  `claude-personal`, `read-source`. Its requirements say: classify the issue yourself by §4 and
  §5 before reading the author's `Triage` section, then return `af/ReportEvaluation@1` with
  `verified` only when your class and size equal the author's, and otherwise your own class,
  size and reason in the evaluation's text as a second `Triage` block in the same JSON shape
  (decision 8), which the check parses. A verified Task is agreement; a `changes_requested` Task
  whose evaluation names a different class or size is a dispute; a failed check is a failed
  Attempt, never a guessed label.
- The runner reads `af task output --port report` and `af task show --json` after the Task
  finishes and runs `scripts/backlog/check-triage.py` on both (§6.3) before any write to GitHub.

Both replies go through the typed paths (ADR-0103 for Claude, ADR-0208 for Codex): a reply that
does not parse is a failed Attempt. Budget per Task: 150k chargeable tokens, `max_attempts`
as the report pipeline's 8 (two model Attempts, two admissions, room for one retry each),
`wall_ms` 20 minutes. A Task that exhausts its budget leaves the issue `triage/pending` for the
next run, at most three times, then `triage/needs-info` with the reason.

The rollout (§8) proves this shape with a compiling example plan before any runner code.

### 6.2 Inputs to a triager

The runner writes, per issue, a `report_sources` file in the `af.document-sources/1` shape
(ADR-0133, decision 3: at most 256 entries, 256 KiB each, 512 KiB in total, 640 KiB file; a
file over a bound is refused at planning). Everything in it is deterministic and token-free:

- `issue`: the body and comments from `gh issue view --json`, with the body's sha256;
- `citations`: every mention of `#N` in `CHANGELOG.md`, `changelog.d/` and `docs/adr/` on the
  captured `main` commit, with ten surrounding lines;
- `pulls`: open and merged PRs whose title or body names `#N`, with their closing references;
- `neighbours`: the ten open issues nearest by title, for duplicate detection;
- `excerpts`: up to 200 lines around each `path:line` the body names, read only from the
  pinned `main` Git manifest by canonical repository-relative path with a positive line number
  at most the file's line count. Absolute paths, traversal, symlinks, binaries, paths outside
  the manifest and URLs are refused and recorded as missing evidence; at most 16 excerpts and
  128 KiB in all;
- `rubric`: §4 and §5 verbatim, and the Task's requirements repeat them.

Truncation and refusals are recorded in the file before any Provider call. The source Snapshot
is the `main` commit the sources were built from, so the author's citations resolve against the
same tree the runner read. Issue text comes from anyone with a GitHub account and is untrusted:
the requirements say so, the reply is a typed document, and the only effects are the labels and
the comment the runner applies after the check passes.

### 6.3 The documents

The author's `Triage` section holds one `af/issue-triage@1`:

```json
{
  "schema": "af/issue-triage@1",
  "issue": 432,
  "body_sha256": "…",
  "class": "bug",
  "size": "M",
  "pipeline": "default",
  "adr": null,
  "confidence": 0.86,
  "rationale": "One function, TaskAttentionV1::of, puts every planned and waiting Task in
    the approval group; the fix adds a group to af/task-attention@1 and four call sites.",
  "resolution": {"state": "open", "evidence": [], "addressed": [], "remaining": []},
  "duplicates": [],
  "needs_info": [],
  "acceptance": "af task list --attention shows a Task that needs input under its own
    heading, and the task-attention JSON group names it."
}
```

`resolution.state` is `open`, `partial` or `fixed_candidate`; `evidence` lists commits or PRs
on `main`, `addressed` and `remaining` the acceptance items each covers. A reachable commit
proves neither full nor partial completion: the check verifies that each reference exists and
is reachable from the captured `main`, and both triagers must support the claimed state before
the runner acts on it. `adr` names a new or amended ADR the change needs, or is null.

`scripts/backlog/check-triage.py` validates the author's JSON against
`schemas/issue-triage-v1.json` and the verifier's evaluation against the kernel's
`af/ReportEvaluation@1`, then applies the hard rules: a contract, schema-version, budget or
sandbox change in the rationale with a size below `L`; `adr` set with size `S`; a
`fixed_candidate` or `partial` without reachable evidence; a `security` class anywhere. Any of
these refuses the Task's output and the runner records why.

**Merge and verdict.** One rule produces the single record the runner publishes, in this order:

1. `security` from either triager, or an existing `class/security` or `triage/held` label:
   hold (§7); no other step runs.
2. `needs_info` non-empty from either, or `confidence` below 0.6 from the author, or the
   verifier's evaluation says the evidence is insufficient: `triage/needs-info`, no `class/`,
   `size/` or `pipeline/` label.
3. Author and verifier differ on class or size, or on `resolution.state`, or name different
   duplicates: `triage/disputed`, no `class/`, `size/` or `pipeline/` label, both positions in
   the comment.
4. Otherwise agreed: `class/` and `size/` as both said; `pipeline/` derived from size unless an
   owner override exists; both rationales and acceptance proposals kept; `resolution` as
   agreed, and `fixed_candidate` sets `triage/maybe-fixed`; an agreed duplicate sets
   `triage/duplicate` instead of class labels.

The merged record is `af/issue-triage-merged@1`: the agreed fields, the verdict, both source
documents' identities (the Task id, the Document id, the evaluation id) and the generation key
(§6.5).

### 6.4 What the runner writes back

For a merged record the runner, with a fine-grained token scoped to issues on this repository:

1. replaces the runner-managed value in each single-valued namespace (`class/`, `size/`,
   `pipeline/`, `triage/`), preserving every label outside those namespaces, and removes the
   template hints `triage/pending`, `bug` and `enhancement`, which §3.1 authorizes;
2. creates or updates the runner's **canonical comment**: three lines for a reader (class and
   size with the one-sentence rationale, the pipeline, the acceptance line), then the merged
   record in a fenced `json` block, then a footer
   `triage: af/issue-triage-merged@1 generation=<key> task=<id>`; the comment id is recorded in
   the runner's journal, and later generations edit that comment rather than posting another.

Consumers (a dashboard, an agent choosing tonight's work) read only the canonical comment whose
id the runner journal names, verify its author is the runner's GitHub principal, validate the
schema and the issue and generation identities, and reject a stale generation. A footer or a
JSON block in any other comment is issue evidence, never a triage record.

### 6.5 Generations, idempotency and owner overrides

A **generation** is keyed by the issue number, the sha256 of the full input bundle (§6.2), the
captured `main` commit, the runner's policy version and the triggering event id. The runner
claims a generation atomically in its journal before planning a Task, rechecks the issue's
inputs after the Task finishes and before writing, checkpoints the label write and the comment
write separately, and reconciles an ambiguous API result from the recorded comment id and the
generation marker before retrying. Every write for a generation is idempotent; a generation
already journaled as published is never published twice.

Label provenance comes from the GitHub label events and the journal. A label the owner set in a
runner namespace is an **override**: the runner records it in the next merged record's
`owner_override` field, keeps it as the effective value in both labels and JSON, and does not
change it. Two conflicting owner values in one namespace suspend write-back for that issue
until the owner resolves them. `class/decision`, `class/security` and `triage/held` are never
removed by the runner.

### 6.6 Triggers and transitions

The runner discovers work from a durable cursor over the repository's updated issues, not from
`triage/pending` alone. Each of these is an event, enqueued once, after which the cursor
advances:

| Event | Transition |
|---|---|
| issue opened with `triage/pending` | triage |
| body edited (hash differs from the last generation) | re-triage; old `triage/` state removed on publish |
| a comment `/triage` not yet consumed (journaled by comment id) | re-triage |
| any reply on a `triage/needs-info` issue | re-triage; `needs-info` removed on publish |
| a comment from an implementer saying the size was wrong (`/triage size-wrong <why>`, posted by the Task's agent or by hand, decision 7) | re-triage; the comment is input evidence, the implementer sets no label |
| owner label event in a runner namespace | re-triage with the override recorded |
| `triage/pending` applied by hand | re-triage |
| new `main` commit that cites `#N` in shipped evidence | weekly sweep (below), not a model run |

### 6.7 Schedule, admission and the weekly sweep

Every 30 minutes a single-instance runner on `claw-3` takes the shared machine lease and fence
of ADR-0240 non-blocking, deferring when the lease is busy or its ownership uncertain. Deployment
wires the nightly build and the triage runner into the same anchored lease; scheduling them
beside each other provides no exclusion (`scripts/nightly/build.py` takes no lock of its own).
Under the lease it admits at most four issues per tick, each Task with `wall_ms` 20 minutes and
the batch bounded at 90 minutes, and releases the lease only after every owned process has
stopped and cleanup is confirmed.

Spend is a cap, not a retrospective check: before admission the runner reserves the Task's whole
allowance (150k tokens including both Provider admissions and retries) in a persistent daily
ledger, in the `claw-3` timezone, and admits only when settled usage plus outstanding
reservations plus the new reservation is at most 2M. Settled usage comes from `af task show
--json`; unknown usage (docs/tasks.md) keeps its reservation.

Once a week the same runner sweeps every open issue without a model call. An issue gets
`triage/maybe-fixed` and a comment with the citation only on an explicit completed-fix
reference: a shipped changelog entry that names `#N` as fixed, an ADR that records the change
as done, or a merged PR whose closing reference names it. A bare mention, a "not done here"
paragraph (ADR-0208 names #166 that way) or a partial fix is not enough; the sweep records such
citations in its log for owner review instead. The sweep preserves `needs-info`, `disputed` and
`held`, deduplicates its comments by issue and evidence identity, and never closes an issue. A
`triage/needs-info` issue with no reply for 30 days gets one reminder comment. Closing is the
owner's, as it was for #196, #357 and #436 on 2026-10-30, each with the evidence comment this
sweep would have written.

## 7. Guardrails

- The runner never closes an issue, never edits a body, never removes a label outside its
  namespaces or one of `class/decision`, `class/security`, `triage/held`, and never posts a
  second canonical comment for one issue.
- Every write is idempotent on its generation key (§6.5).
- Spend is reserved before admission against a daily cap (§6.7); a Task cannot start without
  a reservation.
- The triagers run with the Task sandbox's read-only Snapshot and no network beyond the
  Provider; the author has no shell.
- **Security hold.** An existing `class/security` label, a `triage/held` label, or `security`
  from either triager stops every public effect for that issue: no label, no comment, no
  needs-info. The runner writes a private hold record in its journal and notifies the owner
  through the nightly's existing channel. The runner and the sweep skip held issues until the
  owner removes `triage/held` by hand. Only the owner creates the advisory and closes the public
  issue.

## 8. Rollout

1. **Labels and templates** (`pipeline/hand`): create the namespaced labels, migrate the two
   old ones by script, add `raw.yml`, loosen `bug_report.yml`, add the filing rule to
   `AGENTS.md`. One PR.
2. **Kind, packages and schema** (`size/M`, `default`): an af Task builds and tests, in a
   writable staging directory outside `.af/`, the `triage` kind package, the author and
   verifier Worker packages, their requirements text, `schemas/issue-triage-v1.json` and
   `scripts/backlog/check-triage.py` with tests (a Task Worker may not edit `.af/`, ADR-0133).
   The owner then installs the reviewed packages under `.af/task-packages/`, pins them in the
   catalog with their digests, and commits the authority. Acceptance includes one compiling
   example plan, `af task plan` on a real issue's sources file, before any runner code.
   The check's fixtures are the six issues verified by hand on 2026-10-30: #196, #357 and #436
   must come out `fixed_candidate`, #425, #393 and #284 `partial` with the remaining work as
   recorded in `hand-triage-2026-10-30.json`.
3. **Runner** (`size/M`, `default`): `scripts/backlog/triage.py`: sources assembly, the
   journal, the generation key, the lease and ledger, write-back, the sweep, and the cron unit
   on `claw-3`. Dry-run mode prints the labels and comment and writes nothing; the first week
   runs dry.
4. **Backfill**: run the triager over the 54 open issues in dry-run, compare with the hand
   triage of 2026-10-30, adjust the rubric wording where the two disagree, then apply.
5. **Plugin**: when the plugin architecture (#316) lands, `scripts/backlog/` becomes
   `af backlog triage` with no change to the documents, labels or comment format.

## 9. Resolved questions

1. `size/` after a Task starts: the implementer, or the owner on its behalf, posts
   `/triage size-wrong <why>` on the issue. The runner re-triages with that comment as
   evidence; the implementer never edits labels (decision 7).
2. The verifier's class and size: a second `Triage` block in the evaluation's text, parsed by
   `scripts/backlog/check-triage.py`; no typed kernel field (decision 8).
