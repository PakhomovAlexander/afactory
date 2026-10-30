# Telemetry in the performance laboratory

The laboratory uses [Telemetry](telemetry.md) in two ways, both apart from the timing verdict
([ADR-0214](adr/0214-integrate-telemetry-into-the-performance-lab-as-diagnostics-and-advisory-experiments.md)):

- **Diagnostics.** A diagnostic child run of a comparison can also record its CLI cases at an
  explicitly selected telemetry level and read the recording back through the side's own
  `af telemetry` documents. The child shows phase timings, a timeline and a Perfetto file beside
  its CPU profiles or phase traces.
- **Experiments.** Telemetry's own cost and read latency are measured on a baseline and a
  candidate binary with the bench scripts of [design §12](design/telemetry.md), from the trusted
  control's checkout, on pull requests, nightly, on publication and on request. Every execution
  of a candidate is contained, and only verified results reach the durable history
  ([ADR-0215](adr/0215-contain-telemetry-experiments-and-collect-only-results-verified-under-their-authority.md)).

Neither ever contributes a verdict sample, changes a verdict, or satisfies a required check.
The old hosted calibration cohort's bytes remain pinned by SHA-256. The consolidated
integration also incorporates published main's workflow and the calibration dispatch repair;
that changed producer requires a separate future cohort, declared only from the committed
repair ([the dispatcher runbook](performance-cohort-dispatch.md)). The comparison's CLI Task case
(`cli_task_run_check`) runs `af` at its default level, `spans`, as users do, so the product's
default telemetry cost stays inside that case's measured time.

## ADR numbering after source reconciliation

Published-main ADR0001–0213 keep their files, content and references, including ADR-0213's
CLI usage test isolation. The eight unpublished laboratory decisions move together to
ADR0214–0221. Their filenames, titles and references in documentation, source comments,
help, configuration comments and generated schema descriptions all use the final numbers.
The protected inventory and staged policy are regenerated with the existing generator;
substantive decisions and behavior are unchanged.

| Previous laboratory number | Reconciled decision |
| --- | --- |
| 0213 | [0214: Telemetry diagnostics and advisory experiments](adr/0214-integrate-telemetry-into-the-performance-lab-as-diagnostics-and-advisory-experiments.md) |
| 0214 | [0215: Containment, verification and collection](adr/0215-contain-telemetry-experiments-and-collect-only-results-verified-under-their-authority.md) |
| 0215 | [0216: Reference-generation throughput](adr/0216-construct-reference-fixture-bytes-without-per-byte-python-loops.md) |
| 0216 | [0217: Calibration dispatch and group arguments](adr/0217-bind-calibration-dispatch-to-the-checked-main-source-and-separate-group-arguments.md) |
| 0217 | [0218: Accounting append and reservation settlement](adr/0218-reapply-an-accounting-append-once-after-only-this-writers-heartbeat-renewals.md) |
| 0218 | [0219: Native gate child ownership](adr/0219-own-native-gate-children-without-leaving-the-enclosing-process-group.md) |
| 0219 | [0220: Scoped Store fixture clock](adr/0220-control-store-fixture-time-with-a-scoped-thread-clock.md) |
| 0220 | [0221: Performance Lab and GA integration](adr/0221-integrate-the-performance-lab-with-ga-and-control-short-attempt-fixtures.md) |

## Supported matrix

| Case | Telemetry capture | Phase spans | CPU profile |
| --- | --- | --- | --- |
| Native library cases (`canonical_*`, `cas_*`, `source_capture`, `state_projection`, `authority_*`, `persistence_fs`) | none: the probe installs no telemetry subscriber (`no_telemetry_subscriber`) | `perf-trace` recipe, probe region | perf (Linux) or samply (macOS), probe region |
| `cli_discovery` (`--version`, `--help`) | none: discovery records no telemetry by design (`records_no_telemetry`) | `perf-trace`, startup region | whole process |
| `cli_task_start` (start without execution) | none (`records_no_telemetry`) | `perf-trace`, command region | whole process |
| `cli_state_read` (`task show`, `review ledger`, `review report`) | none (`records_no_telemetry`) | `perf-trace`, command region | whole process |
| `cli_task_run_check` (`task run --execute`) | yes: one recorded execution per side | `perf-trace`, command region | whole process |

The cases that record no telemetry keep their `perf-trace` coverage. Telemetry does not replace
the trace recipe, and a selected case outside the matrix is an omission with its reason, never
a silent gap. `cli_task_run_check` spawns processes, so like every such case it is diagnosed
only in its contained Linux environment (`--mode linux_container`); on a trusted local host it
is an omission naming that capability.

## Telemetry in a diagnostic child run

```sh
python3 scripts/performance/cli.py profile --plan run.json --case cli_task_run_check__task_cli_task_roots \
  --recipe linux_gnu_trace --mode linux_container --image IMAGE@sha256:... --scratch-fs /perf/scratch \
  --telemetry spans --output child/
```

`--telemetry spans` or `--telemetry content` is accepted with a CPU-profile or trace recipe,
never with counters. For each eligible selected case and each side, the side's diagnostic AF
binary runs the case's command once more with `AF_TELEMETRY__LEVEL` set. It runs in the case's
private HOME, XDG directories, TMPDIR and Store, after its declared setup, and its oracle
judges the output. Then, in the same private namespace and before it is removed, the same binary
runs `af telemetry runs --json`, `show RUN --store ID --json`, `status --json` and
`config RUN --store ID --json`. Each document is a new evidence file, bounded to 4 MiB while it
is written and admitted as a confined regular file. It is read only through the product's
committed schema (`schemas/telemetry-*.json`); the laboratory opens no segment, blob or index.
The child keeps, under `telemetry/<instance>/<side>/`:

