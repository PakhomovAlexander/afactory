- Telemetry, second package: the commands that run a kernel run (`af task run`,
  `af task start --execute`, `af review run`, `af provider doctor`, `af self optimize --execute`)
  now record `af.command > af.run > af.authority.load, af.plan` (one `af/TelemetryNode@1` record
  per plan node), `af.node > af.attempt` with the kernel's exact accounting, and the run's
  `af.sweep`; every other command, and every command at `off`, opens no telemetry directory. From
  the first `af.run` until the seal one consumer thread is the segment's only writer and makes
  every record visible within `flush_latency_ms`; the command's end and the seal are one request
  to it, bounded at 2 s, after which it is abandoned and publishes nothing, leaving the segment
  for the next reader to recover. Every lease that begins kernel execution now commits an
  execution marker to its Task's log, the event `TaskExecutionStarted@1`
  (`af/TaskExecutionStarted@1`) beside `TaskTransition@5`, at every telemetry level. `af telemetry runs`, `show`, `tail` and `gc`
  (`--json`: `af/telemetry-runs@1`, `af/telemetry-show@1`, `af/telemetry-tail@1`,
  `af/telemetry-gc@1`) report a run's executions — numbered by those markers, active time as
  their union, execution time as their sum, elapsed time, history `prefix:k` or `partial`, never
  complete, the open ones named beside the sealed ones in every label and total, the outcome the
  latest execution's own, by ordinal — its span tree and its live tail, which ends only at a
  published seal or a recovery (at once for a run whose every execution was already sealed,
  its sealed records printed), recovers a crashed writer's segment itself, fails naming a
  sealed segment it cannot read, a live one with interior corruption or a followed one gone
  without a seal (the `.corrupt` file another reader's recovery left), exits 1 for an unknown
  run before following anything (naming the unreadable segment instead when one could hold
  it), refuses naming a `.corrupt` segment that held one of the run's executions or exists
  while the run cannot be found, also one renamed after it started, before it ever saw it
  (checked at every look and before it exits 0), and follows a run in one Store, refusing with `--store`
  guidance, once the followed Store's pending records are printed, a second Store that starts
  the same run id while it is followed, even in a segment sealed during its startup or between
  two of its looks.
  Telemetry is the kind `telemetry` of the Storage Budget (`af/storage@4`,
  `af/storage-prune@4`, the Project Store, Survey and claim kinds' `@3` documents extended, both
  planned by the one sweep plan): every registered directory counted once, one entry per segment by what
  its eviction frees, a sealed one last used at its seal time, evicted under ADR-0144's rules
  past `[telemetry] max_bytes` or the whole budget, every file of an entry, its manifest
  included, checked again before either is unlinked and the whole entry left in place, reported
  `changed`, when one is no longer what the inventory found, with temporaries, unreferenced blobs
  and free index pages reclaimed by every applied sweep and an overlapping directory reported
  `overlapping`, never evicted as telemetry.
- Telemetry, third package: every phase of a run is a span of the crate that owns it —
  `af.admission`, `af.context.compose`, `af.harness`, `af.output.parse` and `af.usage.account`
  under an Attempt, `af.gate` with its `af.check` spans and `af.warm.restore`/`af.warm.save`
  under a check node, and `af.snapshot.capture`, `af.cas.materialize`, `af.store.append`,
  `af.store.read`, `af.git`, `af.remote_check` and `af.process` wherever the run does them — each
  named in the catalogue in `docs/architecture.md`. Every supervised process now carries its
  process id and its CPU time and peak memory through every `review-process` result, a
  timed-out, killed or cancelled one included, and its span records them (`af.rusage.*`, absent
  when the platform did not deliver a figure, never zero), read safely from the children totals
  around each reap because the workspace forbids the `unsafe` a direct `wait4` needs — every
  child reap of `af` now goes through `review-process`'s one reap lock, so a figure is never
  another child's. A check
  records `af.check.status` and its comparability key `af.check.fingerprint`
  (`af/check-fingerprint@1`, the environment only as a digest with a runtime path a placeholder
  only where a path equals it or lies under it, a relative `PATH` entry resolved against the
  check's directory, `unknown` when a part is unavailable); a kernel measurement is a run of its own (`af.run.kind = measure`,
  `af.measurement.id`). At `spans` every run an execution works on has its own configuration
  record (`af/TelemetryConfig@2`, as many 16 KiB parts as it needs, each naming its run, an
  environment-origin value as `<env>`, a file under the home as `~/…`), its `configuration` and
  `machine` sections written when the run opens and its `plan` section when the plan resolves,
  with `af.config.text` naming the pipeline's `pipeline.toml`, the Task file and `.af/af.toml`
  by the SHA-256 of their own bytes and their size;
  `af telemetry config RUN [--execution ID]` assembles it, saying per section `complete`,
  `incomplete` or `unknown` (`--json`: `af/TelemetryConfig@2`).
- Telemetry, fourth package: every recording command installs one post-commit observer on its
  Stores, told of each committed transaction once, after its commit, in committed order — a
  generic append, a Task transition, a Round publication, an atomic integration — and never of a
  rollback; from `spans` on each committed event is `af.store.event` (type, sequence, id,
  causation, artifact references, the payload's digest and size), every Report a Campaign admits
  and every change of a summarized Finding field is an `af/TelemetryFinding@2` record (with the
  admitting Worker's bound model when the kernel recorded it) and a status change an
  `af.finding.disposition` event, and its Demands are `af/TelemetryDemand@2` records keyed by the
  Demand id, naming the Demand's own Subject and its Snapshot, with `af.demand.opened`,
  `af.demand.evidence_satisfied` (an admitted reuse that satisfies a stale Demand again
  included), `af.demand.stale` and `af.demand.waived`; every summary carries the committed
  sequence of the event that caused it. From `metrics` on every committed accounting transition records the
  Attempt's charge as the Task projection holds it (`af.usage.state`, by the committed event's
  sequence) and every sidecar write its merged row (`af.usage.wall`, ordered within one
  execution only); `af telemetry runs` now sums the Attempts' latest charges. A mirror failure is
  one problem, never a Store error, and makes the run `af.ledger.coverage = incomplete`. The new
  `af telemetry ledger RUN` (`--json`: `af/telemetry-ledger@1`) prints a run's committed events,
  Finding and Demand summaries and each Attempt's charge beside its sidecar observations, from
  the telemetry directory alone, each Store log in its committed sequence, with the coverage and
  history markers.
- Telemetry, fifth package: the performance contract of design §12 is measurable.
  `make telemetry-gate` builds a release `af` and runs `scripts/telemetry-overhead.py`: `off`
  against `spans` on three pinned, credential-free bench fixtures under
  `fixtures/telemetry-bench/` (four concurrent Attempts, one Attempt streaming 50 MiB, a Review
  appending Store events), 20 randomized pairs each with private homes, the paired overhead's
  median and 95% bootstrap interval per fixture and the dense fixture's drain latency, failing
  (exit 3) past a 2% median, a 3% upper bound or a 10 ms drain difference (`--json`:
  `af/telemetry-overhead@1`); `make check` runs only its one-pair smoke.
  `scripts/telemetry-corpus.py` generates the deterministic read-latency corpus
  `telemetry-corpus-10k` into a directory the operator names and measures `af telemetry status`
  (fresh and unchanged index) and `show` on it, warm and, where the page cache can be dropped,
  cold (`--json`: `af/telemetry-read-latency@1`); an unchanged `status` now opens each blob
  prefix directory once instead of once per object. `docs/telemetry.md` gains a `Performance`
  section with the protocol and this machine's first numbers.
- Telemetry, sixth package: every Claude and Codex Attempt's streams are tapped as they arrive.
  The supervisor's drain offers each chunk to a bounded queue (`[telemetry]
  observer_queue_bytes`) without ever waiting — a full queue drops it and counts it — so the
  process, its bytes, status, deadline, held output and accounting are unchanged, and from
  `spans` on the consumer records validated structure only, timed by arrival: each Claude
  `assistant` message and each Codex turn is `af.turn` with the harness's own per-turn usage and
  stop reason, a Claude message's turn ending as soon as its line is read (a further line of the
  same message is `af.turn.continued` on that turn, which `turns` folds into its row), each `tool_use` block or Codex command and MCP item is `af.tool` with its input and
  output bytes, the other lines are events, every harness identifier is a correlation id
  (`af.corr`) and every untrusted string only a size. A frame too long to judge is withheld
  whole, a dropped chunk is `af.stream.gap` and makes the stream incomplete, and once the
  process has ended Telemetry takes its own release decision over the complete streams and a
  Codex Attempt's `-o` final message, judged for every observed process whatever its ending —
  login-challenge material, a final message that cannot be read whole, a replaced capture or a
  stream the output cap truncated withholds everything pending (`af.stream.withheld`), a
  successful reply's included — before
  `af.harness.ids` lists the originals that pass the credential rules, continued in
  `af.harness.ids.part` events past what one record holds; an identifier past the 4096 a
  process pends or longer than 256 bytes has no mapping and is counted in
  `af.harness.ids.omitted`, with `af.harness.ids.analysis = incomplete`. A Claude Attempt asks for `--output-format
  stream-json --verbose` at `spans` and `content` and keeps `json` at `off` and `metrics`, the
  invocation-cost gate (`scripts/telemetry-invocation-cost.py`, `af/telemetry-invocation-cost@1`)
  having failed; `claude --help` must declare `--verbose`. The Store now keeps of a Claude
  Attempt's stdout only its `result` line (under `stream-json` the only envelope there is), and of a Codex Attempt's its turn and error lines
  beside the `-o` final message — never the native text around them, which classification
  reads in memory — so accounting, classification and raw evidence are the same at every
  level. `af telemetry turns` and `tools` (`--json`: `af/telemetry-turns@1`,
  `af/telemetry-tools@1`) print one row per turn or tool call with its timing, usage, outcome
  and sizes.
- Telemetry, seventh package: at `[telemetry] level = "content"`, and only there, Telemetry
  keeps content as blobs, each redacted whole before any truncation with its source's digest
  and size intact and named by one event: the composed Worker input of `af.context.compose`
  (`gen_ai.content.input`, `gen_ai.content.system`); once Telemetry's release decision withheld
  nothing, a harness's assistant text and Codex agent messages (`gen_ai.content.output`), its
  thinking blocks and reasoning items (`af.content.reasoning`, with `content_reasoning`) and its
  tool arguments and results (`af.content.tool_input`, `af.content.tool_output`, with
  `content_tool_io`); a check's stdout and stderr (`af.check.output`); every mirrored Store
  event's payload (published without waiting for the consumer: a busy consumer delays no Store
  append and no lease renewal; a released stream published under the seal renews the 2 s
  shutdown bound with each content, so a release that outlasts it completes); an admitted Finding's body (`af.finding.body`); and `.af/af.toml`, the
  pipeline's `pipeline.toml` and the Task file (`af.config.text`). A document `af` composed
  treats its identities and counters as typed only in the positions its kind declares — in a Worker
  request's input payloads only where the schema of an artifact type `af` owns declares one, the
  Task's requirements always judged whole. Past `content_attempt_max_bytes` for an Attempt or
  `content_process_max_bytes` outside one, further content is dropped and counted with one
  `budget` problem (a piece publication drops is never charged), and nothing about the Worker
  changes. The index (now `af/telemetry-index@3`; an earlier one is refused until `af telemetry
  reindex`) keeps the FTS5 table `content_fts` at ingest when `content_index` is on and empties
  it when it is off. `af telemetry context RUN ATTEMPT
  [--raw]`, `reasoning RUN ATTEMPT` and `search TEXT [--since D] [--run RUN]` (`--json`:
  `af/telemetry-context@1`, `af/telemetry-reasoning@1`, `af/telemetry-search@1`) read it back;
  `search` reads a bounded number of matches and shows an excerpt only of a blob it verified,
  listing a missing or corrupt one as unreadable; below `content` the first two say the content
  was not captured and `search` is refused. No
  prompt, Warm Set, Worker Notes or optimization source reads any of it.
- Telemetry measures its bytes the way the Storage Budget measures every other kind (ADR-0159
  clause 13): the allocated bytes of regular files, a hard-linked file once, directories and
  symbolic links nothing. `af telemetry status`, `af telemetry gc`, `af storage` and
  `af storage prune` now report the same telemetry numbers on ext4 as on APFS; on Linux they
  are smaller than before by 4096 bytes for every directory under the telemetry root, which
  they no longer count.
- Telemetry merged with the 2026-10-20 security audit: `[telemetry] max_bytes` is enforced, not
  advisory — the Storage Budget's sweep evicts sealed segments past it, and `af telemetry status`
  over the cap warns that the next storage sweep evicts them (its help, `af help telemetry`,
  `docs/telemetry.md` and `schemas/telemetry-status-v1.json` say the same, and name no overhead
  gate). `af/storage-prune@4` carries the audit's `left` (what a removal left in place in a Store
  not proven af's) beside `telemetry`; `af/storage-prune@3` stays superseded and ships no
  schema. Every `review-process` result carries the capture cap's `truncation` beside the
  leader's process id and resource use, a check, an Attempt, a container command or a gate call
  past the cap fails naming it inside its `af.check`, `af.harness`, `af.process` or `af.git`
  span, which ends after that judgement: a capture rejected for its cap ends its process span as
  failed (`error.type = "failed"`) with the cap reason, whatever the exit code, its stream sizes
  and resource use still recorded, and the Linux subreaper's reaps of escaped descendants take the same reap lock. At
  `metrics` a recording command's segment header keeps `argv[0]` and the subcommand chain the
  argument parser matched, which `af task run` and the other recording commands now pass in.
- A blob published again is renamed over its existing file, so the file's age restarts with the
  publication: a sweep that runs while the new reference is still buffered — a detached Store
  mirror publication's, or an attached one's past the bounded wait — and evicts the segment that
  referenced the file before no longer collects it as an orphan before the reference is written.
