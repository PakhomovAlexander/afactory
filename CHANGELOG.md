# Changelog

Every release has a section here before it is tagged: `scripts/release.sh X.Y.Z --compat "…"`
writes it from the pull requests merged since the previous release, and the release workflow
publishes the section as the release notes. The **Authority compatibility** line is mandatory:
it says whether committed `.af/` policy keeps working as is, needs `af onboard --refresh-lock`,
or needs `af onboard --migrate --apply`. Releases before 0.7.1 are described on their GitHub
release pages only.

## [Unreleased]

- A `0.7.1` default cannot read a lock written by `0.8.0` (its `[af]` table is an unknown field
  to the older parser), so it neither dispatches to `0.8.0` nor plans: run `af self update`
  first on such a machine.
- Each reviewer must disposition only its own rows of the prior Finding union — the rows whose
  `source` names its node, plus any orphan row — while still being delivered, and still allowed
  to name, the whole Round-wide union. Membership stays the union, coverage becomes the node's
  own partition, derived by the kernel at delivery time from each row's `source` and the pinned
  pipeline's receiving reviewer nodes. A reviewer that reads a peer's row and finds the claim
  wrong still disputes it, which is the only route to `contested`. The Round's prior-Finding
  document is unchanged: exactly `subject_id`, `round`, `prior_findings`, the three keys `0.7.1`
  reads, so a Round this release starts still resumes on an older `af`.
- The reviewer's input says which rows those are. Nothing delivered tells a Worker its own node
  id, so the coverage keys travel beside the shared document as `required_finding_ids`, and the
  rendered prompt asks for exactly one disposition per key under `required_dispositions`, states
  that another reviewer owes the rest of the Set, and permits — without requiring — a `dispute`
  entry for a peer's row whose claim is wrong. `MAX_PRIOR_FINDINGS_BYTES` measures the rows and
  that key list together.
- What that saves and what it does not: the output-side obligation shrinks from R×N dispositions
  to N across R reviewers, and the prompt now asks for N. The delivered prior-Finding document is
  byte-identical for every reviewer, because the union is delivered whole; the only addition is
  the compact list of keys that node owes. Delivering only the reviewer's own rows would save
  those bytes too, but it would make a cross-reviewer Dispute and a cross-reviewer Proposal claim
  structurally impossible — `contested` is the only route by which peer review challenges a wrong
  claim — so that trade needs its own ADR and is not made here.
- A Scatter that completes without one `Completed` shard (`all_shards_required = false`, every
  shard refused or failed) no longer closes a Round leaving its prior Findings undispositioned:
  its rows become orphan for that Round and join the coverage of the receiving nodes still to be
  delivered theirs.
- The `MAX_PRIOR_FINDINGS_BYTES` refusal names a remedy instead of naming partitioning, which
  was the thing inflating the document it measures.
- The prior `FindingSet@1` delivered to a reviewer is the Round union, and when that is a strict
  reduction of the reducer's Set — rejected, wontfix, and authority-diagnostic rows are not
  Round rows — its Set-level provenance (`selected_report_ids`, `relation_ids`, `resolution_ids`)
  goes with the rows it described, instead of naming Findings the delivered document does not
  carry and a sandbox cannot dereference.
- Reviewer stdout is streamed to the CAS with incremental redaction under a 64 MiB ceiling
  (`MAX_REVIEWER_OUTPUT_BYTES`), counted over the redacted bytes that are actually spooled,
  published and reported; past it the process is ended and the Attempt is malformed output.
  Reviewer stderr is bounded too (1 MiB), and what is kept says so when the tail was discarded.
- An Attempt whose raw output could not be spooled or stored after the process ran is charged,
  not released, and its truncated spool is never published as the Attempt's evidence.
- A capture with no input again gives the child `/dev/null` on stdin rather than an open pipe.
- `af task` records a Worker that reached a provider and then failed on the output ceiling or its
  deadline: `WorkerCompleted@1` and the terminal record carry its published raw output instead of
  reporting no Worker and zero spend. An evaluator refused for mutating its read-only Snapshot is
  logged before the refusal, so the log and the outcome name the same Workers.
