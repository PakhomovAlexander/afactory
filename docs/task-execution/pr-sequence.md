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
