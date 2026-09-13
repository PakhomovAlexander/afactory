# Task implementation review dispositions — 2026-09-14

Status: specialist results retained and source corrections verified locally. The original
three-specialist Campaign is incomplete. The separate Sol recovery closed with Findings;
neither it nor the deterministic Gates establishes a clean combined review or release.
This record documents source fixes. It does not append Findings, dispositions, Demands
or a new verdict to either immutable Campaign.

## Scope and recorded outcomes

The original PR1 Diff candidate was `1228417d013ef09cce05150cbd374589bc00bf9b`, tree
`03d83ac2c8501a2fdcc2cf37b3d50c98ff66dc7e`, against Base
`b8b8963864616c7ef1b662a53270bf42ebe56d07`. Installed `af 0.8.0`, binary SHA-256
`df83de0de24e80d408bbf4c40ddd42cab6353bb61a0adc4dbf1ddb4d154a56f0`, ran Campaign
`task-pr1-final-c8015f1` under pinned policy `c8015f16191d7152f70c831c5e8494ea987f47a8`.
Round 1/epoch 1 stopped at the Gate with no specialist work. The explicitly authorized
restart kept that Campaign and cumulative spend. In epoch 2, the Gates passed and Fable
5.1/high (`claude-personal`) and Opus 5/xhigh (`claude-personal`) returned selected results.
The Sol request was released after the native client rejected its 1,260,018-character input
against its 1,048,576-character limit. Gather and Ledger were suppressed: the canonical
outcome remains `Incomplete`, with no combined FindingSet or DemandSet.

The separately authorized `task-pr1-sol-recovery-1228417-bounded` used GPT-5.6-Sol/high
on `codex-personal`, policy `41398e3c078920725f6bbcb4b8e58fc4d2669038`, and the same
candidate Snapshot. Its Subject was **WholeTree**, with a changed-path discovery index and
bounded file retrieval; it supplied no canonical Diff, Base contents or Git history.
Its one light Round completed Gather/Ledger and closed `Fail(exhausted)` with seven open
Findings. It did not receive the other specialists' findings or private reasoning.
This recovery is useful current-tree evidence, not equivalent before/after coverage.

PR2 received source ports and PR2-specific regressions; these PR1 results do not review PR2's
additional capabilities. PR2 and Hub PR3 still require their requested specialist reviews.
The later PR1 `c2207ca` and PR2 `10c5302` corrections have not been reviewed by a new model call.

## Exact accounting and time

| Recorded invocation | Outcome | Cumulative Campaign tokens | Outer wall, including cleanup |
|---|---|---:|---:|
| Original Round 1/epoch 1 | Gate-blocked Incomplete | 7,603 | 148,444 ms |
| Original Round 1/epoch 2 | Incomplete; two selected specialists | 1,241,037 | 919,479 ms |
| Separate Sol recovery | Fail(exhausted); seven Findings | 414,318 | 1,136,009 ms |

The original Campaign total includes epoch 1; do not add 7,603 again. The two Campaigns
charged **1,655,355 tokens** against the owner's **1,400,000-token** approval: **255,355 over**.
Their selected specialist Attempts account for 1,635,099 tokens and Provider operations
for 20,256. The recovery charged 412,064 for its specialist plus 2,254 for admission;
its 81,920 reservation was not a provider-enforced consumption ceiling. All further paid
review calls are stopped. This document creates no new live review, benchmark or pilot budget.

| Selected specialist | Attempt | Wall | Charged | Input | Output | Cache read | Cache write | Reasoning |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| Fable correctness/architecture | `3cd724638c8ad929fb02e87c6d` | 595,915 ms | 601,356 | 290 | 42,870 | 4,858,815 | 558,196 | not reported |
| Opus performance | `719b5c6167ebeb23bd339ca76e` | 763,121 ms | 621,679 | 46 | 50,472 | 11,962,477 | 571,161 | not reported |
| Sol recovery | `e6eba19ebf45bfc54a10766771` | 906,137 ms | 412,064 | 8,157,599 | 29,185 | 7,774,720 | 0 | 16,356 |

The released original Sol Attempt `c64fa9fecb7673b24c5f859176` recorded 800 ms and no usage
observation. Its reservation was released and it contributed no recorded charge; absent
usage is not rewritten here as a measured provider zero. Selected context totals were
2,520,884 rendered bytes / 630,222 estimated tokens in the original outcome and 13,444 bytes /
3,361 estimated tokens in recovery. These are initial-context observations, not total native
retrieval or billed-token ceilings. Provider counters retain their provider-specific meaning;
cache and reasoning fields must not be added again to the recorded charge.

