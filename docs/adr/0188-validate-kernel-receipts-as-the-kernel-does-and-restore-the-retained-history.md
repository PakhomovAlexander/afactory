# ADR-0188: Validate kernel receipts as the kernel does, and restore the retained history

**Status:** accepted (2026-10-21). Repairs the P9 source of
[ADR-0187](0187-bind-kernel-receipts-to-their-task-results-word-reports-by-what-was-admitted-and-retain-a-real-local-run.md)
(SourceTree `sha256:9194d020ad46039aa934c6d531fa8f44417e93cee57c510c24abe93a8b2627bf`, Snapshot
`sha256:6379afcc95828e6ab94ccab62f5ac08588649bdaa042c806430269ca4a0762e9`) for the
[continuous performance laboratory](../design/continuous-performance.md). The accepted design
is unchanged, and ADR-0187 stands except where this ADR amends it.

| Contract | Version | What changed |
| --- | --- | --- |
| Kernel receipts | `af.perf-kernel-receipts/3` | unchanged shape and schema; the reader applies the kernel's envelope and Task result rules and the documented per-part byte bound |
| Kernel inventory | `af.perf-kernel-inventory/1` | unchanged shape; re-pinned to `sha256:69bddb0deca5051b56f9bf63a9ba12243c2b5b220fa322bb11b06a909903dd31` |

## Context

The P9 Task's mandatory gate passed (654 Python tests with 2 skips, 2,146 Rust tests with 14
skips, the native guarded integration in 595.103 s). The gate's nextest output also labelled
`af_layout::every_authority_path_the_kernel_resolves_is_declared_in_the_layout_table` LEAK
(0.396 s). That test only reads source files and spawns no process. No cause is established, and
nothing here changes for it.

Both semantic reviews still requested changes, and the goal evaluator failed the package:

1. The receipts reader accepted an envelope whose producer `run_id` was one space, although
   `ArtifactEnvelope::validate` refuses it.
2. A failed Task's result could name `candidate` with `artifact_ids = []`; `TaskResultV1::validate`
   refuses every empty present output.
3. A whole stream with a 32,768-byte head and a 40,000-byte tail passed. The root reproduced the
   minimal case: a 32,769-byte tail, in UTF-8 and in base64, with every identity recomputed. The
   schema bounds a part's characters; for base64 that allows 43,692 characters, which is more
   than 32 KiB once decoded.
4. `workflow.sh snapshot` copied a checkout's `.git`. On a clean checkout `git commit` then had
   nothing to commit, and the stage still recorded the checkout's own `HEAD`.
5. The first export left out `history/runs/p8_local_aa/`, so the retained history linked to a
   missing page. `checks/links.json` had checked the work directories before export, and
   ADR-0187 claimed that every link resolved.

## Considered options

1. **Version the receipts as `/4`.** Rejected: no persisted shape changes. The fields, schema
   and writer stay as they are. Only the reader's semantic checks are added, and they restore
   what `/3` and the kernel already promise. Real `/3` receipts the kernel wrote stay valid.
2. **Use Python's `str.strip()` for blank names.** Rejected: `isspace` also counts U+001C to
   U+001F, which Rust's `trim` keeps. The reader spells out the 25 `White_Space` characters.
3. **Treat an empty binding as an absent port.** Rejected: the kernel never emits one, so it is
   impossible provenance, not absence.
4. **Rely on the schema's string bound for parts.** Rejected: it bounds characters, not decoded
   bytes.
5. **Regenerate the history page in place, or note the omission.** Rejected: the retained history
   must open both runs directly, and regenerated content must be proven identical to the bundles.

## Decision

1. **Kernel validation in the reader** (`contracts._kernel_producer`,
   `contracts._kernel_task_result`). Every producer `run_id`, `node_id` and `operation_id` must be
   nonblank after Rust-equivalent trimming; a kernel operation's `node_id` is optional, but if
   present it must be nonblank. A result's `domain_conclusion` must be nonblank. `satisfied`
   requires `completed` execution, evidence, outputs and no missing obligation. Each present
   output must be an exact binding: cardinality `one` names exactly one artifact, and the binding
   is never empty. These checks run in the kernel's order. Names and types are matched whole, so
   a trailing newline that `re.match` with `$` would admit is refused. On the three retained ports, a result
   names an output exactly when one is retained, and otherwise the port is absent. Ports the
   receipts never retain keep their bindings.
2. **Decoded part bound** (`contracts._kernel_stream`). Each decoded head and tail holds at most
   32,768 bytes in either encoding, so a whole stream holds at most 65,536. Exact cut heads and
   tails and whole-stream hashes are unchanged.
3. **Fixtures and parity.** Seven new invalid fixtures each break one rule, with every envelope
   ID, comparison input, Task reference and evidence entry recomputed: blank producer, blank
   operation, blank conclusion, satisfied but incomplete, empty output, cardinality, and the
   root's 65,537-byte stream. The Rust test
   `perf_kernel_measure::receipts_fixtures_refuse_what_the_kernel_s_own_types_refuse`
   deserializes each into `review_core` types. Exactly the named envelope or result fails its own
   `validate`, with the kernel's message, and the valid fixture has no refusal. The test also
   compares the reader's whitespace set with `char::is_whitespace` over all of Unicode, and shows
   that the native exporter's `retained_bytes` cuts any stream over 64 KiB.
4. **Workflow failure semantics** (`tools/workflow.sh`). The snapshot stage copies without `.git`
   (directory or worktree file) into a new `WORK/repo` and commits once in a fresh repository. It
   stops on any failed copy or Git command. Every stage ends at its first failing step, with that
   status recorded.
5. **Retained history restored** (`tools/restore_history.py`, `tools/checks.py exported`). The
   two retained bundles were imported into a new archive and drawn as history. Every retained
   history file was regenerated byte for byte, and the ten listed files matched their recorded
   sizes and SHA-256 before they were copied. The exporter now keeps every page the history links
   and checks the exported directory itself. `checks/links.json` keeps its original meaning, a
   pre-export check of the work directories. `checks/links-exported.json` is the new post-export
   check: 36 links and 23 manifest entries, with no failure.
6. **Fingerprints reconciled, never rewritten.** `measured-source.json` is unchanged. The new
   record, `checks/p9-repair-reconciliation.json`, lists each path this repair changed relative to
   the first finalized tree. `fingerprint.py verify-repair` rebuilds that tree from it and checks
   it against `measured-source.json`. Raw run, build, trace and native bundles are byte-exact.
   Nothing was measured again: the execution, plan, comparison, report, archive and trace paths
   are unchanged. The original commit's Git repository is not retained, so the measured source
   cannot be checked out again.

## Consequences

- **Tests.** `test_kernelguard.Receipts` adds whitespace parity, satisfied coupling, empty and
  multiple bindings, whole-match names, both encodings at and over the part bound (multibyte
  UTF-8 included), and checks that every rebuilt fixture is self-consistent. On the P9 reader
  these give four failures and one error. `test_evidence_tools` runs the snapshot stage on a tiny
  real Git checkout and on a worktree. It also covers a failed commit, an existing destination
  and a failed first step, checks the exporter on a constructed work directory, and runs the
  post-export check on the retained directory. On the P9 tools all six fail or error. The retained real
  receipts are still admitted.
- **ADR-0187 amended.** Its inspection bullet now separates the pre-export check from the
  post-export one.
- **Not changed.** The 15-second lease, the 8-MiB envelope cap, the 4-MiB review limit, the
  32 KiB part and 64 KiB whole-stream limits, every gate, timeout and budget, and the mandatory
  full gate that runs after sealing.
