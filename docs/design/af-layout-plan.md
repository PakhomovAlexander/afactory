# The `.af/` layout and Task evidence — implementation plan

**Status:** executed, 2026-09-22 to 2026-09-23. **Baseline:** kernel `main` 4181706 (v0.9.0-rc.6).
**Execution model:** every package below is one Task file kept outside the repository, compiled
and run through the campaign Pipeline `kernel/implementation-reviewed` from
`.af/task-catalog.toml`. Package IDs are local planning IDs, not issue or PR numbers.

## 1. Problem

A consumer's pull request carried 244,008 added lines, of which 90% were candidate patches,
reviewer results, logs and captured Task inputs under `.af/tasks/<workstream>/` and `docs/`.
The kernel wrote none of them: `af` refuses run state inside the checkout
(`crates/af/src/main.rs`, `Options::resolved_state_dir`) and keeps every recorded artifact in
the Store under `$XDG_STATE_HOME/af`. The files were written by the coordinator driving `af`,
because each new Task file needed the previous Task's candidate and reviews, and a repository
path was the only way to hand them over. Snapshot capture includes every tracked and
untracked-not-ignored path, so the same files then rode along in every later candidate.

Three things made this the path of least resistance:

- Nothing declares what `.af/` may hold. `af onboard` writes a handful of files, and any other
  path under `.af/` is equally unknown to the kernel, so nothing could warn.
- Nothing says where a Task file lives. The kernel's own campaigns kept them under
  `.af/tasks/`, which reads as an endorsement.
- A Task file cannot name another Task's outputs. Chaining implementation to repair to review
  meant exporting artifacts to disk and pointing the next Task at the path.

## 2. Outcome and fixed requirements

Git holds declarations only: configuration, pipeline definitions, worker packages, checks and
policies. Task files, run state, candidate patches, transcripts, reviewer results, receipts and
logs never belong in the repository, and the kernel says so where it matters: in one declared
layout, in a warning when a Task file sits in the checkout, in a report of undeclared `.af/`
paths at plan and delivery time, and in a typed way to reference a prior Task's outputs so
nothing has to be exported to disk.

These decisions are fixed for every package:

- **The layout is data, declared once.** One table in code lists every canonical `.af/` entry
  with its kind (file or directory) and role. `af help`, `docs/design/config.md` and every
  classification read that table. No second list.
- **A Task file is a request, not configuration.** The kernel captures it into the Store at
  plan time, so its on-disk copy is disposable. Its home is outside the checkout; a Task file
  inside the checkout that git would track is warned about, never refused.
- **Undeclared `.af/` paths are reported, not stripped.** Classification runs over the captured
  Snapshot manifest, never over the working tree. The result is advisory by default and a
  policy switch makes delivery refuse before its first Git mutation. Nothing removes files from
  a Snapshot, and Snapshot identity is unchanged.
- **A referenced output grants nothing.** Binding a Task input to a prior Task's output carries
  exact artifact identity into the plan. It carries no acceptance, verification or delivery
  authority; the new Task earns its own.
- **Old records stay readable.** New receipt fields default to empty on deserialization; new
  Task-file fields are optional; artifact types that change get a new version.
- **Nothing is weakened.** No contract, fixture, gate, budget or sandbox boundary changes to
  make a package pass.

## 3. Execution model

Every package runs through `kernel/implementation-reviewed`: one implementer Attempt, seal,
`make check` and `markdownlint` as required checks, two independent reviewers (bugs,
correctness), review acceptance, one independent goal evaluation, then explicit delivery to a
new local worktree. A verification Task (`kernel/verification`) re-runs the checks and the
evaluator on the delivered Snapshot. Each successor package captures the delivered, locally
committed predecessor.

Task files, the bindings file and the state directory live under
`$XDG_STATE_HOME/af/workstreams/af-layout/`, outside every checkout. Provider labels in the
catalog are neutral (`claude-main`, `codex-main`); the uncommitted bindings file maps them to
machine-local accounts, and the independence policy requires distinct principals between the
implementer and every verifier.

