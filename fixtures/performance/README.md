# Performance laboratory fixtures

The data of the continuous performance laboratory
([design](../../docs/design/continuous-performance.md),
[ADR-0162](../../docs/adr/0162-freeze-performance-plans-and-judge-them-by-exact-paired-verdicts.md),
[ADR-0163](../../docs/adr/0163-measure-native-workloads-through-a-public-api-probe-and-independent-golden-oracles.md),
[ADR-0166](../../docs/adr/0166-build-frozen-plans-and-supervise-samples-with-one-authority-instrument.md),
[ADR-0172](../../docs/adr/0172-admit-candidate-files-through-bounded-descriptors-and-report-archive-and-trend-runs-offline.md),
[ADR-0174](../../docs/adr/0174-diagnose-in-separate-child-runs-that-record-only-the-bracketed-region-and-share-one-test-executable-boundary.md)).

- `suite.toml` — the reviewed registry (`af.perf-suite/4`): cases, parameter domains, oracles,
  the native probe and each library case's probe contract and the seed it reads (ADR-0170),
  policies, budgets, recipes,
  environments, tiers and limits. `python3 scripts/performance/cli.py list` validates and
  expands it and checks that the golden entries cover every native instance. Every recipe,
  environment and case is executable where its host provides it (ADR-0169); a host that cannot
  refuses at run time with its reason, which `cli.py capabilities` lists.
- `cli.toml` — the whole-CLI commands (`af.perf-cli/6`, ADR-0168 to ADR-0172): per CLI adapter
  and process instance, the actual AF argv, its untimed fixture setup and the independent
  oracle rules, and the seeds each side writes once per shard. `task show`'s whole output must
  be one complete JSON document whose `task_id`, history numbered exactly 0..9999 in order and
  9,997 lease renewals the supervisor observes past any retained log (`json_stream_*`,
  ADR-0172); whitespace, member order and undeclared fields are free. `cli/setup.sh` performs the
  setups and seed writers. `cli/task-roots` is the CLI Task fixture, whose catalog pins a test
  checks against the reference digests. `cli/campaign-roots` is the model-free review whose
  Campaign the `review ledger` and `review report` commands read; a test checks its pipeline
  pin.
- `cli/tui-open.py` — the stdlib-only 100x30 PTY driver (`af.perf-tui-open/1`). It reports
  canonical decimal millisecond strings for first output and the first browser status-line
  frame. `tui_open` uses the checked `browser_ms` interval as its primary metric; its private
  registry setup invokes each side's `af task list --state` for all 20 copied Stores (ADR-0257).
- `adapters.toml` — the reviewed native probe adapters (`af.perf-adapters/1`): each a fixed
  priority, an equivalence group, the contracts it serves and the exact closure of authority
  example files a build copies into a measured source before compiling it. A test requires each
  closure to be exactly its example's files.
- `diagnostic/real-tool-synthetic/` — retained real perf 6.1 output over an owned synthetic C
  workload, its DSO prefix normalized, for the parser and stack-quality tests; not an AF capture
  (its README).
- `diagnostics.toml` — the diagnostic registry (`af.perf-diagnostics/1`, ADR-0174): selection
  and capture defaults within the hard caps, the one declared sampled event, the DWARF
  alternative and its threshold, the minimum samples that attribute a region, the trace overhead
  target, the counter cases, the child run's limits, the diagnostic recipes (each a verdict
  recipe of `suite.toml` with only features, symbols and frame pointers added) and every external
  tool with its pin, how it reports its version and its provisioning instructions.
- `telemetry.toml` — the telemetry experiments (`af.perf-telemetry-lab/2`, ADR-0214): each
  exactly a bench script of the telemetry design's §12 with its document, the product schema it
  is read through, its levels and the inputs that identify its measurement authority, and one
  bounded profile per trigger (rounds, pairs, scale, corpus size, timeout, work bytes and
  retention). `cli.py telemetry-run` measures it; nothing in it is a verdict sample.
- `telemetry-diagnostic/` — the four `af telemetry` documents (`runs`, `show`, `status`,
  `config`) a debug build of this source's AF (0.12.0-rc.5) wrote for the CLI Task fixture in a
  private HOME on a macOS development host, its directory path replaced by `/scratch/sample` (ADR-0214): the
  telemetry-capture tests read them through the product's schemas and mutate them into every
  partial and hostile state.