| File | What it is |
| --- | --- |
| `runs.json`, `show.json`, `status.json`, `config.json` | the side's own documents, byte for byte |
| `perfetto.json` | Chrome trace-event JSON of `show.json` for Perfetto or `chrome://tracing`: a process per execution, nested tracks, async tracks for what does not nest, instants for open spans; identities, names, statuses and times only, no attribute |

`profiles.json` (`af.perf-profiles/7`) records the selected level, whether it is perturbing
(`content` is), and each capture. A capture names its case, side, source SHA and binary SHA-256,
the run, trace and Store ids, every execution with its segment state and outcome, and the AF
version and target from the configuration record. It holds the phase rows: per span name, its
spans and open spans, the covered time (the union of its spans' intervals, never a sum across
overlapping spans), the self time the product computed summed over the name, and the longest
span. It holds a timeline of at most 512 spans and an evidence state. The state is `complete`
only when every execution was sealed by its writer with nothing dropped, nothing is open or
unreadable, no problem was recorded, the history is a prefix and the configuration record is
complete. Every shown execution must have exactly one observed segment with matching state
and capture metadata. Missing observations or unknown metadata are partial (`history_partial`,
with an explicit unknown capture row); missing or lossy terminal metadata remains
`dropped_or_recovered_capture`. Duplicate observations, contradictory known states and counts
are refused. Extra counts lacking observations make the history partial. Otherwise it is
`partial` with every reason:

`config_incomplete`, `corrupt_segment`, `dropped_or_recovered_capture`, `history_partial`,
`index_absent`, `live_segment`, `open_execution`, `open_spans`, `problems_recorded`,
`unreadable_files`, `unsealed_execution`, `unsealed_segment`.

In both the selected `runs.json` run and `show.json`, `executions.retained`, sealed ordinal
summaries and open execution objects must match their own `executions.list`. Open means a
live/unsealed segment or an execution whose run span never ended, including recovered sealed
segments. Unknown ordinals remain null in the list and absent in open summary objects.
Internal contradictions are refused. A changed inventory or evolving observation between the
two reader instants is `history_partial`; conflicting immutable identities are refused.
Each inventory must map execution IDs and immutable run-span IDs one to one. The native index
uses the latter as a primary key; sharing a span across different executions between readers
is contradictory reassignment. Distinct additions/removals remain partial, without imposing
uniqueness on unrelated optional fields.
Report admission repeats these checks even when every artifact hash has been updated.

A reader that fails, a document over its bound, of another version or refused by its schema, or
one that names another run, Task or execution is an omission (`telemetry_read_failed`,
`telemetry_unreadable`, ...). What it had written stays counted in the evidence and is never
shown. The child's `diagnostic.html` gains a Telemetry section: the captures, each execution,
the phase table and an SVG timeline, every name escaped. `report --diagnostic child/` derives
every capture again from the archived documents, refusing a child whose figures, state or
Perfetto file differ. It adds a Telemetry column per case (`af.perf-report/3`), and copies the
child below the report's `diagnostics/`, where the durable archive keeps it with the report's
views.

## Telemetry experiments

`fixtures/performance/telemetry.toml` (`af.perf-telemetry-lab/2`) declares the experiments and
one bounded profile per trigger:

| Experiment | Script | Levels | Each side judged by |
| --- | --- | --- | --- |
| `overhead` | `scripts/telemetry-overhead.py` | `off` and `spans`, finalization inside every wall time | its own 2%/3% gate over all three fixtures at full scale with 20 pairs, else `not_judged` |
| `invocation_cost` | `scripts/telemetry-invocation-cost.py` | `json` against `stream-json` at `off` and at `spans` | its own `off` gate over every case at full scale with 20 pairs, else `not_judged` |
| `read_latency` | `scripts/telemetry-corpus.py` | the reader at `content` with the content index | the warm bounds, on `telemetry-corpus-10k` only, else `not_judged` |

| Profile | Rounds | Overhead | Invocation cost | Corpus | Timeout | Work bytes | Retention |
| --- | ---: | --- | --- | ---: | ---: | ---: | ---: |
| `pr` | 2 | 3 pairs, full | 2 pairs, smoke | 200 runs | 120 min | 16 GiB | 14 days |
| `nightly` | 2 | 20 pairs, full | 5 pairs, full | 1,000 runs | 340 min | 20 GiB | 30 days |
| `release` | 2 | 20 pairs, full | 20 pairs, full | 10,000 runs | 355 min | 24 GiB | 90 days |
| `manual` | 2 | 3 pairs, full | 2 pairs, smoke | 200 runs | 240 min | 20 GiB | 30 days |

Work bytes are gross bounded filesystem capacity, including ext4 overhead. Each profile adds
12 GiB to its former 4/8/12 GiB workload allowance: AF still requires 10 GiB **free** before
provider admission and checks, leaving 2 GiB for filesystem overhead and headroom. Filling
that headroom still causes admission to refuse; the filesystem and aggregate allocation limits
remain enforced. This costs up to 12 GiB more backing storage per measuring job. Preparation
also requires the unchanged 10 GiB host floor above the image: 26/30/34 GiB free for the
16/20/24 GiB profiles. The actual available capacity is smaller than the gross figure and is
checked by the real runtime regression; no writable path moves off the bounded filesystem.

Corpus preparation explicitly uses `--jobs 1`, a serial path through the same deterministic
writers, without constructing a multiprocessing Pool or semaphore. This is untimed preparation;
the timed reader measurements and their bounds are unchanged. Other generator callers retain
parallel workers when they request more than one job.

