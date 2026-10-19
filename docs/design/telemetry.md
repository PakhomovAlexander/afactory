# AF telemetry and observability

**Design note — 19 October 2026, revision 6 after five af reviews.**

This note designs the component that records what `af` does while it works: every kernel
phase, every process it supervises, every model turn, tool call and reasoning summary a harness
exposes, the configuration a run ran under, the events and Findings the Ledger is built from, the
resources each step used, and every failure with its cause. It is direction for review, not
shipped behaviour. Repository snapshot: `bf027de6` (`origin/main`, af 0.12.0-rc.4; Claude Code
2.1.303 and codex-cli 0.162.1 are the harness versions the facts below were read against). Where
this note and an accepted ADR disagree, the ADR wins; the boundaries of §3 are proposed as
ADR-0151.

## 1. Decision in brief

Build one configurable component, **Telemetry**, with these properties:

1. **Its own store.** Telemetry is written under the state root's `telemetry/`, never into a
   Task or Campaign Store (`events.sqlite`, its `attempt_wall` sidecar, or the CAS). The kernel's
   execution authority never reads it: identity, replay, the Ledger, convergence, budget
   reservations, admission and every Worker input are the same whether Telemetry is on, off or
   deleted. Storage housekeeping (ADR-0144) measures and evicts it like every other kind,
   reading only the structural metadata §3.1 names.
2. **One data model for everything.** Every fact is a span, an event on a span, a bounded
   record, or a content-addressed blob a span or event points at, in the OpenTelemetry shape
   with af's own attribute vocabulary and the GenAI semantic conventions' names where they apply.
   Metrics are derived from spans on read, never stored a second time.
3. **Four levels, machine-only.** `[telemetry] level` is `off`, `metrics`, `spans` (the default)
   or `content`; only machine layers may set it. `content` is the level that keeps model
   contexts, tool arguments and results, reasoning summaries and thinking blocks, bounded raw API
   bodies and check output. Credential-looking material is withheld at every level.
4. **Capture from four sources.** The kernel's own phase spans; every supervised process with
   its exit and resource use; the harness event streams af already reads (Claude `stream-json`,
   Codex `--json`), observed as they arrive and turned into turn, tool-call and reasoning
   records; and, optionally, the harnesses' native OpenTelemetry export, received on a loopback
   port that lives only while the command that runs Attempts is alive.
5. **Answers on the command line.** `af telemetry` lists runs, shows span trees, follows a live
   run, groups failures, finds flakes, ranks bottlenecks, computes the critical path, diffs two
   runs, dumps a model context, searches content, runs read-only SQL, and exports OTLP JSON per
   signal, Perfetto and JSONL. Every command has `--json` and a versioned document type.
6. **Bounded.** Every byte Telemetry retains is counted once in the `[storage]` budget as the
   kind `telemetry`, with its own cap below the budget; it is written append-only in
   per-process segments with a total size limit, which a crash cannot corrupt; and at `spans`
   it costs at most 2% of kernel wall time on the pinned workload of §12, a release gate.

Nothing here changes what a Worker sees (ADR-0028), what the Store records as the authority for
a result, or any sandbox, budget or gate.

## 2. What it must answer

The four jobs the owner named, each reduced to the question it answers and the facts that
answer it:

| Job | Question | Facts recorded |
| --- | --- | --- |
| Performance | Where did the wall time of this run go, and how does that compare with another run? | A span tree with self and total time per phase, Attempt, model turn and tool call; CPU time and peak memory per process; admission waits; gate warm hits and misses; git and Store operation times. |
| Bottlenecks | Which phase, node or wait bounds the run, and which bound runs in general? | The critical path through the plan's dependency graph; self-time distributions per span name (p50, p95, max, share of wall); queueing against service time. |
| Correctness | Did the Worker, the check and the kernel do what the plan said, and is a failure a flake? | Output-contract results per Attempt; check outcomes keyed by Snapshot and check digest, so the same inputs with a different outcome is a flake; Finding summaries against their later dispositions; unknown-usage causes (ADR-0143); model-identity refusals (ADR-0147). |
| Failure analytics | What fails, how often, since when, and what did it look like? | One failure record per failed Attempt, check, process or run with the kernel's own class and reason, the span path, the redacted stderr excerpt, exit code or signal, retry events, and at `content` the turn and tool call in flight. |

"Everything" the owner asked to see, its source, and the lowest level that keeps it:

| What | Source | Level |
| --- | --- | --- |
| Model context: the exact Worker input af composed (manifest, prompt bytes, estimated tokens) | kernel, `af.context.compose` | sizes and digests at `spans`; bytes at `content` |
| Model context: the request the harness actually sent (system prompt, tool definitions, messages), bounded by the harness's own content limit | harness-native raw API bodies (Claude Code), package P9 | `content` |
| Tool calls: name, arguments, result, duration, outcome | harness stream: Claude `tool_use` and `tool_result` blocks; Codex `command_execution`, `mcp_tool_call`, `file_change`, `web_search` items | names, sizes and timings at `spans`; arguments and results at `content` |
| Reasoning: Codex reasoning summaries, Claude thinking blocks | harness stream | counts and sizes at `spans`; text at `content` |
| Configuration: effective configuration with origins, pipeline, policy revision, lock, bindings, Provider registry, versions, host | kernel, once per run, metadata only | `spans`; the texts of the pipeline, the Task file and the policy at `content` |
| Ledger: every committed Store event with type, sequence and causation; every admitted Finding and disposition as a bounded summary | Store commit observer; Finding summaries | structure and summaries at `spans`; event payloads and Finding bodies at `content` |
| Usage and charge: the exact token counters and the charge per Attempt | kernel accounting (ADR-0078, ADR-0143) | `metrics` |
| Resources: CPU time and peak memory of every supervised process; host load and free disk at start | kernel, `wait4(2)` | `metrics` |
| Failures: class, reason, stderr excerpt, exit, retries | kernel and harness stream | classes and counts at `metrics`; records at `spans` |

## 3. Boundaries

These are the clauses proposed for ADR-0151. Each is pinned by a test named in §16.

1. **The kernel's execution authority never reads Telemetry.** No code path that decides a
   state transition, an identity, a replay, a Ledger entry, a budget reservation, an admission
   or a Worker input opens a telemetry record. Storage housekeeping under ADR-0144 — the
   inventory, the sweep, the free-disk floor and the `storage_sweep` record the sweep leaves in
   the Task log — reads telemetry's **structural metadata** and nothing else: file sizes and
   times, lock state, `segment` headers and `seal` records, the `.refs` manifest of a sealed
   segment and the `blob` and `blob_ref` records in the valid prefix of an unsealed one, locked
   or not (§6), and the index's size. It never
   reads a span, an event, an attribute, a blob or an analytic, and no housekeeping decision
   consults a Task's or Campaign's execution decisions. Telemetry is written through one leaf
   crate; its reader module is linked only by `af telemetry`, `af storage` and the TUI.
2. **Telemetry is never a Worker input.** Nothing captured here is composed into a prompt, a
   Warm Set, Worker Notes or an optimization source. Reasoning summaries and thinking blocks are
   recorded for the operator at `content` and reach nothing else; ADR-0028 and the
   self-optimizer's rule that thinking blocks never reach an Observation stand unchanged.
3. **Telemetry never steers.** Under identical clocks and storage conditions, with Telemetry
   `off` and with `content`, the Store's event log of a fixture run is byte-identical.
   Telemetry changes no flag that alters what a harness or a model does; it may change only
   what a harness reports (an output format or a reporting flag), and §7.3 lists every such flag.
   A variable af adds to a harness's environment is visible to a tool that inspects that
   environment, as the adapters' `CLAUDE_CODE_*` variables are today; §7.6 names every such
   variable, and none of them makes the harness carry context into a model request or a tool
   invocation, which is why native tracing stays off.
4. **Machine-only configuration.** `[telemetry]` joins `[self]` and `[storage]` in the
   machine-only tables: a directory, project or local `.af/af.toml` that carries it is reported
   and ignored. The environment form is `AF_TELEMETRY__<KEY>`.
5. **Nothing leaves the machine by default.** The default sink is the local store. An OTLP sink
   is configured explicitly, pushes after the result is printed, is bounded in time and never
   changes an exit code.
6. **Credentials are withheld at every level, and identities survive.** Every untrusted string
   passes the one redaction implementation (the rules review-runner applies to Provider-authored
   text today, moved to the leaf crate) before any truncation, so a bound never removes the
   evidence that makes a fragment a credential, and a line or document too long to be judged
   whole is withheld whole, never cut; a harness's untrusted content, from its streams or its
   native telemetry, is published only after Telemetry's own whole-capture privacy decision over
   everything the Attempt produced (§7.3), and what is recorded live is validated structure
   with kernel-derived correlation ids in place of the harness's own identifiers (§7.3);
   kernel-produced, validated identity fields and numeric counters are typed
   fields the heuristic does not see; environment values are never recorded; Provider grants are
   replaced by placeholders as the adapters already do for captured streams.
7. **Telemetry fails open for the run and closed for the reader.** A telemetry-only write
   error (an unwritable directory, a quota, a locked index) is recorded once as a problem and
   the run continues; ADR-0144's free-disk floor still refuses a check, a measurement or an
   Attempt on a volume below `min_free_bytes`, telemetry or not. A segment or index written
   under another schema version is refused by the reader and never migrated (ADR-0145).
8. **Bounded disk, under the budget's rules.** Every physical byte under `telemetry/` is counted
   once in the Storage Budget; eviction is by sealed segment, least recently used first, never an
   entry in use or used within the last hour, and by age only when `[storage] auto_gc` is on or
   an operator applies a prune (ADR-0144). Corrupt segments, orphaned temporary files and the
   index's unused pages are reclaimed by the same sweep (§6). A live segment has a total byte
   limit of its own. Every directory Telemetry has ever written on this machine is registered
   and inventoried, whatever the current level or `dir` (§6).

## 4. What state of the art means here

| Practice | Where it is established | Decision here |
| --- | --- | --- |
| Traces, not logs, as the primary record; metrics derived from them | OpenTelemetry's data model; Perfetto; Bazel's JSON trace profile | Spans and events are the only primary records; every metric in §9 is a query over them. |
| A shared vocabulary for agent telemetry | OpenTelemetry GenAI semantic conventions (`gen_ai.*`, still *Development* status, now kept in their own repository) | Use `gen_ai.operation.name`, `gen_ai.provider.name`, `gen_ai.request.model`, `gen_ai.usage.*`, `gen_ai.tool.*`, `gen_ai.agent.name`, `gen_ai.conversation.id` and `error.type`; pin the names af uses in `af/TelemetrySpan@1`, so upstream churn never breaks a reader. |
| Content capture is opt-in and separate from structure | Claude Code redacts prompts, tool input and tool output unless `OTEL_LOG_USER_PROMPTS`, `OTEL_LOG_TOOL_DETAILS` and `OTEL_LOG_TOOL_CONTENT` are set; the GenAI conventions mark `gen_ai.input.messages` and its siblings *Opt-In* | The `content` level; content lives in blobs, never in attributes. |
| The harness is the authority on its own API traffic | Claude Code's `claude_code.api_request`, `api_error` and `tool_result` events and its inline, length-bounded raw API bodies; Codex's `[otel]` table | Harness-native telemetry is the fourth source (P9), received on loopback, attributed by `OTEL_RESOURCE_ATTRIBUTES`. |
| Append-only, crash-safe local files with a rebuildable index | Perfetto's streaming protobuf; the OTLP file exporter; journald | Per-process JSONL segments under an exclusive lock, sealed on exit, recovered to their last complete line; a SQLite index rebuilt from the segments by `af telemetry reindex`. |
| Per-process resource accounting | `wait4(2)`; `/usr/bin/time -v`; Bazel's execution log | `af.rusage.*` on every supervised-process span. |
| Critical path and self time over a dependency graph | Bazel's JSON trace profile critical path; Chrome's trace viewer | `af telemetry critical-path` and `af telemetry bottlenecks`. |
| Flake detection from repeated identical inputs | Bazel's and Buildkite's test analytics | Check outcomes keyed by Snapshot digest and check digest. |
| Run-to-run comparison | The continuous performance laboratory (this repository, in implementation); trace comparison in Chrome's tooling | `af telemetry diff`. |
| A real query language as the escape hatch | Honeycomb; ClickHouse-backed tracing; Perfetto's trace processor SQL | `af telemetry sql` over the index, read-only. |

The imports rejected are in §14: an in-process OpenTelemetry SDK with its exporters, a collector
daemon, a Prometheus endpoint, sampling.

## 5. Data model

### Runs and traces

A **Telemetry Run** is the work one kernel run id covers inside one Store: a Task run
(`af task run`, a resume), a Campaign run (`af review run`), a self-optimizer run, a kernel
measurement. A kernel run id is not machine-global — `task_run_id` hashes only the Task ID under
a fixed namespace, and two Stores may hold the same Task ID — so Telemetry namespaces every run
by its Store instance: `af.store_id` is `SHA-256` of the Store directory's canonical absolute
path (the Store carries no instance identity of its own, and this one survives a reboot and is
explainable), and `trace_id` is the first 16 bytes of
`SHA-256("af/trace@1:" || af.store_id || ":" || run_id)`. Every process that works on the run in
that directory — a resume, a later lease — appends to the same trace; a copied Store directory is
a new instance and a new trace; telemetry read from a copied directory with `--dir` keeps the ids
as recorded. The kernel's `run_id` stays an attribute on every record. A process that runs no
kernel run (`af storage`, `af provider status`, `af task show`) records nothing. Telemetry ids
are not kernel identities and carry no authority; the kernel's identities are the join keys.