| Provider operation | Attempt | Charged | Wall |
|---|---|---:|---:|
| Original epoch 1, Codex/Sol | `a3c3cf981689935abd506c788a` | 5,710 | 7,434 ms |
| Original epoch 1, Claude/Fable | `b080e2b022bbbc9eb1bf16b46e` | 480 | 5,329 ms |
| Original epoch 1, Claude/Opus | `480d41e72b27c12bb6f377ac74` | 1,413 | 2,731 ms |
| Original epoch 2, Codex/Sol | `8a1f3e4dfb5a9da5e40eabd66c` | 5,710 | 7,709 ms |
| Original epoch 2, Claude/Fable | `03fd947e017798852d2b2a1539` | 3,276 | 3,827 ms |
| Original epoch 2, Claude/Opus | `a1969a60a26bf12e021ba5f12a` | 1,413 | 3,489 ms |
| Sol recovery, Codex/Sol | `e3f7dd5259ffba3f80583c7ae9` | 2,254 | 7,822 ms |

The three retained outer invocations total 2,203,932 ms (36m43.932s). This excludes operator
waiting, source fixes, preparation and deterministic Gates run separately. Parallel Attempt
walls are not summed to claim Campaign elapsed time. Every recorded supervisor cleanup
confirmed its root reaped and no remaining observed owned process.

## Local dispositions

Fable and Opus rows below are ordinal references within their selected partial results,
not invented canonical Finding IDs. Sol rows use the actual recovery Finding ID prefix.
No canonical Finding was marked rejected, wontfix, fixed or verified by these source edits:
all seven Sol Findings remain open in the original Ledger. The original Fable/Opus outputs
remain partial evidence. No PR1 finding is dismissed wholesale as a false positive.

### Fable — two minor reports

| Report | Local disposition and scope |
|---|---|
| C1 — domain-rejected output loses retry and feedback | Accepted; implemented in PR1 and PR2. Durable domain rejection supplies bounded typed retry feedback and retains charged usage. Only an eligible original Attempt may retry; lost authority, lease, storage and budget errors remain terminal. [ADR-0095](../adr/0095-bind-legacy-task-context-and-retry-output-admission.md). |
| C2 — legacy execution metadata hidden in Requirements | Accepted with impact qualification; implemented in PR1 and PR2. New legacy captures bind invocation metadata to Task/plan authority in Context2; old contexts retain their bytes. Rendering failure itself occurs before Worker dispatch, so a claim of a paid model call on that path is unsupported. [ADR-0095](../adr/0095-bind-legacy-task-context-and-retry-output-admission.md). |

### Opus — four major and three minor reports