`validate.yml` runs `telemetry_runtime_smoke.py` when perf is selected, with actual release AF
built in the pinned GNU image. Full selections require it in both stable validation aggregates;
fast selections without perf explicitly skip it as NOT RUN. Failed, cancelled, missing or skipped
selected smoke jobs fail aggregation. It reproduces the former 4 GiB concurrent fixture failure from public Task state, then
uses the registry's 16 GiB PR capacity for one smoke pair of every overhead/invocation case and
the three reads of a 10-run corpus, through the same contained executor with `--ipc none`.
The test has an eight-minute total deadline, confirms execution and filesystem cleanup, and
never judges a full performance gate. Existing hostile-write, aggregate/unlinked-allocation,
output and diagnostic-reader probes remain separate required validation work. A native
`--native` diagnostic is explicitly uncontained and cannot substitute for this Docker check.

`content` is never measured as an experiment: it is diagnostic only and perturbing.

```sh
# On a Linux host: contained, the only mode for source the operator does not trust.
python3 scripts/performance/cli.py ci-scratch --image /srv/scratch.img --mount /srv/scratch-fs \
  --bytes 21474836480 --telemetry-owner /srv/telemetry-owner
python3 scripts/performance/cli.py telemetry-run --profile manual --trigger manual \
  --mode linux_container --image REGISTRY/IMAGE@sha256:... --scratch-fs /srv/scratch-fs \
  --scratch-owner /srv/telemetry-owner \
  --baseline-af base/af --candidate-af head/af \
  [--baseline-build base/af.build.json --candidate-build head/af.build.json] \
  --baseline-sha BASE_SHA --candidate-sha HEAD_SHA --authority-sha "$(git rev-parse HEAD)" \
  [--experiment overhead]... [--rounds N] [--pairs N] [--scale full|smoke] [--corpus-runs N] \
  --output result/
python3 scripts/performance/cli.py ci-scratch --cleanup --image /srv/scratch.img \
  --mount /srv/scratch-fs --telemetry-owner /srv/telemetry-owner
# On a trusted host, for source the operator trusts (never a pull request):
python3 scripts/performance/cli.py telemetry-run --mode trusted_local ... --output result/
python3 scripts/performance/cli.py telemetry-verify --result result/ [--summary] \
  [--authority-root CHECKOUT_OF_THE_AUTHORITY]
python3 scripts/performance/cli.py telemetry-history --root /srv/af-perf/telemetry \
  --import result/ [--authority-root CHECKOUT] [--bytes-max N] [--results-max N] \
  [--retention-days N]
```

Both binaries run on this host with this checkout's registry, scripts, fixtures, schemas and
laboratory code. That is one measurement authority, identified by the SHA-256 of every file it
reads (`authority_digest` domain v2). The closure includes every `perflab` module and vendor
module, all schemas and performance fixtures, the CLI, telemetry workflow, declared bench
inputs and build/toolchain manifests. Transitive admission, arithmetic, bundle, rendering, build
and containment helpers therefore change its identity too. Rounds alternate which side runs first, and both sides of a round use the
same seed. The machine's performance lock is held for the whole run, and one deadline, the
profile's timeout, bounds everything: the host checks, both version queries, the corpus, every
run and the teardown, with 90 seconds kept for the last teardown and the result.

### Where each execution runs

Every execution goes through one executor (`perflab/telemetryexec.py`, ADR-0215): each side's
`--version` query, the corpus generator and every script run.

- **`--mode linux_container`** runs each execution in its own container of the pinned sample
  image: network none, read-only root, every capability dropped, no new privileges,
  `docker-init` as PID 1, the registry's PIDs (`[containment] pid_limit`), memory with swap
  disabled and CPU set, a file size limit, as this user.
  Daemon logging is disabled (`--log-driver none`), IPC is disabled (`--ipc none`, no
  writable `/dev/shm`), and the build image's existing bounded volume inspection refuses
  declared image volumes before even a version probe runs. Controller captures keep their
  existing byte and allocation limits on the same bounded filesystem.
  - **Binds.** Read-only, at their own paths: only that experiment's declared inputs and that
    side's binary. Writable: only the execution's own new directory under the verified
    disk-bounded scratch filesystem's `telemetry/` entry.
  - **Never bound.** The controller's checkout, the result directory, the capture files, the
    other side's binary and the master corpus.
  - **Teardown.** Every process of the execution lives in the container's PID namespace and
    ends with it. The container is removed once however the execution ended, and the scratch
    filesystem is checked against its inventory.

  **Final filesystem cleanup.** `ci-scratch --telemetry-owner` creates a private control
  directory outside scratch, with a bounded `af.perf-telemetry-owner/2` record binding its
  directory/lock identities and owner UID to the backing image's path, device, inode and
  size and the mounted filesystem's path, device and root inode. The CLI holds its
  nonblocking lock through preflight, every execution and result publication. Before each
  Docker launch the executor durably publishes the exact pending container name; only
  confirmed removal and scratch inventory checks settle it. Uncertainty stops later launches.
  The workflow's unconditional final cleanup uses this ownership record, independently of
  any result JSON. Before executor setup, `filesystem_pending` is durably set for the entire
  work lifetime. Confirmed removal of the final work root clears it. A preparation refusal
  may also settle it when the built-in path proves no work began: image inspection and
  read-only runtime checks precede the host enforcement probe. Entering that probe, any
  injected preparation hook or executor, or controller work withdraws this evidence.
  Settlement uses the original absolute deadline before releasing the machine lock and never
  retries its write. A `stopped` container never clears this fence. Every ownership write
  replaces `state.json` with a synced new file, then syncs the control directory. A read,
  write, file sync or replacement that fails or expires leaves the earlier record and its
  fence in place, so cleanup stays ineligible. A directory sync that fails after the
  replacement leaves the new record in place. The run still reports the failure, and its
  controller launches nothing more. But the record is one its transition already proved: a
  settlement is published only once no work began or the final work root's removal is
  confirmed. So the next controller and the workflow's cleanup are admitted, and nothing a
  possibly live process could reach is deleted.
  Only the record's durability is unproven. If the host stops before the directory reaches
  disk, the earlier, fenced record can return, and cleanup then refuses until an operator
  recovers it. Other failure handlers perform no compensating ownership I/O. Owner admission,
  transitions and terminal settlement share the original absolute deadline and reserve.
  Only `ready` or `stopped` ownership with no pending filesystem work permits unmount
  and image removal. Pending work or launches, a live controller, missing/corrupt/foreign evidence
  or changed filesystem identity refuse all destructive commands and print the retained
  image, mount and control paths. A cancellation before result publication cannot turn into
  cleanup permission. Do not delete these recovery paths to make a failed job look clean;
  an operator must establish that the recorded container and controller have stopped first.
  Earlier owner versions are refused; they are never repaired or migrated automatically.

  A pull request's candidate is measured only this way: `telemetry-run --trigger pr` refuses
  any other mode, and the result contract refuses a pull request's result recorded otherwise.
  The host is checked like a shard's (`runner.contained_host`: cgroup v2 container runtime,
  the image present and pinned, the scratch bound verified by an enforcement fill, the CPUs).
