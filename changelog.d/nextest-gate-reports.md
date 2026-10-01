- Keep nextest CI JUnit reports outside read-only source trees during `scripts/verify.sh`,
  alongside the existing external Cargo target. Unique per-run report directories preserve
  failed-run evidence; temporary store-only tool configuration is removed afterward. Direct
  `make test` retains its existing report location and all test/profile settings are unchanged.
- Run `make check` and `scripts/verify.sh` as an unprivileged user, including in containers.
  The new sealed-source regression requires mode bits to deny writes; root or
  `CAP_DAC_OVERRIDE` bypasses that seal and fails the explicit precondition rather than
  silently skipping the regression.
