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
- Each reviewer receives and must disposition only the prior Findings it reported: the Round's
  prior-Finding document carries a per-node `assignments` partition beside the unchanged union.
- Reviewer stdout is streamed to the CAS with incremental redaction under a 64 MiB ceiling
  (`MAX_REVIEWER_OUTPUT_BYTES`); past it the process is ended and the Attempt is malformed output.
- Scatter shards run at the Slice policy's `max_fanout`, not the host's CPU count.
- The scheduler refills a freed slot immediately, and `max_parallel` (default 4) is a pipeline
  field shown by `af review plan`, the run, and `af review report`.
- `--timeout-secs` is documented as the per-Attempt Worker timeout, not a whole-run budget.
- Persisted `missing_nodes[].reason` for a suppressed node uses the schema spelling
  (`gate_blocked`, `upstream_missing`); Provider failure fingerprints derive from the class's
  serde name, with fingerprints stored by earlier releases still matched.

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