- **`--mode trusted_local`** runs each execution on this host in its own session, for source
  the operator trusts. The result says so. Nothing confines what it writes or reaches, and a
  process that leaves its session cannot be observed. A process of its group that outlives the
  script fails the execution and is killed.

  The script leader stays unreaped through group cleanup, so its group ID cannot be reused
  while signals are sent. Linux uses a complete, bounded `/proc` process-state scan: cleanup
  can be confirmed when only terminal processes remain, including zombies retained by an
  ancestor subreaper. Zombies cannot execute or retain open file descriptors. A terminal
  thread-group leader also needs the same bounded status read reporting exactly one thread;
  another thread may still execute even when that leader is a zombie. A failed or
  incomplete observation, a missing reserved leader or a live survivor makes cleanup uncertain;
  a later successful observation does not erase the error. macOS uses kqueue exit observation
  and its group signal probe, following the shared `review-process` lifecycle semantics; it
  does not claim Linux process enumeration. Other host platforms are refused before launch.

The perf-supervisor is not PID 1 here. Its request bounds a child at the verdict sample's 120
seconds, and a bench script runs for minutes. Raising that protected bound would change the
verdict path, so the container's PID namespace is the boundary that ends every descendant.

What an execution writes, its corpus copy and its stdout and stderr included, is measured while
it writes. Past the profile's `work_bytes` it is stopped. Each file is bounded by
`RLIMIT_FSIZE`, the stdout document by 16 MiB, and the stderr by 16 MiB of which the last
256 KiB is kept.

After each execution the controller checks again that both binaries, the measurement
authority, the master corpus and the result directory are as it left them. Anything changed is
recorded in `execution.problems`, the run fails and nothing more runs. An execution whose
cleanup was not confirmed (an unconfirmed container removal, a scratch filesystem that is not
as it was, an executing group member or unknown process evidence) stops the run too. Its work
directory is kept for the operator (`execution.retained`), and the next run refuses to start while it exists. A result
with a problem or an uncertain cleanup is never `complete`, and `telemetry-run` exits 4.

### What a document must be

The corpus is generated once, by the authority's own generator under the same executor. The
controller then reads its files' facts (segments, their uncompressed and stored bytes, blob files
and their stored bytes) and requires one segment per planned execution. Each read gets its own
copy of the corpus, counted in its room. An index one side builds is never another read's, and
the master corpus is never bound.

A script's stdout is admitted only as the run it was planned as (`telemetrylab.admit`):

- **Every document.** It names exactly this side's binary version, from the same contained
  `--version` query.
- **`overhead`.** The planned pairs, scale and seed; exactly the script's three fixtures,
  each with the planned number of pairs; and the dense fixture's drain latency.
- **`invocation_cost`.** The planned pairs, scale and seed; exactly the cases of that scale,
  each level with the planned pairs.
- **`read_latency`.** The generated corpus: its runs, seed, the name those runs are given and
  every file fact. The trusted generator's layout derives all index counts for that seed and
  scale: executions, spans, events, blobs, shared blobs, blob occurrences and blob bytes.
  `af/telemetry-read-latency@2` also records `content_indexed`; content indexing must be enabled
  and its indexed count must equal all generated text blobs. Correct runs and file sizes cannot
  excuse dropped contents, either during execution or offline verification. Exactly `fresh_status`, `unchanged_status` and `show`, each with the
  script's own bound, one fresh sample and `repeat` unchanged and show samples, each latency
  the median of its samples, `within_bound` and `passed` what the latencies and bounds give,
  and no cold figure where none was measured.

The script's own rule then judges it, recomputed from its raw pairs or samples. For the paired
scripts, the script's own `summarize` must give back the whole document: every difference,
median, interval, verdict and reason. Read latency is judged only on the full 10,000-run
corpus, else `not_judged`. Two documents that agree with each other but not with their plan are
both refused.