```sh
W=$XDG_STATE_HOME/af/workstreams/af-layout
af task plan --file $W/tasks/l1-layout.json --bindings $W/bindings.toml --state $W/state
af task explain layout-l1 --tree --state $W/state
af task run layout-l1 --confirm-plan <full PLAN id> --state $W/state
af task deliver layout-l1 --branch agent/layout-l1 --worktree ../afactory-wt-layout-l1 \
  --confirm layout-l1 --state $W/state
```

A package that ends unverified is fixed by a human or an agent in its worktree and re-run as a
new Task revision; the failed Task's evidence and spend stay recorded. Reserves are not raised
to make a failed Attempt pass.

## 4. Packages

### L1 — Declare the `.af/` layout and the Task file home

**Depends on:** nothing.

Deliverables:

1. A layout table in `review-config` (or the crate that owns `.af/` resolution) listing the
   canonical entries: files `af.toml`, `af.local.toml`, `af.lock`, `README.md`,
   `code-policy.toml`, `task-catalog.toml`, `optimization-policy.json`,
   `optimization-sources.toml`; directories `pipelines/`, `workers/`, `task-packages/`,
   `checks/`, `vendor/`. Each entry carries its kind, who writes it (human, `af onboard`, `af
   catalog sync`) and whether git versions it (`af.local.toml` is the one that is not). A
   function classifies any repository-relative path as declared or undeclared under `.af/`.
2. A test that every `.af/` literal the kernel reads or writes (onboard, config layers, lock,
   catalog, policy loaders) is in the table, so the table cannot drift from the code.
3. `af help config` (or the topic that documents the XDG layout) renders the table from the
   code and states, in one paragraph, what never belongs under `.af/` and where it lives
   instead: Task files, run state, candidate patches, transcripts, reviewer results, receipts,
   logs and measurements go to the Store under `$XDG_STATE_HOME/af`.
4. `docs/design/config.md` gains a section "What `.af/` holds" with the same table and the
   same paragraph, and its claim that `af init` gitignores `af.local.toml` is corrected to
   what the code does.
5. `af task plan`, `af task start`, `af review plan --file` and `af review run --file` warn on
   stderr when the Task file resolves inside the repository and `git check-ignore` does not
   ignore it. The message names the file, says the kernel captured it, and names the
   recommended home. Advisory only; exit codes and `--json` documents are unchanged.
6. The kernel's own Task files under `.af/tasks/warm-layers/` are removed from the repository;
   they are captured in the Store. `docs/design/worker-warm-layers-plan.md` keeps its
   historical commands and gains one sentence saying where Task files live now.
7. One ADR recording the layout decision, linked from `docs/adr/README.md`; a `CHANGELOG.md`
   entry under Unreleased.

Acceptance:

- `af help` and `docs/design/config.md` list the same entries as the code, proven by a test
  or a fixture that renders the table.
- Planning a Task file placed under `<repo>/.af/tasks/x.json` in a fixture repository prints
  the warning; the same file under a gitignored path or outside the repository does not.
- `git ls-files .af/tasks` is empty on the delivered tree.
- Existing fixtures and `--json` outputs are byte-identical.

### L2 — Report undeclared `.af/` paths at plan and delivery

**Depends on:** L1.

Deliverables:

1. A classification over a captured Snapshot manifest: every path under `.af/` is `declared`
   or `undeclared` by the L1 table, with per-group path count and byte total. It reads the
   manifest only; it never walks a working tree or a sandbox.
2. `af task plan` and `af task start` print one advisory line when undeclared `.af/` paths
   exist in the captured source Snapshot, with the count, the byte total and up to ten paths,
   and carry the full list in a typed field of the `--json` document.
3. `af task deliver` records `undeclared_af_paths` (paths and bytes) in `TaskDeliveryRecord`
   beside `ignored_paths`, prints it in the delivery summary, and old receipts deserialize
   with an empty default.