- `golden/native.json` — the native oracle's golden entries (`af.perf-golden/2`): per instance,
  the expected fixture identity, digest and count, with the semantic projection a digest covers;
  per seed a native instance reads, what its writer's check must report (ADR-0170);
  per fixture, its exact size and generation; and the cap boundary. `perflab.reference` derives
  every value from reference bytes it constructs itself or from explicit semantic facts; it never
  runs the product. `python3 scripts/performance/cli.py golden` checks the file byte for byte
  and `golden --write` regenerates it after a reviewed fixture change.
- `schema/` — golden valid and invalid examples of every `af.perf-*` contract, listed in
  `schema/index.json` with the reader error each invalid example must produce and whether the
  committed JSON Schema also refuses it.
- `synthetic/suite.toml` and `compare/` — a SYNTHETIC registry and the golden verdicts of its
  comparison bundles. They exercise the verdict rules; no number in them describes the AF
  product. Only `compare/index.json` and each `verdict.json` are committed: the fixture test
  generates the run bundles with `scripts/performance/tests/compare_fixtures.py` and requires
  identical verdicts, whose bundle identities pin every generated byte; `--write` rewrites the
  goldens (ADR-0179).

- `kernel-measure/` — the staged policy of guarded kernel experiments (ADR-0185), for a
  reviewed human installation into `.af/`: the measures that run the literal isolated bootstrap
  of `scripts/performance/perflab/kernelguard.py` with the protected harness digest it expects,
  their `elapsed_ms` objectives, and the inventory of every protected file
  (`af.perf-kernel-inventory/1`). It is the only subtree the harness digest excludes, holds only
  nonexecuted `.md`, `.toml` and `.json` data, and is re-pinned by
  `python3 scripts/performance/cli.py harness-digest --write` (its README).

There is no `activation.toml`: no measuring authority has been calibrated, so a full plan is
unavailable until a reviewed activation binds a passing calibration cohort. Such an activation
(`af.perf-activation/3`, ADR-0183, ADR-0184) binds the cohort, its sealed parameters and its
readiness as committed files by path and SHA-256, and the environment, recipe and CPU/image
groups the cohort calibrated; an equivalent-adapter bridge also binds its execution evidence
(`af.perf-equivalence/1`). Every active plan verifies those records locally, and the
`Performance activation` workflow's `cli.py activate --verify-proposed` recomputes them from the
retained Actions evidence before the activation change merges.

## Building a frozen plan

`cli.py build` builds a frozen plan's products and its one authority supervisor without
resolving any ref again, exactly as the plan froze them:

```sh
python3 scripts/performance/cli.py plan ... --build-image registry.example/af-build@sha256:...
python3 scripts/performance/cli.py build --plan run.json --repo . \
  --zigbuild-bin /opt/release-tools/bin
```

How a recipe builds is frozen in the plan, in `profile.build` and the recipe identity
`profile.recipe_digest` (ADR-0170). A Linux recipe builds in a pinned build image (ADR-0169): the
recipe's own `container_image`, or, for a recipe without a pin, the image `plan --build-image`
froze. Either is pinned by digest in a canonical form, `sha256:<image ID>` or a lowercase
`name@sha256:<manifest digest>` without a tag, and must be present on the host; it is never
pulled. A manifest digest names its image only under exactly that repository name, and is never
compared with the local image ID. Every compile is a container of it as this user, without
network, with only the run's directories mounted and the authority closure bound read-only; the
image's own Rust, cargo-zigbuild and Zig are verified by version. `plan --trusted-host-build`
freezes a host build of an unpinned Linux recipe instead, recorded as a trusted host build that
claims no filesystem isolation; without one of the two the plan is refused. A macOS recipe
builds on its trusted local host. `build --build-image` may only restate the frozen image: a
build never chooses another, and `compare` requires the recorded image to be exactly the frozen
one. Plans whose effective builds differ have different recipe identities.

The build is the run's first execution: `builds.json` records the run's one execution start and
its absolute total deadline, the start plus the plan's `run_total_minutes`. No later command
resets it; `--deadline-unix-ms` only tightens what one command may use, and no build step runs
past the earlier of the two.

It works below `target/performance/<run>/`: a private bare repository holding exactly the plan's
commits, detached worktrees `worktrees/{authority,base,head}` with their own targets, and a
private Cargo home that `cargo fetch --locked` fills before any build script runs
(`--cargo-home` names a pre-fetched one instead). The musl shipping recipe builds with
`--zigbuild-bin DIR`, a directory whose `cargo-zigbuild` and `zig` report exactly the recipe's
versions; without it, or with another version, the build is unavailable. It keeps `build/`
with `run.json`, `builds.json`, the capped step logs and the read-only `artifacts/` under their
manifest, and removes everything else unless `--keep-worktrees`. Exit 0 means every build succeeded, 4 that a
side, the supervisor or the host's capability did not (the reason is printed), and 2 invalid
input. `cli.py capabilities` reports what this host can execute and why anything is unavailable.