Every planned run is recorded once: `ok`, `failed`, `timeout`, `refused_document` or `not_run`
(the deadline, the work budget, an earlier problem or a failed version query came first). The
result is `complete` only when every run succeeded on a host observed the same at both ends,
every cleanup was confirmed and nothing protected changed.

`result/` holds `result.json` (`af.perf-telemetry-result/2`), every script's document under
`raw/<experiment>/round-<n>-<side>.json`, capped stderr under `logs/`, the offline
`report.html`, the job summary `summary.md` and `manifest.json`. The result records:

- the sources, both binaries' SHA-256, versions and build records (`af.perf-telemetry-build/1`:
  the pinned image, `rustc -V`, `cargo -V` and the command), and the authority digest;
- the execution: its mode and containment, the container's image identity, runtime, CPUs and
  limits, the work and file bounds, the cleanup state, any kept work and any protected state
  that changed, and the mode's limitations in words;
- the effective configuration and its digest, the corpus and its facts, and the host identity;
- every run with its argv, its placeholders kept;
- each side's own figures by level (`within`): the overhead of `spans` over `off`, the drain
  latency, `json` against `stream-json` at each level, and the read figures;
- the cross-revision comparisons by experiment, subject, level, metric and shape. Per round,
  each side's median and the candidate's relative change, and the median change over rounds.
  A round's figures are set side by side only when both documents' protocol (pairs, scale,
  seed, bounds), corpus and machine agree, on one host; anything else is `incompatible` or
  `missing` with its reason.

Every physical duration, byte count and individual latency sample is validated before
summarization and admission. Raw wall time must be positive; raw drain/read latency and byte
counts must be nonnegative and representable. Numeric fields must be finite. Differences,
percentages and their intervals retain their signed meaning. A refused measurement records
`refused_document`, no admitted document or judgment, and `measurement_refusal`: the declared
field path, original numeric token (at most 512 bytes), complete token size/digest, original
document size/digest and a typed reason. No free-form candidate document text is published.
The readable diagnostic names the experiment, round and side. Other runs continue under the
same schedule: mixed results are `partial`, all-refused results are `failed`. Offline
verification replays the numeric refusal under the exact authority and regenerates the report;
upload and collection use that same verification. Version 1 results fail closed without fallback.

### Verification and history

`telemetry-verify` reads a bundle back only under exactly the measurement authority it names.
The checkout at `--authority-root` (default: this one) must have the same authority digest,
file count and registry hash, or the result is refused; a result of another authority is never
kept "as recorded". It then rehashes the bundle and admits every raw document again as its
planned run. It judges every document again with that authority's own scripts, never with
anything of the result. The installed trusted verifier must also match that complete authority;
a retained checkout alone never selects code to execute. Install the matching reviewed verifier
or verification is refused. CLI startup refuses library bytecode caches before importing
`perflab`, and authority admission refuses cached executable artifacts; bench loaders compile
the source bytes directly. Use a clean trusted source installation: `-B` alone prevents writes
but does not prevent cached bytecode reads. And it recomputes the figures, the comparisons, the page and the
summary, refusing anything that differs.

`telemetry-history` holds its root under one lock (`ROOT/lock`) for every import, prune and
write. It keeps each bundle verified so, whole, under `ROOT/results/<result id>/`, staged and
renamed. Importing the same bundle again changes nothing, and another bundle under a kept ID is
refused. Its trusted authority is copied separately to `ROOT/authorities/<digest>/`; history
records `authority_path` and re-verifies under that tree and the matching installed verifier on
every write. Missing, changed or unavailable authority is refused. Authority bytes count toward
the root's bound, and unreferenced authority trees are pruned under the same lock. It is bounded:

- an import that would take the root past `--bytes-max` (32 GiB by default) or
  `--results-max` (2,048) is refused before a result is accepted; unaccepted staging or an
  unreferenced authority from an interrupted import is swept on a later locked command;
- `--retention-days` prunes whole results older than that, oldest first, then any results
  beyond the result bound.

It then writes `ROOT/history.json` (`af.perf-telemetry-history/2`: the bounds, the bytes used,
every result, the groups, what this write pruned and every refused input) and
`ROOT/history.html`. Both reports are fully staged before either is replaced. The owned
`ROOT/.report-staging/` has a fixed marker and at most two report files; entry sweeps partial
writes under the lock and existing collection deadline. Links or foreign entries refuse
cleanup. Capacity includes retained reports, report staging, bindings and other root files,
including foreign leftovers that are counted and preserved. Publication reserves space for
both old reports and both staged replacements; insufficient capacity preserves the complete
reports. Accounting counts allocated file bytes and directory blocks, using file length where
block accounting is unavailable. A series holds only results of one configuration under one measurement
authority, in one execution mode and image, on one host. A different group is never a trend.

## CI: the Performance telemetry workflow

[`.github/workflows/performance-telemetry.yml`](../.github/workflows/performance-telemetry.yml)
resolves each event once with `cli.py telemetry-schedule`, from the trusted control's own
checkout:

| Event | Profile | Control and baseline | Candidate |
| --- | --- | --- | --- |
| pull request to main | `pr` | the base | the merge commit |
| nightly (04:41 UTC) | `nightly` | the tip; baseline the highest stable release reachable from it | the tip |
| release published | `release` | the tag; baseline the highest smaller stable release | the tag |
| dispatch | its `profile` input | `control` input, else `base` | `head` |

A dispatch names full SHAs or fully qualified refs and may narrow `experiments`, `rounds` and
`pairs` within the caps. A Remote Check gate branch is never measured. Nothing is planned
without the repository variable `AF_PERF_GNU_BUILD_IMAGE`, the sample image pinned by digest
that `performance.yml` already uses: no experiment ever runs uncontained in CI.