4. A project policy `[delivery] undeclared_af_paths = "warn" | "refuse"` in `.af/af.toml`,
   default `warn`. Under `refuse`, delivery fails before the prepared record and the first Git
   mutation, naming every undeclared path. The policy value is part of the captured project
   policy identity so a later edit cannot change an admitted plan.
5. Tests and fixtures: a repository with `.af/tasks/x/candidate.patch` and
   `.af/tasks/x/reviews.json` reports both at plan time; delivery under `warn` succeeds and
   records them; delivery under `refuse` refuses with no branch, no worktree and no prepared
   record; a repository with only declared entries reports nothing.
6. One ADR, linked from the index; a `CHANGELOG.md` entry under Unreleased.

Acceptance:

- Classification is a pure function of the manifest and the L1 table; the same Snapshot gives
  the same result on every machine.
- Snapshot identity, the delivered tree and `ignored_paths` are unchanged by this package.
- No path is ever removed from a Snapshot or a delivered worktree.

### L3a — Decide how a Task references a prior Task's outputs

**Depends on:** nothing; sequenced after L2 so one worktree carries the campaign.

Deliverables:

1. One ADR, status proposed, deciding how a Task file binds a Pipeline input port to an
   output of a previously recorded Task in the same Store. It must settle:
   - the Task-file shape (an optional `inputs` table mapping a root port name to
     `{ "task": "<task_id>", "port": "<output port>" }`, or an exact
     `{ "artifact": "sha256:…" }`), and which root ports may be bound (at least `source` and
     `history`);
   - resolution at plan time only, from the `--state` Store, into exact artifact IDs recorded
     in the compiled plan, so resume and replay never read the referencing Task again;
   - type checking against the port's declared `artifact_type` and cardinality, refusing a
     mismatch before any Worker admission;
   - what the referenced Task must be: recorded, its result published, the referenced output
     admitted; whether an unverified Task's `snapshot` may be referenced (decide, with reasons);
   - authority: the reference carries provenance only; no acceptance, verification, plan
     approval or delivery authority crosses Tasks, and the new Task's delivery still compares
     its own source Snapshot against the target repository exactly as ADR-0031 requires;
   - how `af task explain` and `af task show` display the binding, and how the source-origin
     record (`af.task-source-origin/1` or its successor) names the referenced Task and result;
   - the rejected options: exporting the artifact to a file and referencing the path; a
     `--continue-from TASK` flag; cross-Store references; and why each was rejected.
2. A short section in `docs/task-execution/` describing the resulting Task-file shape, linked
   from `docs/README.md`, and the plan of the implementing package L3b (crates, types, tests).
3. The ADR is linked from `docs/adr/README.md`; no code changes in this package.

Acceptance:

- The ADR follows the repository's ADR shape: status line, context, options with rejection
  reasons, decision, consequences.
- Every bullet above has a decision, not a deferral.
- `markdownlint` and `make check` pass on the unchanged code.

### L3b — Implement Task input references

**Depends on:** L3a accepted. Its Task file is written from the ADR after L3a is delivered
and read; deliverables are the ADR's decisions, the schema entries, `af task explain`/`show`
output, the source-origin record, and a fixture Task that binds `source` to a prior Task's
`snapshot` and `history` to its review ledger, then plans, runs and delivers.

L3a decided these in [ADR-0117](../adr/0117-bind-task-inputs-to-recorded-task-outputs.md) and
wrote the crate, type and test plan into
[`docs/task-execution/task-inputs.md`](../task-execution/task-inputs.md). One correction to the
fixture named above: a Task whose `source` is bound to a prior Task's derived `snapshot` cannot
be delivered, because a derived tree has no commit for the target repository's `HEAD` to equal
and ADR-0031's exact comparison has nothing to compare against. The fixture is therefore two
Tasks in one Store — one that binds `history` and delivers, one that binds `source`, runs, and
asserts the delivery refusal — rather than a single Task that does all three.

