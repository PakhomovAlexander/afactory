# ADR-0187: Bind kernel receipts to their Task results, word reports by what was admitted, and retain a real local run

**Status:** accepted (2026-10-21). Closes package p8 (integrated acceptance) of the
[continuous performance laboratory](../design/continuous-performance.md) (design §10, §11, §15,
§17) on the unaccepted p8 source of
[ADR-0186](0186-fold-protected-paths-key-warm-targets-by-the-whole-snapshot-bound-measure-output-and-replan-bridge-runs.md).
It repairs that source's three semantic findings and the root's envelope-identity
counterexample, removes an unmeasured timing claim from ADR-0186, and retains one real run of
the documented local workflow. The accepted design is unchanged. ADR-0162 to ADR-0186 stand
except where this ADR amends them.
[ADR-0188](0188-validate-kernel-receipts-as-the-kernel-does-and-restore-the-retained-history.md)
amends this ADR's receipts reader and retained history.

| Contract | Version | What changed |
| --- | --- | --- |
| Kernel receipts | `af.perf-kernel-receipts/3` | typed Task result and envelope; each retained output bound to its result's reference; `retained` per stream; `/2` is refused |
| Report | `af.perf-report/2` | unchanged shape; the first view selects failed or incomplete cases, and measured wording follows coverage |
| Kernel inventory | `af.perf-kernel-inventory/1` | unchanged shape; re-pinned for the changed harness |

## Context

The predecessor Task (`perf-p8-integration-20261021`, result
`sha256:fbc790159f5c968aec69f2a982a90fda2b08c9e8182a6f59c6b6defd1ad82a83`, snapshot
`sha256:2151cd55c8cabc9a90d23df3be818f1be8a23c097e424a6120b5ba20455c8a72`) is distinguishable in
three parts that this ADR keeps apart:

1. **Its full gate passed.** 649 Python tests with 2 skips, 2,145 Rust tests with 14 skips, the
   native guarded integration in 691.652 s, and the feature and calibration builds. No LEAK label
   appeared. That pass is the gate's, not a review's or the evaluator's.
2. **Reviewers and the root found four defects.** A report's first view listed a passing
   improvement and a non-regression as cases "outside tolerance or incomplete". A measured run
   with a missing planned record still said each pair's samples were recorded, on the page and in
   `summary.md` (two findings). The receipts reader accepted a Task result naming another
   artifact than the retained envelope. Independently, the root rehashed the valid receipts
   fixture with the kernel's domains, changed `runs[0].metrics.wall_ms` to `999` under the
   original content and artifact IDs, and the reader accepted it: it compared only the IDs the
   document reported about itself.
3. **The goal evaluator failed the package** because no real supported workflow had run: the
   available `macos_arm64_local` environment was never sampled, and the browser check had opened a
   synthetic report.

The root's own native build of the predecessor passed; its native test was cancelled on the
superseded source on purpose, which is neither a pass nor a failure of the product. ADR-0186 also
asserted that hashing a whole Snapshot of up to 2 GiB costs milliseconds, which nothing measured.

## Considered options

1. **Compare the Task result's IDs only with the retained `artifact_id` field.** Rejected: the
   field and the envelope are both the document's own claims. The reader recomputes what the
   kernel would, so a changed payload under kept IDs is refused.
2. **Keep `/2` and only tighten the reader.** Rejected: the result and envelope become typed, and
   each stream says what it retains, which changes the published shape. A changed shape takes a
   new version and `/2` fails closed (ADR-0145).
3. **Rehash a cut stream from its head and tail.** Impossible: the middle is gone. The document
   says `head_and_tail`, and the reader verifies sizes and parts, never the omitted bytes.
4. **Select affected cases by status.** Rejected: an improvement and a non-regression have a
   status other than `within_tolerance` but pass. The outcome decides: `fail` or `incomplete`.
5. **Word a measured run once, from provenance.** Rejected: provenance says what kind of records
   exist, not that every planned one does. Coverage, failed samples and unfinished shards decide
   the wording.