- **Build.** Each side is built in its own job, in that image, credential-free with its pinned
  toolchain. `telemetry-contained-build` uses the existing lab image/toolchain verifier to
  locate the image's exact compiler before mounting source. Compilation runs as the runner
  UID/GID with `/src` read-only and an owned `/state` for HOME, Cargo, Rustup, temporary files
  and target outputs. Cargo and rustc run directly from the verified toolchain; candidate
  settings cannot request a Rustup installation. Capability dropping, no-new-privileges,
  read-only image and credential separation remain. Only its binary and build record reach
  the measuring job, by SHA-256. Each side outputs its exact artifact name, so a failed-jobs
  rerun can reuse the successful build without guessing the build attempt. No
  candidate build script runs where the control measures. Dependency fetch is a separate
  `cargo fetch --locked`; executable Cargo configuration is refused before this networked
  step. Compilation is `--locked --offline`, network none. The helper prepares and verifies
  an owned 8 GiB block filesystem for all writable state and controller logs; a supplied
  fresh dedicated filesystem may be smaller. Its required `--source-sha` binds real Git
  provenance, and `--scratch-fs` names the new mount. One `--seconds` deadline (31–4,800,
  default 4,800) covers preparation through cleanup, with 30 seconds reserved for completion.
  The 10 GiB host free-disk floor remains. Only confined, bounded regular binary bytes and
  controller-owned tool identities reach `--output`; the workflow never copies a candidate
  target path. A failed build publishes nothing and retains bounded scratch for inspection.
  Successful teardown of a filesystem the helper prepared precedes publication; existing
  operator-provisioned filesystems are restored to their inventory and left mounted.
- **Measure.** The measuring job refuses any credential in its checkout or environment. It
  prepares a verified disk-bounded scratch filesystem of the profile's work bytes
  (`ci-scratch --bytes`), pulls the image and runs `telemetry-run --mode linux_container` from
  the control's checkout.
- **Admit.** `telemetry-admit` writes `admitted=true` only for a result that verifies under the
  control's authority and holds no credential-shaped value. A missing result, a result that
  does not verify, or one with a credential writes `admitted=false` with its `outcome` and
  fails the job.
- **Upload.** `perf-telemetry-RUN_ID-ATTEMPT` is uploaded with the profile's retention only
  when `steps.admit.outputs.admitted == 'true'`. A valid `partial` or `failed` result is
  uploaded and says what it is. When the measuring step itself failed, the job still fails.

Permissions are read-only, there are no secrets, every action is pinned, and pull requests
cancel superseded runs. The workflow is advisory and is not a required check. Within one
execution the script and the binary it measures share one user. A candidate can therefore still
falsify its own run's document, never the other side's, the controller's or another run's.

The executable build smoke is `python3 -I -B
scripts/performance/tests/telemetry_build_smoke.py`, after pulling the pinned GNU image. The
`telemetry-build-smoke` validation job runs it on a non-root hosted runner with a 0755 source
checkout, no target directory and a minimal Rust package. It uses the real workflow build
command with a real prefetched registry dependency. It checks real source identity, writable
private directories, denied source/controller writes, an operator TCP sentinel (with a
reachable positive control), many small files and unlinked writes against 128 MiB filesystems,
and real Docker metadata/binary/ancestor symlinks, FIFO and oversized outputs. External
sentinels must stay unchanged and refused builds must publish no artifact. A live
CI smoke pass and a subsequent hosted measurement must be observed before rollout; source
checks or a local environment without Docker establish neither.

## Collection: the telemetry history lane

`cli.py telemetry-collect --config FILE` is one bounded, idempotent collection cycle of the
workflow's result artifacts into a durable telemetry history root (`perflab/telemetrycollect.py`,
ADR-0215). It is a lane of its own. The archive collector (`scripts/performance/collect.py`) behaves as
before, never reads these artifacts, and stays pinned to the frozen cohort's control (below);
the only code the lanes share is the bounded ZIP reader, `transport.read_zip`.

```sh
python3 -I -B scripts/performance/cli.py telemetry-collect --config /etc/af-perf-telemetry/collector.toml
```

The configuration (`af.perf-telemetry-collector/1`) is the operator's; an example is
[`scripts/performance/service/telemetry-collector.example.toml`](../scripts/performance/service/telemetry-collector.example.toml).
It names the repository, the credential's environment variable, the durable root, a trusted
mirror of the repository and the refs whose history holds trusted measurement authorities. It
also sets the bounds: lookback, the cycle's time, runs per cycle, artifact bytes, the root's
bytes and results, and retention. A cycle does this:

1. **Lock and prune.** It takes the root's lock and prunes it to its bounds.
2. **List.** It identifies the workflow by its path and lists completed runs within the
   lookback, within the existing ten pages of 100 runs. Every listed run must be of that
   workflow and repository. A count over 1,000, a changed count, duplicate ID or short/excess
   page refuses the listing before processing any run; a truncated newest prefix never
   becomes a complete listing. Actions pagination is not a snapshot: even a consistent
   listing describes only the observed runs, and later cycles list again.
3. **Select.** It orders the complete listing by run ID, starts after the last attempted ID
   in `collection-position.json` (`af.perf-telemetry-collection-position/1`), finishes that
   rotation through its stored upper run ID, then wraps to the oldest and fixes the next
   rotation's upper ID at the largest currently listed. New arrivals cannot extend a rotation
   and starve older retries. It attempts at most `runs_max` distinct runs. Of each run, it takes only
   `perf-telemetry-<run>-<attempt>` artifacts of that run and attempt. Another run's name is
   refused, and a kept result is never downloaded again. Already-kept runs still have their
   artifact lists checked so a higher attempt can be discovered.