- A `GateCompleted@1` artifact is admitted against the shape `CheckResult@1` requires; that event
  type no longer accepts an unvalidated payload for want of a `schema` marker.
- Scatter shards run at the Slice policy's `max_fanout`, not the host's CPU count.
- The scheduler refills a freed slot immediately, and `max_parallel` (default 4) is a pipeline
  field shown by `af review plan`, the run, and `af review report`.
- `--timeout-secs` is documented as the per-Attempt Worker timeout, not a whole-run budget.
- Persisted `missing_nodes[].reason` for a suppressed node uses the schema spelling
  (`gate_blocked`, `upstream_missing`); Provider failure fingerprints derive from the class's
  serde name, with fingerprints stored by earlier releases still matched.
- The evaluator's Worker input is `af/evaluate-input@2`: the sandbox mutation set arrives as a
  bounded summary (counts, a sorted sample, the derived-Snapshot artifact id) instead of the
  complete path lists.
- A derived Snapshot has ceilings on what the implementer may leave behind (4096 entries,
  256 MiB). Past one, the Task ends unverified at the new `snapshot` stage naming the limit
  instead of filing build output into the Store as permanent state.
- Tasks are recorded in a typed Task log with nine schema-backed event types (ADR-0046); a Task
  event referencing an artifact the CAS does not hold is refused.
- Task records written by `0.7.1` stay valid: the fields added to `af/…@1` Task types are
  optional, not `required`, and stored `0.7.1`-era records are validated against the published
  `@1` schemas on every run of the suite. Adding an `outcome.stage` member now bumps the type to
  `af/task-outcome@2` rather than changing `@1`.
- The Worker-input markers have schemas: `af/worker-package@1`, `af/implement-input@1`, and
  `af/evaluate-input@2`. A declared `af/…@N` marker with no schema file fails the suite, and each
  artifact a Worker context manifest names is validated against the schema it claims.
- `af task show --json` history rows carry `task_id`, so an emitted row is the whole
  `TaskEvent@1` envelope.
- The evaluator's prompt names the exact `af/evaluate-input@2` fields it receives, including that
  `mutations.sample` is capped at twenty paths and `truncated` marks the list partial.
- `af onboard --refresh-lock` re-derives the digest of every pinned Worker package that still
  exists, so an implementer or evaluator referenced only from a Task pipeline can no longer keep
  a pin its files no longer match.
- `install.sh` is published inside the signed `SHA256SUMS` set and is fetched by release tag;
  the release workflow verifies each archive after packaging and obtains minisign by pinned
  digest.
- This repository's own `.af/pipelines/review.toml` is the v3 shape `af onboard` emits, gating on
  `scripts/verify.sh` with markdownlint from a lock-pinned toolchain rather than a live fetch.

## [0.7.1] - 2026-09-03

### Authority compatibility

Unchanged; `.af/af.lock` now records the `af` release that wrote it (`af onboard --refresh-lock`
re-pins). Legacy `.review/` policy is still read and upgraded in place by `af onboard --migrate`.

### Changes

- Plan consumer policies with every built af (#48)
- Validate and migrate legacy .review/ policy in af onboard (#49)
- Refresh the hub consumer fixture: one correctness reviewer (#51)
- Make wall-clock, provider usage, and dispositions visible per review (#54)
- Let a Worker node declare its own Attempt cap (#60)
- Render a Worker's exact input token-free (#61)
- Refuse a Worker input that exhausts its Attempt cap before admission (#62)
- Make af self-managed: clap tree, scoped help, completions, af self, dispatch, config ladder (#63)

## [0.7.0] - 2026-09-01

### Authority compatibility

The Ledger node must declare a `review.kernel/DemandSet@1` output; `af onboard --migrate --apply`
adds it to a legacy pipeline.

### Changes

- Harden review planning and provider admission (#31)
