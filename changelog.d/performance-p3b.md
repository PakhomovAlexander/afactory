- The performance laboratory runs frozen plans. `cli.py run` executes exactly one shard of a
  plan from its verified build, in planned order with its start and end sentinels. Nothing is
  rebuilt, retried or reordered. Each binary is rehashed before each launch. Whatever happens,
  the shard writes a terminal `shard.json`; slots past a deadline stay unstarted, and a cleanup
  that is not confirmed stops it. `cli.py aggregate` imports shard bundles from other hosts and
  recomputes the verdict. `fixtures/performance/cli.toml` declares whole-CLI commands and
  independent oracles. They check each side's package version from its own Cargo.lock, help
  tokens, a captured Task, a `task run` whose check(`true`) is proved to have run, and exact
  counts over `task show --json` on 10,000 Task events. Each side writes its own fixtures in an
  untimed setup step. `review ledger` and `review report` stay unavailable until a Campaign
  fixture writer exists. Samples and shards are now `af.perf-sample/2` and `af.perf-shard/2`,
  recording identity, metric scopes, execution and host observations with reasons for every
  unknown. The supervisor's request and result are `/2` and `/3`, adding the setup step and
  whole-stream pattern counts. Earlier versions are refused. The local macOS environment runs
  discovery and nonspawning library cases as `partial_scope`; Linux environments stay
  unavailable until validated on a host with a verified scratch. A container sample now binds
  only its own `sample/` directory, never the scratch filesystem's root. A build refuses when
  the overlaid probe closure changes during a compile, and `cli.py build --zigbuild-bin` builds
  the musl shipping recipe with exactly its pinned cargo-zigbuild and Zig.
  `cargo test --test perf_supervisor` finds its target from cargo's own layout, so a precreated
  target directory works
  ([ADR-0168](docs/adr/0168-run-frozen-shards-with-whole-cli-oracles-and-only-a-per-sample-scratch.md)).
