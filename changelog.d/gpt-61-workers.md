- Move declared Review and Task pipelines to GPT-6.1 Codex Workers: Sol/high implements,
  Terra independently reviews and evaluates, with judging effort preserved. Keep the Pi
  implementer and re-pin native packages, staged examples and legacy Review packages.
- `af onboard --tasks` now generates Sol/high and Terra/high Workers on `codex-main` for every
  review-authority profile. Performance Scenarios use Terra for all independent judging roles.
- Audit the entire shipped Worker catalog and legacy pipeline pins for model and effort drift.
