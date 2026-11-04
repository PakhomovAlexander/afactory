- Nightly build and publish units can read their `root:af-nightly` 0440 secrets through the
  supplementary group `af-nightly`, keeping `af-perf-readers` as their primary group and Docker
  access for the build ([#575](https://github.com/PakhomovAlexander/afactory/issues/575)).
- Telemetry measurements now also admit af 0.14.0-rc.4 and 0.14.0-rc.5 under the existing
  `check-receipt@2` fixture contract; unnamed versions and mismatched requested contracts still
  refuse ([#576](https://github.com/PakhomovAlexander/afactory/issues/576)).
- The native gate publishes one complete tests record across its selected classes, preserving
  failed and flaky tests and streaming build steps. Script step labels use filename stems and
  complete Rust filters are retained in files, keeping the normal Task report fields valid.
  The receipt regression keeps execution evidence outside sealed sources and prints nested
  check logs on failure so report production failures can be diagnosed from gate output.
