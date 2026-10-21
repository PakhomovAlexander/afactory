- `make test`: two runs in one checkout, even overlapping ones, each print the test-time
  summary of their own tests. Each run writes its JUnit into a private store under
  `target/nextest-reports/`. It then copies that JUnit to `target/nextest/ci/junit.xml`, which
  CI uploads, in one rename, or removes that file if it wrote none; runs take turns, so the
  file is the last finished run's. Before, a run could print the totals of another run that
  wrote the shared file first. Exit statuses are unchanged (#219,
  [ADR-0156](docs/adr/0156-give-every-make-test-run-a-private-junit-store.md)).
