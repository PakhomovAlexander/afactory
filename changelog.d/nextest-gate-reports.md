# Fixed

- Keep nextest CI JUnit reports outside read-only source trees during `scripts/verify.sh`,
  alongside the existing external Cargo target. Unique per-run report directories preserve
  failed-run evidence; temporary store-only tool configuration is removed afterward. Direct
  `make test` retains its existing report location and all test/profile settings are unchanged.