## Running a frozen plan

`cli.py run` executes exactly one shard of a frozen plan from its verified build, and
`cli.py aggregate` recomputes the verdict over every planned slot:

```sh
mkdir -m 700 -p target/perf-scratch
python3 scripts/performance/cli.py run --plan run.json \
  --build target/performance/<run>/build --shard 0 --run-dir target/perf-run \
  --trusted-local --scratch target/perf-scratch
python3 scripts/performance/cli.py aggregate --run target/perf-run \
  [--shard-bundle DIR...] --output verdict.json
```

`--trusted-local` is the named macOS environment's mode. `--container --image sha256:...
--scratch-fs MOUNT` runs one pinned Linux sample container per sample: the supervisor is its
PID 1, and only the per-sample `sample/` directory of a verified disk-bounded scratch filesystem
is writable. The image must provide `/bin/sh`, `/bin/true`, `git` and `/usr/bin/python3`.
The mode must be the plan environment's, and a not-yet-executable state refuses the run (exit 4).

The shard runs its slots in planned order: start sentinels, each case's AB/BA warmup and
measured pairs, end sentinels. Each binary is rehashed against the build before each launch;
nothing is rebuilt, retried or reordered. Records are appended to
`shards/<index>/samples.jsonl` as they are made. A terminal `shard.json` and the bundle manifest
are written whatever happens:

- `complete` when every slot has a record;
- `interrupted` at its deadline, the earliest of the run's total deadline from its build, a
  caller's `--deadline-unix-ms` and its sampling deadline (checked against each sample's worst
  case, seed writing included, before it starts), or on a termination signal;
- `failed` after an uncertain cleanup, a supervisor without a reply, a changed artifact, an
  unrestored scratch, unverified container limits, a driver error, or instances with no
  coverage.

A shard started after the run's total deadline refuses before its preflight and creates nothing.
The run's deadline, tightened by `--deadline-unix-ms`, also bounds the preflight (ADR-0171):
every Git read of the authority, the container runtime query and the scratch filesystem's
enforcement fill ends 30 s before it, a step it ended refuses the shard as `run_deadline`, and
no later step starts. A supervisor is never killed in place of its sample: at its backstop or
the deadline it is sent SIGTERM and cancels the sample through the same supervision, and a
launch the deadline could not cancel in time is never made. When nothing confirms that a
sample ended, the shard stops (`cleanup_uncertain`) and keeps the sample's private scratch and
the shard's seeds for recovery, naming them in `shard.json`.
Unstarted slots have no record, so the verdict is incomplete. Exit 0 means the shard completed;
4 means it did not or could not start; 2 means invalid input. `aggregate` imports shard bundles
uploaded from other hosts, never over a different bundle, and exits with the verdict's code.
Concurrent shards need separate hosts: a shard holds the machine's one lock,
`/tmp/af-perf-machine.lock`, from before anything shared is touched until its terminal record,
whatever its user, `TMPDIR` or `HOME` (ADR-0169); a second one refuses with
`concurrent_shard`.

A seed or build output a measured side wrote is admitted through held descriptors that never
follow a symlink (ADR-0172). The whole tree is enumerated before anything is read or sealed, and
entries, depth, logical bytes and the run's deadline are bounded throughout.

Large CLI state is seeded (ADR-0169): before a side's first sample of `task show`,
`review ledger` or `review report`, that side's own binaries write its 10,000-event Task or
Campaign state once, under the supervisor. The shard seals it, and each sample copies it into
its own scratch as untimed setup. `shard.json` records every seed with its digest, and a seed
that changes stops the shard.

Native fixtures are seeded the same way (ADR-0170). The source capture and state projection
probes read a seed their side's own probe writes once per shard through a writer contract: the
committed repository of each file count, and the 10,000-event Task log the application-cold and
prewarmed projections share. The writer's check is judged against the seed's golden entry
before the seed is sealed. Each sample's untimed setup copies the sealed seed into its own
scratch (`setup.sh native_seed_copy`), and the probe captures or projects only its copy: the
copy's time is the sample's `fixture_setup_ns`, the probe's own setup its `setup_ms`, and
neither is the primary metric.

## Reports, archive and history

`cli.py report --run DIR --output NEW_DIR` recomputes a run's verdict and writes `report.html`:
self-contained, every evidence string escaped, and only the bundled script and style allowed by
their hashes. It also writes the matching `report.json` (`af.perf-report/3`), `verdict.json`,
the job-summary table `summary.md` and `evidence/`, the verified bundle members the page links;
`--job-summary $GITHUB_STEP_SUMMARY` appends the table. It exits with the verdict's code. Each
`--diagnostic CHILD_DIR` links a verified diagnostic child run of this run: its copy goes below
`diagnostics/<child run>/`, and the page and summary show each case's flamegraphs, trace,
timeline, coverage, overhead label, telemetry captures and omissions. An unrelated, tampered or unsafe child is
refused, and so is one whose selected cases are not the parent's workloads or whose profiles
claim another kind than the recipe its frozen plan froze (ADR-0179). The rerun command keeps the plan's trigger, revisions, authority, cases, shards,
pairs, seed, environment and build mode, and names what it cannot carry as a placeholder.

`.github/workflows/performance.yml` runs these commands continuously
([ADR-0177](../../docs/adr/0177-schedule-continuous-runs-from-trusted-control-and-move-builds-once-to-bounded-shards.md),
[runbook](../../docs/performance-ci.md)): `ci-schedule` freezes each event's comparisons from the
trusted control, `ci-pack` and `ci-unpack` move one verified build to every shard, `ci-stage`
uploads a shard's evidence in the transport format, and `ci-aggregate` reports what arrived.
`.github/workflows/performance-release.yml` hands each published release to it: its read-only
`ci-release-handoff` and `ci-release-confirm` claim a release's upstream run, tag and SHA once,
before any sample
([ADR-0180](../../docs/adr/0180-hand-off-published-releases-read-only-and-measure-each-claimed-identity-once.md)).

`cli.py history --inputs DIR... --output NEW_DIR` groups runs of compatible identity and links
each run's own report. `cli.py archive` keeps evidence in a content-addressed filesystem archive
that a trusted collector fills from run directories or Actions transport artifacts
(`af.perf-transport/1`); its runbook is [performance-archive.md](../../docs/performance-archive.md).

## Diagnostic child runs

`cli.py profile` creates one separate, bounded child run of a comparison (ADR-0174). It never
measures a verdict sample and never changes the parent's verdict:

```sh
python3 scripts/performance/cli.py profile --plan run.json --verdict verdict.json \
  --recipe linux_gnu_cpu_profile --output target/perf-diag \
  --perf /usr/bin/perf --inferno-bin /opt/inferno-0.12.8/bin
