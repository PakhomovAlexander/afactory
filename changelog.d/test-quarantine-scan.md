- `scripts/test-quarantine.py scan` proposes test quarantine entries from af Tasks' gate flaky
  lists (`af task list --json`, `af/task-list@3` or `@4`, by `--list FILE` or `--state DIR`): an
  identity flaky in at least 2 distinct Tasks within the 7 days ending today becomes a
  `quarantined` entry for 14 days with its sorted Task IDs as evidence. A name counts only when
  it resolves to one default-feature nextest identity, so af 0.13.0's fixture names are listed as
  rejected; without `--write` it changes nothing, `--write` adds the entries through the
  quarantine guards up to the 20-entry cap and names the overflow, and `--json` prints one
  `af.test-quarantine-scan/1` document
  ([ADR-0268](docs/adr/0268-quarantine-flaky-tests-on-gate-evidence-and-confirm-them-on-claw.md)).
