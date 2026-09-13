# Task execution increment

**Status:** implementation in progress, 2026-09-13. Baseline is kernel `5464b38` (0.8.0).
The accepted hub implementation sequence is P00–P14; kernel contracts, fixtures and progress
are canonical here. The hub design is in its `docs/architecture/task-execution.md` and the
delivery plan in `docs/workstreams/task-execution/implementation-plan.md`.

## Fixed product decisions

Task is the durable business abstraction; review is a Task kind. Every Pipeline has public
typed inputs/outputs, and review must compose inside implementation under one Task budget and
history. Shared Pipeline/Worker definitions and locks travel through Git; local bindings remain
captured and cannot weaken mandatory acceptance. Every generated plan requires an authorized
developer's approval before execution. The exact plan, Task revision and authority bind that
approval; model output never provides it.

The engineering value order remains unchanged. Sharability, batteries included and pluggability
supplement it. Claude calls use only `claude-personal`.

## Progress

New legacy Worker captures keep execution identity and wire budgets in a Task/plan-bound
context. Domain-output rejection retains typed retry feedback and exact accounting; internal
Manifest metadata has a separate finite limit. See
[ADR-0095](adr/0095-bind-legacy-task-context-and-retry-output-admission.md).

Task catalog V2 captures an explicit finite Provider admission cost, while V1 keeps its original
4,096-token/45-second allowance. Restoration uses captured authority and existing Task limits;
individual overruns still stop dispatch. See [ADR-0091](adr/0091-capture-explicit-task-provider-admission-costs.md).

The current Product tree `1a49e2d7a55260f23cf76667cebe6199f1e2e210` passes the full Gate
from a read-only Git export with HOME absent: **1,137 tests, zero failures, 15 ignored**
across 133 suites, plus formatting, Clippy and byte-identical fixture reproduction, in
605,202 ms. PR 1's corresponding fixture correction passes 735 tests and its full readonly
Gate. Product `727c0d5` incorporates PR 1 `1228417` without changing the verified Product
tree: the shared helper and equivalent regression already contain the same fix. Copies used
by tests gain only owner-write permission; captured source and executable meaning remain intact.

