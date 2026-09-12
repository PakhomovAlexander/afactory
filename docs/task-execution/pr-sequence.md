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
