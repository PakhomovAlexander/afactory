- Release preparation and every CI profile now check explicit telemetry version contracts and
  exact performance lab schema ownership before merge. Declare `0.15.1-rc.1` against its unchanged
  receipt and fixture contracts; preserve Rust publication schemas and show captured stderr when
  the contained telemetry smoke lacks its run artifact.
  Release contract tests keep mutable fixture copies writable when the AF local gate seals
  source files read-only, while preserving the source seal and all drift assertions.
- Published releases now open one pin-only PR for CI and a second human merge, using
  `scripts/pin-release.sh VERSION --pr` and the scoped `RELEASE_PIN_TOKEN` secret. The old
  `--push` flag is refused. Direct pin pushes to main and the deploy-key bypass are retired;
  reruns reuse the PR and moving-main/credential refusals remain bounded and explicit. See the
  [release decision](../docs/adr/2026-11-09-review-release-contracts-and-pins-before-merge.md).
