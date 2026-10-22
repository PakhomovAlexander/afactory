# Telemetry

Telemetry records what `af` does while it works, in a store of its own under the state root,
never in a Task or Campaign Store. It is for the operator: where the wall time of a run went,
what failed and why, what a Worker saw and did. The boundaries are
[ADR-0153](adr/0153-record-telemetry-in-its-own-store-and-never-let-it-steer.md); the full
direction, with the packages still to come, is the design note
[`design/telemetry.md`](design/telemetry.md).

The kernel never reads it. Identity, replay, the Ledger, budgets, admission and every Worker
input are the same whether Telemetry is on, off or deleted, and nothing it captures reaches a
prompt. Nothing leaves the machine: this release has no sink, and the `sink` keys are refused
at load as not implemented.

## What this release records

A command that runs a kernel run records it: `af task run`, `af task start --execute`,
`af review run`, `af provider doctor` and `af self optimize --execute`. Every other command —
`af --help`, `af --version`, every read command but `af storage` and `af telemetry` — records
nothing and opens no telemetry directory, and at `off` nothing is opened at all. A recording
process writes one segment, opened at its first record:

| Span | Parent | What it carries |
| --- | --- | --- |
| `af.command` | none | `process.command`, and at its end `process.exit_code`; the segment header carries the redacted argv and working directory |
| `af.run` | `af.command` | `af.store_id`, `af.run_id`, `af.run.kind` (`task`, `review`, `optimize`), `af.execution_id` (the segment id), `af.execution.ordinal`, `af.store.sequence_at_start`, `af.task_id`, `af.campaign` and `af.round` for a review run, `af.epoch`, `af.policy_rev`, `af.pipeline`, `af.pipeline.digest`; at its end `af.outcome` and the Task's charge as the kernel accounted it (`af.usage.chargeable_tokens`) |
| `af.authority.load` | `af.run` | `af.policy_rev`, `af.pipeline`, `af.pipeline.digest` |
| `af.plan` | `af.run` | `af.plan_digest`, `af.pipeline`, `af.node_count`, `af.bindings`; then one `node` record (`af/TelemetryNode@1`) per plan node with its kind, Worker package, every node it depends on (up to 64) and `expected_nodes` |
| `af.node` | `af.run` | `af.node_id`, `af.node.kind`; at its end `af.outcome`, and on failure `af.failure.class`, `af.failure.reason` and `error.type` |
| `af.attempt` | `af.node` | `af.attempt_id`, `af.node_id`, `af.epoch`, `af.worker`, `af.provider`, `af.provider.kind`; at its end the kernel's accounting as the Attempt settled (`af.usage.*_tokens` as exact decimal strings, `af.usage.observation`, `af.usage.unknown_cause`) and its outcome |
| `af.sweep` | `af.command` | the Storage Budget's sweep that ends a run: `af.sweep.bytes_before`, `af.sweep.bytes_after`, `af.sweep.evicted` and the telemetry kind's bytes before and after |

Every span of a run also names `af.store_id`, `af.run_id`, `af.run.kind` and `af.execution_id`,
and `af.command` names the first run it worked on at its end.

At `metrics` only `af.command`, `af.run`, `af.gate`, `af.check`, `af.node`, `af.attempt`,
`af.harness` and `af.sweep` are recorded, with no `node` record. A process works on a run as one
**execution**; an `af.run` span covers the time it spent on the run, whichever runtimes it built.