4. **Download.** It downloads within `artifact_bytes_max`. The bytes must have the SHA-256 the
   API declares, and the ZIP is read with the archive collector's bounds (`transport.read_zip`):
   members, ratio, decompressed bytes, no link, special, encrypted, absolute or traversing
   member.
5. **Bind.** It requires `telemetry-plan-<run>-<attempt>`, a separate immutable artifact
   containing only `binding.json` (`af.perf-telemetry-binding/1`), with a matching API digest.
   Planning retains `telemetry-schedule-<run>-<attempt>` and passes its checked receipt as
   a job output. Before measurement, `telemetry-bind-attempt` binds that frozen plan to this
   attempt's original GitHub event and retains the plan artifact. A failed-jobs rerun can reuse
   original plan outputs without resolving moving refs again. Missing, expired, foreign or
   mismatched receipts are explicit refusals; there is no earlier-format fallback.
   The result ID, trigger, profile and all three source SHAs must match that receipt. The
   event's workflow and control's workflow, read as Git objects, must exactly match the
   installed trusted producer. A modified PR workflow cannot mint admitted planning evidence.
   For a PR, `candidate_sha` is the original event merge, whose parents must be the original
   baseline followed by the immutable run `head_sha`; control equals baseline. Linked
   `pull_requests` metadata is mutable and is never a source binding. For nightly/release,
   candidate also equals the run commit; manual candidates remain the frozen explicit plan.
   The checked receipt is retained at `bindings/tl_<run>_<attempt>.json` under the history
   lock and capacity, rechecked when history is read, and pruned with its result.
6. **Authority.** It requires the authority commit to be reachable from a trusted ref. It
   writes that commit's authority files from the mirror's Git objects
   (`telemetrylab.materialize_authority`).
7. **Keep.** It imports the result verified under exactly that authority, then writes
   `history.json`, `history.html` and `collection.json` (`af.perf-telemetry-collection/2`):
   every run attempted, with what was collected, already collected, missing, expired or refused
   and why, what was pruned, and the root's results and bytes.

The scheduling position is an attempted-run position, never a success watermark. Missing,
expired and refused results stay eligible on each rotation while within lookback. Every
attempted run advances it, even after a refusal, so one failed run cannot block the rest. Under
the same history lock it is contract-validated, staged, synced and atomically replaced after
each run. Restart resumes after the last stored position; an interruption before replacement
can repeat that run, and the retained bundles still prevent duplicate imports. A deadline
before a run starts leaves the position unchanged. A failed listing leaves it unchanged too.
The one fixed temporary file is replaced on the next position write; neither file can grow
beyond 1,024 bytes. No run list, credential or artifact evidence is stored in this position.

A listing larger than `runs_max` reports `failed` with the processing cap and pending work;
it does not claim that the oldest artifacts were collected. A stable, fully listable set makes
one rotation in at most `ceil(run_count / runs_max)` cycles if each cycle has time to attempt
its selected runs. Progress requires successful bounded listing and time to process runs.
Listing churn, insufficient cycle time, expiry, lookback, storage and retention can prevent
collection; no cycle promises that all remaining artifacts will eventually be kept. If the
listing exceeds 1,000, the operator must narrow the lookback to a listable window; runs outside
that window are not reached by this collector.

An absent position starts oldest first. A malformed, oversized, linked, unsupported-version or
different-repository/workflow position fails closed before run listing. Stop the collector
and ensure no history command holds its lock before removing `collection-position.json` (and
its `.tmp`, if present) to reset scheduling; do not edit its fields or migrate old versions.
Resetting repeats scheduling from the oldest listed run and leaves retained evidence intact.
The production-cycle regressions in
[`test_telemetry_collect.py`](../scripts/performance/tests/test_telemetry_collect.py) cover
350 newest-first paginated runs, a 137-run cap, retries, verified imports and restart.

A cycle exits 4 when it refused an artifact or ended early. The token is read only by
`telemetry-collect`, from the configured environment variable, and sent only to the API
origin. Signed storage receives no credential, and nothing collected is ever executed.

[`scripts/performance/service/`](../scripts/performance/service/) holds a systemd service and
hourly timer for a future always-on Linux collection host: the mirror is fetched first, then one
cycle runs as an unprivileged user from a pinned trusted checkout. Nothing installs or enables
them. No Mac launchd or cron job, Claw deployment, external sink or public server is part of
this.

### The frozen cohort's lane is left as it is

`fixtures/performance/cohort-quick-20261023-hosted-v6.json` remains byte-identical and disabled
as historical evidence (a test pins its SHA-256). The current workflow incorporates the
published-main and calibration repairs; it is not that old cohort's producer. The retained
deployment record describes an archive collector pinned to the cohort's report and profile
generations; consult the operator's dated records for its current deployment. This change moves the diagnostic
contracts to `af.perf-profiles/5` and `af.perf-report/3`, and ADR-0145 refuses earlier versions
in current readers. So the cohort's original artifacts are collected and read only by that
pinned collector, never by a reader of this checkout. The telemetry lane neither replaces nor
reads it, and the archive's existing reports and evidence are not touched.

## Not provided

- **Implementation evidence only.** At the 2026-10-24 implementation checkpoint, no hosted
  telemetry execution or collection-host deployment was demonstrated. Consult the operator's
  dated deployment and run records for subsequent operational status. The local evidence is
  functional only, and none of its figures is a product speed claim.
