- `make check` runs its tests one process per test across every test binary at once
  (`cargo nextest run --profile ci`), with `TEST_THREADS` set from the machine's cores; the
  five tests that assert against a fixed real-time budget run alone and first, listed in
  `.config/nextest.toml`. The longest tests are split one case per test, the eight largest
  crates link their integration tests as one binary under `tests/it/`, doctests are off (there
  are none), and debug information is line tables only. CI compiles through `sccache` keyed
  by rustc inputs, caches only the crate registry, links with `lld` on Linux, and lints in a
  job beside the tests
  ([ADR-0124](docs/adr/0124-run-tests-in-parallel-processes-and-link-them-once.md)). Measured
  on a 14-core developer machine: the test step 661 s to 289 s, a cold test build 58 s to
  46 s with 122 to 48 executables and 5.7 GB to 3.5 GB, the doctest step 22 s to none.
