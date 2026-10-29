- `.config/test-quarantine.json` (`af.test-quarantine/1`, shipped empty) lists flaky tests by
  nextest identity; `scripts/test-classes.py` classifies a listed test as `quarantine`, a class
  no plan selects, so no Task gate, pull request validation or full release runs it, while its
  siblings stay selected; `make check`'s full nextest run (`make test`) excludes it too.
  `make check`'s `test-catalog` step refuses an invalid or expired entry, more than 20 entries,
  an `#[ignore]` test, an identity outside the inventory and one a feature build's fixed
  selector reaches (quarantine covers the default-feature inventory only), naming the entry and
  the fix
  ([ADR-0264](docs/adr/0264-quarantine-flaky-tests-on-gate-evidence-and-confirm-them-on-claw.md)).
