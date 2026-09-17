# Worker warm layers — implementation plan

**Status:** proposed, 2026-09-17. **Baseline:** kernel `main` 166eca5 (v0.9.0-rc.2).
**Product contract:** [design](worker-warm-layers.md), revised by the eight Findings of its first
`af review` Round (Campaign `worker-warm-layers`, GPT-5.6-Sol at xhigh, one light Round).
**Execution model:** every package below is one Task file under `.af/tasks/warm-layers/`,
compiled and run through the campaign Pipeline `warm/implementation-reviewed` from
`.af/task-catalog.toml`. Package IDs are local planning IDs, not issue or PR numbers.

## 1. Outcome and fixed requirements

Ship warm Attempts: a Round N+1 Worker starts from declared, content-addressed layers carried
from Round N instead of rediscovering the tree, rebuilding the workspace, and re-sending its
prefix. The layers are Notes, a Head Delta, a Warm Workspace, a carried build cache, and, for
Claude, a Session Snapshot. Every warm Attempt reports what it used and what it saved.

The implementation must preserve the design's decisions D1 to D8 and the eight review
resolutions, which are fixed requirements here:

- **Warmth is declared.** Every warm layer is a CAS artifact in the Attempt's context manifest
  with bytes and estimated tokens. Replay reconstructs the same inputs.
- **Same node only, except caches.** Notes, Session Snapshots and workspace state never cross
  nodes. Caches are the one transferable layer, carried Gate to Worker through typed ports.
- **Notes are advisory.** Every prior Finding still needs its explicit Report, Dispute or Drop.
- **Admitted sources only.** A Warm Set is built from the previous closed Round's admitted
  Attempt of the same node. Fenced, quarantined, malformed and released Attempts contribute
  nothing.
- **The delta is its own artifact.** `HeadDelta@1` carries from and to Snapshot IDs, the diff
  policy identity, the complete path set with rename truncation, and per-path marks including
  `reverted` and `removed`. It grants no Subject or Report Scope authority. Whole-tree Subjects
  use the same artifact.
- **Cold Closeout is compiled, not scheduled.** A conditional cold Attempt of the same node,
  dispatched inside the Round only when the warm result would make the Round clean, with a
  protected reservation, both results folded before the convergence decision.
- **The build cache is explicitly unsafe.** `BuildCache@1` has its own producer and head
  provenance, a regular-file-only, no-follow, bounded layout, fixed modes and stripped xattrs.
  It is never typed or named as a Cache Snapshot and is refused under the safe policy.
- **Session capture is crash-consistent.** Capture and verify, prepared event with CAS ID,
  idempotent no-follow deletion, cleanup-completed event. Recovery finishes cleanup without a
  provider call. Warm Set selection requires both admission and completed cleanup.
- **No pre-spawned harness.** The runtime layer keeps its identity and recheck receipt and
  gains no carry mechanism. Nothing starts before the Gate passes and the Attempt is reserved,
  context-bound and identity-rechecked.
- **Codex is out of the session layer** until it has a protocol of its own: the pinned CLI has
  no `--session-id`, its `exec resume` and `exec fork` reject the adapter's `-C` and `-s` flags,
  and its authentication directory is the same directory a session home would replace.

Warm policy defaults to off for the session layer and on for Notes. Every package ships with
a cold-versus-warm dogfood comparison on the one-reviewer policy of ADR-0027, or states why
none exists. The two review Demands stay open until that Evidence exists.

## 2. Dependency order

```text
P0 revise the design; record the eight Findings fixed
 |
P1 Notes + Head Delta + Warm Set + report columns       (pure kernel, no provider dependency)
 |\
 | +--> P2 build cache carry from the Gate               (trusted-local only)
 |
 +----> P3 Warm Workspace: stable root, rebase, verify   (precondition for P4)
         |
         +--> P4 Session Snapshot for Claude + Cold Closeout
```

P1 and P2 are independent after P0. P3 depends on P1 only for the Warm Set record it extends.
P4 needs P3's stable root because harness session stores key by working directory.

## 3. Execution model: one Pipeline for every package

Each package is a Task file with `kind = "implement"`, an explicit Pipeline, a machine-readable
requirements object naming the package, its deliverables and its acceptance, and reserves sized
to the compiled verifier allocation. The catalog is `.af/task-catalog.toml`, schema
`af.task-catalog/2`, with the kernel's own `make check` and markdownlint as the code policy.