6. **Use the synthetic writer or a stand-in binary for the workflow evidence.** Rejected: that is
   not measured evidence. The retained run builds the real products and runs real samples.

## Decision

1. **Receipts are recomputed and bound** (`contracts.validate_kernel_receipts`,
   `af.perf-kernel-receipts/3`). A Task's `result` is `TaskResultV1` exactly, and each envelope is
   `ArtifactEnvelope` exactly: a producer is an Attempt or a kernel operation, and an absent
   subject Snapshot is omitted, never `null`. For each of `baseline`, `candidate` and
   `comparison`, the result names an output exactly when one is retained, with its one artifact
   ID, type and Snapshot; a failed Task that never produced its candidate retains none and its
   result names none. A port retains only its own artifact type. The reader recomputes each
   envelope's `content_id` over the payload and its `artifact_id` over the envelope without that
   field, with RFC 8785 canonical JSON as `review_store::canonical` writes it (UTF-16 member
   order, safe integers, no float) under the kernel's two domains. A whole stream's ID is
   recomputed as its blob content ID. A stream longer than 64 KiB keeps its first and last 32 KiB
   with `retained = "head_and_tail"`; its ID and SHA-256 are the exporter's, read from the
   kernel's store, and no reader rehashes the omitted middle.
2. **Fixtures are generated and cross-checked**
   (`scripts/performance/tests/receipts_fixtures.py`). The valid fixture holds a completed Task
   with all three outputs and a refused Task with only its baseline, every identity computed with
   the kernel's rules. Thirteen invalid fixtures each break one rule: the old version, a `null`
   subject, a changed Snapshot, envelope ID or provenance, the mutated `wall_ms`, a contradictory
   Task reference, a retained output the result does not name and the reverse, a missing
   repetition, a wrong stream size, ID or retention claim. A Rust test passes every valid envelope
   through `review_store::validate_envelope` and each whole stream through `blob_content_id`, and
   sees the kernel refuse the two mutated envelopes. The native integration exports `/3`.
3. **The first view shows problems only** (`report.affected_cases`, `report.passed_text`). It
   counts and tabulates the cases whose outcome is `fail` or `incomplete`. Passing improvements
   and non-regressions stay in the Cases table and `summary.md` as passes. A run with no such case
   says every case passed, counted by status, so an improvement is never called within tolerance.
4. **Measured wording follows coverage** (`report.comparison`). Only a measured run whose every
   planned record was admitted and succeeded in complete shards keeps the sentence that each
   pair's samples were recorded. Any other measured run says "Measured evidence, incomplete" and
   states how many planned records were admitted, missing and failed and which shards did not
   complete. With a missing or failed record it says not every planned pair was recorded and
   succeeded, and that only fully recorded cases have an interval; with every record admitted in
   a shard that did not complete, it says exactly that. A mixed run appends the same counts. Synthetic and empty wording is unchanged. The page and the summary share the text.
5. **No unmeasured hashing claim.** ADR-0186's option 3 no longer says what hashing a whole
   Snapshot costs. One observation, not a bound: `kernelmeasure.sources_digest` over a 1,906-file,
   21,285,119-byte export of the measured commit took 107.8 to 146.3 ms, median 117.4 ms, in five
   repetitions on an Apple M3 Max under macOS 27.0.1 with a load average near 26 from other work.
   An earlier export of 1,904 files under a load average near 50 took a median of 208.0 ms.
   Nothing is claimed for a larger Snapshot or another host. The 200,000-entry and 2-GiB
   bounds and the hashing of every file are unchanged.
6. **A real local run is retained** (`docs/performance-evidence/p8-local/`). Its README records the
   commands, exits, host, toolchain, identities, results and how to regenerate them; the evidence
   section below summarises it.

## Evidence

[`docs/performance-evidence/p8-local/`](../performance-evidence/p8-local/README.md) holds the
run, its commands, exits and identities, and how to regenerate and reconcile it. In brief:

