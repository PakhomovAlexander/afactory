# ADR-0189: Read a Task result's sets in the kernel's order, and reconcile a second repair through the first

**Status:** accepted (2026-10-21). Repairs the P10 source of
[ADR-0188](0188-validate-kernel-receipts-as-the-kernel-does-and-restore-the-retained-history.md)
(SourceTree `sha256:25afc012dba81b9a3f861628ea0c8bd084d40808405826fb681f7f6ad74e2f63`, Snapshot
`sha256:f96ee4cb6a0a51d90d1c23e2cb39f03c0143085a99797e573e86cb1e7b9e7bc3`) for the
[continuous performance laboratory](../design/continuous-performance.md). The accepted design
is unchanged. ADR-0188 stands except where this ADR amends it.

| Contract | Version | What changed |
| --- | --- | --- |
| Kernel receipts | `af.perf-kernel-receipts/3` | unchanged shape and schema; the reader refuses a Task result set that is not strictly ascending |
| Kernel inventory | `af.perf-kernel-inventory/1` | unchanged shape; re-pinned to `sha256:f8c1578cbbdf2b1f12bdd5d49dba6410c911887f4963fa64da760c2bc9c416ac` |

## Context

The P10 Task's mandatory gate passed (663 Python tests with 2 skips, 2,147 Rust tests with 14
skips, the native guarded integration in 873.377 s), and so did its correctness review and goal
evaluation. One bug-review finding remained. If you reverse the valid fixture's four distinct
`tasks[0].result.evidence` IDs and change no envelope or reference, `validate_kernel_receipts`
still accepts the document. The kernel reads `TaskResultV1.evidence` and `missing_obligations`
with `unique_set`, which refuses any entry that is not greater than the one before it. The
reader therefore admitted a Task result the kernel cannot produce. The root reproduced this for
both fields. Duplicates were already refused by the closed shape's `uniqueItems`, and an
output's `artifact_ids`, a `Vec`, is valid in any order.

ADR-0188's `checks/p9-repair-reconciliation.json` reconciles the P10 tree with the tree that
`measured-source.json` reconciled. Any change to the source invalidates it for the current tree,
but it must stay byte-exact.

## Considered options

1. **Version the receipts as `/4`.** Rejected: no persisted shape changes. Real `/3` receipts
   serialize each set from a `BTreeSet`, so they are already ascending.
2. **Sort the sets on read.** Rejected: the kernel refuses an unsorted set, and so must the reader.
3. **Restrict every array the result holds.** Rejected: `artifact_ids` is a `Vec`, and the kernel
   admits its order as written.
4. **Add the order to the published JSON Schema.** Rejected: JSON Schema cannot express order.
   The schema keeps `uniqueItems`, and the reader adds the order.
5. **Rewrite the P10 record, or remeasure.** Rejected: the record is retained evidence, and a
   receipts-only change alters no measured path.

## Decision

1. **The kernel's set order** (`contracts._kernel_task_result`). Before `TaskResultV1::validate`'s
   rules, as serde runs before `validate`, each of `evidence` and `missing_obligations` must be
   strictly ascending. Rust orders a `String` by its UTF-8 bytes, which is Python's code point
   order, so `"Zeta" < "a-b" < "a_b" < "alpha"`. An unsorted set is refused, never normalized.
   Empty, singleton and ascending sets stay valid, and so does an output's `artifact_ids` in any
   order.
2. **Fixtures and parity.** Four generated fixtures change only a result, so every envelope and
   reference stays the kernel's own: `invalid-unsorted-evidence` (the root's reversed four IDs),
   `invalid-unsorted-missing-obligations`, `invalid-duplicate-evidence` and
   `invalid-duplicate-missing-obligations`. The JSON Schema refuses only the two duplicates. The
   Rust helper `kernel_refusals` now reports a refused `TaskResultV1` deserialization instead of
   panicking. The new test
   `perf_kernel_measure::receipts_reader_admits_result_sets_as_the_kernel_deserializes_them`
   builds eleven variants of the valid fixture at test time. They include the valid controls,
   reversed, case-ordered and duplicated sets, and three output IDs in descending order. It
   requires the laboratory's reader and `review_core`'s deserialization to agree on each one.
3. **Reconciliation chained, never rewritten.** `tools/fingerprint.py rerepair ROOT PRIOR_ROOT
   RETAINED_JSON PRIOR_REPAIR_JSON` first checks that `PRIOR_ROOT` is the tree the prior record
   reconciled. It then rebuilds the finalized tree's lines from that tree and the prior record's
   finalized lines, and records what differs in ROOT, in the existing record format. The new
   `checks/p10-repair-reconciliation.json` was computed from the exact P10 input tree, so the
   existing `verify-repair` checks the current tree against `measured-source.json` through it.
   `measured-source.json`, `checks/p9-repair-reconciliation.json` and every raw bundle, report,
   trace and native receipt are byte-exact. Nothing was measured again.

## Consequences

- **Tests.** `test_kernelguard.Receipts` adds the order and duplicate cases for both fields, the
  valid empty, singleton, ascending and byte-ordered sets, and an output with many IDs in
  descending order. The fixture consistency test now covers the four new fixtures and requires
  every generated evidence set to ascend, except the one counterexample.
  `test_evidence_tools.Reconciliation` chains a second repair on constructed trees. On the P10
  reader, the new Rust test fails at `reversed-evidence: accepted`. The retained real receipts
  are still admitted.
- **Limit.** The measured commit's Git repository is still not retained, so the original commit
  cannot be checked out again. A newly captured source has a new identity.
- **Not changed.** The 15-second lease, the 8-MiB envelope cap, the 4-MiB review limit, the
  32 KiB part and 64 KiB whole-stream limits, every gate, timeout and budget, and the mandatory
  full gate that runs after sealing.