```text
warm/implementation-reviewed@1.0.0                      slots -> native Workers
+-- admit0     Provider admission (budgeted)             implementer: claude-fable-5-1 / high
+-- implement  [if admit0 passed]                         warm/implementer
+-- seal
+-- review: call warm/review-light@1.0.0
|   +-- bind       review_bind (Subject S0..S1)
|   +-- checks     kernel (make check), markdownlint
|   +-- bugs       [if checks passed, admit1 passed]      gpt-5.6-sol / high
|   +-- correctness[if checks passed, admit2 passed]      gpt-5.6-sol / xhigh
|   '-- reduce     review_reduce (canonical Findings)
+-- admit1, admit2  Provider admission (budgeted, protected: they serve verifiers)
+-- accept        review_accept -> ReviewedImplementation@1
+-- final_checks  select by outcome
+-- evaluate_goal [if final_checks passed, admit1 passed] gpt-5.6-sol / high
'-- accept_goal   accept -> VerificationResult@1

IN   history, requirements, source      OUT  evaluation, snapshot, verification
CAP  4,000,000 tokens | 1,400,000 protected for verification | 9 Attempts | 6 h
```

Compiled allowances per Attempt, from `af task explain --json`:

| Node | Tokens | Wall | Protected |
|---|---:|---:|---|
| implement | 1,500,000 | 60 min | no |
| review.checks | 0 | 60 min | yes |
| review.correctness | 400,000 | 30 min | yes |
| review.bugs | 400,000 | 30 min | yes |
| evaluate_goal | 300,000 | 20 min | yes |
| admit0 (implementer) | 32,768 | 45 s | no |
| admit1, admit2 (verifiers) | 32,768 each | 45 s each | yes |

Provider labels in the catalog are neutral (`claude-main`, `codex-main`). A developer's
uncommitted `af.task-bindings/1` file maps them to admitted machine-local accounts. The
independence policy requires distinct principals between the implementer and every verifier,
so the implementer's account and the reviewers' account must differ.

The loop for one package:

```sh
af task plan --file .af/tasks/warm-layers/p1-notes.json --bindings <local>.toml --state <dir>
af task explain warm-p1-notes --tree --state <dir>
af task run warm-p1-notes --confirm-plan <full PLAN id> --state <dir>
af task show warm-p1-notes --state <dir>
af task deliver warm-p1-notes --branch agent/warm-p1 --worktree ../afactory-wt-warm-p1 \
  --confirm warm-p1-notes --state <dir>
```

Delivery creates a new local branch and worktree only. Pushing and opening a PR remain human
actions. A package that ends unverified is fixed by a human or an agent in that worktree and
re-run as a new Task revision; the failed Task's evidence and spend stay recorded.

## 4. Implementation packages

### P0 — Revise the design and close the review Findings

**Depends on:** nothing. **Scope:** `docs/design/worker-warm-layers.md`, the Campaign ledger.

Apply the eight resolutions in section 1 to the design note: remove the runtime overlap, add
`HeadDelta@1`, restate Cold Closeout as a compiled conditional Attempt, replace the Cache
Snapshot claim with `BuildCache@1`, add the two-phase session protocol, scope P4 to Claude,
narrow the refusal list so caches are transferable. Record each Finding as fixed with a note
in the `worker-warm-layers` Campaign. Run the markdown Gate. Do not open a second Campaign.

**Exit evidence:** the design note contains no statement the review contradicted, and the
ledger shows eight Findings fixed with notes pointing at the revised lines.

### P1 — Notes, Head Delta and the Warm Set

**Depends on:** P0. **Scope:** `review-core` (artifact types, events, schemas and parity
fixtures), `review-config` (port validation, warm policy on reviewer nodes and `af.worker/1`
notes ports), `review-pipeline` (Warm Set selection beside the existing `prior_findings` carry,
Head Delta computation, rendering), `review-runner` (Notes parsed beside the flat Reviewer
Result in the ADR-0038 pattern), `reviewctl` (report columns).