**The consumer.** From the moment a process opens its first `af.run` span, one thread — the
consumer — is its segment's only writer until the segment is sealed or abandoned: the open file,
every write, the manifest, the seal, the compression and the rename. Kernel threads only append
to a buffer and signal it; the end of `af.run` neither stops the consumer nor hands the writes
back, so the `af.command` end and every later record are its writes too. It waits on one
condition with one deadline (the oldest pending record's time plus `flush_latency_ms`) and wakes
when a record is appended or the deadline passes; there is no timer and no polling. Every record
is therefore in the live segment within `flush_latency_ms` of its creation while a run is open,
whatever its depth and however silent the Worker. When the command ends it sends the consumer
one terminal request — the records still pending, the command's end among them, the seal, the
manifest, the compression and the publication — and waits for it once, at most 2 s beyond the
consumer's last progress, over all of it: each content a released harness stream publishes under
the seal renews the bound, so a release still in progress completes, at most the Attempt's
allowance of content later; a consumer that makes no progress for 2 s — a write blocked on the
volume — is abandoned: nothing else seals, compresses, replaces or writes the segment, the
command's exit status is its own, the lock ends with the process, `af telemetry status` counts
the segment `unsealed`, and the next reader recovers what was written. The compressed segment
and its manifest are staged in temporary files, and the same bound governs the whole
publication: the deadline check is the last thing
before each step — the manifest's rename, the compressed copy's rename and the unlink of the
live `.jsonl`, which happens only while the deadline has not passed — with nothing between the
check and the step, and it runs again when a rename returns; a consumer that finds the deadline
gone at any check, a blocked write released later among them, publishes nothing further and
removes what it already renamed into place (the file just placed included) and what it staged,
so once the deadline has passed nothing of the seal stays under its final name, and leaves the
live segment for the next reader. A seal is reported only when the live `.jsonl` itself was
unlinked: when its name no longer holds the writer's file at the last step, the seal is
withdrawn the same way and the segment is left unsealed. Whatever a consumer leaves, every
reader, `af telemetry status`, the index and the storage inventory take a `.jsonl` beside a
`.jsonl.gz` or `.refs` of the same segment id as the unsealed segment: those files are stale
staging, never read as sealed (an index that had read the compressed copy forgets it and reads
the `.jsonl` from its start), replaced by recovery, removed by recovery before it removes an
empty `.jsonl` or renames a corrupt one, and removed by the sweep after an hour. A telemetry-only failure
(an unwritable directory, a quota, a locked index) is one line on stderr,
`af: telemetry problem (…), the command is unaffected: …`, recorded in the segment when it can
be.

Every phase below the run, the supervised processes and the configuration record are recorded
too ([below](#phases-processes-and-the-configuration-record)), and the Store mirror, the Finding
and Demand summaries and the accounting snapshots under [The Store mirror and the
ledger](#the-store-mirror-and-the-ledger), and every harness's turns and tool calls under
[Harness streams](#harness-streams); at `content` the content blobs too, under
[Content](#content).

## Phases, processes and the configuration record

Below the run every phase of the design's catalogue is a span of the crate that owns it, each
with its total and self time in `af telemetry show`; the catalogue, one row per span with its
kind, parent and attributes, is in [`architecture.md`](architecture.md#telemetry-span-catalogue),
and a test fails when a source emits a name it does not list. `af.turn` and `af.tool` are the
harness streams' ([below](#harness-streams)); `af.sink` comes later.

| Span | Under | What it carries |
| --- | --- | --- |
| `af.admission` | `af.attempt` | the reservation (`af.admission.reserved_tokens`), the Provider, `af.admission.wait_ms`; `refused` below the free-disk floor |
| `af.context.compose` | `af.attempt` | the composed Worker input: `af.context.manifest` (its digest), `af.context.bytes`, `af.context.estimated_tokens`, `af.context.layers` — never its bytes |
| `af.harness` | `af.attempt` | the model process: `gen_ai.operation.name = invoke_agent`, `gen_ai.provider.name`, `gen_ai.request.model`, `af.native = unavailable` |
| `af.output.parse` | `af.attempt` | `af.output.contract`, `af.output.valid`, the refusal (`invalid_output_contract`) |
| `af.usage.account` | `af.attempt` | `af.usage.observation` (`complete`, `incomplete`, `unknown`), `af.usage.unknown_cause`, the charge |
| `af.gate` | a check node's Attempt, the review Gate | `af.gate.checks`, `af.gate.decision`, `af.check.warm` |
| `af.check` | `af.gate` | the check process: `af.check.name`, `af.check.status` (`passed`, `failed`, `not_run`), `af.check.fingerprint`, `af.check.warm` (`hit`, `miss`, `none`) |
| `af.warm.restore`, `af.warm.save` | a check node's Attempt | the Warm Check Cache: `af.warm.key`, `af.warm.result`, `af.warm.kinds`, `af.warm.bytes` |
| `af.snapshot.capture` | any span of a run | `af.snapshot` (the digest), `af.snapshot.files`, `af.snapshot.bytes` |
| `af.cas.materialize` | any span of a run | `af.cas.op`, `af.cas.files`, `af.cas.bytes` |
| `af.store.append`, `af.store.read` | any span of a run | `af.store.op`, `af.store.events`, `af.store.bytes` |
| `af.git` | any span of a run | the git process, `af.git.op` (its subcommand); `ok` whenever git exited, the code in `process.exit_code` |
| `af.remote_check` | `af.gate` | the gate branch, the checks, the gate pull request, the outcome |
| `af.process` | any span of a run | any other supervised process (a container runtime, a toolchain probe, `gh`), `af.process.kind` |

A span that does not name the run takes `af.store_id`, `af.run_id`, `af.run.kind` and
`af.execution_id` from its parent, and a phase that belongs to a run is recorded only inside one:
a Store read or a git process before a run opens, or on a thread no span was handed to, records
nothing.

**Processes and their resource use.** Every process span — `af.harness`, `af.check`, `af.git`,
`af.process` — carries `process.pid`, `process.command` (the program, never its arguments),
`process.exit_code` or `process.signal`, `af.stdout_bytes`, `af.stderr_bytes`, `af.stderr_held`,
and `af.rusage.user_ms`, `af.rusage.system_ms` and `af.rusage.max_rss_bytes`. The supervisor
(`review-process`, which links no telemetry) carries them through every result it returns, a
timed-out, killed or cancelled process included; the crates that receive the result record the
span. A process span ends after its caller judged the capture: an Attempt, check, gate call or
container command whose output went past the capture cap ends it with `error.type = "failed"`
and the cap reason in `af.failure.reason`, whatever the exit code, its stream sizes those kept
within the cap. The workspace forbids `unsafe` code and no crate it already depends on offers a safe
per-child `wait4(2)` (`nix` 0.31 and `rustix` expose none), so the figures come from
`getrusage(RUSAGE_CHILDREN)`: the change of the process's children totals across a reap is the
reaped child's own (its CPU time, with that of the descendants it waited for, as `wait4` reports
it) only when no other child of the process is reaped between the two readings. So every child
reap of the workspace's non-test code goes through `review-process`'s one reap lock: the
supervisor's own reaps, and every other — a login adapter's, the TUI's hand-off, `$EDITOR`,
`gh`, `git` and `tar` — through `review_process::reap`, whose `wait`, `try_wait`,
`wait_with_output`, `output`, `status` and non-blocking `wait_pid` do what the standard
library's do under that lock, polling a non-blocking reap so the lock is never held while a
child runs. A source test fails on a direct `wait`, `try_wait`, `wait_with_output`, `output()`,
`status()` or `waitpid` anywhere else, so a process's `af.rusage.*` is never another child's.
Those totals keep only the largest peak of any reaped child, so a process's peak is recorded
when its reap raised that maximum and is absent otherwise. A figure the platform did not deliver
is absent, never zero; a process that never started records no process id and no `af.rusage.*`.

**The check fingerprint.** `af.check.fingerprint` is `sha256:` and the 64 hex digits of SHA-256
over the canonical JSON of an `af/check-fingerprint@1` document
([`schemas/check-fingerprint-v1.json`](../schemas/check-fingerprint-v1.json)): the declared check
(program, resolved arguments, timeout, working-directory policy), the executor (platform triple,
the digest of the resolved program's bytes, the toolchain snapshot id when one is declared), a
digest of the effective environment — every name and value the check ran with, after the runtime
paths the check node introduced (the sandbox root, its runtime directory with `HOME`, `TMPDIR`
and a cold `CARGO_TARGET_DIR`, `RUSTUP_HOME`, the warm cache directories) became placeholders,
hashed and never retained — and the Snapshot digest. A runtime path becomes its placeholder only
where a value names it or a path under it (the next character is `/`, a separator between
values or the value's end): a sibling such as `/tmp/a-extra` beside `/tmp/a` is another path and
stays as it is. The program resolves as the check's own
lookup resolves it: a bare name through the effective `PATH`, whose relative entries (and an
empty one) are the check's working directory's, so `PATH=bin:/usr/bin` digests the Snapshot's
`bin/probe`, never a file below af's own directory. Two checks declared identically on the same
Snapshot, executor and effective environment share one fingerprint; a changed argument or a
changed `PATH` value gives another. It is `unknown` when a part is unavailable: a program that
does not resolve, a toolchain declared without its id, a check through a container provider, or
a caller that did not name the runtime paths it introduced.

**Measurements.** A kernel measurement (ADR-0132) is a run of its own: `af.run` with
`af.run.kind = measure` under the measure Attempt, its run id derived from the Task's run, the
Attempt and the measure, `af.measure`, and at its end `af.measurement.id`, the
`af/Measurement@1` it recorded. Each repetition is an `af.measurement.repetition` span with the
repetition's `af.check` below it. Nothing is added to the Measurement the Store records.

**The configuration record.** At `spans` and above every run an execution works on has its
own configuration record, `af/TelemetryConfig@2`: metadata only, written as `config` parts of at
most 16 KiB each. Every part names the run that owns it — `run_id` and `run_span_id`, the run's
`af.run` span — besides the execution (`execution_id`), so a measurement run inside a Task
Attempt has a record of its own, never the Task's; and each belongs to one of three sections,
numbered within it (`section`, `part`, `parts`, `complete`), each section written once per
execution and run. The rows, in the sections' fixed order, are each a key, a value, a `layer`
and an `origin`:

1. `configuration`, written when the run's `af.run` opens: `af --version --json`, one row per
   member (`version`; the dispatcher's variable, when set, is `<env>`); the effective
   configuration, one row per key with the layer it came from and its file and line — a file
   under the operator's home written home-relative (`~/.config/af/config.toml:3`), never as an
   absolute path — and a value whose origin is the environment as `<env>`, with the variable's
   name as its origin, never the value; the digests of `.af/af.toml` and `.af/af.lock`
   (`policy`).
2. `plan`, written when the plan resolves: the policy revision, the pipeline's name, digest and
   nodes, and the Task's id, kind and pipeline (`policy`); the execution bindings — node,
   Worker, Provider, model and effort (`binding`). A measurement's plan section is what it
   measures: its kind, Task, Attempt and measure (`measurement`).
3. `machine`, written when the run opens: the Provider registry entries by name and kind, the
   auth directory only as its grant placeholder (`provider`; `unknown` with the reason when the
   registry cannot be read); the harness versions this process's preflight probed before the
   run opened, and the Claude Code managed-settings digests, empty until the harness-native
   package reads them (`harness`); the host (`host`); the names, never the values, of the
   `AF_*` and `OTEL_*` variables present (`variable`).

So an execution refused while the runtime checks its current plan still has the record's
`configuration` and `machine` sections, and its `plan` section is `unknown`. Every row passes
the credential rules (a secret-named key withholds its value); a row keyed by an identity
position (`af.policy_rev`, `af.pipeline.digest`, `af.af_toml.digest`, `af.af_lock.digest`) keeps
a value that is exactly a digest. At every level that records the configuration, `spans`
included, the record names three texts by the event `af.config.text`, each by the SHA-256 of
its own bytes and their size: the pipeline's `pipeline.toml` as its package declares it (a
Review Campaign's pipeline file as captured), the Task file as `af task start --file` read it,
and `.af/af.toml` — never the CAS artifacts that carry them. A text whose bytes are not at hand
(a Task no Task file made) is named by its kind alone. Their bytes are blobs only at
[`content`](#content).

## `af telemetry config`

```text
af telemetry config RUN [--store ID] [--execution ID] [--json] [--dir PATH]
```

Assembles the run's configuration record in one execution from the parts that name the run
(its run id and its `af.run` span there) and prints, per section, `complete` with its number of
parts, `incomplete` naming the parts that are missing, or `unknown` when none was recorded (what
is left is never presented as the whole record), then its rows in order —
`layer  key = value   # origin` — and the texts it names; the latest execution by default (the
greatest committed ordinal), `--execution ID` for another. The record is `complete` when every
section is, `incomplete` otherwise, `absent` for a run recorded below `spans`. When two Stores
record the run id it lists both and asks for `--store ID`. `--json` prints one
`af/TelemetryConfig@2` document
([`schemas/telemetry-config-v2.json`](../schemas/telemetry-config-v2.json)). Exit
codes: 0; 1 when the directory cannot be read or the run or execution is unknown; 2 for a wrong
command line, a run two Stores share without `--store`, or a refused `[telemetry]` key.

## Executions and retained history

A run is one kernel run id within one Store: `af.store_id` is the SHA-256 of the Store
directory's canonical path, and two Stores holding the same Task are two runs. Each process that
worked on the run is an execution, numbered by `af.execution.ordinal`. Every lease that begins
kernel execution commits one execution marker to the Task's log, its own event kind
`TaskExecutionStarted@1` beside `TaskTransition@5` (`af/TaskExecutionStarted@1`, naming the
lease's writer and epoch,
[`schemas/task-execution-started-v1.json`](../schemas/task-execution-started-v1.json); the
Store refuses it on a finished Task), before its `af.run` span opens, at every level `off` included — the Store never learns the level — and
the ordinal is one more than those markers of the leases before it. An execution the free-disk
floor refuses, or one interrupted, before any Attempt or run report still has its marker and its
ordinal, and the resume takes the next one, so two retained executions never share an ordinal.
`af.store.sequence_at_start` is the run's highest committed Store event sequence when it
started. A lease that ran no kernel run (planning without `--execute`, a decision, a refresh, a
delivery, a recorded sweep) writes no marker and takes no ordinal; its epoch, like every
lease's, is `af.epoch`.

Executions can overlap: a lease is taken over once it expires, without waiting for the fenced
writer to exit. `af telemetry` therefore reports a run's **active** time (the union of its
executions' `af.run` intervals, an overlap counted once), its **execution** time (their sum,
`af.execution.duration_sum`) and its **elapsed** time (first start to last end, idle gaps
included). An execution is **open** while its segment is not sealed (a writer holds it, or one
that stopped left it unsealed) or its `af.run` has no recorded end; its interval ends at its
segment's last record.

Retained segments cannot say how many executions a run had: one may have been evicted, found
corrupt or never written. A run's history is `prefix:k` when executions 1 to k are retained with
their seals and no ordinal between them is missing, and `partial` — with the missing ordinals —
when one is; it is never called complete, and every total says what it covers. Open executions
are listed beside the sealed ones, never hidden behind the prefix: the label reads
`retained executions 1–k` only when no listed execution is open, and otherwise names them —
`retained executions 1–1, open: 2` for a sealed first execution and a live second one,
`retained executions: none sealed, open: 1` at `prefix:0`, `partial (missing executions 1),
open: 3` — and the totals of `runs` (active, execution and elapsed time, usage, charge,
failures) state which executions they cover, each open one marked `open`. A `reindex` changes
none of this.

## Levels

`[telemetry] level` decides what is recorded:

| Level | Records |
| --- | --- |
| `off` | nothing: no directory, no subscriber, no registry entry |
| `metrics` | command, run, gate, check, node, Attempt, harness and sweep spans with identity and numeric attributes; failures; resource use |
| `spans` (default) | every span and event, the configuration record, the Store mirror and Finding summaries; sizes and digests of content, never content |
| `content` | `spans` plus content blobs: Worker inputs, assistant text, reasoning, tool arguments and results, check output, Store event payloads, Finding bodies, `.af/af.toml` ([Content](#content)) |

`AF_TELEMETRY__LEVEL=content af task run TASK_ID` raises the level for one run. `content` is
labelled *perturbing* by `af telemetry status`: it keeps blobs of what Workers saw and did.

### The segment header

Each segment begins with one header record (`af/TelemetrySegment@1`). Its fields by level:

| Field | `metrics` | `spans` and `content` |
| --- | --- | --- |
| `segment_id`, `af_version`, `af_commit`, `pid`, `parent_pid`, `host`, `started_unix_ns`, `mono_origin_unix_ns`, `level` | recorded | recorded |
| `argv` | `argv[0]` and the subcommand path only (`af task show`), exactly as the caller's argument parser matched it and never guessed from the words; `argv[0]` alone when no parsed path was passed in: no option, no other argument | the whole command line; the value after an option named like a secret (`--api-key`, `--password`, `--pw`) or after `-p` is `[withheld]`, and every argument is judged whole by the credential rules below |
| `cwd` | `""`: not recorded | the working directory, judged whole, home prefix as `~` |

## Credentials

Credential-looking material is withheld at every level, before any bound can cut it, by a
bounded heuristic: the same rules `af` applies to a Provider's own words
([`providers.md`](providers.md) lists them). It recognises the common shapes of keys, tokens,
passwords and login challenges; it is not a promise to recognise every secret.

- A text line that looks like a credential (an assignment to a `key`, `token`, `secret`,
  `password`, `passwd`, `passphrase`, `credential` or `auth` name, a password after whitespace,
  a URL carrying a password, a bearer token, a token prefix such as `sk-` or `ghp_`, an AWS
  access key id, a JSON Web Token, a long hex, base64 or lower-case run, a device code, the
  account a login holds, login-challenge material, two long words that are one secret split by a
  space) becomes `[line withheld: looks like credential material]`. A `sha256:` digest and a
  40- or 64-hex git object id are no credentials and pass.
- In a JSON document a member named like a secret keeps its key and loses its value
  (`{"password":"[withheld]"}`, `{"pw":"[withheld]"}`); every other string is judged whole by the
  line rules, whatever its member is named (`{"url":"[withheld]"}` for
  `https://alice:hunter2@…`); an object holding a member whose name itself looks like a
  credential becomes the string `"[withheld]"`. The document still parses.
- A line or JSON document too long to be judged whole is withheld whole, never cut.
- A record attribute whose name itself looks like a credential (a token prefix, a JSON Web
  Token, a long secret run) is never written: the attribute is omitted and counted in
  `af.redacted`, and the record's other attributes stay.
- A record attribute named like a secret (`api_key`, `session_token`, `db_password`) keeps its
  name and loses its value, whatever its shape (text, number, identity, list), counted in
  `af.redacted`; only the counters and enumerations `af`'s own schema names this way (such as
  `af.usage.input_tokens`) keep a value of their type. On a command line, the argument after an
  option named like a secret (`--api-key VALUE`) is withheld the same way.
- Every other field of every record (span and event names, node, Finding and Demand fields,
  configuration rows) is either a kernel identity in an identity position or judged whole by
  these rules.
- An identity is exempt from these rules only when two things hold: the attribute or field is
  one of the identity positions the record declares (`af.store_id`, `af.run_id`, `af.task_id`,
  `af.attempt_id`, `af.execution_id`, `af.node_id`, `af.corr`, `trace_id`, `span_id`,
  `parent_span_id`, the digest fields such as `af.context.manifest` and `plan_digest`, and the
  like), and its value is exactly one of the kernel's identity grammars: a `sha256:` digest of
  64 lowercase hex digits (or a Task's run id, `task:` and a digest), a lowercase-hex id of 16,
  26, 32 or 64 digits (span ids and `af.corr`, Store event ids, trace ids, Store ids), a segment id, an occurrence id
  `segment_id:seq`, or a decimal ordinal. Every other value, an identity-shaped one included
  (`af.run_id = "sk-proj-…"`), is judged as untrusted text, so a credential is withheld there as
  anywhere; a 32-hex id outside an identity position is a long hex run and is withheld too,
  while a digest there passes the text rules like any other digest.
- Token and charge counters are exact decimal strings only. They are, by name and then by
  family (`*` is any text): `af.usage.input_tokens`, `af.usage.output_tokens`,
  `af.usage.cache_read_tokens`, `af.usage.cache_write_tokens`, `af.usage.reasoning_tokens`,
  `af.usage.chargeable_tokens`, `af.admission.reserved_tokens`, `af.context.estimated_tokens`,
  `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`, `gen_ai.client.token.usage`,
  `af.store.sequence_at_start`, `af.usage.*_tokens`, `gen_ai.usage.*`, `af.charge*`. Any
  other value under such a
  name (text, even `"42"`, an identity, an enumeration, a boolean, an integer, a negative, a
  float, a list or a non-canonical decimal) is dropped and counted in `af.dropped_fields`
  before any other rule runs, so it is never withheld under the secret-name rule and never
  written as a JSON number.
  Timestamps are decimal strings too.
- A configuration row whose key is named like a secret (`provider.password`, `sink.auth_token`)
  or passes for a credential itself (`sk-proj-…`, a JSON Web Token, a long secret run, an
  assignment such as `API_KEY=…`) withholds the whole pair: the value and the origin become
  `[withheld]` whatever their length or shape, a credential-shaped key becomes `[withheld]` as
  well, and the pair counts once; other rows keep their values under the rules above. Each
  `config` record counts what it withheld in `af.redacted`.
- Environment values are never recorded; the home directory prefix becomes `~`. When `HOME`
  gives no usable prefix (unset, not absolute, `/`), the segment records one `redaction`
  problem saying home paths are not replaced, never a silent omission.
- A Provider's granted path becomes `<grant:NAME>` in every spelling: as given, through the
  home prefix (`~/.claude-work`), resolved through links, JSON-escaped (`\/Users\/…`),
  percent-encoded (`%2FUsers%2F…`), and written through `.`, `..` or doubled separators
  (`/Users/alice/./.claude-work`).

Each blob occurrence records the rules its bytes passed, `af-redact/2`.

## The `[telemetry]` table

Machine layers only: the built-in defaults, `/etc/af/config.toml`, the user's
`~/.config/af/config.toml` and `AF_TELEMETRY__<KEY>`. A directory, project or local
`.af/af.toml` that carries `[telemetry]` is reported (`af config show` prints
`# ignored: [telemetry] in …`) and ignored. A value outside its bound, or an unknown key, refuses
the load naming the key, and `af telemetry` then exits 2. `af help telemetry` prints the same
table.

| Key | Default | Bound |
| --- | --- | --- |
| `level` | `"spans"` | `off`, `metrics`, `spans`, `content` |
| `dir` | `""` (`$XDG_STATE_HOME/af/telemetry`) | an absolute path overlapping no Store, cache or other telemetry directory |
| `max_bytes` | `"4GiB"` | `256MiB` to `[storage] max_bytes`; the Storage Budget's sweep evicts sealed segments to meet it, and `af telemetry status` warns when the directory is over it that the next sweep evicts |
| `flush_bytes` | `"64KiB"` | `4KiB` to `16MiB` |
| `flush_latency_ms` | `1000` | 50 to 10000 |
| `observer_queue_bytes` | `"4MiB"` | `256KiB` to `64MiB` |
| `segment_max_bytes` | `"256MiB"` | `16MiB` to `4GiB`, the last 64 KiB reserved for the seal |
| `content_max_blob_bytes` | `"16MiB"` | `64KiB` to `256MiB`; larger content keeps head and tail |
| `content_attempt_max_bytes` | `"512MiB"` | `16MiB` to `8GiB` |
| `content_process_max_bytes` | `"64MiB"` | `4MiB` to `1GiB` |
| `content_native_body_max_chars` | `1048576` | 4096 to 8388608 |
| `content_reasoning` | `true` | boolean |
| `content_tool_io` | `true` | boolean |
| `content_model_bodies` | `true` | boolean |
| `content_index` | `true` | boolean |
| `harness_native` | `false` | boolean |
| `sink` | `"none"` | `none`; `otlp` is refused, not implemented in this release |
| `sink_endpoint` | `""` | empty; a URL is refused, not implemented in this release |
| `sink_headers_file` | `""` | empty; a path is refused, not implemented in this release |
| `sink_timeout_ms` | `5000` | 100 to 60000; reserved, no sink reads it |

There is no telemetry `keep_days`: collection by age is `[storage] keep_days` under
`[storage] auto_gc` or an applied prune, as for every kind of entry
([ADR-0144](adr/0144-hold-afs-disk-use-to-a-machine-budget.md)).

## `af telemetry status`

```text
af telemetry status [--json] [--dir PATH]
```

Prints the configured level; the telemetry directory and its bytes — the allocated bytes of its
regular files, a file several hard links reach once, directories and symbolic links nothing, so
ext4 and APFS give the same numbers (ADR-0159 clause 13) — in total against
`[telemetry] max_bytes` and per entry (each segment with its manifest; the blobs, which are only
the two-hex prefix directories under `v1/blobs/` and the digest-named objects and publishers'
temporary files in them; the index with its WAL and shared-memory files; temporary files; locks;
and `unrecognized`: every other file or directory beneath the telemetry directory, `v1/blobs/`
included — a stray name, and a symbolic link wherever it lies, which is never followed —
counted once with its count, its allocated bytes and up to 16 of its names, never read
and never removed, so the total is every file's allocated bytes beneath the directory), each sealed
segment with its seal's `exit` and whether its capture is `complete` or why not (`recovered, drops unknown` for a
segment recovered without its writer's seal, `recovered`, `12 dropped`); the segments by state —
`live` (a writer holds its lock), `unsealed` (nobody holds it: a crashed writer's, recoverable),
`sealed` and `corrupt` (interior corruption, never read); the runs the index knows; the Attempts
whose content was withheld; the stored occurrences whose blob file is missing (reported as
`content unavailable: missing blob`, never a failed query), looked for with `gc.lock` held
shared — when a sweep keeps it longer than two seconds the check is reported `deferred` and no
blob file is looked at; the index after this command
ingested every new segment byte; the distinct problems recorded; what cannot be read (a
directory of another telemetry version such as `v0/` is listed and never read); and every
telemetry directory registered on this machine, with its overlap rechecked: `none`,
`overlapping` with the reason, or `unknown` when the Store registry (`stores.toml`) cannot be
read or is of another version — then every directory's check is `unknown`, the registry problem
is named among the problems (`registry`), and the exit code is unchanged. It never recovers or
removes a segment.

When the directory holds more than `[telemetry] max_bytes`, `status` says so on stderr, naming
the next storage sweep that evicts sealed segments to meet it, and marks the bytes line `over`.
`--json` prints one
`af/telemetry-status@1` document
([`schemas/telemetry-status-v1.json`](../schemas/telemetry-status-v1.json)); its paths are
written as the text output writes them, a path under the home directory as `~/…`.
`--dir PATH` reads
another telemetry directory, such as one copied from another machine. Exit codes: 0; 1 when the
directory cannot be read (or `--dir` names none) or its index is of another schema version;
2 for a wrong command line or a refused `[telemetry]` key.

A `.jsonl` segment in which an invalid line precedes a valid one is unreadable at once: none of
it is indexed, and it is listed as unreadable until `reindex` renames it `.corrupt`. A line over
the 16 KiB record bound is an invalid line, so a sealed segment holding one is refused and
listed, never indexed. An `index.sqlite` of a schema version other than this release's (`af/telemetry-index@3`) is
refused, never migrated or read around: `status` exits 1 naming the file and both schemas, and
`af telemetry reindex` replaces it. Only when another command is replacing the index does
`status` read the segments directly, saying so on stderr; its answer is the same. The index
remembers each sealed segment file it read by its identity (device, inode, size and
modification time): a `.jsonl.gz` replaced or rewritten under its name since is read and
validated again, and one that no longer reads has its earlier rows dropped and is listed as
unreadable, so `status` answers as `reindex` would.

## `af telemetry reindex`

```text
af telemetry reindex [--json] [--dir PATH]
```

Recovers every unsealed segment whose lock nobody holds — keeping its longest prefix of
complete, valid lines, recording the discarded bytes in a seal with `exit = "recovered"`,
marking spans it never ended with no end — or renames it `.corrupt` when an invalid line
precedes a valid one. A recovered seal whose writer never sealed has `dropped` and
`dropped_chunks` `null`: what the writer dropped is unknown, and the capture is incomplete. A
writer that died after writing its seal but before compressing has that seal replaced by the
recovered one, which keeps the writer's counts, so the segment still ends with exactly one seal
and never reads as a clean exit. Then it replaces the index under `index.lock` (taken
exclusively within two seconds; new readers wait on `index.intent` meanwhile), rebuilds it from
every segment and verifies every blob file's digest with `gc.lock` held shared for the whole
scan; when that lock is not had within two seconds no blob file is opened and the document says
`blob_verification = "deferred"` instead of reporting corruption, with the same exit code.
Every query answers the same before and after: the index is derived state, never evidence.
`--json` prints one `af/telemetry-reindex@1` document
([`schemas/telemetry-reindex-v1.json`](../schemas/telemetry-reindex-v1.json)). Exit codes: 0;
1 when the directory cannot be read or another command holds the index; 2 as for `status`.

## `af telemetry runs`

```text
af telemetry runs [--since D] [--kind K] [--json] [--dir PATH]
```

One entry per run, newest activity first: its Store (`af.store_id`), run id, kind, Task or
Campaign, its retained executions and first start, its active, execution and elapsed time, the
latest execution's own outcome — the execution with the greatest committed ordinal, never the
latest start; none while that execution's `af.run` has not ended, never an earlier execution's,
and none when no execution carries an ordinal — the Attempts' exact usage counters and charge, each
Attempt's charge snapshot with the highest `af.usage.sequence` or, when none is retained, its
span end (summed exactly; a counter some Attempt did not deliver is absent, never zero; the
sidecar series never enters them), failures, coverage
(`complete` when every retained segment was sealed by its writer with nothing dropped) and its
history. Its `executions` list every retained execution, the sealed ones' ordinals and, beside
them, every open one (`executions.open`, and one `open:` line each in the text); its label and
`totals_cover` say which executions every total covers, each open one marked `open`.
`--since 14d` keeps runs active within that window (`s`, `m`, `h`, `d`, `w`);
`--kind` keeps one kind. `--json` prints one `af/telemetry-runs@1` document
([`schemas/telemetry-runs-v1.json`](../schemas/telemetry-runs-v1.json)): durations in
microseconds, times as unix nanoseconds in decimal strings. Exit codes: 0; 1 when the directory
or its index cannot be read; 2 for a wrong command line or a refused `[telemetry]` key.

## `af telemetry show`

```text
af telemetry show RUN [--store ID] [--attempt ID] [--span ID] [--depth N] [--json] [--dir PATH]
```

The run's span tree, from each execution's `af.command` down — `af.run`, `af.authority.load`,
`af.plan`, every `af.node` and its `af.attempt`, and the command's `af.sweep` — each span with
its start offset from the run's first record, its total and self time (total less the union of
its ended children), its status (`ok`, `error`, `unset`, or `open` with no recorded end) and its
key attributes; the run's executions, history and label head it, the open executions listed
beside the sealed ones (`run.executions.open`, `(open)` in the text). `--attempt ID` keeps that Attempt's
subtree with its ancestors, `--depth N` stops N levels below the root, `--span ID` prints one
span with every attribute and its events. When two Stores record the run id the command lists
both and asks for `--store ID` (exit 2). `--json` prints one `af/telemetry-show@1` document
([`schemas/telemetry-show-v1.json`](../schemas/telemetry-show-v1.json)). Exit codes: 0; 1 when
the directory cannot be read or the run, Attempt or span is unknown; 2 for a wrong command line,
a run two Stores share without `--store`, or a refused `[telemetry]` key.

## `af telemetry tail`

```text
af telemetry tail [RUN] [--store ID] [--json] [--dir PATH]
```

Like `tail -f`: every live segment (with `RUN`, those that carry that run) is read from its start
and followed, and each span end (with its duration and status) and each event is printed as its
writer writes it. It exits once every followed segment is sealed — its sealed file published
and readable, not merely the seal line written to the live file while the writer still
compresses or publishes — or recovered; with no live segment it waits for one. With `RUN`, a run
whose every execution was sealed before tail started is printed from its sealed files, as the
follow would have printed it, and tail exits 0 at once: nobody is writing it. A `.jsonl`
segment no writer holds (a writer that crashed before its seal, or abandoned its publication)
is recovered by tail as by any reader able to lock it, and its recovered sealed file read; tail
never waits on a prefix that cannot change. A followed segment that cannot be read — a sealed
file, or live bytes with an invalid line before a valid one (interior corruption) — fails tail,
naming the file, with nothing of that read printed (exit 1). So does a followed segment that is
gone without a sealed file tail could validate or a recovery tail itself completed: when another
reader's recovery renamed it `.corrupt`, tail names that `.corrupt` file; it is never taken as
sealed. `--json` prints one
`af/telemetry-tail@1` object per line
([`schemas/telemetry-tail-v1.json`](../schemas/telemetry-tail-v1.json)). With `RUN` tail first
looks the run up, within `--store ID` when given: a run no Store records there exits 1 naming
it, and nothing is followed. A run that cannot be found while a segment is unreadable is never
called unknown, since that segment may hold it: tail names the segment and why it is unreadable
and exits 1 without following anything. A segment recovery renamed `.corrupt` is never indexed,
so tail looks into it for the run: when it held one of the run's executions (one the index had
resolved as the run's when tail started, whatever its damaged bytes still say — the damage may
be the `af.run` line itself — or an `af.run` of the run, or an invalid line naming the run id),
or the run cannot be found at all while any `.corrupt` segment exists, tail names that
`.corrupt` file and exits 1 instead of finishing over the readable executions or waiting for
the corrupt one. Before it calls a run unknown it lists the segments directory again, so a
recovery that renamed a segment `.corrupt` while the run was being looked up is named too.
Tail looks again at every look while it follows and once more before it exits 0: a segment
another reader's recovery renamed `.corrupt` after tail started, before tail ever saw it, fails
tail the same way when it held one of the run's executions. It
then follows that run in one Store for its whole lifetime: the
one `--store ID` names, else the one Store that records it when tail starts. When two Stores
record the run id without `--store`, at the start or once a second Store starts it while it is
followed (also in a segment created and sealed during tail's startup or between two of its
looks), tail prints every pending record of the followed Store, then lists both and asks for
`--store ID` (exit 2); it prints nothing of the second. It never removes a segment.

## The Store mirror and the ledger

**The mirror.** A recording command installs one post-commit observer on every Store it opens.
The Store tells it of every transaction it commits — a generic append, a Task transition, a
Round publication, an atomic integration, a collection's tombstones — once, after the commit
returned, with that transaction's events in committed order, and never of a transaction that
rolled back. The Store links nothing of Telemetry and the observer reads only what it is handed
and the Store's read API: no transaction is longer for it, nothing it does is a Store error,
and the Store's event log is byte-identical at `off` and `content`. From `spans` on each
committed event becomes the event `af.store.event` on the current span (the run's, or, for a
commit before the execution's `af.run` opened, the command's) with `af.store_id`, `af.run_id`
(the Store log: a Task's `task:…` or a Campaign's run id), `af.store.event.type`,
`af.store.event.sequence`, `af.store.event.id`, `af.store.event.causation_id`, `artifact_refs`,
and the payload's digest and size (`af.store.event.payload_digest`,
`af.store.event.payload_bytes`); the payload itself is not recorded. A mirror failure is one
`observer` problem, never a Store error, and makes every run of that Store record
`af.ledger.coverage = incomplete` on its `af.run`; an execution that mirrored everything records
`complete`. `AF_TEST_TELEMETRY_MIRROR_FAIL` (a debug build's fixture setting, never read by a
release binary) makes every mirror write fail.

**Finding and Demand summaries.** From `spans` on, a Campaign log's committed events are folded
into its Ledger projection — the Reports they reference read from the CAS, as the Ledger reads
them — so that `af telemetry ledger` needs no Store. For every Report admitted into the Ledger
— a duplicate, or one that raises the effective severity of a Finding that stays `open`,
included — and whenever any summarized field of a Finding changes, a `finding` record
(`af/TelemetryFinding@2`) carries the Store log (`log`) and the committed `sequence` of the
event that caused it, the Finding's key, severity, effective severity (`out` when its active
claims are wholly out of Scope), status, scope, file, line, title, the latest Report's artifact
id, the node, the Worker, the model the kernel bound the admitting Worker to (from the plan's
bindings, by its Attempt or its node; absent when there is none, as for a command Worker), the
Round and the epoch. A change of status is also the event `af.finding.disposition` on the run
span with `af.finding.id` (the Finding key), `af.finding.status`, `af.finding.previous_status`
and the causing event's `af.store.event.sequence`. A Demand is its own obligation (ADR-0007): a
`demand` record (`af/TelemetryDemand@2`) keyed by the Demand id carries the causing event's log
and sequence, its kind (`required` or `advisory`), whether it is required, the Demand's own
Subject and the Snapshot bound to that Subject — in every later status summary too, never the
head of a Subject a later Round reviews — and its status (`open`, `satisfied`, `stale`,
`waived`), and its lifecycle is the events `af.demand.opened`, `af.demand.evidence_satisfied`
(with `af.evidence.id`: a satisfaction, or an admitted Evidence reuse that takes a stale Demand
back to `satisfied`, with the reused satisfaction's Evidence id), `af.demand.stale` and
`af.demand.waived`, each with `af.demand.id`, never a Finding key. A change committed by a command that records no
Telemetry (an operator's `af review evidence satisfy`, a resolution) is summarized by the next
recording execution that commits to that Campaign's log, at the committed event's own sequence;
the event itself was not mirrored, and the ledger says so (below).

**Accounting: two series, never merged.** From `metrics` on, after every committed Task event
that changes an Attempt's accounting — `UsageObserved`, `Settled` (a recovery's included: usage
that was reported settles at the greater of the observed usage and the reservation, usage that
was never reported at zero with `af.usage.unknown_cause = lease_expired`, ADR-0143) and
`Released` — the event `af.usage.state` on the Attempt's span (the run's when this process did
not open the Attempt) records the Attempt as the Task projection holds it after that commit:
`af.usage.chargeable_tokens` (the retained charge, as `af task show` reports it), the counters of
the usage the transition named, `af.usage.observation` (`complete`, `incomplete`, `unknown`) and
`af.usage.unknown_cause`, the native usage observation when the transition carried one
(`af.usage.native.observation` and `af.usage.native.*_tokens`; a raw artifact of the transition
the CAS holds but cannot read is an `observer` problem naming it, and the event is recorded
without a native observation), `af.usage.source`
(`usage_observed`, `settled`, `released`) and `af.usage.sequence`, the committed event's own
sequence in the Task's log. Every committed write of the `attempt_wall` sidecar records the
event `af.usage.wall` with the merged row (`af.usage.wall.started_unix_ms`,
`af.usage.wall.elapsed_ms`, the row's counters), `af.usage.source = sidecar` and
`af.usage.wall.sequence`, the record's own `seq`. That sequence orders one execution's writes
and nothing else: executions overlap and the sidecar is written without a lease, so a
predecessor's write can land after its successor's. Counters are exact decimal strings in both.

## `af telemetry ledger`

```text
af telemetry ledger RUN [--store ID] [--json] [--dir PATH]
```

One run's ledger from the telemetry directory alone, never a Store: every mirrored Store event
(its log, sequence, type, id, causation, artifact references and the payload's digest and size),
every Finding and Demand summary and every lifecycle event, each Store log in its committed
sequence — an event by its own, a summary or a lifecycle event by the sequence of the event
that caused it — never in the order they were recorded: two Store connections whose post-commit
observers ran in the opposite order, or two overlapping executions of which the later committed
first, read in commit order, identically after a `reindex`. At one sequence the event comes
first, then the Finding summaries, the dispositions, the satisfactions, the Demand summaries and
the other Demand lifecycle events, each by Finding key or Demand id, then by execution and
`seq`. Then each Finding's latest summary (the one of the greatest sequence) with its
dispositions, each Demand's with its lifecycle, both in that order, and each Attempt's
accounting.
What belongs to a run is what is on the spans of its trace, and what is on the `af.command` span
of a segment holding one of its executions and names its Store. A lifecycle event two
executions recorded (the second folded the log from its start) is listed once.

An Attempt's figures are its `af.usage.state` with the highest `af.usage.sequence` — the
kernel's own accounting after its last committed accounting transition, never a value the reader
merges — and beside them the latest `af.usage.wall` of each execution that wrote one, labelled
by its execution, with `wall_order = unknown` when more than one did; the reader never takes a
maximum, a sum or a replacement of those. The run's totals are exact sums of the Attempts'
charge snapshots, with the Attempts whose usage is unknown counted; `af telemetry runs` sums the
same snapshots. The history marker (`prefix:k` or `partial`) says which executions everything
covers.

**Coverage** is `complete` only when every retained execution's segment was sealed by its
writer with nothing dropped and its `af.run` recorded `af.ledger.coverage = complete`, the
history is a prefix with no execution missing or open, no Store mirror problem was recorded,
and every Store log the run's executions mirrored has every sequence from 0 to the last one
mirrored; otherwise `incomplete`, every reason listed (`coverage_reasons`), each log's missing
sequences named. A run recorded at `metrics` mirrors nothing and is `incomplete`. When two
Stores record the run id the command lists both and asks for `--store ID` (exit 2). `--json`
prints one `af/telemetry-ledger@1` document
([`schemas/telemetry-ledger-v1.json`](../schemas/telemetry-ledger-v1.json)). Exit codes: 0; 1
when the directory cannot be read or the run is unknown; 2 for a wrong command line, a run two
Stores share without `--store`, or a refused `[telemetry]` key.

## Harness streams

At `spans` and above every Claude and Codex Attempt's streams are tapped as they arrive. The
supervisor (`review-process`, which links no telemetry) still drains each pipe into the complete
stream the adapter reads; for every chunk it reads it also makes one **non-blocking offer** of
`(process, stream, byte offset, bytes, arrival)` to a queue of at most `observer_queue_bytes`.
The offer takes no lock: the room is reserved by one atomic operation, the chunk goes through a
channel whose send never blocks, and the consumer thread is woken by an unpark, which never
blocks and is never lost. A full queue — a stalled or failed consumer — drops the chunk at once
and counts it; so the drain, what the supervisor collects, its status, its deadline, a held
stdout (`OutputHeld`) or stderr (`stderr_held`) with its retained bytes, and the accounting are
the same as without Telemetry. The process's completion never waits on the consumer, which
finishes the process's chunks afterwards; at the command's end the consumer's bound of the
consumer section above applies — 2 s beyond its last progress, each content of a released stream
it publishes renewing it.

The consumer frames each stream into lines incrementally and records, live, **validated
structure only**: the record's kind, counts, byte sizes, the arrival time of each line (the
drain's clock at the read), the bound model when the harness reported it as the model that
answered, stop reasons and item statuses from their closed sets, tool names that are in the
invocation's own tool set, and **correlation ids** — `af.corr`, the first 16 hex digits of
SHA-256 over the segment id, `:` and the original — in place of every identifier the harness
supplies (session, thread, message, request, tool call, item). Every untrusted string — text,
thinking, tool arguments and results, agent messages, reasoning summaries, error texts,
unparsed lines and stderr — is only a size in a record; at `content` its bytes wait in a
per-process buffer bounded by `content_attempt_max_bytes` (overflow drops the oldest and counts
it), for the content package to write as blobs.

- **Claude** (`stream-json`): `system/init` is the event `af.harness.init` on `af.harness`
  (`gen_ai.conversation.id`, the tools in the tool set, the MCP server count, a version in a
  version's grammar); each `assistant` message is an `af.turn` (`gen_ai.operation.name = chat`)
  from the arrival of the line that began it — the init, or the tool results that went back to
  the model — to the message's line, which ends the turn as soon as it is read, so `tail` and
  `turns` show it ended before any later line arrives; the turn carries the harness's per-turn
  usage (`af.usage.*` as exact decimal strings, `gen_ai.usage.input_tokens` including cache reads
  and writes as the conventions count it, `af.series = turn`), `gen_ai.response.finish_reasons`,
  and the number and bytes of its text and thinking blocks. Claude prints a message as one line
  per content block: a further line of the message that ended the last turn, before the tool
  results go back, opens no turn but is the event `af.turn.continued` on that ended turn, with
  the blocks, tool calls and report the line adds, and `af telemetry turns` folds it into the
  turn's row. Each `tool_use` block is an `af.tool` below the turn its message ended
  (`gen_ai.operation.name = execute_tool`, `gen_ai.tool.name`, `gen_ai.tool.call.id`,
  `af.tool.input_bytes`) until the `tool_result` that answers it (`af.tool.output_bytes`, its
  outcome), and the next turn opens when the tool results go back; `system/api_retry` is
  `af.turn.api_retry` on the open turn; `rate_limit_event`, `permission_denied` and the `result`
  line are `af.harness.rate_limit`, `af.harness.permission_denied` and `af.harness.result` — the
  result's text, which repeats the reply the assistant's text blocks carry, only as its size
  (`af.result.bytes`), never pended as content.
- **Codex** (`--json`): `thread.started` is `af.harness.init`; a turn is an `af.turn` from
  `turn.started` to `turn.completed` (its usage) or `turn.failed`; a `command_execution` or
  `mcp_tool_call` item is an `af.tool` from `item.started` to `item.completed` (its exit code and
  status); `reasoning`, `agent_message`, `file_change`, `web_search` and plan items are the
  events `af.item.reasoning`, `af.item.agent_message`, `af.item.file_change`,
  `af.item.web_search` and `af.item.todo_list` with their sizes and counts; an `error` line is
  `af.harness.error`. Telemetry adds no `-c` key, and a Codex Attempt records
  `af.native = unavailable`.

**Frames and gaps.** A frame longer than `content_max_blob_bytes` before its newline is
discarded whole through the next newline, recording only `af.stream.frame_withheld` with its
size: a line too long to be judged whole cannot be judged at all, so no prefix of it is parsed,
redacted or kept. A chunk whose offset is not the one expected — the queue dropped what lay
between — is `af.stream.gap` with the missing byte range; the partial line is discarded and
framing resumes after the first newline past the gap, so the two sides of a drop are never
joined into a line the harness did not write. A stream shorter than what the supervisor's
drain read in total — past the output cap included, which bounds only what is kept — ends in
a trailing gap. `af.harness` counts the drops and gaps (`dropped_chunks`,
`gaps`), and a stream with any of them, or with a withheld frame, is `incomplete` for turn and
tool analysis.

**The release decision** is Telemetry's own, taken when the process has ended, over its
complete stdout and stderr and the output the harness left outside them — a Codex Attempt's
`-o` final-message file, read in full before the tap ends for every observed process, whatever
its ending and whether or not it is a reply — before any pending byte goes anywhere: when any
of them carries login-challenge material (a device code beside a challenge URL, in one record
or several, in one stream or across both, or a code in a stream identifier with its URL only in
the final message, a successful reply's or a failed process's included), when that file exists
but cannot be read whole (larger than the reply bound, not a regular file, unreadable), when the
adapter replaced both streams with its closed diagnostic, when a failed process's stderr
classifies as an authentication failure, or when the output cap truncated a stream (the
supervisor kept a prefix of what the observer was offered whole, so the decision cannot read
it whole), everything pending for the process — content and
identifiers — is withheld, and `af.harness` records `af.stream.withheld = true` and `af.native.withheld = true`
(`af telemetry status` counts these Attempts). It changes no accounting, no classification and
no adapter behaviour. Otherwise the pending content passes the grant replacement and the
credential rules on its assembled, complete lines — a grant value split across two chunks is
replaced all the same — and one `af.harness.ids` event per process lists each correlation id
with its original (`af.harness.ids`, `corr original` pairs), each original having passed the
credential rules; a list longer than one record's array holds whole (32 pairs, 4 KiB) goes on
in the `af.harness.ids.part` events that follow it, as many as its `af.harness.ids.parts`
says, each numbered by `af.harness.ids.part`, so no admitted mapping is cut. The event also
carries `af.harness.ids.count`, every identifier admitted to the pending buffer, and
`af.harness.ids.withheld`, those the rules or the decision withheld, the stream's counts and its
`af.stream.analysis` (`complete` or `incomplete`). A process pends at most 4096 distinct
identifiers of at most 256 bytes each: one past either bound keeps its `af.corr` in the live
records but has no mapping, `af.harness.ids.omitted` counts these (each once; past 65,536 of
them the count is a floor) and `af.harness.ids.analysis` is then `incomplete`, else
`complete`. Because the `af.harness` span records that
decision, it ends once the adapter's in-memory filter has read the complete streams, not at the
process's exit.

**What Claude is asked to print, and what the Store keeps.** At `spans` and `content` a Claude
Attempt runs `claude -p --output-format stream-json --verbose`; at `off` and `metrics` it runs
`--output-format json`, because the invocation-cost gate failed on this machine
([Performance](#performance)). `claude --help` must declare both `--output-format` and
`--verbose` either way. Under both shapes the adapter's envelope parsing, accounting, refusal
classification and model-identity check read the `result` line, which is the envelope `json`
prints; without one, failure classification reads the native text around the messages, from the
complete capture in memory. Only the `json` invocation's stdout is an envelope without a
`result` line (one JSON object with no `type`); under `stream-json` such an object is native
text, never parsed as an envelope, never a reply and never kept. The Store CAS keeps the
`result` line (or, under `json`, an envelope that frames no message) and stderr, never another
message or native text, however large. A Codex
Attempt's Store keeps its `turn.completed`, `turn.failed` and `error` lines, the `-o`
last-message file and stderr; the native text outside the event framing is classified in memory
and not kept. When the adapter replaced a failed capture with its closed diagnostic, that
diagnostic is what the Store keeps. So an Attempt's accounting, classification and raw evidence
are the same at every level; every other line reaches Telemetry at `spans` and above and nothing
at all below.

**Claude thinking under `--safe-mode`.** Checked by hand on 2026-10-21 with Claude Code 2.1.305
and the model af binds, `claude-opus-5-5` at efforts `high` and `xhigh`, one short `-p`
invocation each with the adapter's flags (`--output-format stream-json --verbose --safe-mode
--restricted --permission-mode dontAsk --strict-mcp-config --tools Read,Glob,Grep`) and its
environment: the stream carries `thinking` blocks — one per message that reasoned, beside its
`text` block — but each holds an empty `thinking` text and only its `signature`. So Claude
reasoning is sizes only: `af.turn.thinking_*` counts the blocks, their size is 0, and at
`content` `af telemetry reasoning` has no Claude text to show.

## `af telemetry turns`

```text
af telemetry turns RUN [--store ID] [--attempt ID] [--json] [--dir PATH]
```

One row per model turn of the run's harness streams, in start order: its harness process,
Attempt and node, its ordinal in its stream, its start (unix nanoseconds and the offset from
the run's first turn or tool call), duration and status (`ok`, `error`, `unset`, or `open` with
no recorded end), its outcome (`succeeded`, `failed`, `incomplete`), its correlation id, the
bound model when the harness reported it as the answering model, its stop reasons, the
harness's per-turn usage (exact decimal strings and its observation; the `turn` series, never
added into an Attempt's or a run's totals, which are the kernel's), and the number and bytes of
its text and thinking blocks and its tool calls. Each row's harness says whether its stream can
be analysed whole — `complete`, `incomplete`, or `open` while the tap has not ended — with the
observer's `dropped_chunks` and `gaps` and whether its content was withheld. `--attempt ID`
keeps one Attempt's turns. When two Stores record the run id the command lists both and asks
for `--store ID` (exit 2). `--json` prints one `af/telemetry-turns@1` document
([`schemas/telemetry-turns-v1.json`](../schemas/telemetry-turns-v1.json)). Exit codes: 0; 1 when
the directory cannot be read or the run or Attempt is unknown; 2 for a wrong command line, a run
two Stores share without `--store`, or a refused `[telemetry]` key.

## `af telemetry tools`

```text
af telemetry tools RUN [--store ID] [--attempt ID] [--json] [--dir PATH]
```

One row per tool call of the run's harness streams, in start order: its harness process, Attempt
and node, the turn that called it, its name when it is in the invocation's own tool set
(`(unlisted)` otherwise), its correlation id, its start, duration and status, its outcome, the
bytes of its arguments and of its result, and a command's exit code and an item's status. Each
row's harness says whether its stream can be analysed whole, as for `turns`. `--attempt ID`
keeps one Attempt's tool calls. `--json` prints one `af/telemetry-tools@1` document
([`schemas/telemetry-tools-v1.json`](../schemas/telemetry-tools-v1.json)). Exit codes as for
`turns`.

## `af telemetry gc`

```text
af telemetry gc [--apply] [--json]
```

What the Storage Budget's sweep would evict from the telemetry kind, listing the telemetry
entries exactly as `af storage` lists them; `--apply` evicts it. The rules are ADR-0144's,
unchanged: least recently used first (a corrupt segment first among equals) while af is over
`[storage] max_bytes` or telemetry over `[telemetry] max_bytes`, and by age past
`[storage] keep_days`, never an entry in use or used within the last hour. Applied, it then
reclaims temporary files older than an hour, blobs no remaining segment references and the
index's free pages, and drops registered directories that no longer exist. An entry found
changed when it is checked again before it is unlinked is left in place and listed under
`changed` (below). Nothing of another kind is touched. `--json` prints one `af/telemetry-gc@1`
document
([`schemas/telemetry-gc-v1.json`](../schemas/telemetry-gc-v1.json)).

## Telemetry in the Storage Budget

Every byte of every regular file beneath every registered telemetry directory is counted once in
`af storage` as the kind `telemetry` (`af/storage@4`,
[`schemas/storage-v4.json`](../schemas/storage-v4.json)), whatever the current level or `dir`,
Telemetry `off` included, by the rule every kind is measured by — the allocated bytes of regular
files, a hard-linked file once, directories and links nothing (ADR-0159 clause 13):

- one entry per sealed or corrupt segment, whose bytes are its **marginal** bytes — what its
  eviction frees: the segment, its manifest and the blobs no other retained segment references —
  recomputed after every removal, so a blob two segments share is freed only with the second;
  a sealed segment's last use is its seal time, the `ended_unix_ns` of the seal record read
  from the file by a structural read that decodes its `segment` header and its `seal` record
  and nothing between them, never the file's modification time, which serves only a corrupt
  segment (and a sealed file whose header or seal cannot be read, which is reported);
- one entry per live segment (a writer holds its lock) or unsealed one, in use;
- one `unreadable` entry per directory of another telemetry version, evicted whole, never read;
- one `shared` entry for what no segment owns alone: the index with its WAL and shared memory,
  the locks, blobs several segments share, orphaned blobs and temporary files.

The sweep that ends every run, `af storage prune` and `af telemetry gc` evict telemetry under the
budget's rules (above). Each eviction is bound to what the inventory found: the inventory records
the telemetry directory's device and inode, those of every file the entry owns — a sealed
segment's `.refs` manifest included — and its last use, and before unlinking any of them,
through the directory descriptors it holds, the sweep checks again that the directory is still
that one, that every file of the entry is still a regular file under its name with the device
and inode inventoried and was not modified within the last hour, that no writer holds its lock
(or a live file of the segment's) and that it was not used within the last hour. An entry is
removed whole or left whole: a sealed segment's compressed copy and then its manifest are each
checked immediately before their own step and renamed under a staging name in the held
directory, a change found at the manifest restores the compressed copy, and only once both are
staged, and checked there again with no file reappeared under either original name, are they
unlinked; a file that took either name restores what was staged wherever its name is free. An
entry any file of which changed —
replaced, locked, used — is left in place whole and reported with what changed, never as
removed
(`telemetry.changed` in `af/storage-prune@4`, `changed` in `af/telemetry-gc@1`). Before it measures or collects anything an applied sweep recovers every
unsealed segment whose lock it can take, and reads the flushed prefix of every one it cannot for
the blobs it references, whether or not Telemetry is on. Every applied sweep then removes
temporary files under `segments/` and `blobs/` older than an hour, and the stale staging beside a
`.jsonl` older than an hour, without a lock, and under
`gc.lock` held exclusively (at most 2 s, else skipped this time) the blobs no retained segment
references; then it drops the evicted segments' rows from the index and, when its free pages
exceed 16 MiB and a quarter of the file, runs `VACUUM` — deleting the index under `index.lock`
when there is no room to vacuum (the next reader rebuilds it), deferring without the lock. What
it reclaims is measured as the allocated bytes of the files removed (`telemetry.reclaimed` in
`af/storage-prune@4`). It
reads only structural metadata: file sizes and times, lock state, manifests, a sealed segment's
header and seal records and the `blob_ref` records of a live or unsealed segment's valid prefix,
read by a structural scanner that checks each line's framing, record type and sequence and
decodes only `segment`, `blob`, `blob_ref`, `problem` and `seal` records, never a span, an
event or an attribute (the recovery an applied sweep runs first is the reader's own, which keeps
only valid lines and so validates the prefix it rewrites); a sealed
segment without its manifest leaves its references unknown and the blob collection refused. A telemetry segment it removes after a run is counted
among the omitted removals of the Task's `storage_sweep` record.

The overlap of every registered directory is rechecked at every inventory: a directory that lies
inside or contains a Store, a cache or another telemetry directory (a Store registered later
inside it, a registry edited by hand) is one `overlapping` entry — counted once by the containing
entry, never evicted as telemetry — and `af storage` names what it overlaps. A registered
directory that no longer exists is reported, then dropped from the registry by the next applied
sweep.

## The store

```text
$XDG_STATE_HOME/af/telemetry/
  v1/
    segments/<started_unix_ms>-<pid>-<nonce>.jsonl      live: one writer under an exclusive lock
    segments/<started_unix_ms>-<pid>-<nonce>.jsonl.gz   sealed: compressed at exit, immutable
    segments/<started_unix_ms>-<pid>-<nonce>.refs       sealed: the blob digests it references
    segments/<started_unix_ms>-<pid>-<nonce>.corrupt    interior corruption: never read
    blobs/<aa>/<sha256>.gz                               content-addressed, compressed, shared
    gc.lock                                              shared by publishers and readers, exclusive by a sweep
    index.lock, index.intent                             the index's lifecycle locks
    index.sqlite (+ -wal, -shm)                          rebuildable projection over the segments
$XDG_STATE_HOME/af/telemetry-dirs.v1.json                every telemetry directory ever written here
```

- **Segments.** One per process, opened at its first record and never before. Each line is one
  record (`af/TelemetrySegment@1`, `af/TelemetrySpan@1`, `af/TelemetryEvent@1`,
  `af/TelemetryConfig@2`, `af/TelemetryNode@1`, `af/TelemetryFinding@2`, `af/TelemetryDemand@2`,
  `af/TelemetryBlob@1`, `af/TelemetryBlobRef@1`, `af/TelemetryProblem@1`, `af/TelemetrySeal@1`;
  [`schemas/telemetry-record-v1.json`](../schemas/telemetry-record-v1.json)) of at most 16 KiB,
  carrying `seq`, `t` and `schema`. An attribute value over its bound (1 KiB for a string, 32
  elements or 4 KiB for an array of strings) becomes, at `content`, a blob occurrence the
  attribute names (an array as one JSON document); at every other level it is cut at the bound
  with a trailing `…`, counted in `af.truncated_fields`. The lock, never the PID in the name,
  says whether a writer is alive.
- **Blobs.** A blob's identity is the digest of the redacted, bounded bytes it keeps; each
  capture of it is its own occurrence record, so two sources that redact to the same bytes share
  one file and keep two occurrences. An occurrence carries its source's identity (the digest
  and size of the original bytes, before any redaction), its media type and the redaction its
  bytes passed; the shared object holds only what is intrinsic to the bytes. A JSON source
  longer than `content_max_blob_bytes` is withheld whole before it is parsed; longer text is
  redacted line by line and cut head and tail. An array of strings over its bound is measured
  before it is encoded: one whose JSON document is longer than `content_max_blob_bytes` is
  withheld whole, keeping its source identity, before anything as large as it is held. Blob
  collection acts only on two-hex prefix directories and digest-named or temporary files that
  are real directories and regular files, relative to the opened directory, and never follows
  a symbolic link out of the telemetry directory; readers never open a segment, blob, lock or
  index file through one.
- **Confinement.** Beneath a telemetry directory the only pathname operation is the first
  open of the telemetry directory itself, once per command or writer (it may be reached
  through a symbolic link you chose); every file and directory beneath it is opened, created,
  locked, renamed, measured and removed relative to a descriptor held from that open, each
  directory opened from its parent without following a symbolic link. A directory renamed or
  replaced, by a link or by another directory, after it was opened therefore changes nothing
  outside the telemetry directory that was opened, and nothing is read from the replacement.
  - *First opens* (one pathname open of the root, then descriptors): `Confined::root`,
    `Confined::open_root`, `Confined::create_root`, `Layout::open_versioned`,
    `Confined::versioned`, `layout::create_versioned`, `layout::exclusive_within` (a lock file
    named by path: the registry's, or a fixture's sweep), the writer's first write
    (`Writer::open_file`), `status::status`, `status::reindex`, `Index::open`,
    `Index::direct`, `index::replace`, `reader::list`, `reader::recover`,
    `reader::recover_all`, `collect::collect`, `collect::roots`, `blob::stage`,
    `blob::reader::{exists, read, content}`, `view::open`, `view::tail_start` (and
    `view::tail_start_interleaved`, its form with a fixture's barrier), `view::tail`,
    `budget::Directory::measure`, `budget::evict` and `budget::reclaim`; `reader::read_live` and
    `reader::read_sealed` read one file a caller names (fixtures) and are never called by the
    crate itself.
  - *Functions that hold descriptors* (they take the opened directory, never a path): the
    writer's `Tree`, `Consumer` (on the consumer thread from the first `af.run` until the seal),
    `publish` and `seal_file` (on the consumer thread); `index::delete_in`, `Index::forget_segments`; `Index::open_in`,
    `Index::direct_in`, `index::replace_in`, `Index::ingest`, `Index::missing_blobs`;
    `reader::list_in`, `read_live_in`, `read_sealed_in`, `recover_in`, `recover_all_in`,
    `manifest_digests_in`; `collect::collect_in`, `collect::roots_in`; `blob::stage_in`,
    `blob::place`, `blob::reader::exists_in`, `blob::reader::present_in`,
    `blob::reader::read_in` (with which `Index::ingest` keeps `content_fts` and
    `content_view::{context, reasoning, search}` read content, through the index's held `v1/`);
    `layout::lock_in`; and
    `status`'s `verify_blobs`, `blob_bytes` and `unrecognized`. `status` and `reindex` open
    `v1/` once and hand it to the index, to recovery, to the blob checks and to direct reading
    when `index.lock` is not had in time.
  - *The index database.* SQLite opens files only by path, so it is given a path that names
    the held `v1/` itself, never its pathname: on macOS its inode path
    `/.vol/<device>/<inode>/index.sqlite` with `SQLITE_OPEN_NOFOLLOW`, and where the file
    system offers no inode path the open is refused with an error naming the index (fail
    closed); on Linux the descriptor's own entry `/proc/self/fd/<fd>/index.sqlite`, which
    SQLite expands itself inside its open (it treats that entry as a link, so
    `SQLITE_OPEN_NOFOLLOW` cannot be set there), and the connection is refused unless the
    file SQLite resolved is in the directory the descriptor names now; on any other system the
    open is refused. The database, WAL and shared-memory files are opened together and must be
    regular files of the held `v1/`.
  - *Outside the telemetry directory*, by path: the directory registry, its lock and the
    canonical paths it records (each registered directory's existence as `status` reports it), a Store's canonical path for
    `af.store_id`, and the free-space figure a segment header reads before the writer's first
    open.

  A blob whose prefix directory is a link reads as `content unavailable: missing blob` and
  `status` counts it missing.
- **Registry.** Before its first write a process registers its telemetry directory
  (`af/TelemetryDirs@1`, [`schemas/telemetry-dirs-v1.json`](../schemas/telemetry-dirs-v1.json)).
  A directory inside or containing a Store, the warm check cache, a release installation or
  another telemetry directory is refused with one problem, and the run continues without
  telemetry.
- **Versions.** A release that changes a record shape writes `v2/`; this release lists any other
  version's directory as unreadable and never reads it.

## Content

At `[telemetry] level = "content"`, and only there, Telemetry keeps content beside the
structure every level records, each piece a blob named by one event:

| Content | Event | On | Allowance |
| --- | --- | --- | --- |
| The prompt bytes the Worker receives, as the kernel composed them | `gen_ai.content.input` | `af.context.compose` | the Attempt's |
| The context manifest the prompt was composed by | `gen_ai.content.system` | `af.context.compose` | the Attempt's |
| A harness's assistant `text` blocks and Codex `agent_message` items | `gen_ai.content.output` | the turn | the Attempt's |
| Its `thinking` blocks and Codex `reasoning` items, with `content_reasoning = true` | `af.content.reasoning` | the turn | the Attempt's |
| Its tool arguments and results, with `content_tool_io = true` | `af.content.tool_input`, `af.content.tool_output` | the tool call | the Attempt's |
| A check's stdout and stderr (`af.stream`) | `af.check.output` | `af.check` | the process's |
| A mirrored Store event's payload | `af.store.event` | as the mirror records it | the process's |
| An admitted Finding's body, keyed by `af.finding.id`, whenever it changes | `af.finding.body` | the run | the process's |
| `.af/af.toml`, the pipeline's `pipeline.toml`, the Task file | `af.config.text` | the run | the process's |

`gen_ai.input.messages`, `gen_ai.output.messages` and `gen_ai.system_instructions` are never
attributes: content is only ever a blob. A configuration text's blob has the text's own
SHA-256, the `af.config.text.digest` every level records, as its `source_digest`.

- **Redaction before truncation.** Every piece is redacted whole under the rules of
  [Credentials](#credentials) — text line by line, JSON as one document — and only then cut to
  `content_max_blob_bytes`, head and tail; a line or document too long to be judged whole is
  withheld whole. A JSON document `af` composed itself keeps its typed values where its kind
  declares them and judges everything else. In the Worker request those are each input and
  feedback value's `artifact_id` and `snapshot_id`, and inside its payload only the identity
  positions the schema of an artifact type `af` owns declares (`af/SourceTree@1`,
  `af/TaskCheckReceipt@1`, `af/TaskRetryFeedback@1`, `af/TaskReviewAssignment@1`,
  `af/TaskReviewSubject@2`, `af/ReviewHistory@1`, `af/TaskRepairContext@1`,
  `af/DocumentContext@1`, `af/WorkerNotes@1`); every other payload — the Task's requirements
  (`af/Requirements@1`, its `specification` included), another Worker's output — is untrusted
  JSON, whatever its members are named (`api_token_id`) or its values look like. In a Store
  payload and the manifest the typed positions are the members named `id`, `*_id`, `digest`,
  `*_digest`, the elements of `*_ids` and `artifact_refs`, and the token counters `tokens` and
  `*_tokens`. A value there keeps only when it is exactly a kernel identity or a number; the
  instructions, the output schemas and every other value are untrusted text judged by the
  credential rules — a credential under an `_id` name or a secret-named member is withheld, and
  `context --raw` then differs from the prompt there, while a digest, which is no credential,
  stays as text and never as a typed identity. Each occurrence's `source_digest` and `original_bytes` describe the bytes as
  they were, so `context` shows that the prompt it prints is the one the Worker received.
- **Harness content follows the release decision.** A harness's text, thinking and tool
  content is pended while its process runs and never written live; once the process has ended
  and Telemetry's own release decision withheld nothing, each piece is judged from its
  original bytes — the Provider grants replaced, credential lines withheld — and kept, in the
  order it arrived, on the turn or tool call it came from. When the decision withholds the
  process's content, none of it is kept. Its error texts, its stderr and its unparsed lines stay
  sizes. A switched-off kind is never even pended; `af.harness.ids` records the switches the
  content was kept under (`af.content.reasoning`, `af.content.tool_io`) and how many pieces were
  kept (`af.content.published`) and dropped (`af.content.dropped`).
- **Allowances.** An Attempt's content — its composed input and its harness's content — is
  bounded by `content_attempt_max_bytes`, and content outside any Attempt by
  `content_process_max_bytes`, counted in retained bytes of the pieces stored: a piece that
  fits but that publication drops (a sweep holds `gc.lock`, a blob file cannot be staged) is
  given back to its allowance. A harness's pending content past the
  Attempt's allowance drops its oldest pieces; at publication, past either allowance every
  further piece of that scope is dropped. A dropped piece still has its event and an
  occurrence with `state = dropped` and no object, the seal's `dropped` counts it, and one
  `budget` problem says which allowance was reached. Nothing about the Worker changes: no
  process is terminated, nothing is cut from what the Store receives, and the run continues.
- **On disk.** A blob is compressed alone, with `flate2`'s gzip. On this repository's own
  sources and documents (558 files, 10.6 MiB), each compressed alone as a blob is, the ratio is
  3.8:1, and 4.2:1 as the JSON strings of tool results; so an Attempt that fills the default
  512 MiB allowance keeps about 135 MiB on disk, and the default stays. Model-written text and
  thinking were not part of that measurement.
- **Search.** Every `af telemetry` command keeps the FTS5 table `content_fts` over the text
  blobs the retained occurrences reference at ingest, when `[telemetry] content_index = true`
  (the default); a blob no occurrence references any more leaves it, and `reindex` rebuilds it
  identically. With `content_index = false` no full-text index is kept: ingest empties
  `content_fts` and its bookkeeping table `content_index`, and rebuilds both from the retained
  blobs once the switch is on again. The index's schema is `af/telemetry-index@3`.

**The content is the operator's and never a Worker's.** Nothing Telemetry keeps is read by any
code that composes a prompt, a Warm Set, Worker Notes or an optimization source; the
self-optimizer's sources are unchanged. A Worker's prompt is the same byte for byte whether the
telemetry directory is empty or full of earlier runs' content. Content is also the operator's to
protect: it is the run's model inputs, outputs and reasoning, kept on this machine under the
storage budget, redacted but not anonymized.

## `af telemetry context`

`af telemetry context RUN ATTEMPT [--store ID] [--raw] [--json] [--dir PATH]` prints the
composed Worker input of one Attempt (`ATTEMPT` is the kernel Attempt id `show` names): its
`af.context.compose` span with the manifest digest, the rendered size, the estimated tokens and
the layers; each blob's occurrence, digest, retained and original sizes; then the prompt.
`--raw` prints only the prompt's retained bytes, exactly. Below `content` it says the content
was not captured, naming the level the execution recorded at; a dropped piece or a missing blob
file is reported for its occurrence. `--json` prints `af/telemetry-context@1`
([`schemas/telemetry-context-v1.json`](../schemas/telemetry-context-v1.json)); `--turn`, the
request the model saw at one turn, comes with the harnesses' own telemetry. Exit codes: 0; 1
when the directory cannot be read or the run or Attempt is unknown; 2 for a wrong command
line, a run two Stores share without `--store`, or a refused `[telemetry]` key.

## `af telemetry reasoning`

`af telemetry reasoning RUN ATTEMPT [--store ID] [--json] [--dir PATH]` prints an Attempt's
thinking blocks and reasoning summaries in the order they arrived, each with its turn, its
occurrence and its retained text, after each of the Attempt's harness processes with the level
it recorded at and its state: `captured`, `not_captured` (below `content`, or
`content_reasoning = false`), `withheld` (the release decision withheld the process's content)
or `open` (not released yet). `--json` prints `af/telemetry-reasoning@1`
([`schemas/telemetry-reasoning-v1.json`](../schemas/telemetry-reasoning-v1.json)). Exit codes as
for `context`.

## `af telemetry search`

`af telemetry search TEXT [--since D] [--run RUN [--store ID]] [--json] [--dir PATH]` finds the
retained text blobs holding `TEXT` as one phrase (no query operators) in `content_fts`, at most
50 blobs and 20 occurrences each, and prints each blob's digest with an excerpt of at most 320
bytes of its redacted text and its occurrences: the event that names it, its time, its run, its
Attempt and its span path from `af.command` down. `--since` keeps occurrences of the last `D`
(`30m`, `12h`, `14d`, `2w`); `--run` keeps one run's. Both filters are part of the query, which
reads matches in pages of at most 51 and stops once it can say `more`, so a common phrase over
a large directory costs no more than the rows it shows. An excerpt is shown only once its blob
was read under `gc.lock` and verified against its digest, as `context` reads it: a blob file
that is missing or corrupt is listed under `unreadable` with the reader's reason (`content
unavailable: missing blob`, `… corrupt blob`), shows nothing, and leaves the full-text index, so
no later search returns its text; one a collection holds is listed and left for the next
search. It exists only at `content`: below it, or
with `content_index = false`, it is refused with exit 2 and a message saying why, because
nothing searchable is kept. `--json` prints `af/telemetry-search@1`
([`schemas/telemetry-search-v1.json`](../schemas/telemetry-search-v1.json)). Exit codes: 0; 1
when the directory cannot be read or the run is unknown; 2 for a wrong command line, a refused
`[telemetry]` key, or a level below `content`.

## Not yet captured

The harnesses' own OpenTelemetry export (with `context --turn`), the OTLP sink, and the
analytics, `sql` and `export` commands belong to later packages of the design note.

## Performance

Design §12 is the contract. At `spans` Telemetry may cost at most 2% of a command's wall time,
its finalization included, on the pinned workload below; this is a release gate, measured, never
labelled away. `content` has no bound (`af telemetry status` calls it *perturbing*). On the
read-latency corpus `af telemetry status` answers in under 2 s on a fresh index and under 200 ms
when nothing is new, and `show` of one run in under 200 ms, warm.

**The overhead protocol.** `scripts/telemetry-overhead.py` runs the three credential-free bench
fixtures of [`fixtures/telemetry-bench/`](../fixtures/telemetry-bench/README.md) through the Task
runtime with scripted harnesses: `telemetry-bench-concurrent` (four concurrent Attempts),
`telemetry-bench-dense` (one Attempt writing 50 MiB of stream-json-shaped output at pipe speed)
and `telemetry-bench-store` (a Task-backed Review whose 1,000 checks and four reviewers append
about 1,180 Store events; the fixture's README says why not 10,000). Each measurement is a pair,
one command at `off` and one at `spans` on the same machine and fixture, in an order drawn at
random per pair, 20 pairs per fixture. A command's wall time runs from exec to exit, so the
seal, the compression and the sweep's telemetry inventory are inside it. Every command runs in a
fresh copy of its fixture — every directory and file it creates given user write, whatever the
modes of the source tree, which the gate materializes read-only — with private `HOME`, XDG
directories, `TMPDIR` and Store, and must exit
as the fixture expects, leaving no telemetry directory at `off` and a segment at `spans`. The
fixture repositories run no background git maintenance (every `git` the script runs carries, and
the pristine repository's own config that every copy inherits holds, `gc.auto=0`,
`maintenance.auto=false` and `gc.autoDetach=false`), because a detached `git maintenance run
--auto` after a commit holds a lock the copy trips over and can run inside a measured command. Per
fixture the script reports the paired differences `(spans - off) / off`, their median and a 95%
percentile-bootstrap interval of the median (10,000 resamples, seeded). **Drain latency** is
measured on the dense fixture's runs: from the harness's last write (its final stderr line names
the unix nanosecond) to the supervisor's drain completion (the modification time of the Worker's
drained stdout in the Store, which `ModelRunner` writes first thing after the supervisor
returns, at the file system's timestamp resolution), `spans` minus `off` per pair. The gate
passes when every fixture's median is at most 2% and its interval's upper bound at most 3%, and
the drain latency's median difference is at most 10 ms; it is judged only over all three
fixtures at full scale with 20 pairs, and anything less is `not_judged`.

```sh
make telemetry-gate          # a release build, 20 pairs per fixture; fails when the gate fails
python3 scripts/telemetry-overhead.py --af target/release/af --pairs 5 --fixture telemetry-bench-dense
python3 scripts/telemetry-overhead.py --judge target/telemetry-gate/overhead.json
```

`make telemetry-gate` prints the table, fails when the script exits 3 (the gate failed), and
writes the `af/telemetry-overhead@1` document
([`schemas/telemetry-overhead-v1.json`](../schemas/telemetry-overhead-v1.json)) to
`target/telemetry-gate/overhead.json`; `--json` prints the document instead, `--judge`
recomputes every figure and the verdict from a recorded document's own pairs, and `--keep`
keeps every run's directory. `make check` runs only a one-pair smoke of the script
(`--pairs 1 --scale smoke`, the nextest test `telemetry_overhead_smoke`), whose verdict is
`not_judged`.

**The read-latency corpus.** `scripts/telemetry-corpus.py generate --out DIR` writes
`telemetry-corpus-10k` deterministically from a seed (`--seed`) into an absent or empty
directory that is not af's state root: 10,000 runs over 20,000 executions, one sealed segment
each, 2,000,000 spans, 4,000,000 events, 200,000 blobs of which a quarter are shared, 2 GiB of
segment JSONL and 1 GiB of retained blob bytes (about 190 MiB and 310 MiB compressed on disk),
written at `content`. `--runs N` scales it; the same seed and scale give byte-identical files.
`scripts/telemetry-corpus.py measure --dir DIR --af target/release/af` then runs, with private
homes and `content_index` on, `af telemetry status` on a fresh index, `af telemetry status` with
nothing new and `af telemetry show` of one run, each warm (every file read once first) and, where
the script may drop the page cache (Linux or macOS as root), cold; otherwise it reports warm-only
and why. It prints the latency, peak memory and index size of each against its bound, or with
`--json` the `af/telemetry-read-latency@1` document
([`schemas/telemetry-read-latency-v1.json`](../schemas/telemetry-read-latency-v1.json)), and
exits 3 when a warm figure is over its bound.

```sh
python3 scripts/telemetry-corpus.py generate --out /scratch/telemetry-corpus-10k
python3 scripts/telemetry-corpus.py measure --dir /scratch/telemetry-corpus-10k --af target/release/af
```

**First measurement** (21 October 2026, af 0.12.0-rc.4 release build, macOS 27.0.1 on an Apple
M3 Max with 14 cores and 36 GiB; the machine ran other work throughout, at load averages
between 14 and 56). `make telemetry-gate`, 20 pairs per fixture, seed 20261021:

| Fixture | Store events | Median `off` | Median `spans` | Median overhead | 95% interval | Verdict |
| --- | ---: | ---: | ---: | ---: | --- | --- |
| `telemetry-bench-concurrent` | 48 | 2,884 ms | 2,965 ms | +2.34% | [+1.68%, +3.12%] | fail |
| `telemetry-bench-dense` | 27 | 827 ms | 847 ms | +3.05% | [−2.30%, +9.48%] | fail |
| `telemetry-bench-store` | 1,178 | 12,001 ms | 13,585 ms | +7.94% | [−9.21%, +37.71%] | fail |
| drain latency, `telemetry-bench-dense` | | 0.637 ms | 0.685 ms | +0.050 ms | [−0.018 ms, +0.195 ms] | pass |

**`spans` does not pass the 2% gate on this machine in this measurement.** The drain latency is
within its bound. The dense and Store fixtures' intervals are as wide as the machine's load made
them (one Store command took 5.4 s and another 28.5 s), so their medians say little; the
concurrent fixture's interval lies wholly above zero, about 80 ms a command, and its upper bound
is over 3%. A gate run on a quiet machine decides the release; this one records the first figures.

The corpus (seed 20261021): 10,000 runs, 20,000 executions, 2,000,000 spans, 4,000,000 events,
200,000 blobs (50,000 shared), 2,148,452,663 bytes of segments (+0.05% of 2 GiB; 192 MiB on disk)
and 1,071,508,005 bytes of blobs (−0.21% of 1 GiB; 308 MiB on disk), generated in 27 s with eight
processes; measured at a load average of 9 to 12, warm-only (`purge` needs root on macOS):

| Read | Warm | Peak memory | Index after | Bound | Verdict |
| --- | ---: | ---: | ---: | ---: | --- |
| `status`, fresh index | 102,580 ms | 324 MiB | 1.65 GiB | 2,000 ms | over |
| `status`, nothing new | 9,417 ms | 103 MiB | 1.65 GiB | 200 ms | over |
| `show` of one run | 242 ms | 33 MiB | 1.65 GiB | 200 ms | over |

All three reads were over their bounds on that tree. A profile of the unchanged `status` spent
almost all of it in `Index::missing_blobs`, which opened a blob's prefix directory
(`Confined::child_dir`, an `openat`) for every distinct digest of the 250,000 stored
occurrences, one object at a time. It now
groups the retained digests by their `blobs/<aa>/` prefix and opens each prefix directory once,
judging every object in it with `fstatat` (`blob::reader::present_in`): at most 256 directory
opens whatever the number of occurrences, which a unit test counts. The figures above are the
first measurement's, before that change; the fresh figure includes ingesting all 8.6 million
records into an absent index, and the fresh-ingest and `show` figures are reported as measured,
not changed here.

**Invocation cost.** Design §12's second gate decides whether a Claude Attempt can ask for
`stream-json` at every level: the supervisor keeps a process's whole stdout until it ends and
the adapter filters it only then, so the stream's bytes are paid even at `off`.
`scripts/telemetry-invocation-cost.py` measures it with the overhead protocol above — the same
pairs, randomized order, 20 pairs, private homes and the bootstrap interval — on two fixtures of
[`fixtures/telemetry-bench/`](../fixtures/telemetry-bench/README.md),
`telemetry-bench-invocation-one` (one Claude Attempt) and `telemetry-bench-invocation-four`
(four concurrent ones), whose scripted harness answers the same final result under both shapes
with 1, 16 or 64 MiB of intermediate tool output per Attempt and prints only the result under
`json`. Each pair is one command of each shape at one level, `off` and `spans`; per run it
records the command's wall time, its peak RSS (with its reaped children) and, for one Attempt,
the drain latency taken as the overhead gate takes it (at the drained result's CAS write, the
same known follow-up), and it stops when a run's accounting (the Task's charge, its Attempts,
every usage record) differs between the shapes or the levels. The gate passes when at `off`
every case's median wall difference is at most 2% and its median peak-RSS difference at most
the stream's bytes plus 16 MiB; a failed gate keeps two shapes, `json` at `off` and `metrics`
and `stream-json` at `spans` and `content`, which the envelope-parity fixtures
(`claude_stream_result_envelope_is_the_json_envelope`, `claude_accounting_identical_at_every_level`)
pin to the same accounting, classification and Store bytes. `--json` prints the
`af/telemetry-invocation-cost@1` document
([`schemas/telemetry-invocation-cost-v1.json`](../schemas/telemetry-invocation-cost-v1.json)),
`--judge` recomputes a recorded one, and `make check` runs its one-pair smoke
(`telemetry_stream_json_cost_gate`), always `not_judged`.

```sh
cargo build --release --locked --bin af
python3 scripts/telemetry-invocation-cost.py --af target/release/af \
    --report target/telemetry-gate/invocation-cost.json
```

The measurement that decided it (21 October 2026, the release build of the package that added
the gate, on the machine of the first measurement above, load average 50 at the start and
between 20 and 95 throughout; seed 20261021, 20 pairs per case and level; the differences are
`stream-json` minus `json` per pair):

| Case | Level | Median `json` | Median `stream-json` | Median wall | 95% interval | Median peak RSS | Stream bytes | Verdict |
| --- | --- | ---: | ---: | ---: | --- | ---: | ---: | --- |
| one Attempt, 1 MiB | `off` | 1,285 ms | 1,396 ms | +1.76% | [−6.17%, +16.57%] | +0.6 MiB | 1.0 MiB | pass |
| one Attempt, 1 MiB | `spans` | 1,458 ms | 1,511 ms | +3.71% | [−3.44%, +13.10%] | +2.8 MiB | 1.0 MiB | |
| one Attempt, 16 MiB | `off` | 1,545 ms | 1,298 ms | −4.91% | [−20.10%, +6.01%] | +17.6 MiB | 16.1 MiB | pass |
| one Attempt, 16 MiB | `spans` | 1,709 ms | 1,704 ms | +5.88% | [−0.44%, +11.28%] | +35.8 MiB | 16.1 MiB | |
| one Attempt, 64 MiB | `off` | 1,874 ms | 2,111 ms | +20.34% | [−0.43%, +32.38%] | +66.0 MiB | 64.4 MiB | fail |
| one Attempt, 64 MiB | `spans` | 1,757 ms | 2,649 ms | +31.27% | [+12.32%, +68.97%] | +127.3 MiB | 64.4 MiB | |
| four Attempts, 1 MiB | `off` | 3,158 ms | 3,464 ms | +5.88% | [−3.16%, +13.05%] | +0.7 MiB | 4.0 MiB | fail |
| four Attempts, 1 MiB | `spans` | 3,127 ms | 3,291 ms | +5.91% | [−15.62%, +22.18%] | +5.2 MiB | 4.0 MiB | |
| four Attempts, 16 MiB | `off` | 2,437 ms | 2,556 ms | +0.04% | [−5.44%, +3.33%] | +68.1 MiB | 64.4 MiB | pass |
| four Attempts, 16 MiB | `spans` | 2,677 ms | 2,645 ms | +1.02% | [−0.88%, +4.72%] | +131.0 MiB | 64.4 MiB | |
| four Attempts, 64 MiB | `off` | 2,977 ms | 2,921 ms | −3.50% | [−14.08%, +8.59%] | +261.2 MiB | 257.7 MiB | pass |
| four Attempts, 64 MiB | `spans` | 3,147 ms | 3,869 ms | +18.20% | [+11.16%, +28.46%] | +508.0 MiB | 257.7 MiB | |

The one-Attempt drain latency differences at `off` were +0.37 ms, +6.40 ms and +25.18 ms for
1, 16 and 64 MiB, and at `spans` +7.54 ms, +122.11 ms and +479.37 ms.

**The gate failed** — the one-Attempt 64 MiB case's median wall is +20.3% at `off`, and the
four-Attempt 1 MiB case's +5.9%; every peak-RSS difference is within the stream's bytes plus
16 MiB — **so this release keeps two invocation shapes**: `json` at `off` and `metrics`,
`stream-json --verbose` at `spans` and `content`. The figures are from a loaded machine and
their intervals are wide (the 64 MiB interval reaches down to −0.4%), but the decision needs
every case within its bound, and a 64 MiB stream that costs a fifth of a command's wall time at
`off` is not within it. At `spans` the stream also pays Telemetry's own work on the complete
streams — the tap, and the release decision's scan of both streams for login-challenge
material — which the overhead gate above bounds for `spans` as a whole. A first, interrupted run
of this measurement, before the release decision's scan was confined to observed processes,
read the complete streams at `off` too and measured +17.6% for 16 MiB and +55% for 64 MiB at
`off` (one Attempt, 20 pairs each); it is superseded by the table above.
