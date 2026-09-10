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
- [ ] P01: versioned Task/Pipeline/plan/approval contracts and compatibility fixtures.
- [ ] P02/P03: common Store lifecycle and approvals; typed Pipeline compilation.
- [ ] P04–P06: shared execution, implementation Task, review Task and legacy parity.
- [ ] P07–P09: shared packages, embedded Review, bounded repair and fix verification.
- [ ] P10–P12: selection, bounded generation, developer approval, export and starters.
- [ ] P13/P14: Jira/document demonstrations, compatibility, benchmark and consumer release.

The configured milestone review is one light Round with three required reviewers:
`correctness=claude-personal` (Fable 5.1/high), `bugs=codex-personal` (GPT-5.6-Sol/high),
and `performance=codex-personal` (GPT-5.6-Sol/high). Packages and the review Pipeline are
content-locked using installed release `af 0.8.0`. Required checks are Markdownlint and
`scripts/verify.sh` (the complete kernel gate). No implementation milestone review has run yet.

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
