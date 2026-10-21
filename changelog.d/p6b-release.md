- Published releases are now measured. `performance-release.yml` runs when a release run
  completes. Its read-only handoff checks that the run is this repository's `release.yml`,
  that the release is published under its exact tag and SHA, and that `sign and publish`
  succeeded in some attempt, so a later failed pin retry still counts. It claims the release's
  upstream run, tag and SHA once, before any sample, and measures only the first valid claim.
  A later run of the same release links that claim's result, and a claim without a result is
  reported incomplete and never measured again on its own; only a `remeasure_of` dispatch
  measures again, keeping the parent claim. The measurement calls the token-free
  `performance.yml` and stays advisory. A CLI trace that outgrows its evidence room is now
  always `trace_over_cap`, also when the watch cancelled the command while it was still
  running (`af.perf-schedule/2`, `af.perf-release-claim/1`, `af.perf-release-handoff/1`;
  [ADR-0180](docs/adr/0180-hand-off-published-releases-read-only-and-measure-each-claimed-identity-once.md)).