The earlier Provider-currentness, heartbeat, native usage and recording-recovery checkpoints
remain included. Published `5aa6bd5` passes historical
[CI run 34723107534](https://github.com/PakhomovAlexander/afactory/actions/runs/34723107534),
including container probes. The owner instructed working without CI today; no CI wait, rerun
or billing action is queued. Current local corrections remain unpushed.

Claude personal authentication is confirmed through its configured personal profile, and all
nine first-Campaign admissions passed. Each PR Campaign then stopped at a local Gate before
specialist review. The failures are corrected and verified; renewed review admissions still
await explicit approval. The original three Campaigns retain 20,935 charged tokens and zero
specialist Attempts, Findings or Ledgers. Docker's authorized restart also completed.
Separately budgeted document calibration, live pilot/boundary evidence and supported consumer
migration remain required. Deterministic Gates do not provide live-model performance evidence.

## Earlier verified checkpoints

The following records describe their original checkpoint boundaries; the current result above
supersedes their pending-CI and next-conformance notes. Replaced current-status passages are
retained in the [2026-09-13 status archive](task-execution/status-archive-2026-09-13.txt) and
[pre-readonly-completion archive](task-execution/status-before-readonly-completion-2026-09-13.txt).

Native cancellation and billing-completeness code `0ec8467` matches frozen tree `6a656c62`
and passes the full gate: **1,115 tests, zero failures, 15 ignored** across 132 suites, plus
formatting, Clippy, documentation tests and byte-identical fixtures. Native controlled calls stop
the owned process group and retain usage; incomplete billing preserves original reservations
and known floors through CAS recovery. Valid native reports retain their previous identities.
The Linux stderr fixture now emits and checks its own diagnostic. Its published CI failed the Linux stdin fixture described above;
common TaskRuntime/heartbeat forwarding passes the later full gate. See
[ADR-0087](adr/0087-control-native-task-invocations-through-the-shared-supervisor.md) and
[ADR-0088](adr/0088-retain-native-billing-completeness-with-task-usage.md).

Native conformance checkpoint `582ad8e` exactly matches frozen tree `d5943472` and passes
**1,093 tests, zero failures, 15 ignored** across 129 suites, plus formatting, Clippy,
documentation tests and byte-identical fixture reproduction. Native multi-turn usage retains
exact components and charges through failed output, CAS outage, timeout and reopened inspection.
Codex final-message reads are bounded and refuse symlinks and nonregular files. Expired Review
publication recovery pins the exact failed-report output prefix, executes no new work and cannot
mark the Task Satisfied. Fresh inspection@8 preserves raw history and unchanged Store bytes.
Published `0a95a94` failed Check in CI `34717558032` because the native-output fixture
expected stderr that it did not emit; container-probes passed. The next checkpoint corrects
that fixture without changing production output limits.
See [exact native usage](adr/0085-retain-exact-native-task-usage-across-multiple-turns.md) and
[recording recovery](adr/0086-record-expired-review-publication-without-restarting-work.md).

The installed Review CLI checkpoint `fdffb1c` passed its full 1,078-test local gate. New Review
and Provider doctor use one common Task, original resources and canonical outcomes; actual CLI
checks cover shared probes, Round continuation, SIGKILL recovery and missing-state refusal.
Published `b2ce782` failed CI Check during a fake-runtime executable setup; container-probes
passed. The new checkpoint contains the fixture-only correction without changing deadlines.
Native cancellation and malformed/missing usage conformance now pass the later local gate.
Caller forwarding passes the later full gate; live Provider and P14 evidence remain in progress. See
[ADR-0084](adr/0084-route-new-review-commands-through-the-common-task.md).

The common Task Broker checkpoint `1a9ab83` matches frozen tree `34fc57e` and passes the
full local gate: **978 tests, zero failures, 15 ignored**, formatting, Clippy, documentation
tests and byte-identical fixture reproduction. Provider readiness and business work use
separate captured policies, credentials and original Attempts. Failed readiness blocks work;
late receipts, panic and CAS failure preserve exact paid usage in the same Task ledger.
CLI show/explain/list verify typed Broker evidence and cumulative charges after reopen.
The published checkpoint also passes CI `34702550379` at documentation head `c55f96e`.
The owned Review checkpoint `d9babab` matches frozen tree `47c73ec` and passes the full gate:
**1,009 tests, zero failures, 15 ignored** across 125 suites, plus formatting, Clippy,
documentation and byte-identical fixture reproduction. Registered slices use the same
scheduler and ledger, and canonical completion recovers without repeating paid work.
The continuation checkpoint `faeb933` matches frozen tree `cff20f3` and passes the full gate:
**1,018 tests, zero failures, 15 ignored** across 125 suites, plus formatting, Clippy,
documentation tests and byte-identical fixture reproduction. Numeric Rounds and input epochs
retain one Task, its original limits and cumulative paid usage; every successor plan requires
its own admission. Fresh CLI inspection preserves handoff history and historical plans.
Post-Round Integration checkpoint `6fe82f0` matches frozen tree `a53e534f` and passes the full
gate: **1,044 tests, zero failures, 15 ignored** across 125 suites, formatting, Clippy,
documentation tests and byte-identical fixtures. Published `f453da1` passes Check and
container-probes in CI `34711466871`. Prepared proposals use
one original Task Attempt for the shared check sequence; Empty/Conflict selections require no
Attempt. A successful commit requires a complete Review of the derived Snapshot within the
same Task. Focused tests cover writer fencing, atomic two-log rollback and late charge after
successful checks. All seven real CLI public-schema tests pass, including original generations
and inspection@7 after Integration and Handoff@2. The later CLI cutover passes its full local gate.
See [ADR-0083](adr/0083-run-post-round-integration-within-the-original-task.md).
The debug-profile correction `97bb1c7` passes the same full gate. It addresses the earlier
CI recovery deadline failure without changing Task limits; published `2cae97c` passes both
checks in CI `34708574515`. The proposed
[owned-child decision](adr/0081-register-owned-review-children-in-the-common-task-runtime.md)
records registration, factual recovery and immutable completion. See
[Review compatibility](task-execution/review-compatibility.md) and
[ADR-0080](adr/0080-bind-broker-evidence-to-the-original-task-attempt.md).

The earlier shared Review operations and Round-fencing checkpoint passes the full gate at `5b1461e`
(internal `9a81fbe`): 839 tests, zero failures, 15 ignored, plus documentation tests and frozen
reproduction. The preceding captured frontend passes CI at `50e9e29`.
The common Review CLI and Provider doctor cutover passes its full local gate.
Historical paid Campaigns retain their captured execution path. The detailed checkpoint history is
in [the three-PR record](task-execution/pr-sequence.md).

- [x] P00: unchanged-source baseline gate and fixture identities recorded below.
- [ ] P01: contracts, schemas, ADR-0046/0047 and fixtures implemented; one review Round
  completed, Findings corrected and final gate passed locally; main integration remains pending.
- [ ] P02/P03: common Store lifecycle, approvals, legacy links and typed compilation are
  implemented and verified locally; PR integration remains pending.
- [ ] P04–P06: common implementation and Review command execution, Task-file CLI and verified
  local delivery pass real-process fixtures. Fixed command implementation now uses the common
  runtime; common Review CLI cutover passes its full local gate; live probes remain.
- [ ] P07–P09: local Worker replacement, Git catalog sync, Task-kind packages, embedded Review
  and bounded repair are implemented. Heavy history continuation passes the full gate.
- [ ] P10: deterministic captured selection and CLI routing pass the full gate; PR integration pending.
- [ ] P11: bounded generation, shared planning accounting and signed developer decisions are
  implemented and pass the full gate; PR integration pending.
- [ ] P12: portable export and contract tests pass the full gate. The complete starter catalog
  passes the full gate, including CLI execution, repair, planning, approval and reuse.
- [ ] P13: document execution and independent acceptance pass the full gate. Read-only issue
  capture and local/Jira source conformance pass the full gate.
  Ticket revision refresh and result-scoped delivery pass the full gate.
  Common Review compatibility is implemented and verified. Live supported-environment evidence,
  external PR review and integration remain required.
- [ ] P14: compatibility, benchmark and consumer release.

The owner authorized the complete plan in [three PRs](task-execution/pr-sequence.md).
[Review report inspection](task-execution/review-report-inspection.md) separates exact current
Task accounting from immutable cumulative report snapshots, including Provider and business Attempts.
The [run diagnostics and recovery boundary](task-execution/run-reports.md) preserve failures
before an Attempt starts and retry domain publication without invoking the Worker again.
[Reservation and context binding](adr/0066-reserve-task-attempts-before-binding-exact-context.md)
let adapters render the actual persisted Attempt identity before execution.
The [Review compatibility map](task-execution/review-compatibility.md) records the extracted
operations and versioned CLI compatibility boundaries.
PR 2's current composition checkpoint is described in [local bindings](task-execution/local-bindings.md)
and [embedded Review](task-execution/embedded-review.md).
The [shared catalog walkthrough](task-execution/shared-catalogs.md) describes explicit Git sync
and immutable Task-kind profiles. The [bounded repair walkthrough](task-execution/bounded-repair.md)
records distinct complete-Review and targeted-fix acceptance guarantees.
The [heavy Review walkthrough](task-execution/heavy-review.md) carries independent repair evidence
into the next discovery Round while preserving original claims and history.
The [selection walkthrough](task-execution/selection.md) explains fallback, automatic ranking and
persisted reasons without Planner calls.
The [generated-plan walkthrough](task-execution/generated-plans.md) describes the fixed Planner,
typed compiler repair, one-budget handoff and exact signed approval before generated execution.
The [export walkthrough](task-execution/export.md) describes portable bundles, static contract
fixtures and second-developer reuse without another Planner call.
The [issue walkthrough](task-execution/issues.md) covers read-only capture, explicit revision refresh and local/Jira bindings.
The [document walkthrough](task-execution/document.md) runs a credential-free release-note Task
with captured sources, protected content checks, independent acceptance and recorded Markdown output.
The [starter walkthrough](task-execution/starters.md) creates software, document, review and repair
definitions plus a reusable Planner from the supported contracts and actual byte pins.
Each PR receives one light Round with Fable 5.1/high on `claude-personal` for correctness
and architecture, Opus 5/xhigh on `claude-personal` for performance, and GPT-5.6-Sol/high on
`codex-personal` for bug bounty. These replace the P01 reviewer selection for future PRs.
Required checks remain Markdownlint and `scripts/verify.sh` (the complete kernel gate).
Policy `452b752` is content-locked using installed `af 0.8.0`; all three requested models
passed bounded Provider preflight (11,377 chargeable tokens, no Gates or reviewer Workers).

PR 1 is in progress. The compiler expands calls, proves branch availability and typed selection,
and checks coverage against final-output lineage. Exact package recompilation rejects edited
graphs, closures and Worker bindings. The common Store persists plans, developer decisions,
invocations, reservations, starts, settlements and selected outputs under fenced writer leases.
Recovery reuses settled successes and retains failed, abandoned and late usage. Legacy read-only
links preserve original history and charge identities without duplicating execution.

Command Workers use the existing scheduler and process runner through `TaskRuntime`. Captured
input/output schemas and exact context manifests govern each Attempt. A document fixture proves
execution, replay and final acceptance. A pagination fixture proves durable candidate capture,
S0-to-S1 lineage and independent checks/evaluation on S1. Failed and unavailable checks feed
typed unsuccessful/inconclusive results without invoking the evaluator. Scheduler guards are
kept outside Worker inputs; runtime caches are kept outside candidate trees.

The Task-file CLI supports planning, running, explanation, inspection and explicit local delivery.
Plans and execution use captured source, packages and checks across process boundaries; editing
live files cannot change a resumed plan. Finished runs do not repeat Attempts. Writer leases
are renewable and explicitly released for immediate plan/run handoff. Delivery uses the existing
checked worktree/recovery implementation with a typed journal in the common Store.

Generic Claude and Codex Task adapters preserve non-review output, raw responses and reported
usage on failure. The captured Worker host checks exact bindings, model/effort and context caps
before dispatch. A deterministic model fixture proves schema-failure retry charges one shared
Task allowance. Retry guidance is a typed failure code tied to its Attempt and contract, not a
copy of the failed response. Native Provider identity capture and charged admission now pass the
Task-file CLI fixtures. Each distinct effective capability runs a fixed internal probe under the
same Task budget. Shared slots reuse it; a failed probe retains usage and dispatches no business
Worker. Token-free planning captures canonical account identity, and account changes on resume
invalidate the recorded binding. See [Model bindings](task-execution/model-bindings.md).

Standalone `af review --file` now executes through the common runtime. Its typed Subject binder
and atomic gather/reducer reuse historical Finding/Demand semantics without a second Store or
allowance. Complete finding-bearing Review Tasks retain `changes_requested` and exit 3; missing
reviewers remain incomplete without authoritative partial sets. Both initial execution and
replay preserve the domain exit. See the [Review Task walkthrough](task-execution/review-task.md).

The heavy continuation checkpoint deterministic gate passed with **783 tests, zero failures and
15 existing opt-in probes ignored**, across 105 suites. Formatting, Clippy and frozen synthetic
reproduction passed.
All eleven implementation/delivery behavior cases now pass through the common Store, with their
original source preserved as a compatibility fixture. Native timeout and storage-failure tests
retain reported usage. The final compatibility gate also covers preserved per-Check process
deadlines inside the aggregate check Attempt. P11 adds the authenticated developer command
boundary and generated execution. Live Provider boundary probes and remaining native Review conformance belong to the remaining
increment. See the
[Task-file walkthrough](task-execution/task-file.md),
[ADR-0048](adr/0048-compile-task-ports-and-fence-developer-plan-decisions.md) and
[ADR-0049](adr/0049-run-task-workers-through-shared-durable-attempts.md).

## P01 contract checkpoint

The additive `review-core::task` contracts cover immutable Task revisions, typed public Pipeline
boundaries, bounded static operator declarations, exact plans and developer decisions, Worker
independence, review history and targeted repair continuation. No new execution is admitted yet;
compiler, Store authority and runtime checks remain P02–P06 work. The implementation branch is
`agent/task-execution`; this checkpoint does not claim integration into main or a release.

The requested three-reviewer light Round completed on `066b392`, using installed `af 0.8.0`
and trusted policy/base `98bb904`. Result: **8 Findings (5 major, 3 minor; one duplicated issue)**
and **1 required identity-test Demand**, `Fail(Exhausted)` at the one-Round cap. All concrete
corrections are implemented locally, including canonical wire ordering, typed round-trip identity,
nonempty result outputs, receipt-derived incomplete precedence, Command-worker independence,
compact negative fixtures, and cached schema validators. The original positive fixture IDs and
frozen review artifacts remain unchanged. See [ADR-0047](adr/0047-preserve-task-wire-identity-and-review-completeness.md).

The [audit and dispositions](task-execution/p01-review.md) include the exact
[Campaign report](task-execution/p01-campaign-report.md), three preliminary Gate failures,
corrections to pre-existing test isolation, and all usage. The report's review wall-clock is
**7m45s** and total chargeable usage is **483,057 tokens**, including **48,969** Provider-admission
tokens across Gate recovery. The original Subject still has eight open Findings and one open
Demand; post-review code/test changes are not current-Subject verification receipts. No follow-up
P01 Campaign ran. The newly authorized PRs receive their own reviews of their complete changes.

Final deterministic gate: **667 tests passed, zero failures, 15 existing opt-in probes ignored**
across 89 suites; formatting, Clippy and synthetic fixture reproduction passed. Markdownlint
checked 96 files with zero errors. Prior candidate gates also passed on read-only archives and
with the exact cleared Gate environment. No model review was repeated after these corrections.

**Current resume:** complete the remaining P14 calibration and live evidence, external PR
reviews, and supported consumer/release migration. Exact native usage, expired-Waiting factual
recovery, heartbeat cancellation, heavy continuation, Jira refresh and result-scoped delivery
are implemented and included in the current 1,137-test source tree and the recorded earlier
Linux CI checkpoint.

Token-free preflights fit all three PRs at their recorded candidate revisions; their exact
limits and evidence are in the [three-PR record](task-execution/pr-sequence.md). Those fits are
not authentication, model usage or review verdicts, and a changed candidate needs a new render.
The original approved 761,856-token Document calibration stopped after its first admission
charged 5,712 tokens against 4,096 reserved, with no assessment or retry. The new source-bound
proposal passes independent preparation checks and awaits approval for 1,990,656 reserved
tokens. The performance pilot has a distinct unapproved budget.
Continue P00–P14 in the approved three PRs; completed deterministic gates do not replace these
remaining release requirements.

## P00 baseline evidence

Before changing tracked source, ran `make check` at `5464b38` with the pinned Rust 1.88.0
toolchain and an isolated external target directory. Result: **655 tests passed, 0 failed,
15 ignored** across 88 reported test suites. Ignored tests are the existing opt-in live model
and environment probes; this baseline does not claim those ran. Formatting and Clippy passed,
and `fixtures/synthetic/generate.sh --check` reproduced all fixture bytes exactly.

The exact baseline commit preserves the historical fixture/test corpus. These SHA-256 values
identify the selected compatibility anchors:

| File | SHA-256 |
|---|---|
| `fixtures/synthetic/MANIFEST.tsv` | `ebe802e244b99d05240a7b073c5c2e70f2334282292507b23aa0418637b30dea` |
| `schemas/reviewer-result-v1-conformance.json` | `fe309a0e8304906bce135afca612f466eb895de0b1177b7bc7f1d43ace7ae9cc` |
| `fixtures/compatibility/task-implement-v0.8.0.rs.fixture` (original regression source) | `21da1453f236d8f09d1ab4b15ce41274e8b785d5b260491d26d407a2ccee5cb7` |

Compatibility obligations include legacy review result parsing and ledger replay, Campaign
authority, incomplete/resumable review, existing Task completion/failure/delivery, consumer
policy versions and CLI exit statuses. Later packages must extend this evidence without
rewriting the frozen corpus or treating a generic Task completion as review approval.
