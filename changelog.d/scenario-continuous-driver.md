- Add retained native Scenario driving with a 30000 ms idle cadence, two finite scoped
  publication checkpoints, atomic checkpoint/resume consumption, bounded resume allowance,
  durable blockers and restartable foreground service operation. Validate the complete schedule
  before paid startup and preserve immutable Task, repair and publication deadlines.
- Canonicalize the command-Worker refusal fixture from its native template pin, preserving
  strict source identity on aliased macOS temporary roots and zero-dispatch refusal.
  See the [driver decision](../docs/adr/2026-11-16-drive-scoped-implementation-with-retained-progress.md).