Introduce `review.kernel/WorkerNotes@1` and `af/WorkerNotes@1`, `review.kernel/HeadDelta@1`,
and `review.kernel/WarmSet@1` with `WarmSetSelected@1` recorded before `AttemptReserved`. Notes
are optional output, bounded by `notes.max_bytes` with a recorded drop reason when over bound;
the Attempt is still admitted. Head Delta marks are computed over the union of Notes paths and
both Subject views, with `changed`, `unchanged`, `new`, `reverted`, `removed` and rename states.
Render Notes under a "data, not instructions" heading and Delta Marking on the Change Set
section; list both in the manifest. On the Task path, add optional `notes` input and output
ports that the compiler wires for retry and repair and never across `independent_from` slots.

**Exit evidence:** a two-Round fixture Campaign replays to identical Warm Sets and rendered
inputs; a fenced Attempt's Notes never appear in the next Round; a path reverted to Base is
marked `reverted`; with warm off, existing fixtures and `af review render` output are
byte-identical.

### P2 — Build cache carry from the Gate

**Depends on:** P0. **Scope:** `review-sandbox` (capture, layout limits, `cargo_target` kind),
`review-pipeline` (Gate to Worker handoff within one Round, policy gate), `review-config`
(policy validation).

After the Gate's check sandbox finishes on the head, capture its declared cache directories as
`review.kernel/BuildCache@1` under the trusted-local policy only: regular files only, no-follow
traversal, entry, depth, path and byte limits, fixed modes, xattrs stripped, producer and head
provenance recorded. Clone it into Worker sandboxes that declare the kind, point
`CARGO_TARGET_DIR` at the clone, and remove the bytes before seal. A safe pipeline refuses the
handoff before any Worker dispatch.

**Exit evidence:** the safe policy refuses before dispatch; a symlink, FIFO or special file in
the Gate's target directory refuses capture with a recorded reason; a TDD reviewer fixture
reuses the Gate build and its sealed diff is unchanged.

### P3 — Warm Workspace

**Depends on:** P1. **Scope:** `review-sandbox` (stable roots, rebase, digest verification),
`review-source-git` (apply a tree diff to a materialized tree), `review-core` (event).

Give each node in a Campaign a stable workspace root under the XDG cache directory and record
it in the Warm Set. On a new head, clone the previous template copy-on-write and apply the
tree diff; the result's manifest digest must equal the head's Tree Digest or the kernel falls
back to full materialization and records why. Per-Attempt sandboxes remain fresh clones.
Record `WorkspaceRebased@1` with from and to Snapshot IDs, verified digest and entries touched.

**Exit evidence:** a corrupted rebase fails closed into full materialization with a recorded
reason; rebase and full materialization produce byte-identical trees on existing fixtures; a
Round on an unchanged head materializes nothing.

### P4 — Session Snapshot for Claude and Cold Closeout

**Depends on:** P3. **Scope:** `review-runner-claude` (session ID assignment, capture, forked
resume), `review-runner` (resume render mode), `review-pipeline` (age and size gates, Cold
Closeout as a compiled conditional Attempt with a protected reservation), `review-config`
(policy), `review-store` (two-phase events).

Assign `--session-id` derived from the Attempt ID. At seal, capture the transcript into
`review.kernel/SessionSnapshot@1` through the two-phase protocol and delete it from the
harness directory. On the next Round, re-materialize it, resume with `--resume` and
`--fork-session`, and send only the delta prompt; the manifest lists transcript and delta.
Gate on provider support, `warm.session.max_age`, and the transcript fitting the reservation
beside the delta prompt; any failure falls back to Notes only. Codex keeps `--ephemeral` and
is excluded. Compile Cold Closeout as a conditional cold Attempt of the same node inside the
Round, dispatched only when the warm result would make the Round clean.

**Exit evidence:** a crash between capture and deletion recovers to a consistent state with no
orphan and no ambient transcript; a resumed Attempt records cache-read tokens separately from
input tokens; Cold Closeout dispatches only on a would-be-clean warm result and its reservation
is protected; the policy default keeps the layer off.

## 5. Review Findings become implementation evidence