L3b found a second correction while building that fixture. A bound `history` is an exact
`af/ReviewHistory@1`, but a Pipeline whose Review would *continue* the referenced Round cannot
consume it: restoring a Round recomputes it, and the recomputation requires every reviewer
result to retain the current Task's `af/Requirements@1`, which a predecessor's results never
can. So a bound `history` feeds a Pipeline that reads the ledger — the repair case this plan
describes in §3 — and the fixture's successors run a Pipeline that hands the ledger to their
implementer. Continuing a Round across a Task boundary needs its own decision about
requirements retention; ADR-0117 did not make one and L3b did not relax the rule.

## 5. Validation and rollback discipline

Each package is one Task with its own reviewers and evaluator, delivered to its own worktree
and verified there by a verification Task before the next package captures it. A package that
fails review or verification is fixed in its worktree by a human or an agent and re-run as a
new Task revision. Anything unverified is not merged.

## 6. Execution record

### L1

Task `layout-l1`, plan `sha256:135e4bbf…`, source Snapshot at commit 53c463e. The implementer
(Claude Opus 5, high) completed one Attempt of 538,605 chargeable tokens and sealed a candidate
with every deliverable except the deletion of `.af/tasks/warm-layers/`: its sandbox exposes no
shell and no delete tool, which it reported instead of shipping a knowingly red test. The Task
Gate then failed on two pre-existing tests unrelated to the package,
`concurrent_identical_setup_is_idempotent` and
`concurrent_adds_are_serialized_without_losing_entries` in `crates/af/tests/provider_registry.rs`,
so both reviewers were suppressed and the Task ended `changes_requested` after four Attempts.

The candidate was materialized from the Store by its manifest (`CandidateTree@1` →
`Manifest` → blobs) onto the campaign branch and finished by hand in commit d66bfbc: the nine
Task files removed with `git rm`, and the Gate failure fixed at its root — APFS answers a
concurrent `openat(O_CREAT)` of the same lock name with a spurious `ENOENT`, so both provider
lock openings now retry it a bounded number of times (`open_lock_at` in
`crates/af/src/providers.rs`). `make check` then passed apart from one load-only flake in
`review-source-git` (`tree_diff_ignores_candidate_textconv_and_hostile_diff_configuration`,
green three of three alone). Review runs as Campaign `layout-l1` on the `p1-review` Pipeline
over the diff against `main`; verification follows as Task `layout-l1-verify`.

Kernel findings: an implementer Worker without a shell cannot delete a file, so a package whose
deliverable is a deletion needs either a delete tool or a human step; and a Gate that fails on a
test the package never touched still costs the whole review, which argues for the Gate reporting
the failing test's path set against the Change Set.

### L2

Task `layout-l2`, plan `sha256:3957d760…`, source Snapshot at commit 0bcdd37. The implementer
completed one Attempt of 615,335 chargeable tokens and sealed every deliverable: the manifest
classification and `PathGroup` in `review_config::layout`, the one-line advisory and the refusal
text, the `undeclared_af_paths` field in the plan document and the delivery receipt, the
`[delivery] undeclared_af_paths` policy captured into the project policy identity, ADR-0116,
schemas and a fixture. The Task Gate failed on one existing test,
`reviewed_implementation_preserves_evidence_when_independent_work_fails`, which requires the
`start --execute` and `run` documents of one Task to be equal: the candidate wrote the field only
from the plan path, so `run` lacked it. Fixed by hand: every presentation now derives the same
group from the recorded source Snapshot (`recorded_undeclared_af_paths`), which also makes `af
task show` report it for old Tasks — the campaign's own L1 Task shows its nine former
`.af/tasks/warm-layers/` files, 20,149 bytes. ADR-0116 was corrected to say so.