| Report | Local disposition and remaining limit |
|---|---|
| O1, major — repeated full repository hashing | Partly addressed in PR1 and PR2: one fresh validated source read supplies both Snapshot and Manifest within an operation, and derivation avoids duplicate reads. Later independent authority boundaries still verify current content. The review's approximately 32/30-pass counts and 50k-file/1GiB extrapolation describe the reviewed source, not measured current performance. No claimed wall-time speedup or removal of every repeated pass. [ADR-0097](../adr/0097-share-validated-source-reads-within-one-operation.md). |
| O2, major — 4,096-token admission underestimates native cost | Addressed in PR2 by explicit catalog2 admission costs and captured common Review/doctor costs. PR1 still captures 4,096/45s; this is not claimed fixed there. Existing captures retain their original allowance. Worker reservations and bytes/4 estimates still do not predict complete native consumption; actual overruns remain charged and stop dispatch. Observed cold/warm costs are observations, not fixed provider overhead. [ADR-0091](https://github.com/PakhomovAlexander/afactory/blob/10c5302420c680fbb720b9010972d2eb37a9ba7c/docs/adr/0091-capture-explicit-task-provider-admission-costs.md), [ADR-0092](https://github.com/PakhomovAlexander/afactory/blob/10c5302420c680fbb720b9010972d2eb37a9ba7c/docs/adr/0092-capture-common-review-admission-reservations.md). |
| O3, major — evaluator Manifests hit Worker input limit | Accepted; implemented in PR1 and PR2. Context2 uses a separate bounded 8MiB host metadata reader, while final delivered Worker input remains at most 1MiB. It is a finite compatibility reader, not an unlimited repository or generic Manifest guarantee. Old Context1 rendering remains exact. [ADR-0095](../adr/0095-bind-legacy-task-context-and-retry-output-admission.md). |
| O4, major — base64 Diff duplicated in Worker context | Accepted for explicitly selected Review generation2 in both PRs. Compact Subject2 binds an exact readable patch file; initial context remains at most 1MiB, ChangeSet authority at most 4MiB. Materialization checks fresh bytes, source collisions and final source identity. The actual >780KiB test proves readable transport, not native tokenizer savings. Absent `review.generation` keeps policy1 independently of catalog generation. New PR2 product starters choose generation2. [ADR-0094](../adr/0094-bind-task-review-assignments-and-readable-inputs.md), [ADR-0099](../adr/0099-select-task-review-generation-independently-of-provider-costs.md). |
| O5, minor — repeated projection/compile/package work | Partial. PR2 already has immutable compiler/typed decoding improvements; Store listing now uses one checked projection per Task in both PRs. Current reference closure is freshly verified on warm use. The claimed original 19/6/44/25 call counts are not current PR2 measurements. Package bytes still use integer-array JSON with a 16MiB source-byte bound and 8MiB envelope bound; that representation and its lower effective capacity remain an open performance/capacity follow-up. No package-format redesign or whole-runtime speedup is claimed. |
| O6, minor — recursive repeated Review reduction | Partial, PR2 only. Bounded round/repair memos share work inside one serialized synchronous domain operation; each external entry clears them. Warm/cold mutation tests prove that old digests cannot authorize changed evidence. Cross-callback reconstruction remains intentional; no measured elimination of the review's estimated quadratic Campaign cost. PR1 has no new operation memo. [ADR-0098](https://github.com/PakhomovAlexander/afactory/blob/10c5302420c680fbb720b9010972d2eb37a9ba7c/docs/adr/0098-scope-review-memos-to-one-domain-operation.md). |
| O7, minor — listing replays every Task twice | Accepted; implemented in both PRs. Store `map_tasks` passes the checked projection to the CLI once. The performance change retains fresh verification of corrupt execution references. [ADR-0096](../adr/0096-revalidate-task-execution-evidence-on-cached-replay.md). |

The following three **benchmark-demand declarations** remain in Opus's partial result.
They never became canonical Demands because its Campaign produced no Ledger. They are not
silently dropped, fulfilled by unit tests, or marked wontfix:

| Declaration | Evidence and open work |
|---|---|
| D1 — before/after source-verification cost on 50k files / 1GiB | No warm/cold instrumented end-to-end measurement was run. The implementation removes specific duplicate reads and deliberately keeps later integrity checks. A representative cost measurement remains outstanding. |
| D2 — current native probe usage per bound provider/model/effort | Historical and later retained observations establish that 4,096 can be insufficient; deterministic substitutes prove accounting and admission behavior. The requested current cold/warm native matrix was not run. It requires separately approved paid calls; no allowance is inferred here. |
| D3 — provider-token cost of base64 versus readable patch | Readable file transport and bounded retrieval are proven synthetically; Claude/Codex tokenizer or billing savings were not measured. A paid matched comparison remains outstanding and unauthorized by this documentation task. |

### Sol — five major and two minor Findings

| Finding | Local disposition |
|---|---|
| F1 `229dcb1c7634`, major — admission rejection skips retry | Duplicate of C1; same PR1/PR2 fix and negative controls. It remains its own open canonical Finding. |
| F2 `ca6b6a4af0d8`, major — missing prior-Finding coverage | Accepted for generation2 in PR1/PR2. Exact reviewer-scoped canonical assignments require one allowed disposition per assigned Finding; missing, duplicate and unassigned answers refuse at admission and reduction. New V1 reservations with eligible prior Findings refuse; old selected V1 results replay. Omission did not erase old Findings—the defect was completeness without an explicit answer. [ADR-0094](../adr/0094-bind-task-review-assignments-and-readable-inputs.md). |
| F3 `8e3a9e3a457c`, major — nonexistent Attempt producer | Accepted for generation2 in PR1/PR2. Canonical Report, Demand and disposition provenance retains the exact selected flattened Task producer; logical reviewer names stay semantic sources. Forged provenance refuses. Strict typed payload comparison accepts canonical numbers and omitted nullable fields without accepting changed claims or unknown fields. Frozen legacy provenance remains unchanged. [ADR-0094](../adr/0094-bind-task-review-assignments-and-readable-inputs.md). |
| F4 `99b740266306`, major — warm Store skips execution evidence | Accepted; implemented in PR1/PR2. Cached projections retain and freshly verify the complete execution reference closure. Warm/fresh corruption controls cover execution records and associated evidence. [ADR-0096](../adr/0096-revalidate-task-execution-evidence-on-cached-replay.md). |
| F5 `d4dbc76e3e6d`, major — independent failure prevents terminal result | Accepted; implemented in both PRs, including PR2 reviewed implementation. Actual failed nodes make execution Exhausted before acceptance. Passed obligations plus independent failure are Inconclusive; actual failed verification remains Unsatisfied. Receipts, outputs, failed nodes and charges remain durable through finish/reopen. Proposed [ADR-0093](../adr/0093-derive-code-task-acceptance-from-execution-and-evidence.md). |
| F6 `c3237dfdc23d`, minor — expanded node limit off by one | Accepted; both PRs check capacity before root, primitive and Select insertion. Existing Provider absolute-cap checks remain. Exact-cap and one-extra controls pass; no captured cap is raised. |
| F7 `d676c43b6af0`, minor — invalid wall consumes Task ID | Confirmed/fixed in PR1; PR2 selection already prevented the reported ID-consumption effect. Both now validate positive duration and overflow before state creation/capture. No claim that PR2 reproduced the original symptom. |

## Verification and immutable evidence

The fixes are in integrated PR1 `c2207ca` (tree prefix `4b0796a`) and PR2 `10c5302`
(tree prefix `b1e9283`). The final PR1 Gate passed **759 tests, zero failures, 15 ignored, 98 suites**,
fixture reproduction, formatting and Clippy in **297,311ms**. All 567 exported source
files and directory bytes/modes remained unchanged. The [final Gate receipt](../../../afactory-wt-task-execution/.scratch/task-pr1-readonly-full-gate-04/root-summary.json)
binds process SHA-256 `dacbd7ed3c23af3d2bbb097ef5c5298ee0a095e8c1234eb9c954295ccce90be2`.
The PR2 full Gate is running under root supervision; no result is presumed here.
The earlier PR1 `a22be510` Gate passed 758 tests, 15 ignored, 98 suites in 298,346ms;
it predates the two-file typed-number correction and remains historical evidence.

Final focused correction receipts are [PR1 normalization](../../../afactory-wt-task-pr1-review-fixes/.scratch/review-fixes-01/normalization-followup.json)
(SHA-256 `64c653e9927f5ce53ccf3ce1866942237477ede75d3cc971935cc3616e1b0f5e`)
and [PR2 composition](../../../afactory-wt-task-pr2-review-fixes/.scratch/review-fixes-01/port-final.json)
(SHA-256 `3d63f10fb32ff270e4c6331a0e6602b5e25b43f10651edf14aeffe8299f967e8`).
They retain raw commands/results, intermediate failures and source hashes. PR1's typed-stage
and actual seven-case Runtime controls pass; PR2's schema105, Task-file17, heavy2, repair5,
starter4, selector2 and Runtime controls pass with scoped all-target Clippy and formatting.
The original 13 PR1 Review fixture files and 45 PR2 Review/repair files remain unchanged.
These are deterministic regressions with synthetic adapters/command Workers, not new reviewers.

Raw original evidence is [epoch 2 outcome](../../../afactory-wt-task-execution/.scratch/task-pr1-external-review-20260913-03/run.stdout)
and [Sol recovery outcome](../../../afactory-wt-task-execution/.scratch/task-pr1-sol-recovery-20260913-05/run.stdout) in the
PR1 worktree. Their exact identities, selected result objects, seven Provider charges,
Attempt-wall rows, full Finding IDs and cleanup receipts are indexed in the retained
[source/accounting packet](../../../afactory-wt-task-product/.scratch/ws5-review-disposition-proposal-01/source-accounting.json).
Fable result: `sha256:971ca25ba02aae3fd965875777c0b0b31ea8af1e23947ba0a0f1e6c465e07c64`;
Opus result: `sha256:5f7f6ea6ad67e0cd7606ad9d8817daec98df7c85b5a3cad3a4138f4349be5009`;
Sol result: `sha256:211df03c4c3f523c4fedc52d09e9b6f2a4cbdde335ecc1b33e9a60e9257438a1`.