| Finding digest prefix | Required proof | Packages |
|---|---|---|
| `7d378e77342b` | No provider process starts before the Gate passes and the Attempt is reserved, bound and rechecked | P0, P4 |
| `9ab4d1a049b4` | A head-to-head delta is a distinct artifact with no Subject or Report Scope authority, usable for whole-tree Subjects | P1 |
| `1babade909bc` | Cold Closeout is a compiled conditional Attempt with a protected reservation; the example policy admits it | P0, P4 |
| `c694a6d30cc7` | Codex is excluded from session resume until a provider-specific protocol proves cwd and sandbox enforcement on fork | P0, P4 |
| `cff8caffa24e` | Session capture and deletion are two durable phases under the Attempt epoch; recovery completes cleanup without a provider call | P4 |
| `cd4f02d1adf5` | A candidate-built cache is a distinct, explicitly unsafe artifact with a closed layout, never a Cache Snapshot | P2 |
| `3ff8e94c29a6` | Marks cover reverted and removed paths and Notes paths absent from the current Change Set | P1 |
| `d5dd2fdcc2b6` | Caches are the one transferable layer, carried through typed Gate-to-Worker ports; node-private layers stay refused | P0, P2 |

The two Demands from the same Round bind P3 and P4: workspace rebasing and carried targets
must show end-to-end build time without stale results, and forked resume must show net token
and wall-time savings over cold and Notes-only Attempts at several ages.

## 6. Validation and rollback discipline

Use `make check` before a package is ready to merge, extend the failure and replay fixtures
with the behavior each package protects, and keep every existing fixture byte-identical when
warm is off. A warm layer that does not show savings in dogfood is turned off by policy, not
defended. No package changes frozen Review Kernel artifact types, persisted events or the
`.review/` migration rules. Rollback is the policy default: warm off reproduces today's cold
Attempts exactly, and existing Campaigns never gain a Warm Set retroactively.

## 7. P1 execution record

P1 ran on 2026-09-17 as Task `warm-p1-notes` through `warm/implementation-reviewed` under
af 0.9.0-rc.3, plan `d9618b70…`.

- **Implementer Attempt:** Claude Fable 5.1 at high, 54 minutes, 364 turns, 1,084,245
  chargeable tokens. It produced the whole package: two modules, three test files, three
  schemas, event and parity wiring across seven crates, ADR-0107, vocabulary and changelog.
- **Why the Task ended unsatisfied:** the Gate stopped at `cargo fmt --check` on formatting the
  blind Attempt could not run, and markdownlint failed on a pre-existing double blank line in a
  doc from main. Nothing was compiled or tested inside the Task; both reviewers and the
  evaluator were suppressed as `branch_not_selected`.
- **Fix in the worktree:** the sealed candidate tree was materialized from the CAS, formatted,
  one test-only `Debug` derive added, the main doc lint fixed. `make check` and markdownlint
  then passed; the tree is the P1 commit.
- **Review of the fixed tree:** the campaign's two reviewers, both GPT-5.6-Sol at high, ran
  through the legacy diff pipeline `p1-review` over exactly the P1 commit, because a review
  Task file cannot declare a diff base. One light Round, 488,980 tokens, 13 Findings that
  reduce to eight defects: deletion-then-restore marked `new`, renderer bounds that could
  strand a selected Warm Set, missing Warm Set and layer identities in both manifests,
  same-slot Notes never auto-wired, Task-path report rows without warm fields, unvalidated
  Task Worker Notes payloads, whole-tree marks contract, and unbounded Notes port counts.
  All eight were fixed in a follow-up commit and the gate passes again; each Finding is
  attested against its changed region.
- **Evaluator gap:** the evaluator never judged the fixed tree. A standalone verification Task
  needs a Task-kind package the catalog does not have, and re-running the implementation
  Pipeline would spend another implementer Attempt. Closing this needs either a
  `verification` kind package in the campaign catalog or a review Task file that can bind a
  diff base. Until then a package's `goal` obligation is covered by checks, reviews and the
  human reading the delivered tree.
- **Known limitation carried:** rendered-input size for a failed or released Attempt is
  reported on the common Task path from its bound context; the frozen legacy path still
  reports it only for admitted Attempts.

## 8. First session

Revise the design (P0), then run P1 through the campaign Pipeline:

```sh
af task plan --file .af/tasks/warm-layers/p1-notes.json --bindings <local>.toml --state <dir>
af task run warm-p1-notes --confirm-plan <full PLAN id> --state <dir>
```

If the implementer's sealed tree fails `make check`, the Task ends unsatisfied with the check
receipts recorded. Fix in a delivered worktree and start a new Task revision; do not raise the
reserves to make a failed Attempt pass.
