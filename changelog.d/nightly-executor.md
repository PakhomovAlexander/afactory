- The independent performance executor measures nightlies: `cli.py executor … submit --kind
  nightly --tag nightly-YYYY.MM.DD --head SHA` derives the baseline as the highest stable release
  reachable from the frozen main commit, keys the original by tag and work so a resubmission is
  idempotent, and refuses `--work profile` and a core nightly on a configuration whose tier is not
  `nightly`. Core results (`af.perf-executor-result/3`) name each side's built `af` by SHA-256 and
  size; request/1 and result/2 originals stay readable. The musl build image holds
  `cargo-zigbuild` and `zig` together in `/opt/af-perf-tools/bin`. The nightly tier runs four
  shards of 30 sampling minutes within a 180-minute deadline that counts the build, so a nightly's
  serial shards fit inside the unchanged hard maxima
  ([ADR-0266](docs/adr/0266-measure-nightlies-as-originals-of-the-independent-executor.md)).