**Executions.** A trace holds one **execution** per command or lease that worked on the run:
each process's `af.command` span, with `af.execution_id` equal to its segment id, and under it
that execution's `af.run` span covering the time this process spent on the run. Configuration
(§7.4), plan identity (`af.plan` with the pipeline digest and the node list) and the node graph
are recorded per execution, because a resumed or refreshed Task may run under another
configuration or plan hours later. Executions can overlap: a lease is taken over after it
expires without waiting for the fenced predecessor to exit (ADR-0089), so a suspended writer may
wake and end its `af.run` span after its successor started. The reader defines, per run:
**active time**, the union of its executions' observed `af.run` intervals, an overlap counted
once; **execution time**, the sum of the same intervals, reported beside it as
`af.execution.duration_sum`; **elapsed time**, from the first execution's start to the last
one's end, idle gaps included; and **wall time** of an execution, its own `af.run` duration. An
interval without an end (an execution whose segment was never sealed) ends at its segment's last
record time and is marked `open`. `runs` prints active, execution and elapsed time; `config`,
`critical-path` and `diff` work on one execution, the latest by default, `--execution ID` for
another, and `diff --by run` compares two runs' active totals. Fixtures pin a resume under a
changed configuration after an idle interval, and a paused writer that loses its lease, a
successor that starts, and the original process ending later: active time counts the overlap
once.

**Retained history.** Capture completeness (nothing dropped or cut while writing) and retained
completeness (nothing evicted since) are different facts, and a reader cannot learn from retained
segments how many executions a run had: a later execution may have been evicted or found corrupt,
and the kernel's own count lives in the Store, which `af telemetry` does not consult. Each
execution's `af.run` span records `af.execution.ordinal`, its number among the run's executions,
which the kernel derives from the Store's committed lease records when it starts, and
`af.store.sequence_at_start`, the Store's highest committed event sequence at that moment. A
reader, after ingest, reindex or a `dir` export, labels a run's history `prefix:k` when
executions 1 to k are retained with their seals and no ordinal between them is missing, and
`partial` when one is; it never says a run's history is complete. Every total, ledger view and
analytic over a run says `retained executions 1–k` (or `partial`, with the missing ordinals
listed), and a figure a retained accounting snapshot (§7.5) supplies is shown with its
snapshot's sequence. Fixtures evict the first execution of a resumed run (`partial`), and remove
the latest execution before a reindex and a `dir` export (`prefix:1`, never complete).

### Records

A segment holds one JSON object per line. Every record carries `seq`, `t` and `schema`: `seq`
is the record's sequence in its segment, a JSON integer that is 1 for the header and one more
than the previous record's for every record the writer appends, unique within the segment and
bounded by `segment_max_bytes`; `t` names the record; `schema` names its versioned type. A
derived segment (§11) keeps every record's `seq` unchanged, so its sequence has gaps and is
never renumbered, and every identity below that is built from a sequence uses `seq`.