```

`--case ID` (repeatable, at most 8) selects explicitly; `--verdict` selects the parent's
regressed cases, then its inconclusive ones, by instance ID (3 by default, `--max-cases` at most
8). The command freezes a child plan of scope `diagnostic` (`af.perf-run/7`, with the recipe's
declared kind, base recipe and tools), builds the diagnostic recipe like
`cli.py build` (or takes `--build DIR`), and works within one wall deadline (15 minutes, at most
30 with `--total-minutes`), a 128-MiB byte cap and a build cap, all checked while it runs.
`--mode` is explicit and recorded against the containment the plan's environment declares;
everything runs under the build's authority supervisor and the machine lock. `trusted_local`
runs on this host under process-group containment. `linux_container` (with `--image` pinned by
digest and `--scratch-fs`) takes a shard's preflight, including the enforcement fill of the
disk-bounded scratch filesystem, and runs each execution in its own sample container: network
none, read-only root, no capabilities, the environment's PID, memory, swap and CPU limits, and
only the run's own directory of that filesystem writable (ADR-0176). Every file a tool or
candidate wrote is admitted as a confined bounded regular file. A step whose files together
outgrow the room is cancelled and refused, and the byte usage recorded is the evidence's actual
size, with an 8-MiB administrative reserve inside the cap. It
writes `run.json`, `profiles.json` (`af.perf-profiles/6`), `diagnostic.html`, the captures,
flamegraphs, traces, counters and, with `--telemetry spans|content`, each eligible CLI case's
telemetry documents (`telemetry/<case>/<side>/`, ADR-0214), and a bundle manifest into the new
`--output` directory. It
exits 0 when everything requested was captured and 4 when anything was unavailable; each
omission names its reason and exact reproduction.

- **CLI cases**: each execution runs the side's own AF binary, directly or through the declared
  PTY driver, in a private HOME, XDG and TMPDIR after its setup. The supervisor's observation of
  the whole output is judged by the command's oracle. A CPU profile records each fresh process
  whole; a trace's region is the startup of a discovery command or the dispatched command.
  Cases declared as requiring Linux containment run only with `--mode linux_container`.
- **CPU profiles** (`linux_gnu_cpu_profile`, `linux_musl_cpu_profile`): `perf record` starts
  disabled and the probe's `perf-control` handshake enables it for exactly each region,
  acknowledged by perf. One
  recording repeats the probe as fresh executions of its image on fresh scratch and seed copies,
  each message judged as it arrives on the handshake channel, until 10 s of region at 997 Hz (at most 30 s
  and 1,999 Hz), the repetition cap or the deadline. Frame pointers first, then one 32-KiB DWARF
  capture when unknown frames exceed the declared ratio. Raw `perf.data`, scripts, statistics,
  build IDs and every repetition's judged message are kept. Fewer samples than the declared
  minimum are kept but never rendered. The pinned inferno renders each side and the normalized
  difference in both directions; every SVG is kept as written and sanitized for embedding.
  Only the sanitized SVG is linked or embedded; inferno's original, which may hold active
  markup, is archived evidence the page names by path and hash and never links (ADR-0179).
- **Phase traces** (`*_trace`): the `perf-trace` probe or AF exports metadata-only spans
  (`af.perf-trace/1`) beside a Perfetto-compatible export and an offline timeline; paired runs
  against the parent's verdict build (`--parent-build`) measure the median overhead, labelled
  `perturbing` above 2%. Each trace states its region's coverage; a setup that filled the
  recorder leaves `region_missing`, never a complete trace.
- **Counters** (`linux_gnu_counters`): Callgrind instructions of the bracketed region and DHAT
  allocations of the whole process, for the declared small cases, as explanatory proxies.
- **samply** (`macos_arm64_samply`): an optional local whole-process capture of a self-built,
  unhardened probe, saved without a viewer.

Tools are configured explicitly and never installed or searched for on `PATH`. Nothing grants
a privilege or changes a kernel setting. On Docker Desktop, perf records a mounted binary at its
`/run/host_virtiofs/...` path, so the executor mounts the binaries there too for symbols and
build IDs.

## Running the native probe

The probe is the `perf-probe` example of `review-pipeline`; the supervisor is the
`perf-supervisor` example of `review-process`. A diagnostic sample runs in explicitly trusted
local mode only. It takes both binaries from a verified build, after rehashing them:

```sh
mkdir -m 700 -p target/perf-scratch
python3 scripts/performance/cli.py probe --trusted-local \
  --build target/performance/<run>/build --side baseline \
  --instance authority_runtime_new__package_issue_255_large \
  --scratch target/perf-scratch
