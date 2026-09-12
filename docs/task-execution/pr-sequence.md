# Task increment: three-PR delivery

The owner authorized the complete P00–P14 plan and three PRs on 2026-09-11. The original
package numbers remain the acceptance checklist; they are not separate PRs.

| PR | Repository | Scope | Required demonstration |
|---|---|---|---|
| 1 | afactory | P00–P05 and standalone P06 runtime: contracts, compiler, Store, approval guard, implementation cutover and Task-file review | Local implementation and standalone review use the same scheduler and durable Attempt path |
| 2 | afactory | Legacy P06 review cutover, P07–P13 and kernel P14: packages, embedding, repair, selection, generation, authenticated approval, export, starters, Jira/document and release preparation | Implement a ticket with embedded review; generate, approve, export and reuse a no-fit plan |
| 3 | afactory-hub | Accepted design, examples, tracking and P14 consumer migration | Supported released binary and exact consumer lock validate together |

PR 2 depends on PR 1. PR 3's active release pin moves only after the corresponding release
exists with verified checksums. The owner requested PR delivery; release merging remains the
final publication decision. A missing released asset cannot be replaced by a development build
or an invented digest.

The legacy Review entry-point conversion is grouped with PR 2's final composition and repair
semantics. PR 1 already runs standalone Review Task files through the common runtime; it retains
the original runner for existing Review format entry points. This boundary changes neither the
full-plan acceptance checklist nor the requirement to complete Review migration before release.

Each PR receives one light external `af review` Round: Fable 5.1/high on `claude-personal`
for correctness and architecture, Opus 5/xhigh on `claude-personal` for performance, and
GPT-5.6-Sol/high on `codex-personal` for bug bounty. These replace the previous P01 reviewer
configuration for future PRs; the historical P01 Campaign is unchanged. Fix concrete Findings
and run the deterministic gate without starting another Campaign to obtain a clean verdict.

Baseline: `b8b8963`, the latest observed kernel main. Its change since the recorded P00
baseline is documentation only. P01 fixes remain in `c286e18` and retain their test evidence.
Default integration fixtures use command Workers. The live pilot requires its own bounded
budget before inference; deterministic fixtures do not substantiate a live-model product claim.

PR 1 preflight at `fbcffa3` measured 310,029–310,166 input tokens per specialist for the complete
diff. Its review policy therefore reserves 400,000 tokens per Attempt and 1,400,000 for the
Campaign, including admission and bounded failure overhead. This is one light Round; the input
measurement does not claim that any model inference has already run.

