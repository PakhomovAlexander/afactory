- The performance laboratory's diagnostic child runs (`cli.py profile`) now run in the contained
  Linux mode: `--mode linux_container --image … --scratch-fs …` takes a shard's preflight, then
  runs every execution in its own sample container, with only the run's own directory of the
  verified disk-bounded scratch filesystem writable. Outputs staged there are admitted into the
  evidence. CLI cases are diagnosed through each side's own AF binary, each execution in a
  private HOME and XDG. A state read uses that side's own seed, and every command's whole
  output is judged by its oracle. Musl shipping diagnostic recipes sit beside the GNU and macOS
  ones. A supervised run whose stdout was truncated fails before any oracle, even when its
  retained prefix parses. A step whose files together outgrow the evidence room is cancelled
  and refused, with every byte it wrote kept and counted. A seed tree is admitted and sealed
  before its confined copy into each repetition. Each trace states its region's coverage, so a
  setup-only trace is never complete (`af.perf-profiles/4`,
  [ADR-0176](docs/adr/0176-diagnose-cli-cases-and-contained-linux-runs-and-bound-every-step-and-seed-before-it-is-used.md)).
  The `af` browser now flushes typed keys before it shows a finished command's exit line, so an
  Enter pressed as the line appears is never discarded.
