- Honor the manual `nobuild` label on pull requests with no source, pipeline, build input or test
  changes. Hosted CI reports builds, tests, smoke and performance work as skipped and still checks
  the AF pin and PR report. A PR changing only `.af/af.lock` qualifies.
  [ADR-2026-11-20](../docs/adr/2026-11-20-honor-the-manual-nobuild-pr-label.md)