PR 1 is draft [#75](https://github.com/PakhomovAlexander/afactory/pull/75). Its first CI run
passed live container probes but exhausted deadline headroom in the Provider account-change
fixture before any Attempt. The fix computes executable identity once per process and runs the
SHA-2 dependency at production optimization in debug builds. Task deadlines and the fixture's
verifier reserve remain unchanged. The corrected local gate passes 732 tests, with 15 ignored,
plus Clippy, formatting and frozen synthetic reproduction. The external Campaign remains
paused for `claude-personal` login at its captured candidate `456779b`; no reviewer has run.

PR 2 is draft [#76](https://github.com/PakhomovAlexander/afactory/pull/76), stacked on PR 1.
Its sharing checkpoint at `0aa8aaf` passed 740 local tests, formatting, Clippy and frozen
reproduction. CI passed the complete Check step and live container probes, then ran out of disk
while linking the additional CLI smoke binary. CI now keeps line-level debug information and
disables incremental object caches; the smoke command and all checks remain intact.

The P09 checkpoint adds bounded repair, exact current-S2 per-Finding decisions, a distinct
repair-allowed acceptance type and captured project permission. The repair fixture preserves
one original Round and seven parent Attempts, including five protected verification Attempts.
Heavy history continuation remains part of the full-plan checklist.

The bounded-repair checkpoint passes the complete gate: **745 tests, zero failures, 15 ignored**
across 99 suites, plus formatting, Clippy, frozen reproduction and Markdown checks. Claude
personal authentication remains unavailable; no PR 2 reviewer has run.

An additional real-process interruption test kills the CLI during a started fix-verifier
Attempt, waits for its fenced lease to expire, and resumes the same continuation. The original
Round and attestation IDs remain identical; the lost Attempt remains charged (eight total).

CI for `2c37b00` passed Check, CLI smoke and live container probes in run `34633787737`.
The P10 working checkpoint adds deterministic selection with preserved request/decision
provenance, automatic trusted ranking, explicit fallback and captured-root resume. Selection
refusals distinguish unknown facts, ambiguity, budget, capabilities and semantic no-fit.
The complete gate passes **754 tests, zero failures, 15 ignored**, across 101 suites, with
formatting, Clippy and frozen reproduction. The additional critical-path deadline check passes
the real CLI selection matrix; final Clippy and Markdown checks also pass.

CI for `bdb7ac2` passed container probes but the interruption observer failed to catch the
started fix-verifier boundary. The observer now reads only newly appended durable events,
kills the CLI immediately on the target Started record, and validates the full projection
then. Its wait uses the admitted Task deadline; Worker deadlines, the Task budget, original
Round identity and eight-Attempt recovery assertions remain intact. The targeted crash test
passes locally with this observer.

CI for `0feeefc` passes Check, CLI smoke and live container probes in run `34638391894`.
The P11 working checkpoint adds bounded generation, typed compiler repair, signed developer
approval/rejection/revocation and an atomic planning-to-execution barrier on the common ledger.
Targeted CLI tests prove nested generation, the approval pause, captured replay, bounded failure
and refusal when earlier planning leaves insufficient resources. Store tests retain paid Planner
charges and late usage while rejecting changed business inputs, limits and predecessor identity.
The full gate passes **772 tests, zero failures, 15 ignored**, across 103 suites, plus formatting,
Clippy and byte-identical frozen reproduction. Full binding-independence errors participate in
bounded compiler repair. The generated implementation fixture embeds Review and adds only the
admitted empty-history constructor before exact approval. Claude personal remains signed out;
external PR reviews are paused, and no PR 2 reviewer has run.

CI for `a52b804` passes Check, CLI smoke and live container probes in run `34644327889`.
The P12 export checkpoint has passing targeted proof for nested generated-definition export,
unchanged originating approval state, relocated bundle pins, exact contract fixtures and real
second-developer execution with three Attempts and zero Planner calls. Default shared Workers
replace local overrides in the exported closure; preparation, embedded Task state and unsafe
destinations are refused. The full export gate passes **775 tests, zero failures, 15 ignored**, across 103 suites,
plus formatting, Clippy, frozen reproduction and Markdown checks. The starter pack remains
outstanding. Claude personal is still signed out; no PR 2 reviewer has run.

CI for `9800756` passes Check, CLI smoke and live container probes in run `34646435520`.
The document checkpoint adds typed source/draft/document contracts, a captured document policy,
data-only environments, source/content checks and exact-artifact independent acceptance through
the common runtime. The token-free starter factory emits actual package pins and contract fixtures.
Real CLI tests cover captured replay, Markdown output without overwrite, failed checks, unsafe
links, stale evaluation, a negative verdict and an unavailable verifier. The full gate passes
**779 tests, zero failures, 15 ignored**, across 104 suites, plus formatting, Clippy and frozen
reproduction. The remaining starters, Jira adapter, Review migration and release evidence are
still required; no external PR 2 reviewer has run.

The heavy continuation checkpoint adds a typed, recomputed bridge from independent S2 repair
receipts into the next admitted discovery Round. It preserves the original S1 Round and claim
identity; full S2 acceptance still requires all reviewers and current checks. It works with
targeted acceptance disabled. Negative, unavailable, stale and rediscovered claims prevent
acceptance, while a later changed Subject reopens previously fixed Task claims. The complete
gate passes **783 tests, zero failures, 15 ignored**, across 105 suites, plus formatting, Clippy,
frozen reproduction and Markdown checks. Starter packaging and legacy Review CLI migration
remain separate work; no external PR 2 reviewer has run.

CI for `1f0214f` passes Check, CLI smoke and live container probes in run `34652351563`.
The P12 starter checkpoint emits software, planning and combined catalogs from supported typed
definitions with actual package pins. The tutorial executes implementation, standalone and embedded
Review, both repair contracts and document Tasks without credentials. Generated execution waits
for a signed decision; a second checkout reuses the exported plan with zero Planner calls.
Structured tutorial requirements keep private Task prose out of shared Worker code; the original
export privacy regression remains unchanged and passes. The complete gate passes **786 tests,
zero failures, 15 ignored**, across 106 suites, plus formatting, Clippy and frozen reproduction.
Markdown checks pass. Jira revision capture, legacy Review migration and P14 remain required.
Claude personal is still signed out; no external PR 2 reviewer has run.

The P13 source checkpoint captures exact local JSON/TOML or read-only Jira fields into normalized
Task input before selection. Raw response, field values and normalized text retain distinct
identities; unsupported content and missing selected requirements fail explicitly. Native source
transport keeps credentials off argv and Worker inputs, bounds response bytes and time, and
cancels through the shared process supervisor. The source substitution fixtures and real CLI
issue-to-embedded-Review-to-delivery flow pass. The complete gate passes **793 tests, zero failures,
15 ignored**, across 111 suites, plus formatting, Clippy and frozen reproduction. Markdown checks
pass. No live Jira request or model call ran; source refresh remains the next P13 checkpoint.

CI for `7cd7662` passes Check, CLI smoke and live container probes in run `34656763527`.
The refresh checkpoint adds one atomic issue-revision/plan barrier, remaining-capacity selection
and shared lease renewal. Targeted CLI tests retain the original S0 and allowance after completed
work, reuse generated definitions with fresh signed approval and wait when remaining Attempts
are insufficient. Store and budget tests retain failed/late usage and refuse authority changes.
The full refresh and result-scoped delivery gate passes **802 tests, zero failures, 15 ignored**,
across 112 suites, plus formatting, Clippy and frozen reproduction. Delivery recovery retains the
exact result identity; a pending delivery blocks refresh until reconciled. Both revisions can be
delivered independently without changing earlier worktrees. No external PR 2 reviewer has run.

The requirements-acceptance checkpoint adds an independent `goal` obligation alongside Review
on the selected final Snapshot. Fifteen targeted CLI cases pass across goal refusal, heavy Review,
issue capture, refresh, repair and starters; eight prior Task-file cases also pass. Native Planner
capacity tests cover command and model bindings without duplicating Provider admission. The
complete gate, including exact-requirements provenance and standalone issue Review, passes
**804 tests, zero failures, 15 ignored**, across 113 suites, plus formatting, Clippy and
byte-identical frozen reproduction. Markdown checks pass. Earlier verification caught stale
fixture counts for the additional protected evaluator; those assertions now include its cost.
An earlier sandbox-cache test also observed an empty-stderr Git failure; its exact binary and
both subsequent full runs passed that suite without changing its code or limits.

CI for the preceding refresh checkpoint at `3f467c7` passes Check, CLI smoke and live container
probes in run `34660902042`. Legacy Review migration and the remaining P14 evidence are still
required. Claude personal remains signed out; no PR 2 reviewer has run.

The legacy Review migration now has a shared connection boundary for the Task runtime and
Review domain evidence. Exact reviewer input resolution and sealed Proposal preparation are
separated from execution accounting. All 68 pipeline tests pass after the Store/input
extraction; targeted Proposal, Integration and Task-runtime cases also pass after the output
extraction, with Clippy. The new integration case observes the real Started barrier and writes
through the same Store before settlement, then replays with one Attempt. The complete local gate passes **805 tests, zero failures,
15 ignored**, across 113 suites, plus formatting, Clippy and frozen reproduction. The legacy
CLI still uses its historical execution owner at this checkpoint; converting
that owner and its bounded Scatter/continuation semantics remains required.

CI for `4acc628` passes container probes but both heavy-review cases reach the unchanged
90-second Task deadline after ten Attempts, before the final goal evaluator can start (run
`34663288212`). The local full gate passes those cases. Repeated captured-plan validation is
being investigated; no Task deadline, verifier reserve or acceptance assertion has been relaxed.

The correction in `b4b1bd6` freezes the compiler borrow and reuses only structural validation,
while rehashing captured authority on every use. Store operations reuse their fresh projection
inside one operation and revalidate publication after domain callbacks, including replay
([ADR-0064](../adr/0064-reuse-structural-validation-with-fresh-authority-checks.md)).
All 807 executable tests pass, with zero failures and 15 ignored; formatting and Clippy pass.
The first documentation-test invocation saw in-progress source from the next checkpoint against
the earlier compiled dependencies. Restoring the exact committed source and rerunning documentation
tests plus frozen reproduction passes. Combined gate coverage is 113 suites. No limits or
assertions were relaxed. The same local heavy positive command fixture decreased from 35.14s
to 25.89s; this is not a live-model benchmark. Linux CI for `facbfb4` passes Check,
CLI smoke and container probes in run `34666567019`.

The next P06 boundary persists a typed report for every common scheduler run, including
pre-Attempt context failures. A trusted idempotent domain-publication hook runs after Task
settlement/publication and before downstream dispatch. Lost acknowledgement leaves durable
diagnostics and a recoverable waiting Task; reopening reuses the same output and paid Attempt
([ADR-0065](../adr/0065-persist-task-run-diagnostics-and-recover-domain-publication.md)).
Schema parity, all 21 Task Store tests, all 70 pipeline tests (one opt-in probe ignored),
17 representative CLI tests, formatting, Clippy and Markdown pass. The complete gate at
`d0d4ad0` passes **811 tests, zero failures, 15 ignored**, across 113 suites, including
documentation tests and byte-identical frozen reproduction.
Legacy Review still requires the actual reservation/context and domain-selected-evidence
adapters; these recovery primitives alone do not complete its cutover.

The next Review preparation boundary reserves the real common Attempt before pure context
capture and binds admitted context before start ([ADR-0066](../adr/0066-reserve-task-attempts-before-binding-exact-context.md)).
Legacy combined preparation records remain readable. Runtime and Store tests reject changed
identity and unbound starts, release unstarted failures, fence old writers and preserve paid
replay. The complete gate at `7ba2338` (the same code as stacked `2d8421e`) passes
**814 tests, zero failures, 15 ignored**, across 113 suites, plus formatting, Clippy,
documentation tests and byte-identical frozen reproduction. Markdown passes.
The preceding report checkpoint `c45a55b` passes Check, CLI smoke and container probes in
CI run `34668202469`. Legacy Review conversion remains required.

The next compatibility extraction isolates one adapter invocation, exact Attempt-context binding,
and sealed canonical result/provenance capture from legacy scheduling and accounting. The typed
`TaskReviewResultMetadata@1` receipt retains result identities and explicit Proposal disposition.
A common Store currentness check refuses unstarted, revoked and settled work. Metadata schema
parity, all three reservation/currentness tests, and all 71 pipeline tests pass (one opt-in probe
ignored), including Proposal, broker, Scatter and replay coverage. The complete workspace gate
at `d63d8b2` passes **816 tests, zero failures, 15 ignored**, across 113 suites, including
documentation tests and byte-identical frozen reproduction. Formatting, Clippy and Markdown pass. The [compatibility map](review-compatibility.md) records the
remaining selected-evidence, bounded Scatter and Round-continuation wiring.

The next Store checkpoint projects a selected and published common Task result into canonical
Review through `TaskReviewResultSelected@1`. A typed admitted context fixes routing and exact
inputs; the Store compares Task and Review prefixes in one transaction. Receipt and Proposal
guards consume the checked selection without adding a legacy Attempt or charge. Two Store
regressions cover publication order, revocation, replay, forged append, changed routing or
metadata, Proposal disposition and competing-Store comparisons. All 60 schema parity tests pass.
All 146 Store tests (two opt-in probes ignored), all 71 pipeline tests (one opt-in probe ignored),
workspace Clippy, formatting and Markdown pass. The full workspace gate at `2445f8f` (stacked as
`f54d16a`) passes **819 tests, zero failures, 15 ignored**, across 113 suites, including
documentation tests and byte-identical frozen reproduction. The preceding operation checkpoint
`63aa222` passes Check, CLI smoke and container probes in CI run `34670847456`. The legacy entry-point
and broker adapters, owned Scatter and original-allowance Round continuation remain required.

The common usage checkpoint commits cumulative Provider observations during a started Attempt,
retains the remaining reservation, and blocks further effects on overrun. Lower terminal reports
and writer-loss recovery preserve paid evidence without changing original settlement bytes.
Scope, reservation, replay and recovery regressions pass. The full gate at `53ce07c` (stacked as
`4349baa`) passes **823 tests, zero failures, 15 ignored**, across 113 suites, plus formatting,
Clippy, documentation tests and byte-identical frozen reproduction. Markdown passes. Canonical
Review selection at `96d4061` passes Check, CLI smoke and container probes in CI run
`34672300202`. The actual legacy frontend/broker, Scatter and Round-continuation cutover remain.

The in-flight usage checkpoint at `166bc45` passed CI run `34674082508`: Check, CLI smoke and
container probes. The next installed frontend checkpoint compiles explicit Review contracts,
original-node mappings, typed fan-in lanes, inherited Gate conditions and shared Provider guards.
It preserves flat and enveloped artifact identities and captures an exact Round before Generation.
The compiler/graph suites pass 123 tests, zero failures; the new schema and real-Store capture
checks pass, and workspace Clippy passes. The complete gate at `7013f8d` (stacked as `eb9d808`)
passes **834 tests, zero failures, 15 ignored**, across 114 suites, plus formatting, Clippy,
documentation tests and byte-identical frozen reproduction. Markdown passes. This does not yet
cut over the legacy CLI or execute Scatter through the common runtime. ADR-0069 records the
boundary.

The next compatibility checkpoint extracts canonical Review operations and publication into
shared domain state while retaining the original legacy execution owner. Task dispatch is
fenced by the exact current Review Round, including inside the SQLite write comparison, while
late usage and settlement remain durable. A common invocation-publication hook recovers before
any Attempt/context capture. The first Store/pipeline compatibility run passes 222 tests with
zero failures and three ignored across 26 suites; four targeted Round-boundary tests and three
publication/context tests pass after the final additions. The complete gate at `9a81fbe`
(stacked operations at `a9193a8` and interpreter correction at `5b1461e`) passes **839 tests,
zero failures, 15 ignored**, across 114 suites, plus formatting, Clippy, documentation tests
and byte-identical frozen reproduction. The first full run failed because Apple's test Python
shim added SDK environment variables; resolving the actual interpreter before isolation keeps
all original transport assertions and fixes the fixture. The captured frontend at `50e9e29`
also passes CI run `34676584751`: Check, CLI smoke and container probes.
Captured plan admission, actual host/CLI wiring and the previously listed cutover gates remain.

Captured Review loading is now shared with the legacy CLI. Both recorded authority layouts,
light/heavy convergence, Snapshot reachability and strict package/policy checks are retained.
Two direct loader regressions and all 19 Campaign lifecycle tests pass; workspace Clippy passes.
The next common budget checkpoint adds named aggregate token scopes, protected verification
inside each scope, retained scope authority across graph replacement and late usage charged
to every original scope. The Attempt/graph/Store regression run passes 214 tests, zero failures,
two ignored; all seven final scope cases and the real Store reopen/refused-retry case pass.
Workspace Clippy and changed-document Markdown pass. A complete gate follows. These changes
support captured executable-plan admission; they do not yet connect the legacy CLI to Task
execution. ADR-0071 records the shared authority and scope-lifetime boundaries.

The captured-authority/scoped-budget full gate at internal `8af1b2a` stopped at the Codex
500 ms usage-retention fixture. Concurrent repetition localized the empty capture to a fake
executable that had not reached its first instruction. A bounded, empty fixture-readiness
branch now prepares that path before the unchanged measured invocation; 32 repetitions with
four test binaries in flight pass all original assertions.

A separate supervisor correction retains already-read output for non-timeout failures,
including typed stdin failure, read failure and held stdout. Both native adapters keep exact
`u64::MAX` usage while refusing the message. The focused process/adapter gate passes 80 tests
with one ignored; Store, pipeline, source and check regressions pass 302 with four ignored.
These are targeted results, not a replacement for the interrupted full gate
([ADR-0072](../adr/0072-retain-process-output-independently-of-transport-status.md)).

The Store also checks captured retry eligibility before either reservation API, refuses
concurrent pending Attempts and recovers a previously selected output without paying again.
The captured Worker, Provider and planning wrappers forward the domain decision. The actual
captured Review failure-class policy remains part of its executable adapter
([ADR-0073](../adr/0073-check-task-retry-eligibility-before-reservation.md)).

The final code sweep found a held-pipe drain race in that checkpoint: a reader could append
its last chunk after the collector had already taken the buffer. The follow-up retains the
drain through bounded post-kill completion, sharing one cleanup deadline between streams and
preserving the original failure status. A delayed-prefix regression covers stdout and stderr.

The full gate at `aa6ca7a` was explicitly stopped after the code sweep found that race, before
all CLI tests, documentation tests and reproduction completed. It is not a passing full gate.
The shared Review domain now also owns canonical RunReport publication with execution-owner
spend supplied explicitly, including the legacy uncapped `None` value. Its report algorithm is
unchanged; the legacy wrapper retains post-report automatic Integration. Verification of this
extraction and the drain fix is recorded separately from the interrupted run.

The corrected drain/report checkpoint passes 155 process, native-adapter and pipeline tests
with two ignored, all 19 Campaign lifecycle tests, workspace Clippy and Markdown checks. The
complete gate is being restarted; the last completed full gate remains the recorded 839-test
checkpoint. Lossless persistence of usage beyond safe JSON/SQLite integers remains a separate
common Task conformance correction; native adapter retention alone does not complete it.

The `e81457e` full gate stopped in the model environment-isolation fixture: an otherwise
completed child was reported as retaining stdout. Whole-suite repetition reproduced a separate
killed-child drain delay. Inspection of pinned Rust 1.88 localized the sibling-pipe inheritance
window to macOS's non-atomic pipe/close-on-exec setup. Shared buffered and duplex launch now
serialize child creation on Apple platforms, leaving execution and drains concurrent. The
unchanged model-supervision suite passes 20 consecutive runs; a new 12-worker process fixture
passes 24 concurrent batches and preserves bounded execution. Focused Clippy passes. The failed
full run remains recorded; another complete gate is required for this correction.

The process-isolation checkpoint at product `d7a65ac` (identical tree at frozen `e649654`)
passes the complete local gate: **864 tests, zero failures, 15 ignored** across 119 suites,
plus formatting, Clippy, documentation tests and byte-identical reproduction. The product
commit is pushed; exact-head CI run `34684161093` is being monitored. Its first watcher
attached before the new jobs appeared, so those initial successes are not this commit's
CI evidence. The current watcher attached to both new pending jobs.

The exact-usage checkpoint adds decimal Task usage and version-2 accounting records while
retaining original version-1 receipts. One widened aggregate ledger retains all scopes and
sibling reservations, including historical spend followed by `u64::MAX`. The common runtime
persists typed Worker and Provider usage before output publication; a monotonic TEXT sidecar
survives writer loss without SQLite numeric coercion. Task inspection/list versions carry
exact strings in JSON and text. The numeric-only pre-execution selection-refusal contract
remains unchanged because it records no paid execution.

Core, budget, Store, pipeline and runner regressions pass **405 tests, zero failures, six
ignored**, across 44 suites. Campaign lifecycle, Task-file, implementation and source-refresh
CLI tests pass; the corrected selection suite and both native-model CLI cases pass separately.
The new fake-native failure proves exact wide usage in run, show and JSON/text list output,
with one Provider call and an incomplete result. Workspace Clippy passes. This checkpoint's
full gate follows; no external PR specialist has run. ADR-0074 separately records process
creation isolation, and ADR-0075 records the exact usage representation and recovery boundary.

The exact-usage checkpoint at product `6db8633` (identical frozen tree `6d88f4b`) passes the
complete local gate: **876 tests, zero failures, 15 ignored**, across 119 suites, plus formatting,
Clippy, documentation tests and byte-identical reproduction. The earlier `d7a65ac` Linux CI run
`34684161093` passed container probes but failed the repair interruption fixture: recovery
completed fix verification, then the original Task deadline expired before goal evaluation.
That failure remains recorded rather than being replaced with the local result.

A sampled local run exposed repeated typed-envelope decoding and identity checks in Task
projection. The typed CAS reader now returns one fully verified envelope per read, preserving
fresh integrity, exact type/version and domain checks (ADR-0076). Store and configuration
regressions pass **264 tests, zero failures, two ignored** across 20 suites; all five repair CLI
tests pass with the original deadline and recovery assertions. The full gate for this change
and the captured resource compiler remains required; no external PR specialist has run.

The captured resource compiler now loads a real persisted Review Round and rederives Worker
reservations/timeouts, bounded retry capacity, parallelism, complete Gate-sequence time and
original numeric-Round/Node/FanOut caps on the common Task ledger. Reopen produces the same
graph; smaller Task limits, mode changes and damaged captured authority are refused. The
resource regressions cover retry charges, absent legacy caps, atomic refusal, multi-check wall
bounds and shard scope membership. Exact executable-policy/plan admission and operation-host
wiring remain outstanding; this compiler output alone cannot authorize execution.

The canonical serializer also copies unchanged UTF-8 spans and compares ASCII keys directly,
retaining the same bytes and UTF-16 ordering for other keys. Every Unicode scalar and mixed
escaped text agree with the independent JSON string encoder. The combined Store/configuration
regressions pass **267 tests, zero failures, two ignored** across 20 suites, the two captured
Review integration tests pass, and workspace Clippy passes. The unchanged interruption fixture
passes in 42.30 seconds locally; a prior sampled run was 46.48 seconds and the typed-read-only
run was 44.39 seconds. These single local measurements do not establish Linux CI or pilot
performance; the complete gate and exact-head CI must still be recorded.

The captured-resource and typed-read checkpoint `7c36468` (identical frozen tree `de86bdf`)
passes the full local gate: **885 tests, zero failures, 15 ignored** across 119 suites,
formatting, Clippy, documentation tests and byte-identical fixture reproduction. Exact-head
CI run `34687039677` is in progress; the preceding Linux deadline failure remains recorded.

Read-only captured-input recompilation now refuses missing or forged wrappers without repairing
CAS. Historical reconstruction checks the exact recorded Round while current dispatch still
requires the active epoch. The compiler derives opaque Ledger encoding from captured finding
identity and Scatter result version from inherited Finding Set contracts. Thirteen configuration
regressions and four real captured-Round integration tests pass; workspace Clippy passes.

A real fixture CAS outage at either Provider or Worker return preserves full reported usage in
SQLite before canonical publication. After real lease expiry, reopening settles the exact
charge once (including `u64::MAX + 7` aggregate usage), preserves the original Task allowance,
and permits no further model invocation or successful Worker output. All twelve common-runtime
tests pass. This does not claim recovery of raw output bytes that never reached CAS. No PR
specialist has run; isolated Claude subscription authentication remains unresolved.

The installed `LegacyReviewPlanCompiler` now prepares and admits exact captured Review plans
through the existing TaskAuthority boundary. Three closed public schemas describe the captured
Task policy, original-file dependency wrappers and effective invocation policy. Original package
IDs/digests remain distinct from the wrapper content identities required by common plan closure.
Every Reviewer metadata receipt, Gate outcome and Scatter result supplements public-output
acceptance coverage. Native runners require matching backend/model/effort and common Provider
admission; binding edits cannot turn them into command Workers or bypass the Round token scope.

Six captured-Review integration tests and all twelve common-runtime tests pass, including real
Store plan admission, reopen, missing-artifact refusal without CAS repair, graph/binding/allowance
forgery, exact native settings and schema closure. The 105-schema parity suite passes 64 tests;
workspace Clippy passes. ProviderTaskDomain now forwards domain invocation publication, and the
existing lost-ack recovery fixtures exercise that wrapper. No specialist or live model was used.
The operation host, canonical Ledger companion outputs, result acceptance/replay, broker and
owned Scatter execution, and same-Task heavy-Round advancement remain unfinished.
