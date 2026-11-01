- The owner's nightly train runs unattended on the VM as systemd units: `af-nightly.timer`
  starts `af-nightly-build.service` at 22:05 UTC, and the build hands over to
  `af-nightly-measure.service`, which hands over to `af-nightly-report.service` whatever the
  measurement did, which hands over to `af-nightly-publish.service` (units in
  `scripts/performance/service/`). The new `scripts/nightly/measure.py submit --config
  nightly.toml` submits a built night's core and telemetry originals to the nightly executor and
  cycles it until both end or `[measure] deadline_minutes` (default 300) pass. Every step takes
  `--date`, or `--night-of UNIT` (the UTC date systemd last started UNIT, which the units use),
  or else today's UTC date, and prints the night it chose; `report.py compose --output-root DIR`
  writes `DIR/<date>` or the next free `DIR/<date>.<k>`, `publish.py publish --report-root DIR`
  publishes the newest, and the configuration's new `reports_root` is the default root of both.
  The build also reads the key's passphrase from the systemd credential
  `af-nightly-sign-password`. [`docs/nightly.md`](docs/nightly.md#the-nightly-chain) is the
  runbook ([ADR-0273](docs/adr/0273-run-the-nightly-chain-as-systemd-units-on-the-owners-vm.md)).