Kernel finding, fixed in this package's commit because it blocked every Gate on this machine:
the process supervisor killed a child's process group *after* reaping the child. A reaped pid is
free for reuse and a group id is reserved only while the group has a member, so on a loaded
machine running thousands of short git commands the `SIGKILL` landed on an unrelated, freshly
spawned git that had recycled the pid — seen as `git rev-parse` of a just-created commit failing
with an empty stderr in `review-source-git` fixtures, twice in three full `make check` runs and
never alone. `wait_exact_cancellable` now observes the exit without reaping (`waitid(WNOWAIT)`
on Linux, a kqueue `NOTE_EXIT` on macOS and the BSDs), kills the group while the leader is
still a zombie and its id still reserved, then reaps. This is the load-only flake the L1 record
attributed to `review-source-git`.

Review Campaign `layout-l2` (same Pipeline and reviewers) reported three Findings, all fixed in
the next commit and attested: the supervisor still killed the group after the reap on its
failure paths (the leader now owns the child, every kill checks an unreaped state under one lock,
and reaping is the final operation on every path); a non-UTF-8 child of a declared directory
classified as undeclared (only the first segment under `.af/` must be text now); and a Document
or optimization Task lost the field on replay because it has no `source` input (the observation
is now a typed `af/UndeclaredAfPaths@1` artifact in the revision's provenance, recorded only when
non-empty so a clean tree keeps its revision identity).

The first verification Task, `layout-l2-verify`, failed its `markdownlint` check on
`docs/design/tui.md`, a draft that was sitting untracked in the working tree and that a
`git add -A` had swept into the fix commit; it is untracked again in commit fe3cd31 and left on
disk. `layout-l2-verify-v2` (167,674 tokens) then ended `verified`, and the three fixes carry
positive fix verifications citing it. Lesson for the campaign runner: stage the candidate's
manifest and the hand fixes by path, never the whole working tree.

### L3a

Task `layout-l3a-v3`, plan `sha256:39389336…`, source Snapshot at commit fe3cd31 — the first
package to run the whole Pipeline end to end: implementer (639,992 tokens over the Task), seal,
both checks green on the first Gate, both reviewers, review acceptance and the independent
evaluator. The reviewers and the evaluator agreed on the gaps: the cardinality rule contradicted
itself (a `many` output could bind a `one` port when it happened to hold one artifact), the
exact-artifact form had no `explain`/`show` rendering or delivery diagnostic, re-rooting a
derived `source` never said to publish a new `af/SourceTree@1` envelope for the re-rooted
Snapshot, a parentless generation-2 source referenced again was undecided, and advancing the
inspection document to `@12` missed its one existing consumer, the self-optimizer's history
adapter. All five are settled by hand in ADR-0117 and `docs/task-execution/task-inputs.md`, with
the L3b plan and tests extended to match. Verification Task `layout-l3a-verify-v2` (checks and
the independent evaluator, 75,071 tokens) ended `verified` on commit df0d660.

### L3b

Task `layout-l3b`, plan `sha256:f4084794…`, source Snapshot at commit df0d660. The implementer
completed one Attempt of 1,023,262 chargeable tokens and sealed every deliverable — the
`review-core` binding types, the generation-2 origin and its reader, the `af` resolution module
with re-rooting and the published record, `explain`/`show`/`@12` presentation, the
self-optimizer allowlist, the delivery refusal, four schemas with parity entries, the unit tests
and a three-Task fixture — and found one more correction while building that fixture: a bound
`history` cannot feed a Pipeline that would *continue* the referenced Round, because restoring a
Round recomputes it against reviewer results that must retain the current Task's requirements;
so a bound `history` feeds a Pipeline that reads the ledger, which is the repair case this plan
describes. The Task Gate failed on `cargo fmt` alone, so both reviewers were suppressed. Hand
fixes on the materialized candidate: the formatting, a missing `Debug` derive the new unit tests
needed, and one schema defect the fixture exposed — `af/task-inspection@11` validates execution
records with a `oneOf` over five record generations that a real settled record satisfies more
than once, so `@12` validates them with `anyOf` and says why.

