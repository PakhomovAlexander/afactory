- The performance laboratory builds and supervises its samples. `cli.py build --plan run.json`
  builds a frozen plan's two products and one authority-built `perf-supervisor` from detached
  worktrees under `target/performance/<run>/`, using a credential-free private repository,
  locked offline builds and an allowlisted environment. It verifies each AF binary's embedded
  commit, overlays the authority probe from `fixtures/performance/adapters.toml` in fixed
  priority order, and writes `builds.json` (`af.perf-builds/3`) beside hashed, read-only
  artifacts. The supervisor times each child from launch to its observed exit, drains capped
  output, forwards termination signals and establishes descendant lifecycle. In a Linux sample
  container it is PID 1 and kills and verifies every escapee. `cli.py capabilities` reports what
  the host can run and why. `cli.py probe` now needs `--trusted-local`, runs under the
  supervisor, refuses spawning cases and removes its scratch only through verified tombstones.
  A failed sample keeps its original diagnostic ahead of a bounded cleanup note
  ([ADR-0166](docs/adr/0166-build-frozen-plans-and-supervise-samples-with-one-authority-instrument.md)).