- **No product exports.** `af telemetry export`, `bottlenecks` and `critical-path` do not exist
  in this release. The Perfetto file and the phase rows are the laboratory's own derivation
  from `af telemetry show`.
- **No telemetry for the uncovered cases.** The native probe installs no telemetry subscriber,
  and discovery, startup and state-read commands record none; their phases are the `perf-trace`
  recipe's (see the matrix).
- **No observed cgroup limits.** A contained execution's limits are the ones its container was
  started with; unlike a verdict sample's, they are not read back from its cgroup.

The `authority_runtime_new` investigation that used these paths is
[`performance-runtime-new-investigation.md`](performance-runtime-new-investigation.md).

## Collection deadline and protocol upgrades

One absolute monotonic `collection_seconds` deadline covers transport, Git object subprocesses,
authority materialization, verification, copies, retention and history/report construction.
Git receives the existing `Budget`; the trusted collector's Unix main thread also uses its
interruptible timer across synchronous work. The collector refuses an unsupported thread or an
existing interval timer. It runs no candidate binary, downloaded code or credential-bearing
helper worker.

Status and position replacements reserve their peak allocations under the history lock,
including initial ownership records. They share the marked report staging directory;
interrupted owned writes are counted and swept on restart, while foreign root `.tmp` files
are counted and retained. A compact incomplete reserve can invalidate an old success by
rename even when the root is full. Capacity refusal leaves a failed/incomplete record and
preserves complete reports. Final `collection.json.used_bytes` includes status, position,
reports, the reserve and any owned leftovers, including the final status's own allocation.

Five seconds of that same deadline are reserved for bounded cleanup and atomic status writing.
An expired import leaves unaccepted staging for a later locked sweep. If expiry follows the
verified atomic rename, the accepted result stays kept and is recorded accurately. If history
construction expires, collection v2 reports failed with `results` and `used_bytes` null, rather
than fabricated zeroes or counts copied from a previous cycle. A terminal record that could
not be completed also has `ended_unix_ms: null`. An atomic incomplete record
precedes work and terminal cleanup; interrupted final I/O leaves that record. The history lock
is released on all exception paths, including entry cleanup and interruption immediately after
open or flock. A temporary Python SIGALRM handler latches expiry only during nonblocking
descriptor publication and close, delivering it immediately after ownership is recorded or
released. This also works with a cancelled unmasked resolver still alive: masking only the
main thread cannot prevent Python from running its handler there. Nested unwinds restore
handlers; neither the absolute timer nor the caller's signal mask changes. Nonblocking flock
runs with ownership already registered.
Failed acquisition closes only its own descriptor and leaves the other owner alone.

An interrupted HTTP wait first revokes the exchange and shuts down/closes its published socket.
The raw-to-TLS descriptor transfer is synchronized with abort. Cleanup waits at most one second,
within the request's absolute deadline and enclosing cycle timer. A system resolver may stay
blocked in its daemon thread: cancellation disables subsequent connections, TLS and requests,
but does not stop that thread. Missing cleanup confirmation prefixes a bounded exception
message on Python 3.9+, preserving its type without the Python 3.11 exception-note API.
Outer cycle detail, persisted per-run/per-artifact refusals and shared archive refusals retain
it, even when a request times out before the cycle. Confirmed cleanup adds no warning.
An interrupted thread join is never evidence that it stopped. The service timeout does not
replace this CLI deadline.

The raw read-latency document and schema, experiment registry, history and collection now use
v2. Earlier shapes and incomplete authority identities are refused without compatibility
fallback. Result v1's shape is unchanged. Historical artifacts require their matching trusted
verifier and complete authority; a newer verifier never silently admits them. Tests use
synthetic documents, not claimed measurements; hosted execution and deployment still require
operator evidence.

Filesystem cleanup is part of completion. A container that stopped can still leave a directory
its owner cannot traverse; the controller never changes its permissions to manufacture cleanup.
The trusted hosted owner is fenced before deletion. Any deletion failure retains the exact work,
image and mount recovery paths, blocks later launches and makes `ci-scratch --cleanup` refuse.
Restore or remove that state only through explicit operator recovery after investigating it.
Safe pre-launch refusal and confirmed deletion remain automatically cleanable.

The run's deadline starts before binary, authority and CLI build-record admission. Bounded
non-following reads, corpus copies, integrity checks, cleanup and publication consume that same
allowance; synchronous phases are interrupted rather than checked only afterward. A terminal
interruption can leave unadmitted evidence without a complete bundle. Diagnostic telemetry
readers use the native supervisor and shared sample containment: IPC and daemon logging disabled,
implicit image volumes refused before launch. The mandatory hosted Docker smoke exercises this
reader route beside the eight build and six measurement cases; local unit tests are not a live
Docker result. See [ADR-0222](adr/0222-fence-filesystem-cleanup-and-bound-telemetry-admission-through-publication.md).

Shared supervision owns exceptions after `Popen`, including pipe and start-callback failures.
It attempts the existing bounded cancellation protocol; local cleanup requires a valid reply.
Contained cleanup requires confirmed removal even when launch raised an ordinary exception.
Typed ownership evidence survives the tool runner, including cancellation unwinds. An opaque
executor exception is uncertain; only proven pre-launch refusal is a safe launch error.
Profile retains namespaces, decoder/renderer staging and both work roots on uncertainty and
admits no bundle or later launch. The smoke fixture supplies the same required work ownership
fields as a real Profile; its ready/`docker ps` assertion and ten-second observation stay intact.
Hosted run 40267246869 job 121392212426 failed that observation after eight build and five
measurement cases; its cause remains unproven. Fresh hosted CI and live Docker remain required.