The full `make check` then failed once more on the load-only signature — a fixture's
`git rev-parse` dying with an empty stderr, this time in `review-sandbox` — which pointed at the
one place the supervisor fix had not reached: the provider probes in `crates/af/src/providers.rs`
poll `try_wait` and then end the probe's process group *after* the reap, the same recycled-pid
kill. The supervisor crate now exposes `ExitWatch` and `try_reap_killing_group`, a `try_wait`
that observes the exit without reaping, ends the group, and reaps last; every probe polls
through it, and the PTY probe ends its group only while its leader is still unreaped. A failed
git with no stderr now reports its exit status, so the next such failure reads as a signal or an
exit code rather than as silence. Review Campaign `layout-l3b` (same Pipeline and reviewers) reported seven distinct defects, two
of them raised by both reviewers: an exact-artifact source binding trusted the envelope's
subject over its payload and could re-root a different tree; `af task refresh` rebuilt
provenance from port artifacts and dropped the binding record; the selection helper preserved
every non-port provenance record, which changed the identity of a source-less Task that carries
an `af/UndeclaredAfPaths@1` record and no bindings; the Claude PTY probe still reaped before
ending its group; refusal text echoed unsanitized Task-file labels; the `@12` schema's `anyOf`
no longer tied an execution record's type to its payload generation; and delivery labelled a
re-bound generation-2 source with the first Task instead of the current binding. They are
closed by a repair Task, `layout-l3b-repair`, run through the same Pipeline with the seven
findings and their regression tests as its requirements.

`layout-l3b-repair` (611,867 tokens) fixed all seven at the cited locations with the named
regressions — an issue-refresh fixture, an exact-artifact envelope naming two Snapshots, a
source-less Task's provenance bytes, a PTY leader that exits behind a SIGHUP-ignoring
descendant, hostile labels in refusal text, mismatched type/payload schema pairs, and a
re-bound generation-2 source through delivery — and its Gate failed on `cargo fmt` alone; the
formatted candidate passes every touched suite locally.

Verification Task `layout-l3b-verify` (142,311 tokens) passed its checks and failed on the
evaluator, which read the repaired tree against the requirements and found five more gaps: an
exact-artifact source whose recorded port named no Snapshot slipped through admission (now the
three identities must agree, and an absent one cannot); `--uncommitted` still captured the
invoking checkout before a bound `source` resolved (now the two are refused together, before
any capture); the byte-identity evidence and the self-optimizer `@12` test proved less than
claimed (the fixture now asserts the unbound Task stays at `@9`, and the adapter test reads a
real `af task plan --json` receipt checked in as `fixtures/task-runtime/bound-inputs/
inspection-bound.json`); and ADR-0117 carried the previous day's date. Fixed by hand in commit
bc0c6f3; `layout-l3b-verify-v3` (143,770 tokens) ended `verified`, and the nine ledger entries
of Campaign `layout-l3b` carry positive fix verifications citing it.

On the rebase onto the GA legacy removal, the `@12` inspection generation was folded back into
the single `@11` document (an optional `input_bindings` field), the three ADRs became 0115,
0116 and 0117 behind main's own 0112–0114, and the design note `docs/design/config.md` that L1
extended no longer exists, so `af help config` alone carries the layout table.

### Totals

| Package | Task tokens | Review tokens | Verification tokens |
|---|---:|---:|---:|
| L1 | 538,605 | 337,982 | 78,988 |
| L2 | 615,335 | ~330,000 | 167,674 |
| L3a | 639,992 | in-Task | 75,071 |
| L3b | 1,023,262 | ~340,000 | 142,311 + 143,770 |
| L3b repair | 611,867 | — | — |

Every package was implemented by a Task, reviewed by two independent reviewers, and verified
by an independent evaluator on the commit that ships. Three kernel defects surfaced along the
way and are fixed in this branch: the APFS `openat(O_CREAT)` race on the provider lock files,
the process supervisor's post-reap group kill, and the same kill in the provider probes.
