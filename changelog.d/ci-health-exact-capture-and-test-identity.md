- Preserve exact CI health snapshots across repeated observations of several failing attempts
  with one signature. Journal metadata version 2 atomically records the latest capture; changed
  evidence still refuses the Scenario Wait before recurrence updates. Version 1 state is refused
  on upgrade ([ADR-0145](../docs/adr/0145-read-only-what-this-release-writes-and-version-every-changed-shape.md)).
- Select the lexical first failing test from the combined bounded annotations and available job
  log, retaining annotation fallback when logs are unavailable.
