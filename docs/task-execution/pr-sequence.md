# Task increment: three-PR delivery

The owner authorized the complete P00–P14 plan and three PRs on 2026-09-11. The original
package numbers remain the acceptance checklist; they are not separate PRs.

| PR | Repository | Scope | Required demonstration |
|---|---|---|---|
| 1 | afactory | P00–P06: contracts, compiler, Store, approvals and shared execution | Local implementation and standalone review use the same scheduler and durable Attempt path |
| 2 | afactory | P07–P13 and kernel P14: packages, embedding, repair, selection, generation, export, starters, Jira/document and release preparation | Implement a ticket with embedded review; generate, approve, export and reuse a no-fit plan |
| 3 | afactory-hub | Accepted design, examples, tracking and P14 consumer migration | Supported released binary and exact consumer lock validate together |

PR 2 depends on PR 1. PR 3's active release pin moves only after the corresponding release
exists with verified checksums. The owner requested PR delivery; release merging remains the
final publication decision. A missing released asset cannot be replaced by a development build
or an invented digest.

Each PR receives one light external `af review` Round: Fable 5.1/high on `claude-personal`
for correctness and architecture, Opus 5/xhigh on `claude-personal` for performance, and
GPT-5.6-Sol/high on `codex-personal` for bug bounty. These replace the previous P01 reviewer
configuration for future PRs; the historical P01 Campaign is unchanged. Fix concrete Findings
and run the deterministic gate without starting another Campaign to obtain a clean verdict.

Baseline: `b8b8963`, the latest observed kernel main. Its change since the recorded P00
baseline is documentation only. P01 fixes remain in `c286e18` and retain their test evidence.
Default integration fixtures use command Workers. The live pilot requires its own bounded
budget before inference; deterministic fixtures do not substantiate a live-model product claim.