```

`--supervisor PATH --binary PATH` name the two binaries directly instead. `--instance` takes any
instance of a library case that `list` shows with an available oracle and that spawns no
process, or `sentinel_canonical`. A case that spawns processes (source capture) needs Linux
namespace containment and is refused with exit 4 before anything is created. The driver builds
the `af.perf-probe-request/2` from the registry and creates a private 0700 namespace with a
random name below `--scratch`. The supervisor runs one probe process in its own process group
with the case's timeout. The driver then removes the namespace through its handle and verified
tombstones, and prints its judgment. The sample is `passed` only when the supervised lifecycle
is acceptable, the message's identity, fixture digest, digest and count equal the golden entry,
and the namespace's removal was confirmed.

`--scratch` must be owned by you and writable by no one else. That private directory is the
trusted local ownership boundary. Processes of your own user are trusted, not excluded: the
report says `same_user_isolation: not_provided`. Nothing else in `--scratch` is touched. Exit 0
is passed, 3 a failure of this side, 4 an unsupported case, and 2 invalid input. A failure's
kind is the sample contract's: `oracle_mismatch`, `lease_expired`, `nonzero_exit`,
`missing_required_metric`, `lifecycle_violation`, `launch_failure` or `timeout`. An unconfirmed
cleanup is `lifecycle_violation`, or is appended, bounded, after the sample's own diagnostic.

The probe itself removes nothing: it creates `probe-work` in the empty scratch it is given,
writes only below that, and leaves it. Whoever runs the probe directly owns that scratch and
removes it afterwards.
`cli.py judge --request PATH --stdout PATH --exit-code N` judges a recorded execution the same
way.

Each sample generates its own fixture through the product's writers below the scratch; setup is
reported as `setup_ms`, apart from the timed region's `wall_ms`. A diagnostic sample of a
seeded instance first writes its own seed through the same probe's writer contract, judged
against the golden seed entry, and reads it once. Expect seconds of setup for the issue #255
packages, and tens of seconds for the 10,000-event Task log, which is written through one
FULL-synchronous transaction per event. A sample is a diagnostic: it is not a Performance
Sample of a run, makes no speed claim, and a macOS timing establishes no Linux budget. Paired
execution is `cli.py run`.
