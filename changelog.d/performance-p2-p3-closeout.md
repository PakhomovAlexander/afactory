- The performance laboratory now writes the source capture and state projection fixtures once
  per side and shard through each side's own probe. Each writer's check is judged against its
  own golden entry before the fixture is sealed, and every sample copies it privately as
  untimed setup. How a recipe builds, its image included, is frozen in the plan's recipe
  identity (`cli.py plan --build-image` or `--trusted-host-build`), and a build never chooses
  another image. A `name@sha256:` image is compared by its full reference. The `review ledger`
  and `review report` oracles check every declared Finding and a findings summary consistent
  with them. The supervisor's host backstop covers a sample's setup as well as its child. The
  whole run is bounded by one execution start and total deadline, recorded when its build
  begins. The plan, build, shard, probe request, golden, CLI and registry contracts are now
  `af.perf-run/3`, `af.perf-builds/5`, `af.perf-shard/4`, `af.perf-probe-request/2`,
  `af.perf-golden/2`, `af.perf-cli/3` and `af.perf-suite/3`, and earlier versions are refused
  ([ADR-0170](docs/adr/0170-seed-native-fixtures-freeze-the-build-image-and-bound-the-run-by-one-deadline.md)).
