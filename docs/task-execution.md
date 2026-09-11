# Task execution increment

**Status:** implementation started, 2026-09-10. Baseline is kernel `5464b38` (0.8.0).
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

- [x] P00: unchanged-source baseline gate and fixture identities recorded below.
- [ ] P01: contracts, schemas, ADR-0046 and fixtures implemented; deterministic gates pass;
  external review is running under the approved personal Provider bindings before package closure.
- [ ] P02/P03: common Store lifecycle and approvals; typed Pipeline compilation.
- [ ] P04–P06: shared execution, implementation Task, review Task and legacy parity.
- [ ] P07–P09: shared packages, embedded Review, bounded repair and fix verification.
- [ ] P10–P12: selection, bounded generation, developer approval, export and starters.
- [ ] P13/P14: Jira/document demonstrations, compatibility, benchmark and consumer release.

The configured milestone review is one light Round with three required reviewers:
`correctness=claude-personal` (Fable 5.1/high), `bugs=codex-personal` (GPT-5.6-Sol/high),
and `performance=codex-personal` (GPT-5.6-Sol/high). Packages and the review Pipeline are
content-locked using installed release `af 0.8.0`. Required checks are Markdownlint and
`scripts/verify.sh` (the complete kernel gate). The owner confirmed that the Sol bug role is
a reviewer, alongside cross-review and performance review.

## P01 contract checkpoint

The additive `review-core::task` contracts cover immutable Task revisions, typed public Pipeline
boundaries, bounded static operator declarations, exact plans and developer decisions, Worker
independence, review history and targeted repair continuation. No new execution is admitted yet;
compiler, Store authority and runtime checks remain P02–P06 work.

`make check` passes with **664 tests, zero failures and 15 existing ignored probes**. The original
synthetic fixtures still reproduce byte-for-byte. New schema/type fixtures include forbidden
approval identities, duplicate inputs, hidden tagged-variant fields, missing evidence and stale
repair claims; canonical content IDs use the existing digest domain. Markdownlint passes after
removing two pre-existing extra blank lines from the workstream archive. External review is the
remaining P01 check.

The review candidate is `d1873733987c1392c4a27df60ee6162e7d77aa58`; policy and Diff base are
`98bb904630c1c9f8e1b151fd359c874b465211a2`. Installed `af 0.8.0` successfully planned the three
bindings with 300,000 tokens per Attempt and 1,000,000 per Round. Each first input is about
49,800 tokens; the planned focus is P01 contracts and compatibility, with later compiler/Store
execution explicitly outside this candidate.

The initial launch on 2026-09-10 was rejected before execution because automatic approval review
required explicit `codex-personal` destination authorization. The owner supplied that approval on
2026-09-11 for both Sol reviewers; Fable continues to use only `claude-personal`.

Campaign `task-contracts-p01-20260910` started under installed `af 0.8.0`, with external state at
the workspace's `.review-state/task-contracts-p01-20260910`. Round 1, epoch 1 stopped Incomplete
at the required test Gate: a pre-existing consumer compatibility test copied a read-only fixture
with its permissions intact and then tried to edit that copy. Markdownlint passed, and no
reviewer Attempt ran. Provider admission spent **14,691 tokens**: bugs 5,710, correctness 3,271,
performance 5,710. These are preflight costs, not review findings or a review pass.

The correction shares a fixture-copy helper among the consumer, migration and render tests. It
adds owner-write permission only to disposable copies, preserves source fixture bytes and modes,
and resolves fixture roots through `AF_WORKSPACE_ROOT` for cached review builds. The corrected
candidate also rejects explicit nulls in optional Task/Pipeline fields, matching the schemas.
Resume the same incomplete light Round using a recorded replacement candidate and
`--restart-round`; retain the previous epoch and spend. Do not start a replacement Campaign.

The next implementation step is P02/P03. Store integration must use `EventStore::append_batch`'s
existing CAS publication barrier and immediate transaction, with a separate Task transition
validator; the legacy Campaign validator's permissive tail is not Task admission. Add fenced
writer leases and exact authorized decisions before enabling new-format dispatch. P03 lowers
the public typed boundaries into the existing graph scheduler; it must not introduce a second
executor. ADR-0046 records the remaining contract-to-runtime obligations.

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
| `crates/reviewctl/tests/task_implement.rs` | `21da1453f236d8f09d1ab4b15ce41274e8b785d5b260491d26d407a2ccee5cb7` |

Compatibility obligations include legacy review result parsing and ledger replay, Campaign
authority, incomplete/resumable review, existing Task completion/failure/delivery, consumer
policy versions and CLI exit statuses. Later packages must extend this evidence without
rewriting the frozen corpus or treating a generic Task completion as review approval.
