- The performance laboratory's shards and scratch enforcement fills take one machine lock,
  `/tmp/af-perf-machine.lock`, before touching anything shared. It does not depend on `TMPDIR`
  or `HOME`, and is held until the shard's terminal record. `review ledger` and
  `review report` are now measured over a 10,000-event Campaign: each side's own
  `af review run` writes it, and that side's Findings Ledger writer tops it up. Large Task and
  Campaign state is written once per side and shard as a sealed seed. Every sample copies the
  seed privately, and a changed seed stops the shard. Linux recipes build in a pinned build
  image (`cli.py build --build-image`): no network, only the run's directories mounted, and the
  authority closure read-only. Host builds need an explicit `--trusted-host-build`. Every
  recipe, environment and case is executable where its host provides it; elsewhere the run
  refuses with the host's reason. The CLI registry, shard and build records are now
  `af.perf-cli/2`, `af.perf-shard/3` and `af.perf-builds/4`, and earlier versions are refused
  ([ADR-0169](docs/adr/0169-coordinate-a-machine-seed-each-side-once-and-build-linux-recipes-in-a-pinned-image.md)).
