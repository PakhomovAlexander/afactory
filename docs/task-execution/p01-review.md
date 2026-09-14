# P01 contract review — 2026-09-11

Status: one light review Round completed; concrete corrections implemented and deterministic gates passed locally. This is not reviewer-confirmed convergence.

## Scope and authority

One light Campaign, `task-contracts-p01-20260910`, using installed `af 0.8.0`. Policy and
Diff base: `98bb904630c1c9f8e1b151fd359c874b465211a2`. Current candidate:
`066b3924cef1d22c913183461d47373930712cda`. State is outside Git under the workspace's
`.review-state/task-contracts-p01-20260910`.

Required reviewers are correctness (Fable 5.1/high, `claude-personal`), bugs
(GPT-5.6-Sol/high, `codex-personal`), and performance (GPT-5.6-Sol/high, `codex-personal`).
The owner explicitly confirmed the bug-review role and authorized both personal Codex
bindings. Required Gates remain Markdownlint and the complete kernel verification script.
The captured cap is 300,000 tokens per Attempt and 1,000,000 per Round.

The review covers additive Task/Pipeline contracts, schemas, identity/parity fixtures,
Worker independence and targeted repair continuation. Runtime, compiler and Store admission
remain subsequent packages; no new-format execution is enabled by this candidate.

## Gate recovery

All runs remain in Round 1; no reviewer Attempt ran in the first three epochs.

| Epoch | Candidate | Recorded result |
|---|---|---|
| 1 | `d187373` | Incomplete: a consumer test preserved read-only fixture permissions on a mutable copy |
| 2 | `a0d4c62` | Incomplete: process-supervision elapsed-time assertion failed once |
| 2, exact resume | `a0d4c62` | Reused the blocked Gate; no additional spend |
| 3 | `a0d4c62` | Incomplete: a render CLI test assumed a host HOME |
| 4 | `066b392` | Both Gates passed; all three reviewers selected; `Fail(Exhausted)` with 8 Findings and 1 required Demand |

Corrections preserve every required check and assertion. A shared test helper adds owner-write
permission only to disposable fixture copies and honors the runtime fixture root. The render
helper now provides isolated HOME/config/state and disables self-update network checks.
The process timing test passed all 12 focused reruns; its deadline and elapsed-time assertion
were not changed. A complete read-only archive passed all 665 tests, and a second archive
passed with exactly the Gate environment (`PATH`, `LC_ALL=C`, `TZ=UTC`). Both retained all
15 existing opt-in probe exclusions and byte-identical synthetic fixture reproduction.

The contract correction in `a0d4c62` also rejects explicit JSON null for optional properties,
which previously disagreed with the schemas. Its omission/null cases have parity regressions.

The installed CLI caches a completed Gate even when its outcome is blocked. Re-executing that
Gate therefore requires the supported `--restart-round` epoch transition. These restarts retain
the Campaign, original policy, one-Round limit and accumulated spending. They are not fresh
Campaigns or extra closed review Rounds.

## Review results and accounting

The [verbatim Campaign report](p01-campaign-report.md) records one closed Round with
**8 open Findings (5 major, 3 minor)** and **1 open required Demand**. Two major Findings
identify the same set-ordering defect. No Finding was rejected or marked wontfix.

The report's Round wall-clock is **7m45s**. Total Campaign chargeable usage, including
all Gate-recovery preflights, is **483,057 tokens**. Reviewer Attempts consumed **434,088**;
Provider admission consumed **48,969**. Prior incomplete runs' reported spend is cumulative
and must not be added to the final total again.

| Reviewer | Attempt wall | Chargeable tokens | Input | Output | Cache read | Cache write | Reasoning |
|---|---|---:|---:|---:|---:|---:|---:|
| Fable cross-review | 5m25s | 147,664 | 578 | 24,747 | 2,046,842 | 122,339 | not reported |
| Sol bug review | 7m45s | 152,628 | 1,569,646 | 15,558 | 1,432,576 | 0 | 12,467 |
| Sol performance review | 5m38s | 133,796 | 1,097,771 | 10,745 | 974,720 | 0 | 8,036 |

These are the Provider's usage fields as printed by `af review report`, not interchangeable
billing categories; cached usage is not added again to chargeable tokens.

## Dispositions in the correction branch

| Finding prefix | Local correction and evidence |
|---|---|
| `de8f7f5493b2` | Present result outputs require an artifact; satisfied results require outputs. Added empty-many/output-map negative cases in both schema and Rust. Store proof of Task-required names/types remains P02. |
| `a0eb580f585f` | Outcome validation now requires explicit receipt-derived completeness. The cross-product regression permits only exit 4 when required nodes are missing. Complete review Task goals may still be satisfied with exit 3; acceptance is not used as a completeness proxy. |
| `569cb4877873` | Set deserialization requires ascending unique wire order; typed identity and permutation tests cover all set families. |
| `520a73bfba9d` | Same set-ordering correction; also made empty default fields explicit so typed loading cannot introduce identity-changing defaults. |
| `4953c3abc5a6` | Added explicit trusted Command-package independence policy, enabled by default. It preserves distinct-principal Model/Model checks and rejects Command bindings under stronger model/Provider diversity requirements. |
| `6dbcc88a6831` | Semantic negative cases must pass schema validation and fail Rust admission; unknown case classifications fail the test. |
| `5df129ada210` | Replaced repeated payloads with fingerprinted mutation recipes. Corpus fell from 66,184 to 15,450 bytes despite six added cases: 50,734 fewer bytes (76.7%). At the review estimator's four bytes/token, that removes about 38,050 tokens across three first inputs. |
| `d3a72ae86a32` | Shared schema resources are parsed once; validators are compiled lazily once per schema and reused. Legacy schema assertions no longer register the Task resource. No unmeasured wall-time speedup is claimed. |

Required Demand `ba8febf828e5` has deterministic evidence in
`crates/review-store/tests/task_contract_identity.rs`: all nine positive contracts retain their
original content IDs through typed deserialization/serialization, and eleven nontrivial set
permutations are rejected. Every compact negative recipe reproduces its intended invalid
payload fingerprint. No original positive content ID or frozen synthetic fixture was changed.

The original reviewed Subject remains immutable. Its ledger still reports eight open Findings
and one open Demand: code/test corrections on a later Snapshot are not current-Subject
verification receipts. This light Campaign will not be rerun after corrections. The next step
is P02/P03 implementation after this local checkpoint. The final `make check` passed **667 tests,
zero failures and 15 existing ignored probes** across 89 suites, including formatting, Clippy and
byte-identical synthetic fixtures. Markdownlint passed for 96 files. Runtime receipt and Store
authority enforcement remain subsequent implementation packages.