| `t` | Type | Fields |
| --- | --- | --- |
| `segment` | `af/TelemetrySegment@1` | `segment_id`, `af_version`, `af_commit`, `pid`, `parent_pid`, `argv` (redacted), `cwd` (home-relative), `host` (`os`, `arch`, `cpus`, `memory_bytes`, `load1`, `free_bytes`), `started_unix_ns`, `mono_origin_unix_ns`, `level`. The first line of every segment. |
| `span_start` | `af/TelemetrySpan@1` | `trace_id`, `span_id`, `parent_span_id` (null for a root), `name`, `kind` (`internal`, `process`, `client`), `start_unix_ns`, `attributes`. |
| `span_end` | `af/TelemetrySpan@1` | `span_id`, `end_unix_ns`, `status` (`ok`, `error`, `unset`), `attributes` known only at the end (exit, usage, outcome). |
| `event` | `af/TelemetryEvent@1` | `span_id`, `time_unix_ns`, `name`, `attributes`, optional `blob` (digest) and `blob_media_type`. |
| `config` | `af/TelemetryConfig@1` | one part of the metadata-only configuration snapshot of §7.4: `execution_id`, `part`, `parts`, `complete`, and the rows this part carries; as many parts per execution as the record bound needs. |
| `node` | `af/TelemetryNode@1` | one plan node of an execution: `execution_id`, `plan_digest`, `node_id`, `kind`, `package`, `depends_on` (a record field of up to 64 node ids, outside the attribute bounds), `expected_nodes`. §7.1. |
| `finding` | `af/TelemetryFinding@1` | the bounded summary of an admitted Finding, §7.5. |
| `demand` | `af/TelemetryDemand@1` | the bounded summary of a Demand, keyed by Demand id, §7.5. |
| `blob` | `af/TelemetryBlob@1` | the shared object: `digest` (`sha256:` and 64 hex, over the retained redacted bytes), `bytes` (retained), `stored_bytes` (compressed), `media_type`, `redaction` (`af-redact/1`). Written once per digest per segment; the content is in `blobs/`. |
| `blob_ref` | `af/TelemetryBlobRef@1` | one occurrence: `occurrence_id` (`segment_id:seq`), `digest`, `span_id` and the owning event's `seq`, `source_digest` (over the original bytes), `original_bytes`, `truncated`, `state` (`stored`, `dropped`). An event names its content by `occurrence_id`. |
| `problem` | `af/TelemetryProblem@1` | `time_unix_ns`, `what` (`write`, `blob`, `receiver`, `budget`, `redaction`, `sink`, `observer`), `reason`. Recorded once per distinct `what` and `reason` per process. |
| `seal` | `af/TelemetrySeal@1` | `ended_unix_ns`, `exit` (`ok`, `error`, `signal`, `recovered`), `spans`, `events`, `blobs`, `dropped`, `dropped_chunks`, `discarded_bytes`, `refs` (the name of the segment's `.refs` manifest). The last line; a recovering reader writes it with `recovered`. |

**Rules.** Timestamps are unix nanoseconds encoded as decimal strings in segments (the OTLP JSON
mapping; a JSON number loses precision in some readers) and as integers in the index. Durations
come from the monotonic clock; the segment header records the monotonic-to-unix offset once. A
`span_id` is the first 8 bytes of `SHA-256(segment_id || ":" || seq)` of its `span_start`
record: unique across writers and reproducible from the segment, in a derived segment too.

**Record bound.** One encoded record, keys, escaping and arrays included, is at most 16 KiB.
Attributes are a flat map from string to string, integer, float, boolean, decimal string or
array of strings: at most 64 per record, keys at most 128 bytes, string values at most 1 KiB,
arrays at most 32 elements and 4 KiB together. A value over its bound becomes, at `content`, a
blob occurrence the attribute names; at every other level it is cut at the bound with a
trailing `…` marker. A record that would still exceed 16 KiB drops attributes from the end of
the writer's attribute order until it fits; `af.truncated_fields` counts cut values and
`af.dropped_fields` counts dropped ones, and 256 bytes of every record are reserved for these two
counters so they are never themselves dropped. The same bound and rule hold at every level,
including the levels that forbid content blobs.

**Exact counters.** The usage and charge counters of `af/TaskTokenUsage@3` are canonical
`u128` decimal text in the kernel (`DecimalU128`). They are decimal strings in every record,
`TEXT` in the index, and are aggregated exactly in af. An OTLP numeric metric carries such a
value only when it fits the metric's number type exactly (an integer sum up to 2^63 − 1); a
value that does not fit is omitted from the metric, the exact string stays on the span
attribute, and the export's resource lists the omitted metric names in `af.omitted_metrics`. A
value the source did not deliver is absent, never zero (ADR-0143).

### Attribute vocabulary

`af.*` carries kernel facts; `gen_ai.*` carries the conventions' names with their meaning;
`process.*` and `error.type` are the stable OpenTelemetry names.

| Attribute | On | Meaning |
| --- | --- | --- |
| `af.store_id`, `af.run_id`, `af.run.kind` (`task`, `review`, `optimize`, `measure`), `af.execution_id` | every span | the Store instance, the run and the execution (§5) |
| `af.task_id`, `af.campaign`, `af.round`, `af.epoch` | run and below | the Task or Campaign, its Round and epoch |
| `af.node_id`, `af.node.kind`, `af.attempt_id` | node and below | the plan node and its Attempt |
| `af.worker`, `af.provider`, `af.provider.kind` (`claude`, `codex`) | attempt and below | the Worker package as `name@version`, the registry Provider and its kind |
| `af.native` (`received`, `unavailable`) | `af.harness` | whether the harness's native telemetry (§7.6) arrived for this Attempt |
| `af.stream.withheld`, `af.native.withheld` | `af.harness` | the Attempt's release decision withheld its pending content; no stream content, or no native content, was published (§7.3, §7.6) |
| `af.corr` | live harness records | a kernel-derived correlation id standing in for a harness-supplied identifier until the release decision (§7.3) |
| `af.snapshot`, `af.policy_rev`, `af.pipeline`, `af.pipeline.digest` | run | what was reviewed or built, under which authority |
| `af.phase` | internal spans | the span name of §7.1 |
| `af.outcome`, `af.failure.class`, `af.failure.reason` | attempt, check, process, run | the kernel's own class (`model_refused`, `model_unavailable`, `invalid_output_contract`, `deadline_expired`, `gate_failed`, `unknown_usage`, `refused`, `unavailable`, `failed`, `timed_out`, `lease_lost`) and its reason text |
| `af.usage.input_tokens`, `af.usage.output_tokens`, `af.usage.cache_read_tokens`, `af.usage.cache_write_tokens`, `af.usage.reasoning_tokens`, `af.usage.chargeable_tokens` (decimal strings), `af.usage.observation` (`complete`, `incomplete`, `unknown`), `af.usage.unknown_cause` | attempt, turn | the exact counters of `af/TaskTokenUsage@3` and the observation state (ADR-0078, ADR-0143); the Attempt's values are the kernel's accounting, the turn's are the harness's per-turn report (§9) |
| `af.context.bytes`, `af.context.estimated_tokens`, `af.context.manifest` (digest) | `af.context.compose` | the composed Worker input |
| `af.check.name`, `af.check.fingerprint`, `af.check.status` (`passed`, `failed`, `not_run`), `af.check.warm` (`hit`, `miss`, `none`) | check | the declared check, its comparability fingerprint (§9), its outcome and the warm cache result |
| `af.admission.wait_ms`, `af.admission.reserved_tokens` | admission | queueing before dispatch |
| `af.store.event.type`, `af.store.event.sequence`, `af.store.event.id`, `af.store.event.causation_id` | store event mirror | the mirrored Store event |
| `af.ledger.coverage` (`complete`, `incomplete`) | run | whether every committed event, Finding and Demand summary of the run reached Telemetry at capture time |
| `af.history` (`prefix:k`, `partial`), `af.execution.ordinal`, `af.execution.duration_sum`, `af.store.sequence_at_start` | run, execution | retained completeness and execution time (§5) |
| `af.usage.sequence`, `af.usage.source` (`usage_observed`, `settled`, `released`) | `af.usage.state` events | the ordering key (the Store's committed event sequence) and the committed transition of an accounting snapshot (§7.5) |
| `af.usage.wall.sequence` | `af.usage.wall` events | the record's `seq`, ordering sidecar observations within one execution only, a separate series (§7.5) |
| `af.measurement.id` | measure | the kernel measurement (ADR-0132) the spans belong to |
| `af.git.op`, `af.cas.op`, `af.cas.bytes`, `af.store.op` | git, CAS and Store spans | the operation |
| `process.pid`, `process.command` (the program only), `process.exit_code`, `process.signal`, `af.rusage.user_ms`, `af.rusage.system_ms`, `af.rusage.max_rss_bytes`, `af.stdout_bytes`, `af.stderr_bytes`, `af.stderr_held` | process spans | the supervised process |
| `gen_ai.operation.name` (`invoke_agent`, `chat`, `execute_tool`), `gen_ai.provider.name` (`anthropic`, `openai`), `gen_ai.agent.name`, `gen_ai.request.model`, `gen_ai.response.model`, `gen_ai.conversation.id`, `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`, `gen_ai.response.finish_reasons`, `gen_ai.tool.name`, `gen_ai.tool.call.id`, `gen_ai.tool.type` (`function`) | harness, turn and tool spans | the conventions' names; `gen_ai.usage.input_tokens` includes cached tokens as the convention says, and the exact split stays in `af.usage.*` |
| `error.type` | any span with status `error` | the stable OpenTelemetry attribute, equal to `af.failure.class` |
| `af.redacted`, `af.truncated_fields`, `af.dropped_fields` | any record | how many values the redaction withheld, the bound cut, the bound dropped |

`gen_ai.input.messages`, `gen_ai.output.messages` and `gen_ai.system_instructions` are never
attributes; at `content` their bytes are blobs named by events (`gen_ai.content.input`,
`gen_ai.content.output`, `gen_ai.content.system`).

## 6. The Telemetry Store

```text
$XDG_STATE_HOME/af/telemetry/
  v1/
    segments/<started_unix_ms>-<pid>-<nonce>.jsonl      live: one writer under an exclusive lock
    segments/<started_unix_ms>-<pid>-<nonce>.jsonl.gz   sealed: compressed on exit, immutable
    segments/<started_unix_ms>-<pid>-<nonce>.refs       sealed: the blob digests it references
    blobs/<aa>/<sha256>.gz                               content-addressed, compressed, shared
    gc.lock                                              shared by publishers and readers, exclusive by the sweep
    index.lock                                           shared by every index connection, exclusive to replace the index
    index.sqlite (+ -wal, -shm)                          rebuildable projection over the segments
$XDG_STATE_HOME/af/telemetry-dirs.v1.json                every telemetry directory ever written on this machine
```

**Writer.** A process that starts a kernel run opens one live segment lazily, at its first span,
and never before: `af --help`, `af --version` and every read command other than `af storage`
and `af telemetry` never touch the directory. The writer takes an exclusive advisory lock
(`flock`) on the live segment for its lifetime; the lock, not the PID, says whether a writer is
alive. Records are serialized into a buffer of `flush_bytes`, guarded by a mutex, and appended
with one `write(2)` of whole lines when the buffer is full, when a span of depth at most 2 ends,
and at exit. The one thread Telemetry adds is the **consumer**, and it is the **sole owner** of
the segment: the open file, every `write(2)`, the manifest, the seal, the compression and the
rename are its work and no other thread's; kernel threads only append to the buffer and signal
it (until P2 adds the thread, P1's writer runs the same consumer code on the appending thread,
so there is never a second owner). It starts when the process opens
its `af.run` span — at `off` there is no segment and no consumer — and stops when that span
ends, and it waits on one condition with one deadline. It wakes when the stream queue of §7.3
receives a chunk, when a kernel thread appends a record to the buffer, or when the deadline
passes, the deadline being the oldest pending record's time plus `flush_latency_ms`; after each
wake it drains the queue, writes, and flushes the buffer when the queue is empty or the deadline
has passed, then recomputes the deadline and sleeps. There is no periodic timer and no polling:
a quiet process sleeps without a deadline. So while a run is open every record — kernel or
stream, at any depth, with the harness silent or busy — is visible in the live segment within
`flush_latency_ms` of its creation, and a stream line within that bound of its arrival. A
fixture drains the queue, then emits a deep kernel record while the child stays silent, and
`tail` sees it within the bound. One process writes exactly one segment; concurrent
Attempts in that process share it through per-thread span stacks whose parent ids are handed
to the worker threads explicitly.

**Segment limit.** `segment_max_bytes` bounds the live file across every record kind. The writer
reserves the last 64 KiB for terminal records (`problem`, `seal`); when the rest is exhausted it
accepts no further record of any kind, keeps only bounded in-memory counters of what it dropped,
records one `problem`, and at exit writes the seal from the reserve with the `dropped` count.
Content outside an Attempt (check output, configuration texts) is bounded by
`content_process_max_bytes` the same way an Attempt's content is bounded by
`content_attempt_max_bytes`.

**Sealing and recovery.** At exit the consumer writes the `.refs` manifest (the sorted digests
of every blob its records reference, one per line), appends `seal`, compresses the segment and
renames it. **Shutdown** is bounded: when the run ends the main thread signals the consumer and
joins it for at most 2 s; a consumer that does not stop in time — a write blocked on the volume,
a stalled queue — is abandoned, and the process exits with the segment unsealed: no other thread
seals, compresses or replaces it, the records never written are simply absent, the exit status
is the run's, and the lock ends with the process, so the next reader recovers the segment as
below with everything the consumer had flushed; `status` counts an unlocked `.jsonl` segment as
`unsealed` until then. A reader that can take the lock of a `.jsonl` segment owns its recovery, exclusively:
it keeps the longest prefix of complete, valid lines, records the discarded suffix as
`discarded_bytes` in the seal it appends with `exit = "recovered"`, marks spans without an end
`end_unix_ns = null`, writes the manifest from the kept records, then compresses and renames. An
invalid line before a valid one is interior corruption: the segment is renamed `.corrupt`,
excluded from every query, counted in the budget as an entry of its own and listed by `status`.
Compression is `flate2` with its pure-Rust backend; the extension names the codec, so a later
codec coexists.

**Blobs.** A blob's identity is the digest of what is stored: the writer takes the source bytes,
redacts them whole under the current redaction version — line by line for text, as one document
for JSON — and only then, when needed, truncates the redacted result to `content_max_blob_bytes`
(head and tail halves), so a bound never removes the assignment name or the JSON structure a
credential is judged by; a single line or JSON document longer than `content_max_blob_bytes`
cannot be judged whole and is withheld whole, as review-runner withholds a line it cannot bound.
The `digest` is `SHA-256` of the retained bytes before compression. Fixtures place a credential
across each truncation boundary and an oversized line. The same source captured under another
bound or another redaction version yields
other retained bytes and so another identity, never a wrong reuse; the occurrence record carries
`source_digest` and `original_bytes` so the two captures can be related, and `truncated` and
`redaction` describe the occurrence, not the shared object. A blob file is written once per
digest into a temporary file under `blobs/` and renamed into place; an existing file of that
digest is reused after its size is checked, and `reindex` verifies every file's digest. Blobs are
shared across segments and runs: an identical Worker input is stored once.

**Publication.** A blob file exists before any reference to it can be read, and a reference is
published before the file can be collected: the writer takes `gc.lock` shared, non-blocking;
renames the completed blob file into place; appends the `blob` object record (once per digest
per segment) and the `blob_ref` occurrence record and flushes the buffer; releases the lock. If
the lock cannot be taken at once — the sweep holds it exclusively — the blob is dropped, counted
in `dropped`, and the occurrence record says `state = dropped`; execution never waits on
collection. A crash after the rename and before the flush leaves an unreferenced file, which the
sweep removes as an orphan once it is an hour old; a crash before the rename leaves a temporary
file, removed the same way. A reader that reads blob files holds `gc.lock` shared for the read;
a reference whose file is nevertheless missing (a hand-deleted file, a copied directory) is
reported for that occurrence as `content unavailable: missing blob`, counted by `status`, and
never fails the query. The sweep takes `gc.lock` exclusively, waiting at most 2 s, and skips blob
collection this time if it cannot. A blob is a **root**, never removed, while a sealed segment's
`.refs` manifest names it or a `blob_ref` record in the valid prefix of any retained `.jsonl`
segment references it, locked or not: before it collects anything the sweep recovers every
unsealed segment whose lock it can take (a crashed writer's, which then has a manifest) and
reads the flushed prefix of every segment it cannot lock (a live writer's) for references, in
that order, whether it runs after a run or from `af storage` with Telemetry `off`; those `blob`
and `blob_ref` records are the structural metadata §3.1 permits. Fixtures pause a sweep between
the rename and the reference flush, read a reference the moment it is flushed while another
thread publishes, crash a publisher at each step, then run a sweep and a reindex, and crash a
writer after its reference flush, age every file past an hour and sweep before any reader has
opened the directory (`telemetry_unsealed_references_are_roots`). Two sources that redact to
identical retained bytes share one object and keep two occurrence records, through reindex and
export.

**Index.** `index.sqlite` (WAL, `synchronous = NORMAL`, the bundled SQLite with FTS5 and JSON1)
holds `segments`, `runs`, `executions`, `spans` (with `self_ns` computed at ingest), `events`,
`configs`, `nodes`, `findings`, `demands`, `blobs`, `blob_refs`, `problems` and, when
`content_index = true`, the FTS5 table `content_fts`
over text blobs. A reader ingests every sealed segment the index has not seen and the tail of
each live segment beyond its last ingested byte offset, in one transaction; ingest is idempotent
by `span_id`, segment id with `seq`, and digest. Every connection to the index holds `index.lock`
shared for its lifetime; replacing the index — `reindex`, and the low-space deletion below —
takes it exclusively, waiting at most 2 s, then checkpoints, closes, unlinks the database with
its WAL and shared-memory files, and recreates it, so no connection ever sees a database
replaced under it. When the exclusive lock cannot be had, `reindex` reports it and the sweep
defers reclamation; when a shared lock cannot be had within 2 s a command reads the segments
directly and says so on stderr. `af telemetry reindex` rebuilds the index; every query answers
identically before and after (§16). Fixtures run concurrent readers through a `reindex` and the
deletion path. The index is derived state and is never evidence of anything.

**Budget.** Telemetry's retained bytes are the physical union under `telemetry/`, counted exactly
once: live, sealed and corrupt segments with their manifests, blobs, temporary files, and the
index with its WAL and shared-memory files. `af storage` reports that total as the kind
`telemetry`, and one entry per sealed or corrupt segment whose `bytes` are the marginal bytes
its eviction frees — the segment, its manifest and the blobs no other segment references —
recomputed after every removal, with `last_use` the seal time (a corrupt segment's: its
modification time). Eviction follows ADR-0144 unchanged: the sweep after every run removes
telemetry entries least recently used first when `telemetry` exceeds `[telemetry] max_bytes` or
the whole budget is exceeded, corrupt segments first among equals; never an entry in use (a live
or locked segment, shown as `in_use`) and never one used within the last hour; collection by age
(`[storage] keep_days`) only when `[storage] auto_gc` is on or an operator runs
`af storage prune --apply` or `af telemetry gc --apply`. Every sweep also removes temporary files
under `segments/` and `blobs/` older than one hour whose writer holds no lock, and blobs no
remaining segment references (under the exclusive `gc.lock`). After it drops evicted rows from
the index it reads `freelist_count` and `page_size`: when the free pages exceed 16 MiB and a
quarter of the file, it runs `VACUUM`, and if that fails for lack of space it replaces the index
under the exclusive `index.lock` as above, leaving none, which the next reader rebuilds; without
the lock it defers. Reclaimed bytes are measured
as allocated bytes, never as deleted rows. Fixtures cover a blob shared by two segments, an index
larger than its segments, and an inventory holding only old corrupt segments, orphan temporaries
and free index pages, each brought under the cap.

**Directory registry.** `[telemetry] dir` may point anywhere that does not overlap other
accounted state, and ADR-0144's registry of Stores (`Task`, `Review`, `ReviewRoot`) does not know
telemetry, so before its first write a process registers the canonical path of its telemetry
directory in `telemetry-dirs.v1.json` (an `af/TelemetryDirs@1` document, machine-local,
deduplicated by canonical path, versioned under ADR-0145 like the Store registry); a process that
cannot register records one `problem` and writes no telemetry. A root is refused at registration,
with one `problem` and no telemetry, when it is inside or contains another registered telemetry
root, a registered Store, the warm check cache, a release installation or any other directory the
storage inventory accounts on its own — `af storage` measures a Store's whole allocated directory,
so a telemetry root inside one would be counted twice and could be removed with it. The inventory
rechecks overlap every time (a Store registered later inside a telemetry root, a registry edited
by hand): an overlapping telemetry root is reported `overlapping`, counted once by the containing
entry, and excluded from the telemetry kind's eviction. `af storage` and the sweep inventory every
registered directory whatever the current level or `dir`, under the same rules, and
`af telemetry status` lists them. A registered directory that no longer exists is reported and
dropped from the registry by an applied prune. Fixtures write to directory A, switch to B, then
inventory and prune with Telemetry `off`; refuse a telemetry root inside a Task Store; refuse
nested telemetry roots; and report a Store registered inside an existing telemetry root.

**Versioning.** The directory is versioned (`v1/`). A release that changes any record shape
writes `v2/` and leaves `v1/` for `af storage` to list as `unreadable_telemetry` and evict;
nothing migrates (ADR-0145).

## 7. Capture

### 7.1 Kernel spans

The substrate is the `tracing` crate (`tracing` and `tracing-core`; no `tracing-subscriber`) with
af's own `Subscriber`, which turns span and event records into segment lines. `tracing`'s macros
give every call site a name and typed fields, and the performance laboratory's phase spans use
the same substrate (§13). At `off` no subscriber is installed and a span site costs one relaxed
atomic load.

| Span | Kind | Parent | Set at end |
| --- | --- | --- | --- |
| `af.command` | internal | root | argv (redacted), cwd, exit code, `af.run_id` once known |
| `af.run` | internal | `af.command` | `af.run.kind`, Task or Campaign, outcome, totals of usage and charge (from Attempt accounting, §9) |
| `af.authority.load` | internal | `af.run` | policy revision, pipeline name and digest, lock pin |
| `af.plan` | internal | `af.run` | plan digest, node count, bindings; followed by one `node` record per node (§5) |
| `af.snapshot.capture` | internal | `af.run` | Snapshot digest, file count, bytes |
| `af.cas.materialize` | internal | `af.run` or a node | bytes, files, `af.cas.op` |
| `af.warm.restore`, `af.warm.save` | internal | node | key, hit or miss, bytes |
| `af.gate` | internal | `af.run` | decision, warm hit or miss |
| `af.check` | process | `af.gate` | `af.check.*`, exit, rusage, output sizes |
| `af.node` | internal | `af.run` | `af.node.*`, outcome |
| `af.attempt` | internal | `af.node` | `af.attempt_id`, epoch, outcome, failure class, the kernel's usage and charge |
| `af.context.compose` | internal | `af.attempt` | manifest digest, bytes, estimated tokens, layer list |
| `af.admission` | internal | `af.attempt` | wait ms, reserved tokens, Provider |
| `af.harness` | process | `af.attempt` | `gen_ai.operation.name = invoke_agent`, provider, model, session id, exit, rusage, stream byte counts, `af.native` |
| `af.turn` | client | `af.harness` | `gen_ai.operation.name = chat`, the harness's per-turn usage, stop reason, response model |
| `af.tool` | internal | `af.turn` | `gen_ai.operation.name = execute_tool`, tool name, call id, outcome, input and output bytes |
| `af.output.parse` | internal | `af.attempt` | contract type, valid or the refusal reason |
| `af.usage.account` | internal | `af.attempt` | observation state and cause; every kernel accounting write is the event `af.usage.state` (§7.5) |
| `af.store.append`, `af.store.read` | internal | any span of a run | event count, bytes, `af.store.op` |
| `af.git` | process | any span of a run | `af.git.op`, exit, rusage |
| `af.remote_check` | internal | `af.gate` | the gate branch, pull request number, poll count, outcome |
| `af.sweep` | internal | `af.command` | bytes before and after, entries evicted |
| `af.sink` | client | `af.command` | the OTLP push: requests per signal, bytes, status |

A Pipeline holds up to 64 static nodes, so the graph is not an attribute: `af.plan` writes one
`node` record per node with its full `depends_on` list and `expected_nodes`, at `spans` without
blobs, and a reader that finds fewer node records than expected reports the critical path
`partial` or `unavailable`, never computed over a missing edge; a fixture runs a 64-node
pipeline. Every span site is a row of this table's successor in `docs/architecture.md`, and a
test (`telemetry_span_catalogue`) fails when a name is emitted that the table does not list.

### 7.2 Processes and resources

Every process `review-process` supervises gets a `process` span. `nix` 0.31 does not expose
`wait4`, so the supervisor's one reap (`Child::wait` and `try_wait` today, after its `waitid` and
kqueue observation) becomes `libc::wait4` on both platforms, and the supervisor carries the
child's `ru_utime`, `ru_stime` and `ru_maxrss` (the last normalised to bytes per platform) as
`rusage: Option<Rusage>` through every result it returns: its internal capture, `SupervisedCapture`
(the model Attempt path through `ModelRunner::capture_process` and the cancellable check path),
`SupervisedOutput`, and `SupervisedDuplexOutput` (the streaming git path), including the results
of a timed-out, killed or cancelled process and every conversion between them; a value the
platform did not deliver is `None`. `review-process` stays the dependency-neutral leaf of
ADR-0026 and links no telemetry: the crates that already receive those results (`review-runner`,
`review-check`, `review-source-git`, `review-sandbox`, `review-pipeline`) record the span from
them, with the stdout and stderr byte counts and `stderr_held`. Fixtures cover a successful model
process, a cancelled process, a timed-out check and a duplex git operation. The streams
themselves reach the Store exactly as today and reach Telemetry only as §7.3 and `content` say.
Host facts (`load1`, `free_bytes`) are read once per segment, at open.

### 7.3 Harness streams

**Streaming observer.** `review-process` drains a child's pipes on its own threads into complete
`Vec<u8>` streams, without arrival times; when a drain does not finish within its grace period
it kills the process group and, for stdout, reports the transport failure `OutputHeld`, while for
stderr it keeps the process status and sets the `stderr_held` flag, retaining the bytes either
way (ADR-0072); and `ModelRunner::capture_filtered` sees the complete streams only after the
process has ended. A live tail and arrival-based turn and tool timings need the bytes as they
arrive, so `review-process` gains a bounded `OutputObserver` whose one operation is a
**non-blocking enqueue**: for every chunk the drain thread reads it offers `(process, stream,
byte_offset, bytes, arrival_mono_ns)` to a bounded queue of `observer_queue_bytes`, the offset
being the chunk's position in that stream; a full queue drops the chunk at once, and the drop is
counted. The drain thread never does more than that offer, so an observer
can neither delay the drain, nor change what the supervisor collects, nor alter status, deadline
or accounting. The queue's **consumer** is Telemetry's one thread (§6), alive for the run: it
frames, redacts and writes the records and flushes the segment whenever the queue runs empty.
Process completion never waits on it: the supervisor returns its result as soon as the drain
ends, the consumer finishes that process's queued chunks afterwards, and when the run ends with
the consumer still busy it is joined for at most 2 s, then abandoned as §6 says: the process
exits with the segment unsealed, nothing else seals or replaces it, and the next reader recovers
what was flushed. A stalled consumer (a fixture injects a blocking write) leaves the drain, the
collected output and the supervision outcome unchanged, and so do a held stdout and a held
stderr, each with its existing classification and its retained bytes; the stalled consumer
delays the command's exit by at most the join and leaves a segment the next reader recovers to
its flushed prefix (`telemetry_stalled_consumer_leaves_recoverable_segment`). `ModelRunner` passes the
observer through a `capture_observed` variant of `capture_filtered`; the adapters'
complete-stream filtering and the Store's raw capture are unchanged.

**The tap.** The consumer frames lines incrementally. A frame that exceeds
`content_max_blob_bytes` before its newline is discarded whole through the next newline: the
consumer records only a withheld marker with the frame's size and never parses, redacts or
retains any prefix of it, because a line too long to be judged whole cannot be judged at all
(§3.6, as `bounded_cli_line` judges the complete line before shortening it). A complete line
is parsed for structure. **What is recorded live** — while the process runs, at `spans` and
above — is validated structure only: the record kind from the closed vocabulary of §7.3, counts,
byte sizes, arrival times, correlation ids (below), tool names that are
in the invocation's own tool set, stop reasons and item statuses from their closed enumerations,
and the bound model's id when the reported model equals it. **Every untrusted string** — text,
thinking, tool arguments and results, agent messages, reasoning summaries, error texts, unparsed
lines, and every identifier the harness supplies — is never published live: at `spans` only its
size is recorded; at `content` its bytes
are pended in a per-process buffer bounded by the Attempt's content allowance (overflow drops
the oldest pending content and counts it). **Correlation ids.** A session, thread, message,
request or tool-call id the harness supplies is an untrusted string — a printable grammar admits
a token — so a live record that must join to another (a `tool_result` to its `tool_use`, a
`turn.completed` to its `turn.started`) carries `af.corr`, the first 16 hex digits of
`SHA-256(segment_id || ":" || original)`, computed without retaining the original outside the
pending buffer; after the release decision one `af.harness.ids` event per process lists each
correlation id with its original string, each original having passed the credential rules of
§8, so a join is reproducible and no original precedes the decision. Fixtures put token-shaped
and challenge-shaped values in every id position of both harnesses
(`telemetry_harness_ids_never_published_live`). **The release decision** is Telemetry's own
and unconditional: the adapters decide whole-capture privacy only for a failed completion
(`crates/review-runner-claude/src/task.rs` returns before `redact_capture` for a successful
envelope, `crates/review-runner-codex/src/task.rs` for a successful final message), so a
successful capture can carry a device code beside a challenge URL. When the process has ended,
before any pending byte is published, Telemetry evaluates `carries_auth_material` over the
complete stdout, the complete stderr and the Attempt's pending native content (§7.6), and
withholds every pending byte of the Attempt — stream and native — when any of them carries
login-challenge material, when the adapter replaced the streams (`redact_capture`), or when the
native failure classification is an authentication one; `af.harness` then records
`af.stream.withheld = true` and `af.native.withheld = true`, and `status` counts the withheld
Attempts. The decision reads what the adapters read and changes no accounting, no
classification and no adapter behaviour. Otherwise
the pending content passes the grant replacement the adapters apply to the whole stream and the
credential rules of §8 and becomes blobs. A grant value split across two chunks is therefore
still replaced, because replacement runs on assembled, complete content. The tap records the
arrival time of every line (the drain's clock at the read, not the consumer's). A chunk whose
offset is not the one expected marks a **gap**: the consumer discards its partial frame, records
the event `af.stream.gap` with the missing byte range, skips to the first newline after the gap
and frames from there, so bytes on either side of a drop are never joined into a record the
harness did not emit; a drop at the end of a stream is a trailing gap. Dropped chunks and gaps
are counted on `af.harness` (`dropped_chunks`, `gaps`), and a stream with any gap is
`incomplete` for turn and tool analysis. Tests cover a stream smaller than `flush_bytes` whose
child waits until the test has seen its last structural line in `tail`, a line delayed until
after the harness's last turn, a cancelled Attempt, a credential split across a chunk boundary,
a credential crossing the frame limit, a device code on one line followed by a challenge URL on
the next (and the same wrapped across lines, split across stdout and stderr, and inside a
successful Claude envelope and a successful Codex final message,
`telemetry_release_decision_covers_success`) with nothing of
either visible in `tail`, a blob or an export, a full queue, a dropped middle chunk whose
neighbours would otherwise form valid but altered JSON, and a stalled consumer.

**Claude.** The invocation changes from `--output-format json` to
`--output-format stream-json --verbose` (`stream-json` requires `--verbose` under `-p`);
`CLAUDE_INVOCATION` and its unit test change with it, and a CLI whose help page lacks either
option is refused as today (ADR-0146). This is a reporting flag: the `result` message is the
envelope `json` printed, and the adapter's envelope parsing, `model_usage::account`, refusal
classification and model-identity check read that line and nothing else. The other lines become
records: `system/init` (model, tools, MCP servers, `claude_code_version`, session id, into
`af.harness` attributes and `gen_ai.conversation.id`); each `assistant` message (a `BetaMessage`
with `id`, `model`, `stop_reason`, `usage` and content blocks) closes an `af.turn` with its usage
and opens one `af.tool` per `tool_use` block; each `user` message carrying `tool_result` blocks
closes the matching `af.tool` by `tool_use_id`; `system/api_retry` (`attempt`, `max_retries`,
`retry_delay_ms`, `error_status`, `error`) is an event on the open turn; `rate_limit_event` and
`permission_denied` are events on `af.harness`. `thinking` and `text` blocks are counted and sized
at `spans` and kept as blobs at `content`. Turn and tool times are the observer's arrival times;
a message's own `timestamp`, when present, is an attribute; the harness's own `duration_ms`
arrives with P9. The adapter grants a Worker `Read,Glob,Grep` and, by its declared access,
`Edit`, `Write` and `Bash`, never the Agent tool, so no subagent messages exist.

**Raw-output rule.** Today the Claude adapter stores its whole stdout (the one envelope) in the
Store CAS and the Codex adapter stores its whole JSONL stream there. With the stream switch the
Store would receive every Claude message. The rule becomes uniform: the Store CAS keeps, for every
harness, the lines the adapter's accounting and failure classification read — Claude's `result`
line; Codex's `turn.completed`, `turn.failed` and `error` lines and its `-o` last-message file —
plus stderr; every other line goes to Telemetry at `spans` and above and is dropped otherwise.
This changes Store content, not any typed shape; `keep_worker_transcripts` keeps its meaning; and
a fixture pins that an Attempt's accounting and classification are identical for the same stream
under every level.

**Codex.** `codex exec --json` already emits `thread.started` (the `gen_ai.conversation.id`),
`turn.started`, `turn.completed` with `usage` (`input_tokens`, `cached_input_tokens`,
`output_tokens`, `reasoning_output_tokens`), `turn.failed`, `error`, and `item.started`,
`item.updated` and `item.completed` for `agent_message`, `reasoning`, `command_execution`
(`command`, `aggregated_output`, `exit_code`, `status`), `file_change`, `mcp_tool_call`,
`web_search` and plan items. A turn becomes an `af.turn`; `command_execution` and `mcp_tool_call`
items become `af.tool` spans from `item.started` to `item.completed`; `reasoning` and
`agent_message` items are events with sizes, and blobs at `content`. Codex exposes no per-request
timing in this stream. Telemetry adds no `-c` key: the adapter allows a package only `--model`
and one `model_reasoning_effort`, and `model_reasoning_summary` stays whatever the user's Codex
configuration says. Codex's native OpenTelemetry export would need `-c otel.*` keys this design
does not admit, so Codex Attempts record `af.native = "unavailable"`; admitting a closed,
adapter-owned set of telemetry-only `otel` keys is a later design, after a probe has shown that
`codex exec --ephemeral` honours them, and is not part of P9.

The complete list of flags Telemetry may add: Claude `--output-format stream-json --verbose`,
always, at every level, one invocation shape — unless the invocation-cost gate of §12 fails, in
which case `json` stays the shape at `off` and `metrics` and `stream-json` is the shape at
`spans` and `content`, both pinned by the envelope-parity fixture; and, with P9 and only when
`harness_native = true`, the Claude environment of §7.6. Nothing else.

### 7.4 Configuration snapshot

At each execution's `af.run` start (§5) the writer records the configuration snapshot
`af/TelemetryConfig@1`: a metadata-only, allow-listed document, permitted at `spans` because it
holds no model input and no value a Worker wrote, written as one or more `config` **parts**,
each a record within the 16 KiB bound carrying `execution_id`, `part` (its ordinal), `parts`
(the expected count) and `complete` (whether this is the last part), with the rows split across
parts in one fixed order (version, configuration rows, policy and pipeline, bindings, Providers,
harness versions, host, variable names), so that a valid 64-node plan with its bindings fits
without a content blob. `af telemetry config` assembles the parts and says `incomplete` when one
is missing. It carries: `af --version --json`; the effective configuration as one row per key
with the layer it came from, the file (home-relative) and line, and the value — except that a
value whose origin is the environment layer is recorded as `<env>`, so an override's value is
never retained; the policy revision; the pipeline's name, digest and node list; the digests of
`.af/af.toml` and `.af/af.lock`; the Task's id, kind and pipeline for a Task run; the execution
bindings (node, Worker, Provider, model and effort); the Provider registry entries by name, kind
and model, with grant paths replaced by placeholders; the harness versions the preflight probed;
the digest of each Claude Code managed-settings file the probe of §7.6 read, when one exists; the
host facts; and the names, never the values, of the `AF_*` and `OTEL_*` variables present. The
texts of the pipeline, the Task file (which carries the request the Workers receive) and
`.af/af.toml` are blobs at `content` only, named by the event `af.config.text`.
`af telemetry config RUN [--execution ID]` prints the execution's assembled record and, at
`content`, the texts. Fixtures pin both representations, prove an environment-layer value
absent, and assemble a configuration whose rows exceed 16 KiB from its parts.

### 7.5 Store commit observer and Finding summaries

At `spans` and above, Telemetry mirrors every event the kernel commits to a Store, whichever
path inserted it: `append`, `append_batch`, the ordinary Task transitions through
`append_batch_inner`, Round publication and atomic integration. The mechanism is one post-commit
observer on `EventStore`, invoked once per successful transaction commit with that transaction's
events in committed order, and never for a rolled-back transaction. Each event becomes
`af.store.event` on the current span with `af.store.event.*` and `artifact_refs`; at `content`
the payload is a blob. A mirror failure is a `problem`, never a Store error, and marks the run
`af.ledger.coverage = incomplete`.

**Accounting snapshot.** Telemetry never re-derives usage, and no single kernel write is the
charge: the `attempt_wall` sidecar merges only its own earlier values and appends no Store
event, the Task log's committed `UsageObserved` retains a floor in the Task projection without
touching the sidecar, and recovery settles an abandoned Attempt whose usage was reported at the
greater of its observed usage and its reservation, and one whose usage was never reported at
zero with `unknown_usage.cause = lease_expired` (ADR-0143). Two series, never merged. The
**charge series**: after every committed Task event that changes an Attempt's accounting —
`UsageObserved`, `Settled` (including recovery, in both branches), `Released` — the kernel emits
the event `af.usage.state` on the Attempt's span at `metrics` and above, taken from the Task
projection after the commit: the retained charge and counters as `af task show` reports them,
`af.usage.observation` and `af.usage.unknown_cause`, the separately identified native
observation when one exists, `af.usage.source` naming the transition, and `af.usage.sequence`,
the committed event's own sequence, which is unique per run. The **wall series**: every sidecar
`record_wall` write emits the event `af.usage.wall` with the merged sidecar row, `af.usage.source
= sidecar` and `af.usage.wall.sequence`, the record's `seq`, so several sidecar
writes with no Task event between them keep their order and never compete with the charge. That
order holds within one execution and nowhere else: executions overlap (§5) and `record_wall`
writes without a lease (ADR-0089 keeps late usage after a lease is lost), so a predecessor's
write can land after its successor's, and no segment-local sequence, segment start time or
lease ordinal says which is later. The
reader shows, per Attempt, the charge snapshot with the highest `af.usage.sequence`, shows
beside it the latest wall observation of each execution that wrote one, labelled by execution
and with the order across executions `unknown`, and never computes a maximum, a sum or a
replacement of its own; after a reindex the same records give the same answer. A fixture with a
decreasing, an absent and a resumed late observation, several sidecar writes with no intervening
Task event, a sidecar write after a committed floor, a recovery whose observed usage is below the
reservation, an abandoned Attempt that reported nothing (charged zero, labelled unknown), and a
predecessor execution that writes the sidecar after its successor has
(`telemetry_wall_order_is_per_execution`, identical before and after a reindex)
yields the kernel's figures at every level.

A Ledger is not a projection of events alone: its Findings come from the Report artifacts the
events reference (ADR-0005; `apply_report` reads the Report from the CAS). So that `af telemetry
ledger` and `findings` need no Store, the kernel records a `finding` record,
`af/TelemetryFinding@1`, at the moment it admits a Report into the Ledger and whenever a
disposition changes: Finding key, severity, effective severity, status, scope, file, line,
title, the Report's artifact id, the node, Worker, model, Round and epoch; the body only at
`content`. Dispositions (`fixed`, `rejected`, `wontfix`, contested) are events on the run span with the
Finding's key. A Demand is an independent obligation with an identity of its own (ADR-0007) and
need not belong to any Finding, so it has its own `demand` record, `af/TelemetryDemand@1`,
keyed by the Demand id and carrying its kind, whether it is required, its Subject and Snapshot
ids and its status, and its lifecycle — opened, evidence satisfied with the Evidence id, stale —
is events keyed by the Demand id, never by a Finding's key. A run whose summaries or events were
dropped, cut or never written is `incomplete`, and every analytic over it says so.
`af telemetry ledger RUN` renders the committed events, Finding and Demand summaries in order.
Tests cover an ordinary Task transition, a Round publication and an atomic integration, each
mirrored after its commit and absent after its rollback, and a standalone Demand with its
satisfaction.

### 7.6 Harness-native telemetry (P9)

Claude Code exports OpenTelemetry metrics, log events and beta traces when
`CLAUDE_CODE_ENABLE_TELEMETRY=1`. The events carry what af cannot see from the stream:
`claude_code.api_request` (`request_id`, `duration_ms`, input, output, cache-read and
cache-creation tokens, `cost_usd`), `claude_code.api_error` (`status_code`, `attempt`),
`claude_code.tool_result` (`duration_ms`, `success`, `error_type`, input and result sizes) and,
with `OTEL_LOG_RAW_API_BODIES=1`, every request and response body inline, cut by the harness at
`CLAUDE_CODE_OTEL_CONTENT_MAX_LENGTH`. The `file:<dir>` form, which writes every body untruncated
to disk, is not used: af cannot bound what the harness writes while it runs, and an unbounded
staging directory defeats the budget. Codex Attempts are `af.native = "unavailable"` in this
design (§7.3), and so is a Claude Attempt whose harness did not export.

**Destination proof.** Setting exporter variables does not by itself make the loopback receiver
the destination: Claude Code's managed settings may set `OTEL_EXPORTER_OTLP_*` variables, in
which case it removes the conflicting developer-set variables at startup, and a per-signal
endpoint set there routes that signal elsewhere. So P9 enables capture only for what it has
proven: before the first Attempt of a run (and again whenever the harness version or a readable
managed-settings file's digest changes), af starts the bound harness with the environment below,
a nonce in `OTEL_RESOURCE_ATTRIBUTES` and a minimal turn-free input, and waits a bounded time for
each enabled signal's first export carrying the nonce to arrive at the loopback receiver; a
signal whose export does not arrive is not enabled, and no content flag (`OTEL_LOG_*`,
`OTEL_LOG_RAW_API_BODIES`) is ever set unless every signal that can carry content (logs; traces
stay off, below) has been proven to arrive at the loopback, at the request path the fixture
pins; af also reads the platform's
managed-settings files when they are readable and records their digests in the configuration
record, and a managed exporter setting it can see is reported in `af telemetry status`. What the
proof cannot see — a managed policy that routes a signal af disabled, such as metrics, to the
organisation's collector — carries no content and is outside af. Fixtures cover a managed
endpoint override, a managed per-signal endpoint override and a beta-tracing endpoint override,
each leaving the affected signal unavailable and the content flags unset.

**Receiver.** When `harness_native = true` the process that runs Attempts binds one listener to
`127.0.0.1:0` at its first Attempt and closes it when its last Attempt ends. It is a thread of the
running command, not a daemon; it accepts only `POST /v1/logs`, `/v1/traces` and `/v1/metrics`
with `Content-Type: application/json` (`OTEL_EXPORTER_OTLP_PROTOCOL=http/json`), bodies of at most
16 MiB, from loopback, answers `200 {}`, and hands each record to the consumer, which treats it
as it treats a stream line (§7.3): validated structure is recorded live under the Attempt the
resource attributes name — the event name from the closed `claude_code.*` vocabulary, counters,
durations, byte sizes, status codes, and correlation ids derived from `request_id`,
`request_body_id`, `message.id` and `message.uuid` as §7.3 derives them — and every untrusted
string (prompts, responses, tool parameters and content, bodies, error texts, any attribute
outside the closed set) is pended in the same per-Attempt buffer under the same allowance and
released or withheld by the Attempt's release decision, which evaluates the pending native
content with the streams; a record that arrives after the decision is dropped and counted. A
device code in a native record beside a challenge URL in stderr, or split across two native
records, withholds the Attempt's content whole (`telemetry_native_through_pending_barrier`).
The environment added to a Claude Attempt: `CLAUDE_CODE_ENABLE_TELEMETRY=1`,
`OTEL_LOGS_EXPORTER=otlp`, `OTEL_TRACES_EXPORTER=none`, `OTEL_METRICS_EXPORTER=none`,
`OTEL_EXPORTER_OTLP_PROTOCOL=http/json`, `OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:<port>`
and the three per-signal variables with their complete URLs,
`OTEL_EXPORTER_OTLP_LOGS_ENDPOINT=http://127.0.0.1:<port>/v1/logs`,
`OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=http://127.0.0.1:<port>/v1/traces` and
`OTEL_EXPORTER_OTLP_METRICS_ENDPOINT=http://127.0.0.1:<port>/v1/metrics` — a signal-specific
endpoint is used as given, with no path appended, while only the generic endpoint receives the
signal's path (the OTLP exporter specification; Claude Code's own examples carry the paths), so
the destination-proof fixture pins the request paths the receiver sees —
`OTEL_LOGS_EXPORT_INTERVAL=1000`,
`OTEL_RESOURCE_ATTRIBUTES=af.store_id=<id>,af.run_id=<id>,af.attempt_id=<id>`, and at `content`
`OTEL_LOG_USER_PROMPTS=1`, `OTEL_LOG_TOOL_DETAILS=1`,
`OTEL_LOG_ASSISTANT_RESPONSES=1`, `OTEL_LOG_RAW_API_BODIES=1` and
`CLAUDE_CODE_OTEL_CONTENT_MAX_LENGTH=<content_native_body_max_chars>`. **No native traces.**
Claude Code's beta tracing is not a reporting switch: its documentation says that when tracing
is active "Bash and PowerShell subprocesses automatically inherit a `TRACEPARENT` environment
variable" and each model request carries a `traceparent` header, so a Worker that inspects its
environment, or a request the model sees, would carry context Telemetry put there (§3.3).
`CLAUDE_CODE_ENHANCED_TELEMETRY_BETA` is therefore never set, `OTEL_TRACES_EXPORTER=none`, af
sets neither `TRACEPARENT` nor `TRACESTATE` (which `-p` sessions read), and
`OTEL_LOG_TOOL_CONTENT`, which requires tracing, is not set: tool content comes from the stream's
`tool_result` blocks. Admitting traces is a later decision that needs an adapter-owned setting
proven to suppress both propagations. What a tool subprocess can see with `harness_native =
true` is exactly the variables listed here, as it sees the adapters' `CLAUDE_CODE_*` variables
today; `telemetry_native_adds_no_worker_visible_context` asserts, on the scripted harness, that
the environment af adds equals this list with no trace context, and the live check of §16 does
the same against the real harness. The content switches of
§8 govern every path alike: `OTEL_LOG_TOOL_DETAILS` is set only with
`content_tool_io`; `OTEL_LOG_ASSISTANT_RESPONSES` only with `content_reasoning` or
`content_tool_io`; and `OTEL_LOG_RAW_API_BODIES` only with `content_model_bodies` **and**
`content_tool_io`, because a request body carries the conversation's earlier tool arguments and
results and Claude Code documents that enabling bodies implies everything the tool switches
reveal — with `content_tool_io = false` no body is captured and the Attempt records
`af.native.bodies = "unavailable: content_tool_io"` rather than a filtered body. Bodies already
redact extended-thinking content on the harness side; stream `thinking` blocks follow
`content_reasoning`. A fixture disables each switch and proves the content absent both where it
is emitted directly and inside a later request body. The sandbox, permission mode and tool set
are untouched, and `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` stays set; P9's
first deliverable is the probe that proves export still works under it, `--safe-mode` and
`--restricted`. If it does not, the source is unavailable and no flag is relaxed. Usage the
harness reports natively is recorded under its own names (`claude_code.*`) and never added to af
totals.

**Bodies.** Byte limits are authoritative, and so are the harness's correlation ids: a request
body is never associated by arrival order. Claude Code emits one `api_request_body` per request
attempt — a retry is another body — with `request_body_id`, `model` and `query_source`, and one
`api_response_body` per successful response with the same `request_body_id`, the `request_id`
of the `api_request` and `api_error` events, and the response's `message.id` and `message.uuid`.
Telemetry pairs request and response by `request_body_id`, joins the pair to the stream's
assistant message by `message.id` (the `id` of the `BetaMessage`; `message.uuid` matches the
stream message's `uuid` where the harness emits one), and to the API events by `request_id`; a
request body with no response (a failed attempt) is kept as that turn's attempt with no
response; a body whose `query_source` is not the main conversation (`compact`) or that no
message claims stays `unassociated`, listed by `context` but never part of a turn's chain. Each
request body is stored as one `af/TelemetryBodyDelta@1` record with `state`
(`complete`, `truncated`, `dropped`, `missing_base`), `kind` (`checkpoint`, `append`), `base`
(for `append`: the digest of the previous request's reconstructed body) and a `blob`. A
**checkpoint** blob is the whole request JSON as the harness delivered it, redacted. An
**append** is allowed only when the previous request's reconstructed body is `complete`, the new
request's `messages` array begins with the previous one's `messages` array element for element
(canonical JSON equality), and every other member of the request object (`model`, `system`,
`tools`, `tool_choice`, `max_tokens`, `temperature`, `thinking`, `metadata` and any other key)
is canonically equal; its blob is the JSON array of the appended messages, and reconstruction is
the base body with those messages appended. Anything else — a changed tool list or parameter, a
compaction, a changed system prompt, a base that is not `complete` — stores a checkpoint. The
last request of an Attempt is always a checkpoint. A checkpoint or append whose blob does not fit
`content_max_blob_bytes` and the Attempt's allowance is `truncated` or `dropped` with its
original byte count; a body the harness itself cut at `content_native_body_max_chars` is
`truncated` too, so an upstream cut never yields `complete`; and a later append whose base is not
`complete` is `missing_base`. `af telemetry context RUN ATTEMPT --turn N` reassembles the
context from a `complete` chain and reports `complete`; with any other state in the chain it
prints what it has and reports `partial` or `unavailable`, never claiming the whole request.
Tests cover unchanged messages with changed tools, unchanged messages with a changed parameter, a
base over the blob bound, an upstream cut, an exhausted Attempt allowance, a delta whose base was
dropped, a retried request, a compaction request, bodies delivered out of order, a missing
response event, and a Worker whose output exhausts every telemetry allowance without filling the
volume or being terminated. The joins are version-specific by Claude Code's own statement; the
probe of P9 records the harness version, and a version below 2.1.274 has no bodies.

### 7.7 Measurements

A kernel measurement (ADR-0132) runs under `af.run` with `af.run.kind = measure` and
`af.measurement.id`; its repetitions are child spans. Telemetry adds nothing to the measurement's
own record in the Store; the laboratory's diagnostics read the spans by that id.

## 8. Levels, configuration and redaction

| Level | Records | Typical size per Task run |
| --- | --- | --- |
| `off` | nothing: no directory, no subscriber | 0 |
| `metrics` | `af.command`, `af.run`, `af.gate`, `af.check`, `af.node`, `af.attempt`, `af.harness` and `af.sweep` spans with identity and numeric attributes; failure records; rusage; no turns, tools, configuration, mirror, summaries or blobs | tens of KiB |
| `spans` | every span and event of §7; the `config` record; the Store commit mirror and Finding summaries; sizes and digests of content; no blobs | low MiB |
| `content` | `spans` plus blobs: composed Worker inputs, assistant text, thinking and reasoning, tool arguments and results, check stdout and stderr, Store event payloads, Finding bodies, configuration texts, bounded raw API bodies when P9 delivers them | tens to hundreds of MiB, bounded by `content_attempt_max_bytes` and `content_process_max_bytes` |

The `[telemetry]` table, machine-only, with flat keys like `[storage]`:

| Key | Default | Bounds | Meaning and failure behaviour |
| --- | --- | --- | --- |
| `level` | `"spans"` | `off`, `metrics`, `spans`, `content` | another value refuses the configuration load, naming the key |
| `dir` | the state root's `telemetry/` (`$XDG_STATE_HOME/af/telemetry`) | an absolute path that neither contains nor lies inside a Store, a cache or another telemetry root (§6) | unwritable or overlapping: one `problem`, no telemetry, the run continues |
| `max_bytes` | `"4GiB"` | `256MiB` to `[storage] max_bytes` | telemetry entries evicted LRU past it, under ADR-0144's rules |
| `flush_bytes` | `"64KiB"` | `4KiB` to `16MiB` | buffer size before a write |
| `flush_latency_ms` | `1000` | 50 to 10000 | the longest a record waits in the buffer while a run is open (§6) |
| `observer_queue_bytes` | `"4MiB"` | `256KiB` to `64MiB` | the drain-to-consumer queue; a full queue drops chunks |
| `segment_max_bytes` | `"256MiB"` | `16MiB` to `4GiB` | the live segment's total bound across every record kind, with a fixed 64 KiB terminal reserve |
| `content_max_blob_bytes` | `"16MiB"` | `64KiB` to `256MiB` | larger content is truncated head and tail |
| `content_attempt_max_bytes` | `"512MiB"` | `16MiB` to `8GiB` | past it an Attempt's further content is dropped with a `problem` and a `dropped` count |
| `content_process_max_bytes` | `"64MiB"` | `4MiB` to `1GiB` | the same bound for content outside any Attempt (check output, configuration texts) |
| `content_native_body_max_chars` | `1048576` | 4096 to 8388608 | what the harness may put inline in one raw body (`CLAUDE_CODE_OTEL_CONTENT_MAX_LENGTH`, UTF-16 code units) |
| `content_reasoning` | `true` | boolean | keep thinking blocks and reasoning summaries at `content` |
| `content_tool_io` | `true` | boolean | keep tool arguments and results at `content` |
| `content_model_bodies` | `true` | boolean | keep raw API bodies at `content` when P9 delivers them |
| `content_index` | `true` | boolean | maintain the FTS5 index over text blobs |
| `harness_native` | `false` | boolean | P9's receiver and harness environment |
| `sink` | `"none"` | `none`, `otlp` | push after each run |
| `sink_endpoint` | `""` | an `http://` or `https://` URL | required with `sink = "otlp"`, refused otherwise |
| `sink_headers_file` | `""` | a path | one `Name: value` per line, read at push time, never recorded |
| `sink_timeout_ms` | `5000` | 100 to 60000 | the push is abandoned past it with a `problem`; the exit code is unchanged |

There is no telemetry `keep_days`: collection by age is `[storage] keep_days` under
`[storage] auto_gc` or an applied prune, as for every kind (ADR-0144).
`AF_TELEMETRY__LEVEL=content` is the one-run override, as the ladder already defines it.
`af help telemetry` documents the table and the levels.

**Redaction** is structured, at every level and on every path (attributes, blobs, excerpts, the
`config` record, received native telemetry, exports), with one implementation in the leaf crate
that review-runner re-exports for its own use:

- A **typed field** the kernel produced and validated — a digest (`sha256:` and 64 hex), a run,
  attempt, node, segment or span id, a numeric counter, a timestamp, an enumerated value — is
  written as that field and never passes the heuristic, which would otherwise withhold every
  digest as a 32-hex run.
- Every **untrusted string** — Provider-authored text, tool arguments and results, stderr, a
  free-text attribute, each line of a text blob — passes the existing rules
  (`looks_like_credential`, `WITHHELD_LINE`, the token prefixes, long hex and base64 runs, JWTs,
  secret-looking assignment names), per line for text and per string value for JSON. A
  digest-looking run inside Provider text is withheld as today; appearance earns no exemption.
- A **JSON document** (the `config` record, a mirrored payload, a Finding body, a tool argument
  or result that is JSON) is redacted recursively and key-aware, never per line, so it stays
  parseable: a member whose name matches the secret-name rule (`key`, `token`, `secret`,
  `password`, `passwd`, `credential`, `auth`, as `secret_assignment` matches them today) has its
  value withheld whole, whether string, number or object — `{"password":"violet"}` becomes
  `{"password":"[withheld]"}`; a member whose **name** itself passes for a credential under the
  value rules (a token prefix, a JWT, a long secret run: `{"sk-proj-…":"ok"}`, which the
  whole-line filter catches anywhere in a line today) withholds the object that contains it
  whole, the object becoming the string `"[withheld]"`, because a name cannot be rewritten into
  one an untrusted document could not also carry — and every remaining string value then passes
  the value rules.
  Only the fields af's own schema types as identities or counters are exempt, by position, in
  af's own records; a document from a harness has no exempt field.
- Environment values are never recorded; Provider grant values (`HOME`, `USER`, `CODEX_HOME` and
  the auth directories) are replaced by `<grant:NAME>`; the home directory prefix becomes `~`.

A redaction is counted on the record (`af.redacted`); a blob records `redaction = "af-redact/1"`,
and a rule change bumps that version so a reader can tell. Fixtures plant credentials of every
kind — short passwords under secret names, nested secret members, numeric credentials, tokens
and keys in text, and token prefixes, JWTs and long secret runs as member names at every depth
(`telemetry_redaction_withholds_credential_member_names`) — beside genuine digests and ids on
every path and prove the former withheld
and the latter intact, with every JSON document still parseable. `af telemetry export` refuses `--content`
without `--yes-content`, and prints the blob count and bytes it is about to write first.

## 9. Analytics

**Accounting source.** The kernel's Attempt accounting is the one source of usage and charge
totals: an Attempt's figures are the `af.usage.state` event with the highest sequence (§7.5),
which is the kernel's own merged state, and `af.attempt`'s end attributes carry the state known
at that moment. Run totals are sums of those Attempt states. Turn counters
(`af.turn`) are the harness's per-turn report (Codex's adapter already sums them into the Attempt
total) and serve turn-level analysis only; they are never added into a run or Attempt total. Usage
the harness exports natively (P9) is a third series under its own names and is never added
either. A fixture with several turns, a resume and a late usage update yields the same aggregate
at `metrics`, `spans` and `content`.

**Metrics** are queries over spans; the names are the OpenTelemetry ones where they exist, so an
OTLP export carries them unchanged.

| Metric | Unit | From | Attributes |
| --- | --- | --- | --- |
| `gen_ai.client.token.usage` | tokens | `af.attempt` (totals); `af.turn` only with `af.series = turn` | `gen_ai.token.type` (`input`, `output`, `cache_read`, `cache_write`, `reasoning`), model, provider, worker, node, `af.series` (`attempt`, `turn`, `native`) |
| `gen_ai.client.operation.duration` | s | `af.turn`, `af.harness` | operation, model, provider |
| `af.attempt.duration` | s | `af.attempt` | outcome, failure class, worker, node |
| `af.phase.duration` | s | every internal span | `af.phase` |
| `af.tool.call.duration` | s | `af.tool` | tool name, outcome |
| `af.gate.duration`, `af.check.duration` | s | `af.gate`, `af.check` | check, warm |
| `af.admission.wait` | s | `af.admission` | provider |
| `af.process.cpu.time` | s | process spans | `state` (`user`, `system`), process kind |
| `af.process.memory.max_rss` | bytes | process spans | process kind |
| `af.store.op.duration`, `af.git.op.duration` | s | `af.store.*`, `af.git` | op |
| `af.usage.chargeable_tokens` | tokens | `af.attempt` | worker, node, provider |
| `af.failures` | count | failure records | class, provider, model, node, check |

**Failure analytics.** A failure record is a `span_end` with status `error` plus the events on
it. `af telemetry failures --since 14d --group class,provider,model` prints counts, first and
last seen, the share of Attempts, and the three most recent span ids; `--show ID` prints one
record with its redacted stderr excerpt, exit, retries, rusage and, at `content`, the last turn
and tool call in flight. The classes are the kernel's own; Telemetry invents none.

**Correctness analytics.** `af.check.fingerprint` is `af/check-fingerprint@1`: `SHA-256` over
the canonical JSON of the declared check (program, arguments, timeout, working-directory policy),
the executor identity (platform triple, the resolved program's version string or digest, the
toolchain snapshot id when one is declared), a digest of the **effective environment** the check
ran with — every name and value, after the runner has replaced the runtime paths it knows it
introduced (the sandbox root, `CARGO_HOME`, `RUSTUP_HOME`, `CARGO_TARGET_DIR`, the warm cache
directory, the temporary directory) with placeholders, hashed and never retained in plaintext,
because `PATH` and the toolchain homes decide which subordinate tools run — and the Snapshot
digest; when any part is unavailable, or the runner cannot name the runtime paths it introduced,
the fingerprint is `unknown` and the check is excluded from flake detection and listed
separately. `af telemetry flakes --since 30d` lists checks that were executed more than once
under one fingerprint with both `passed` and `failed` among the outcomes; `not_run` is never an
outcome in that comparison and is reported on its own, so a refused execution followed by a pass
is not a flake. It also lists reviewer nodes whose output failed its contract. Fixtures pin the
fingerprint, a changed execution input, identical environment names with different values (two
fingerprints, never a flake pair), and a refused run followed by a pass. `af telemetry findings --since 30d` joins the Finding summaries of §7.5 with their
later disposition events per Worker and model and prints the share rejected or wontfix, a
precision proxy; a run marked `incomplete` or `partial` is counted separately and labelled.
Unknown-usage causes and model-identity refusals appear by class in `failures`.

**Bottlenecks and the critical path.** `af telemetry bottlenecks --since 7d --top 20` ranks span
names by total self time with p50, p95, max and share of run wall, and links the worst three
span ids. `af telemetry critical-path RUN` walks the node graph recorded by `af.plan` with the
recorded start and end times and prints the longest chain, each link's self time and the wait
before it (admission, lease, gate). Overlapping spans are never summed into wall time; wall time
is the run span's.

**Diff.** `af telemetry diff RUN_A RUN_B` aligns spans by name and node, prints total and self
time, usage and charge per aligned row with the difference and the ratio, and lists spans
present on one side only.

**Search and SQL.** `af telemetry search 'text' [--since D] [--run RUN]` queries `content_fts`
and prints matching digests with the span path and a bounded excerpt; it exists only at
`content`. `af telemetry sql 'SELECT …' [--csv]` runs one statement against the index under
`PRAGMA query_only = ON` and an authorizer that denies everything but reads, with a 30 s
statement timeout; a write is refused naming the rule.

## 10. Commands and outputs

`af telemetry` is read-only except `reindex`, `gc --apply` and `export`. Every command ingests
new segment bytes, then reads the index; every command prints human text on a terminal and one
JSON document with `--json`; progress and warnings go to stderr. `--dir PATH` reads another
telemetry directory, such as one copied from another machine.

| Command | Prints | `--json` type |
| --- | --- | --- |
| `status` | level, directory, bytes against the cap (total and per entry), segments live, unsealed, sealed and corrupt, runs, withheld Attempts, index freshness, problems | `af/telemetry-status@1` |
| `runs [--since D] [--kind K]` | store, run id, kind, Task or Campaign, executions, first start, active and elapsed time, outcome, tokens, charge, failures, coverage, history | `af/telemetry-runs@1` |
| `show RUN [--attempt ID] [--span ID] [--depth N]` | the span tree with start offset, total, self, status and key attributes; `--span` prints one span with its events | `af/telemetry-show@1` |
| `tail [RUN]` | follows the live segments (all, or the run's) and prints spans and events as they end, like `tail -f`; exits when the writer seals | `af/telemetry-tail@1`, one line each |
| `turns RUN [--attempt ID]`, `tools RUN [--attempt ID]` | one row per turn or tool call with timing, usage, outcome and sizes | `af/telemetry-turns@1`, `af/telemetry-tools@1` |
| `context RUN ATTEMPT [--turn N] [--raw]` | the composed Worker input; with P9, the request the model saw at turn N, with its `complete`, `partial` or `unavailable` state | `af/telemetry-context@1` |
| `reasoning RUN ATTEMPT` | thinking blocks and reasoning summaries in order | `af/telemetry-reasoning@1` |
| `config RUN [--execution ID]` | the execution's configuration record and, at `content`, the texts | `af/TelemetryConfig@1` |
| `ledger RUN` | the committed Store events and Finding summaries in sequence, with the coverage marker | `af/telemetry-ledger@1` |
| `failures`, `flakes`, `findings`, `bottlenecks`, `critical-path RUN [--execution ID]`, `diff A B [--by execution]` or `[--by run]` | §9 | `af/telemetry-failures@1` to `af/telemetry-diff@1`, one type each |
| `search`, `sql` | §9 | `af/telemetry-search@1`, `af/telemetry-sql@1` |
| `export` | §11 | none; the file is the output |
| `reindex` | rebuilds the index | `af/telemetry-reindex@1` |
| `gc [--apply]` | what the sweep would evict from the telemetry kind; `af storage` shows the same | `af/telemetry-gc@1` |

A `RUN` argument is the kernel run id; when two Stores share it, the command lists both with
their `af.store_id` and asks for `--store ID`. Exit codes follow `af help exit-codes`: 0; 1 when
the store is unreadable or a run is unknown, stderr naming it; 2 for a wrong command line or a
refused configuration key. Each `--json` document has a schema under `schemas/telemetry-*.json`
and a parity fixture. The TUI gets a Telemetry pane in P10 that shows what `show` and `tail`
print for the selected Task; it reads only what those commands read.

## 11. Exports and sinks

| Format | Content | Consumer |
| --- | --- | --- |
| `otlp-json --signal traces`, `--signal logs`, `--signal metrics` | one `ExportTraceServiceRequest`, `ExportLogsServiceRequest` or `ExportMetricsServiceRequest` in the OTLP JSON mapping (nanosecond strings, hex ids, resource attributes `service.name = af`, `service.version`, `host.*`, `af.store_id`), directly postable to `/v1/traces`, `/v1/logs` or `/v1/metrics`; `--signal` is required | any collector or backend (Jaeger, Tempo, Honeycomb, Grafana) through `curl` or the `otlp` sink |
| `perfetto` | Chrome JSON trace events: one process track per af process, one thread track per Attempt, `X` events for spans, `i` events for events, flow events along the critical path; overlapping non-nested work on async tracks | Perfetto UI, `chrome://tracing`, the laboratory's HTML report |
| `jsonl` | the segment records of the selection, concatenated, records only (a blob is its reference record; content never inline) | `jq` and ad-hoc analysis |
| `dir` | the selection in the Telemetry Store's own layout: segments holding only the selected records, their manifests, every blob those records reference, and `manifest.json` (`af/TelemetryExport@1`: the selection, its completeness per run, the af version, counts and bytes) | `af telemetry --dir` on any machine, after `tar` or `rsync` by the operator |
| `csv` | one table of spans with the attributes `--columns` names | spreadsheets, R, Python |

`af telemetry export --format F --out PATH|- [--run RUN|--since D] [--content --yes-content]`
writes one file, or for `dir` one directory; `--content` includes blobs, as the blob files of a
`dir` export or as a sidecar directory for `perfetto`, and is refused for the other formats.
**Selection.** A segment belongs to a process, and one process may serve several runs (a Task
whose review node runs a Campaign writes both streams to one segment), so `--run` and `--since`
never copy a segment whole: the export writes, for each source segment, a derived segment that
keeps the source's header, segment id and record sequences and holds only the records of the
selected runs (their spans, events, `config` parts, `node`, `finding`, `demand`, `blob` and
`blob_ref` records and the segment's `problem` and `seal` records), so every span id and
occurrence id is unchanged, plus a `.refs` manifest of the blobs those records reference and
those blob files alone; `manifest.json` lists the selected runs and, per run, whether every
retained execution is present. A `dir` export read with `--dir` answers every query exactly as the
source did for the selection; a parity fixture exports several segments sharing blobs, one over
16 KiB, and compares, and a second fixture puts two runs with distinct blobs in one segment,
their records interleaved so the selected records' `seq` values are not consecutive, and
proves the export of one discloses no record and no blob of the other while every span id,
occurrence id and owning-event reference survives and a reindex of the export answers
identically (`telemetry_export_preserves_record_sequence`).
The `otlp` sink runs `af.sink` at the end of each run after the
result is printed: it posts the run's traces, logs and metrics to the three signal endpoints
under `sink_endpoint` in requests of at most 4 MiB each, with the headers from
`sink_headers_file`, bounded as a whole by `sink_timeout_ms`, and records the outcome per signal
on the segment; a failure is a `problem`, and the command's exit code is the run's. An exported
file is validated by an independent receiver: the OpenTelemetry Collector's `otlp` receiver, run
from its pinned release in CI, accepts each signal's fixture export; a golden fixture pins the
shape between runs.

## 12. Performance contract

- At `off` a span site costs one relaxed atomic load; `af --help`, `af --version` and every read
  command other than `af storage` and `af telemetry` never open the directory. Pinned by a test
  that runs them against a fresh state root and asserts no telemetry directory exists afterwards.
- At `spans` the median overhead on the pinned workload below is at most 2% of command wall
  time, telemetry finalization included. This is a release gate: the benchmark must pass; there
  is no labelling alternative. `content` has no bound, and `af telemetry status` labels it
  *perturbing*.
- A record is serialized without allocation beyond its attribute strings; nothing is formatted
  at `off`; a record is at most 16 KiB (§5) plus blob references.
- Writes are `O_APPEND` and happen only at the flush points of §6; no `fsync` on the hot path;
  the seal does one. The output observer adds one non-blocking enqueue per drained chunk and
  never blocks the drain.
- Reads are bounded on the pinned corpus below: `af telemetry status` answers in under 2 s on a
  fresh index and under 200 ms when nothing is new; `show` on one run in under 200 ms.
- Content capture copies bytes the observer already holds; it never re-reads a Provider session
  file. Compression runs on the Attempt's thread after the harness exits, outside the model's
  wall time.

**Overhead protocol.** The workload is three pinned, credential-free fixtures run through the
Task runtime with fake Providers: `telemetry-bench-concurrent` (four concurrent Attempts of a
scripted harness), `telemetry-bench-dense` (one Attempt whose scripted harness emits 50 MiB of
stream-json at pipe speed), and `telemetry-bench-store` (a Task whose checks and nodes append
10,000 Store events). Each measurement is a pair of commands on the same machine and fixture,
`off` and `spans`, with the order randomized per pair, 20 pairs per fixture, the command's wall
time measured from exec to exit so finalization (seal, compression, index-free) is inside it; the
report gives each fixture's paired-difference distribution, its median and a 95% bootstrap
interval, and the gate passes when every fixture's median is at most 2% and its interval's upper
bound at most 3%. Drain latency is measured separately on `telemetry-bench-dense`: the time from
the harness's last write to the supervisor's drain completion, `off` against `spans`, reported
the same way with a 10 ms bound on the median difference. The laboratory's native cases join
this set when they exist.

**Invocation cost.** Switching Claude to `stream-json` is paid at every level, because the
supervisor keeps a process's whole stdout in memory until it ends and the adapter filters only
then. The protocol therefore also compares the current `json` invocation with `stream-json`, at
`off` and at `spans`, on scripted-harness fixtures with identical final results and intermediate
tool output of 1 MiB, 16 MiB and 64 MiB, alone and as four concurrent Attempts, measuring command
wall time, peak RSS and drain latency and verifying identical accounting and classification. The
gate at `off`: median wall within 2% of `json` and peak RSS at most the stream's bytes plus
16 MiB above `json`. If that gate fails, the design falls back to two invocation shapes — `json`
at `off` and `metrics`, `stream-json` at `spans` and `content` — with the envelope-parity fixture
pinning both, and this note records the measurement that decided it.

**Read-latency corpus.** `telemetry-corpus-10k` is generated deterministically by a fixture tool
from a seed: 10,000 runs over 20,000 executions, 2,000,000 spans, 4,000,000 events, 200,000
blobs of which a quarter are shared, 2 GiB of segments and 1 GiB of blobs, with `content_index`
on. Fresh-ingest `status`, unchanged-index `status` and a single-run `show` are measured
separately on it, each after dropping the page cache for the cold figure and once warm for the
warm figure, on the pinned CI hardware, reporting latency, peak memory and index size; the
bounds above are the warm figures, and the cold figures are reported beside them.

## 13. Relationship to the performance laboratory and the self-optimizer

The continuous performance laboratory design (accepted 2026-10-19, its first package in
implementation) planned a default-off `perf-trace` compile feature for phase spans, a Perfetto
export and a 2% overhead target. This design amends that plan: the runtime level replaces the
feature; the laboratory's phases (canonicalization, CAS, projection, authority, startup
composition) are rows of §7.1's catalogue; its Perfetto export is `af telemetry export --format
perfetto`; its overhead measurement is the `off` against `spans` pair; and verdict samples run at
`off`, which costs the atomic load. The laboratory's measurements stay the Store's facts and
Telemetry links to them (§7.7). The laboratory's package 1 takes this amendment.

The self-optimizer's History Capture keeps reading its declared native sources and never
Telemetry; Observations exclude thinking blocks by rule, and this design adds no Telemetry source
to `.af/optimization-sources.toml`. A later design may declare a bounded, typed extract of
Telemetry — never reasoning, never raw content — as an optimization source; that is a separate
decision with its own ADR.

## 14. Considered options

- **Telemetry tables in `events.sqlite`.** One file, one budget entry. Rejected by the owner's
  requirement and by the boundary: the Store is the kernel's authority, and its GC, identity and
  replay must not see telemetry; a 100 MiB Attempt transcript next to a 4 KiB event log also
  threatens the Store's 200 ms read target.
- **An in-process OpenTelemetry SDK (`opentelemetry`, `opentelemetry-otlp`, `tonic` or `reqwest`,
  `tokio`).** The standard answer. Rejected: it brings an async runtime and a TLS stack into a
  synchronous kernel for a wire format af writes with `serde_json`; af keeps the data model and
  the JSON mapping and drops the SDK.
- **A collector daemon or an always-on receiver.** Rejected by the performance value (no daemon
  in the kernel, no ambient polling). The receiver of §7.6 lives inside the running command and
  dies with it.
- **A Prometheus endpoint.** Needs a process to scrape. Rejected; metrics are exported in OTLP
  JSON and derived on read.
- **Text logs through the `log` crate.** Unstructured and unjoinable. Rejected.
- **Reading the harnesses' session files instead of the stream.** Claude's transcripts live
  under `~/.claude` in an undocumented shape and af removes them unless `keep_worker_transcripts`
  is set; Codex runs `--ephemeral`. Rejected: the documented streams are the contract, and P9
  adds the harness's own telemetry.
- **Only the harnesses' native telemetry, no kernel spans.** Misses the kernel, which is where
  "make af faster" lives, and is unavailable where a harness does not export. Rejected as the
  sole source; kept as the fourth.
- **Tapping the complete streams after the process ends.** What `capture_filtered` offers today.
  Rejected: no arrival times, no live tail; the bounded observer at the drain is the one place
  the bytes pass as they arrive.
- **Sampling.** One machine, one operator; completeness is the point. Rejected; the bounds are
  bytes.
- **A compile-time feature for spans.** Two binaries, and verdict builds that cannot be inspected
  when they misbehave. Rejected for the runtime level with an atomic check.
- **Blobs in the Store CAS, linked from Telemetry.** Dies with the Store's GC and mixes concerns.
  Rejected for a telemetry-owned, content-addressed blob directory.
- **A receiver port per Attempt.** Simple attribution, many sockets. Rejected for one per process
  with resource attributes.
- **The `file:<dir>` form of raw API bodies.** Untruncated and unbounded by af while the harness
  runs. Rejected for the inline form the harness cuts at a length af sets.
- **A telemetry `keep_days` of its own.** Age-based deletion by default is what ADR-0144 rejected
  after it collected a developer's history. Rejected; the storage rules apply unchanged.
- **A Store instance id written into the Store.** Changes the Store's contents for Telemetry's
  sake. Rejected for the digest of the Store directory's canonical path.
- **A synchronous observer on the drain thread.** Simplest, and a slow write would hold the
  pipe. Rejected for a non-blocking enqueue and one consumer thread that lives while a run is
  open.
- **Deleting index rows as reclamation.** SQLite keeps freed pages in the file. Rejected for
  `VACUUM` past a free-page threshold, and deletion of the rebuildable index when space is short.
- **An archive format of af's own for cross-machine reading.** One more versioned shape with a
  reader of its own. Rejected for exporting the store's own directory layout, which `--dir`
  already reads.
- **Filtering disabled content out of raw bodies.** Keeps bodies under `content_tool_io = false`
  at the cost of a filter that must find every tool block in every API shape. Rejected for not
  capturing bodies at all when a switch they imply is off.
- **Scanning for telemetry directories.** Finds nothing outside the default root. Rejected for a
  machine-local registry of every directory written.
- **Publishing stream content live.** A live transcript in `tail`. Rejected: a whole-capture
  privacy decision (a device code beside a challenge URL) exists only once the streams are
  complete, and the adapters take none for a successful reply, so Telemetry takes its own over
  streams and native records alike; live records are validated structure, content follows the
  decision.
- **Native beta traces.** Tool timings from the harness's own spans. Rejected while tracing
  injects `TRACEPARENT` into tool subprocesses and `traceparent` into model requests (§7.6);
  logs carry the per-request figures af wants.
- **Publishing harness identifiers live.** Simple joins. Rejected: a printable grammar admits a
  token; live records carry kernel-derived correlation ids and the originals follow the
  decision.
- **Summing execution durations as active time.** Simple, and wrong after a lease takeover,
  when two executions overlap. Rejected for the union, with the sum reported beside it.
- **Trusting the exporter variables as the destination.** Managed settings override them.
  Rejected for a behavioural proof per signal before any content flag is set.
- **Codex `-c otel.*` keys in this design.** Their names and `codex exec` support are unverified.
  Deferred to a later design behind a probe; no Codex invocation changes here.
- **zstd for segments and blobs.** Better ratio and speed, one more C build. Rejected for v1;
  `flate2`'s pure-Rust backend is already in the dependency graph of the tools the repository
  uses, and the extension names the codec, so zstd can follow without a shape change.

## 15. Packages

Each package is one `kernel/implementation-reviewed` Task: one implementer Attempt (1,500,000
tokens, three hours) that must leave a gate-passing candidate, then the gate, two reviewers and
the evaluator, with follow-up rounds as needed and one `kernel/verification-reviewed` Task
before each pull request (ADR-0142). The six packages of the first revisions are cut into ten
so that each fits one Attempt; the order is binding, and each package's acceptance assumes the
earlier ones. The change note `changelog.d/telemetry.md` is created by P1 and extended by every
later package; ADR-0151 lands with P1 and no package adds another ADR.

**P1 — the crate: records, segments, blobs, redaction, levels, index, status.** A new leaf
workspace crate `review-telemetry` (depending on `serde`, `serde_json`, `tracing-core`, `flate2`
with its pure-Rust backend, `rusqlite`, and nothing in the workspace; linked by `af` in this
package, later by `review-pipeline`, `review-runner`, `review-check`, `review-source-git` and
`review-sandbox`, never by `review-process`): the records of §5 with the record bound, the
locked segment writer with its total limit, terminal reserve, `.refs` manifest, sealing and
recovery, blob objects and occurrences with retained-bytes identity, redaction before truncation
and the publication protocol, structured redaction moved from review-runner (which re-exports
it), the `tracing` subscriber, the trace and execution identities, the directory registry with
its overlap rule, the index with its lifecycle lock, the `[telemetry]` table with
`af help telemetry`, `af telemetry status` and `reindex`, ADR-0151, `docs/telemetry.md`.
Acceptance: `telemetry_segment_recovery_keeps_complete_prefix`,
`telemetry_segment_recovery_refuses_interior_corruption`,
`telemetry_writer_lock_survives_pid_reuse`, `telemetry_segment_limit_bounds_every_record_kind`,
`telemetry_record_bound_holds`, `telemetry_index_rebuild_is_identical`,
`telemetry_index_replacement_under_lock`,
`telemetry_machine_only_table_is_ignored_in_project_layers`,
`telemetry_redaction_keeps_identities`, `telemetry_redaction_withholds_secret_members`,
`telemetry_redaction_withholds_credential_member_names`,
`telemetry_redaction_precedes_truncation`, `telemetry_record_sequence_is_persisted` (every
record carries `seq`; a derived segment with gaps reindexes to the same ids),
`telemetry_blob_identity_is_retained_bytes`,
`telemetry_blob_occurrences_survive_dedup`, `telemetry_blob_publication_survives_paused_sweep`
(a paused sweep, a concurrent reader, a crash at each step), `telemetry_missing_blob_is_reported`,
`telemetry_trace_namespace_separates_stores`, `telemetry_dirs_overlap_refused`,
`telemetry_off_touches_nothing`, and schema and parity fixtures for the two documents.

**P2 — kernel integration: the consumer, the first spans, the storage kind, runs/show/tail/gc.**
The consumer thread as the segment's sole owner, with its wake conditions, the bounded
shutdown and the visibility bound; the spans `af.command`,
`af.run`, `af.authority.load`, `af.plan` with `node` records, `af.node`, `af.attempt` and
`af.sweep`; the `telemetry` storage kind with physical-union accounting, ADR-0144 eviction,
reclamation of every class, the registered-directory inventory with the overlap recheck;
retained history as a prefix; `af telemetry runs`, `show`, `tail` and `gc`. Acceptance:
`telemetry_off_opens_nothing`, `telemetry_never_steers`, the reader-import source test,
`telemetry_default_opens_no_socket`, `telemetry_visibility_latency_holds` (including a deep
kernel record under a silent child), `telemetry_executions_of_a_resume`,
`telemetry_executions_overlap_after_takeover`, `telemetry_history_prefix_only` (the latest
execution removed before reindex and export), `telemetry_history_partial_after_eviction`,
`telemetry_budget_counts_physical_bytes_once`, `telemetry_eviction_follows_storage_rules`,
`telemetry_sweep_reclaims_every_class`, `telemetry_unsealed_references_are_roots`,
`telemetry_stalled_consumer_leaves_recoverable_segment`,
`telemetry_dirs_registry_reaches_every_directory`,
`telemetry_dirs_overlap_reported_at_inventory`, `telemetry_write_failure_is_fail_open`,
`insufficient_disk_still_refuses_admission`, `telemetry_plan_nodes_at_64`, and golden fixtures
for the four commands.

**P3 — full span coverage, process resource use, the configuration record, the check
fingerprint.** The remaining spans of §7.1 except `af.turn`, `af.tool` and `af.sink`, with the
catalogue table in `docs/architecture.md`; `libc::wait4` rusage through every `review-process`
result type and its recording by the supervising crates; the `config` parts and
`af telemetry config`; `af/check-fingerprint@1` and `af.check.status`; measurements under
`af.run`. Acceptance: `telemetry_span_catalogue`, `telemetry_rusage_on_every_result_type`
(model, cancelled, timed-out check, duplex git), `telemetry_rusage_is_absent_not_zero`,
`telemetry_config_record_withholds_environment_values`, `telemetry_config_parts_assemble` (a
configuration over 16 KiB), `telemetry_check_fingerprint_pins` (a changed execution input,
identical environment names with different values, `unknown` without the executor identity),
and a golden fixture for `config`.

**P4 — the Store commit observer, Finding and Demand summaries, accounting series, the ledger
view.** The post-commit observer over every commit path; `finding` and `demand` records and
their lifecycle events; the charge and wall series of §7.5; `af telemetry ledger`. Acceptance:
`telemetry_mirror_covers_every_commit_path`, `telemetry_mirror_skips_rolled_back_transactions`,
`telemetry_findings_from_summaries_only`, `telemetry_demands_keyed_by_demand`,
`telemetry_accounting_snapshot_is_the_projection` (decreasing, absent and resumed observations;
several sidecar writes with no Task event between; a sidecar write after a committed floor;
recovery below the reservation; an abandoned unreported Attempt charged zero and labelled
unknown), `telemetry_usage_series_separate`, `telemetry_wall_order_is_per_execution`,
`telemetry_usage_exact_above_u64`,
`telemetry_aggregates_identical_across_levels`, and a golden fixture for `ledger`.

**P5 — the performance contract.** The three bench fixtures, `scripts/telemetry-overhead.py`
with the paired protocol and the gate rule, `make telemetry-gate`, the one-pair smoke in
`make check`, `scripts/telemetry-corpus.py` with the three read-latency measurements, the
`Performance` section of `docs/telemetry.md` with this machine's first numbers. Acceptance:
`telemetry_overhead_smoke`, `telemetry_corpus_is_deterministic`, `telemetry_overhead_gate_rule`,
and the measured report in the pull request.

**P6 — harness streams: the observer, the tap, Claude stream-json, Codex items, turns and
tools.** `OutputObserver` in `review-process` with the non-blocking enqueue and byte offsets;
the consumer tap with frame withholding, live structure only with correlation ids, pending
content, Telemetry's own release decision and the gap protocol; Claude `stream-json --verbose`
with the raw-output
rule; the Codex item tap; `af.turn` and `af.tool` with the `gen_ai.*` attributes; the
invocation-cost measurement and its fallback; `af telemetry turns` and `tools`. Acceptance:
`telemetry_observer_sees_lines_before_exit`, `telemetry_observer_survives_cancellation`,
`telemetry_observer_redacts_split_credential`, `telemetry_frame_over_bound_withheld_whole`,
`telemetry_stream_withheld_until_decision` (a device code then a challenge URL, wrapped, and
split across stdout and stderr), `telemetry_release_decision_covers_success`,
`telemetry_harness_ids_never_published_live`, `telemetry_observer_full_queue_drops_and_counts`,
`telemetry_observer_gap_never_joins_fragments`, `telemetry_observer_keeps_held_outcomes`,
`telemetry_stalled_consumer_never_blocks_drain`, `telemetry_observer_failure_never_blocks_drain`,
`claude_stream_result_envelope_is_the_json_envelope` (fixture pairs),
`claude_accounting_identical_at_every_level`, `codex_accounting_identical_at_every_level`,
`telemetry_store_bytes_do_not_grow_with_level`, `telemetry_claude_messages_become_spans`,
`telemetry_codex_items_become_spans`, `telemetry_stream_json_cost_gate`, and golden fixtures
for `turns` and `tools`.

**P7 — the content level.** Blobs for composed Worker inputs, assistant text, reasoning, tool
arguments and results, check output, Store event payloads, Finding bodies and configuration
texts, behind their switches and allowances; the FTS5 index; `af telemetry context`,
`reasoning` and `search`. Acceptance: `telemetry_content_blob_bounds_and_truncation`,
`telemetry_content_allowances_hold`, `telemetry_search_only_at_content`,
`telemetry_content_switches_hold`, the Worker-context fixtures of §16.2 with a telemetry
directory full of content, the credential fixture through every blob path, the source test that
no prompt-composing, Warm Set, Worker Notes or optimization-source code imports the reader, and
golden fixtures for the three commands.

**P8 — analytics, exports and the sink.** `failures`, `flakes`, `findings`, `bottlenecks`,
`critical-path`, `diff`, `sql`; `export` in `otlp-json` per signal, `perfetto`, `jsonl`, `dir`
with selection and `csv`; the `otlp` sink. Acceptance: a golden fixture per command from one
recorded fixture run, `telemetry_sql_is_read_only`,
`telemetry_otlp_signal_files_accepted_by_collector`,
`telemetry_otlp_omits_unrepresentable_counters`, `telemetry_perfetto_export_is_valid_trace_json`,
`telemetry_dir_export_round_trips`, `telemetry_export_run_selection_discloses_nothing_else`,
`telemetry_export_preserves_record_sequence`,
`telemetry_sink_failure_keeps_exit_code`, `telemetry_critical_path_partial_when_nodes_missing`,
`telemetry_flakes_ignore_not_run`.

**P9 — harness-native telemetry (Claude, logs only).** The destination proof, the loopback
receiver feeding the pending-content barrier, the Claude environment derived from the content
switches with no trace context, inline raw bodies as `af/TelemetryBodyDelta@1` joined by ids,
`context --turn`. Acceptance: `telemetry_native_destination_proven` (a managed endpoint, a
managed per-signal endpoint and a beta-tracing override each leave the signal unavailable and
the content flags unset; the request paths pinned), `telemetry_native_through_pending_barrier`,
`telemetry_native_adds_no_worker_visible_context`,
`telemetry_receiver_accepts_only_loopback_json`, `telemetry_native_unavailable_relaxes_no_flag`,
`telemetry_native_usage_never_joins_totals`, `telemetry_bodies_states_and_reassembly` (changed
tools, changed parameter, upstream cut, dropped base), `telemetry_bodies_join_by_ids` (retry,
compaction, reordered delivery, missing response), `telemetry_content_switches_govern_native`,
`telemetry_native_exhaustion_neither_fills_nor_kills`, and a credential-free fixture of Claude
Code events.

**P10 — the TUI pane.** The Telemetry pane renders the `show` and `tail` documents and nothing
else; the TUI's open and state-read timings are unchanged; pseudo-terminal goldens.

## 16. Acceptance

The design is done when every boundary of §3 has its test and every package's acceptance list
is green:

1. §3.1 and §3.3: `telemetry_never_steers` runs the Task fixture at `off` and at `content` under
   the same injected clock and a storage budget with enough headroom that the sweep evicts
   nothing in either run, and asserts byte-identical Store event logs, including the
   `storage_sweep` records, and identical `af task show --json` apart from wall times; a source
   test asserts that no crate outside `af`, the TUI and the leaf crate's own reader module
   imports that reader; `telemetry_executions_overlap_after_takeover` counts an overlap once;
   `telemetry_native_adds_no_worker_visible_context` pins the variables a tool subprocess can
   see under `harness_native = true` (§7.6).
2. §3.2: the Worker-context fixtures that already prove parent transcripts and other Workers'
   reasoning absent gain a telemetry directory full of content and prove the prompt bytes
   unchanged.
3. §3.4: `telemetry_machine_only_table_is_ignored_in_project_layers`.
4. §3.5: `telemetry_default_opens_no_socket` — no listener and no connection with
   `sink = "none"` and `harness_native = false`.
5. §3.6: the credential fixture passes through every path and every export, with credentials
   placed across each truncation boundary; `WITHHELD_LINE` stands where the fixture planted
   material, every planted digest and id is intact, and every JSON document still parses;
   `telemetry_frame_over_bound_withheld_whole`, `telemetry_stream_withheld_until_decision`,
   `telemetry_release_decision_covers_success`, `telemetry_harness_ids_never_published_live`
   and `telemetry_native_through_pending_barrier` prove that no prefix of an unjudgeable frame,
   no harness identifier and no byte of withheld stream or native content is ever published.
6. §3.7: `telemetry_write_failure_is_fail_open` injects telemetry-only failures (a read-only
   telemetry directory, a quota on it, a locked index) and asserts one `problem` and a successful
   run; `insufficient_disk_still_refuses_admission` fills the work volume below `min_free_bytes`
   and asserts ADR-0144's refusal with Telemetry at `content`; a `v0/` directory is listed
   unreadable and never read.
7. §3.8: `telemetry_budget_counts_physical_bytes_once`,
   `telemetry_eviction_follows_storage_rules` (in use, last hour, age only under `auto_gc` or an
   applied prune), `telemetry_segment_limit_bounds_every_record_kind` with sustained shallow
   events after exhaustion, `telemetry_dirs_registry_reaches_every_directory`,
   `telemetry_dirs_overlap_refused` and `telemetry_dirs_overlap_reported_at_inventory`.
8. §12: the overhead gate under its protocol, the invocation-cost gate with its fallback rule,
   the read-latency corpus measurements, and the no-directory test.

Verification items, each owned by the package that resolves it: whether Claude Code's
`stream-json` carries `thinking` blocks under `--safe-mode` with the models af binds (P6; if not,
Claude reasoning is sizes only and this note says so); whether Claude Code exports OpenTelemetry
under `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`, `--safe-mode` and `--restricted`, and to the
loopback alone under this machine's settings, and that a Bash tool run by the real harness under
the §7.6 environment sees neither `TRACEPARENT` nor `TRACESTATE` and exactly the variables §7.6
lists, checked by hand once per harness version and recorded in `docs/telemetry.md` (P9); and
the deflate ratio on real content, which
decides whether the `content_attempt_max_bytes` default stays (P7). Codex native telemetry is
outside this design (§7.3).