- **Measured source.** This Task's pre-sealing source, committed into a task-owned repository as
  `acc14e9a77ff69381a521eea1662e80b18b23574` (tree `3d1c9c780f55ceda85361a916b7dbe229e1b3347`,
  protected harness `sha256:d6c26e84e05c086e98238a71128c637051e4b38189180e6244622cce01bf8760`).
  Only documentation and the evidence directory changed afterwards; `measured-source.json` lists
  each such path and the digest every other file must keep. A first complete run of an earlier
  source preceded it and is superseded; the evidence README records it.
- **Workflow.** On a shared Apple M3 Max host in `macos_arm64_local`, with rustc 1.88.0:
  `capabilities` and `list`; a frozen partial-scope A/A plan (`p8_local_aa`, nightly tier's
  macOS profile, 20 pairs) of three critical cases, the CLI case `cli_discovery__command_version`
  among them; an offline locked build (124 s); one shard in trusted local mode, 152 of 152
  records and healthy; `aggregate` and `compare`, a `pass` with measured evidence that does not
  satisfy the required check (partial scope, proposed policy). A `macos_arm64_trace` child run
  recorded four complete phase traces, each labelled `perturbing` on this host, and is linked from
  the report without changing its verdict. The run was archived, materialized and drawn as
  compatible history.
- **Inspection.** Before export, in the work directories, all 36 local links of the report, its
  diagnostic page and both history pages resolved below their directories, with the 7 listed
  downloads' hashes (`checks/links.json`). The first export left out the original run's history
  copy, so the retained history's link to it did not resolve. ADR-0188 restored that copy from the
  retained bundles. Checked after export, the retained directory resolves all 36 links
  (`checks/links-exported.json`). Headless Chrome 155
  filtered the real report's 190 rows to 0 and back, opened its 3 disclosures and logged no
  console or CSP message. That interaction completed; separately, Chrome exited 2 after
  `Browser.close` with `Teardown watchdog expired` and left no process.
- **Reproduction.** The report's recompute, report and rerun commands, run as printed, gave a
  byte-identical verdict, the same report apart from the diagnostic link it was not given, and a
  plan differing only in run ID, parent and digest. Executed, that rerun recorded 152 of 152
  healthy records and passed; it is in the archive and history beside the original. In the
  superseded first run, the same rerun was `incomplete`: its CLI case was inconclusive under a
  load average near 80.
- **Native integration.** On the first run's source, the guarded kernel integration passed in
  761.22 s, and its exported `af.perf-kernel-receipts/3` document of 11 Tasks, which the
  reader recomputes, is kept as `kernel-receipts.json`. The gate's run on the sealed source is the
  authoritative one.
- **Unavailable here.** `linux_container`, both Linux environments and their cases, Linux perf,
  samply and inferno: this host is macOS, no verified disk-bounded scratch is configured, and no
  profiler is configured. Nothing was installed, signed or reconfigured.

## Consequences

- **Tests.** `test_kernelguard.Receipts` regenerates the fixtures and requires identical bytes,
  checks the canonical encoder against literal kernel bytes, and refuses the root's mutated and
  contradictory documents. `test_report` adds a regression beside a passing improvement and a
  passing non-regression, the same run without the regression, and measured runs with a missing
  record, with failed samples, with an unfinished shard and complete, on the page and in
  `summary.md`.
  `perf_kernel_measure::receipts_fixtures_carry_the_kernel_s_own_envelope_and_stream_identities`
  is the Rust cross-check.
- **Not established.** The retained run is one same-source A/A comparison, with its executed
  rerun, on a shared developer host: functional local evidence of the workflow, not a product
  speed, a sensitivity, a trace-overhead qualification, a Linux result or a hosted demonstration.
- **Pending operational evidence.** Unchanged from ADR-0186 and the acceptance matrix: the
  five-day hosted cohort and its activation, qualified Linux captures and trace overheads,
  twenty-sample shipping runs, five pull-request and five A/A cold and warm cost runs, hosted
  staging, archive collection, ruleset coexistence and the operator's installation of the staged
  policy.
- **Not changed.** The 15-second lease, the 8-MiB envelope cap, the 4-MiB review limit, every
  gate, timeout and budget, the sample and build limits of design §14, and advisory enforcement.
