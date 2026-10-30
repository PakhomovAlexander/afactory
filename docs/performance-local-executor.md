# Independent performance execution on Claw

`cli.py executor` computes on the VM when `backend = "local"`. It needs neither an Actions
runner nor a workflow/job/artifact/run ID or `GITHUB_*` environment. `github_actions` instead
dispatches the existing Performance or Performance telemetry workflow. Failure never selects
another backend. Both paths remain advisory; completion is independent of a passing verdict.
This guide installs nothing. The coordinator owns deployment after acceptance.

## Install trusted control and source access

Use a Linux x86-64 VM with Docker, ext4 loop devices, sufficient disk above the unchanged
10-GiB free-space floor, current supported Python and the image's pinned Rust 1.88.0. Install
verified build/sample images by registry digest beforehand; compute never pulls an unpinned
image. The GNU recipe uses the GNU image; shipping musl needs its existing Zig/cargo-zigbuild
image and tools. Begin with the GNU profile. Provision CPU/memory for the frozen environment:
core samples use two logical CPUs, 4 GiB memory, 256 PIDs and 2 GiB enforced scratch; telemetry
uses its registry's two CPUs, 8 GiB, 1,024 PIDs and profile-specific scratch bound. The trusted
contained telemetry builder retains its 8-GiB aggregate filesystem and offline compilation.

Install the exact accepted committed producer as a root-owned, non-writable checkout at
`/opt/af-perf/control`, without bytecode caches. Do not use a moving candidate checkout.
The controller checks installed authority files against `control_sha` before submission and
execution. A changed producer needs a reviewed new installation and distinct cohort; never
invent a SHA to get past preflight. No candidate workflow, hook, harness or configuration
executes on the host. Candidate build scripts run only inside existing build containers.

Create private accounts `af-perf-source` and `af-perf-exec`, and group `af-perf-readers`.
The source account owns a credential-free bare source mirror
`/var/lib/af-perf-executor/source.git`, readable by the group and not writable by compute.
Configure its origin without an embedded secret. Keep fetch credentials only in that source
account's protected environment/credential mechanism (`/etc/af-perf/source-token.env` in the
example), with no credentials stored in the mirror. Git uses its normal trusted authentication
mechanism; an environment file alone does not install a credential helper. Supply exact PR
merge/head objects to this mirror through the controller when submitting a PR.
The trusted controller grants Git `safe.directory` only to that exact configured mirror,
allowing reads across these accounts without a wildcard or global configuration. Mirror paths
with symlink aliases refuse. Ordinary repository readers keep their ownership checks.

The compute account owns a private 0700 journal directory's parent. Each local core/diagnostic
build gets a private Cargo home: the existing contained fetch retrieves locked dependencies,
then compilation is offline. No shared writable cache or developer Cargo home is admitted.
The entire private build directory counts against the unchanged worktree byte limit. Local
build steps use the telemetry builder's resource ceiling: 8 GiB memory, 512 PIDs, no writable
IPC or daemon log, and an 8-GiB file limit. Images with implicit writable volumes refuse.

Copy [executor.example.toml](../scripts/performance/service/executor.example.toml) to
`/etc/af-perf/executor.toml`, owner root or compute, mode 0600/0644 without group/other write.
Replace its deliberately unusable zero SHA and image digests with real accepted identities.
Review paths, repository, machine/journal IDs, byte caps, queue bound, total wall bound, tier,
local environment, shard count, case selection, telemetry profile and diagnostic settings.
The configuration is `af.perf-executor/4`; misspellings and obsolete v1, v2 and v3
configurations refuse. `[core.windows]` (optional; absent, or JSON `null`, keeps the tier's
own) declares the `shard_sampling_minutes` and `run_total_minutes` a core plan freezes instead
of its tier's; a declared table must supply both numbers (an empty table refuses), within the
same hard maxima (90 and 180 minutes for `nightly` and `release`, 30 and 120 for `quick`); they
are part of the frozen configuration digest and the plan digest, and a calibration original
refuses them. They are a `local` backend setting: `performance.yml` has no windows input, so a
`github_actions` configuration that declares them is refused when it is loaded, before any
original is reserved or dispatched. Only the full release comparison recipe below declares them. `diagnostic.observer = "host_perf"` selects the trusted host CPU observer described
below; unset, perf runs inside the contained child.
For `tier = "nightly"` or `"release"` with `environment = "linux_x86_64_musl_claw"`, set
`zigbuild_bin` to the canonical absolute directory **inside the pinned build image** holding
`cargo-zigbuild` and `zig` (or its `python3 -m ziglang`). For example,
`zigbuild_bin = "/opt/af-perf-tools/bin"` is valid only if that verified image actually installs
its tools there. It is not a host tool path or automatic discovery. Omission refuses local
musl configuration; relative paths, aliases such as `..`, PATH lists and shell expressions
refuse. GNU needs no setting. Both parent and diagnostic builders receive the frozen directory
and verify the unchanged recipe pins: cargo-zigbuild 0.23.4, Zig 0.15.2 and Rust 1.88.0.
Changing the directory changes the frozen configuration and environment identity.
Finish or discard prior-version journals with their original control before upgrading;
state/result v1 receipts are also refused rather than migrated. New originals are written as
`af.perf-executor-request/3` and `af.perf-executor-result/3`; request/1, request/2 and result/2
originals already in a journal stay readable by `status` and `verify`
([ADR-0266](adr/0266-measure-nightlies-as-originals-of-the-independent-executor.md)). The
producer check is ADR-0240's and unchanged: `verify` still refuses an original written under
another installed control. Profile limits, oracles, N/K, tolerances,
sentinel rules and the free-disk floor are unchanged. The example reserves 256 GiB overall,
32 GiB each for native core archive and local telemetry history; capacity exhaustion stops
new work, never deletes original receipts. Provision a filesystem/quota at that overall bound
as a hard backstop. Keep daemon image/log storage bounded separately; sample/build containers
retain their existing disabled logging/IPC and write boundaries.

Grant only the trusted account's existing `hostprep` operations through an operator-reviewed
sudo policy for loop images/mounts under this journal: noninteractive mount/unmount and
ownership setup. Candidate containers receive no sudo, Docker socket, credentials or host
homes. Docker access itself is privileged operator authority, restricted to trusted control.
The units are examples, not an automatic privilege installer.

Precreate `/tmp/af-perf-machine.lock` as root, 0644, regular, singly linked, in the system's
sticky `/tmp`. Never replace an existing lock to provision it. A pre-existing owner-created
0644 lock is supported too. Verify every manual user/service sees the same device/inode:

```sh
stat /tmp/af-perf-machine.lock
```

Freeze that inode in `/etc/af-perf-machine-lock.json`, root-owned and not writable by
other accounts. Create the file once with actual values; never copy an example inode or
replace the lock to match an anchor. For example, as root after checking the existing lock:

```sh
python3 -I - <<'PYCODE'
import json, os, stat
info = os.lstat("/tmp/af-perf-machine.lock")
assert stat.S_ISREG(info.st_mode) and info.st_nlink == 1
assert stat.S_IMODE(info.st_mode) == 0o644
path = "/etc/af-perf-machine-lock.json"
with open(path, "x") as output:
    json.dump({"schema": "af.perf-machine-lock-anchor/1",
               "device": info.st_dev, "inode": info.st_ino}, output)
    output.write("\n")
os.chmod(path, 0o644)
PYCODE
```

Independent compute refuses a missing anchor. Once installed, every common-lock entry point
also checks it: a private `/tmp` cannot create a different lock. A reboot/mount change that
invalidates the anchor requires explicit operator inspection, never an automatic repin.
Install the [tmpfiles exclusion](../scripts/performance/service/af-perf-machine-lock.tmpfiles.conf)
so age-based cleanup cannot remove the lock inode or an uncertainty fence.

`PrivateTmp=no` is mandatory for compute. Avoid private mount namespaces: loop-mounted scratch
must also be visible to the Docker daemon. The compute unit deliberately uses account/file
permissions rather than systemd filesystem options that would split these namespaces.
The source-only fetch service may use a private temporary namespace because it measures nothing.

## Queue and recovery outcomes

Each service invocation drains one admitted original before observing main. If idle, it may
observe and execute one main original in that invocation. After draining existing work it
queues a newly observed tip for the next invocation; a busy full queue defers discovery until
space exists. The finite queue bound and exact submission identities remain enforced.
Authority failures still refuse; they are not treated as backpressure.

Diagnostic build-container removal uncertainty stops all further diagnostic launches, even
when diagnostic scratch is empty. The child plan and `profile/cleanup-uncertain.txt` retain the
named build-container recovery detail; existing build paths remain available. Both standalone
profile runs and executor-borrowed leases keep the common fence. Inspect and confirm all owned
containers and resources stopped before operator recovery. A confirmed-clean diagnostic build
failure remains a reportable omission.

Calibration originals recheck their frozen cohort/member day after capacity scanning and
again after source preparation, immediately before the first builder can launch. A missed
original becomes terminal `expired` in `af.perf-executor-state/2`, without a build or sample,
and is never retried or replaced. If the day expires during source preparation after claim,
`af.perf-executor-result/3` (result/2 before it) records `not_executed_off_schedule`; its start
timestamp records the claim, not an executed sample. Pre-claim expiry retains null start/result
fields. Native local
readiness reports either case as `invalid` / `off_schedule`, retains the request provenance
and original denominator, and grants no qualification. Future-day submissions still refuse.

## One local run

Run as the compute account from the trusted installation. These commands require full real
commit identities; shell variables below are operator inputs, not suggested SHAs.

```sh
python3 -I -B /opt/af-perf/control/scripts/performance/cli.py executor \
  --config /etc/af-perf/executor.toml submit --work core \
  --base "$BASE_SHA" --head "$HEAD_SHA" --key "$EXPERIMENT_KEY"
python3 -I -B /opt/af-perf/control/scripts/performance/cli.py executor \
  --config /etc/af-perf/executor.toml cycle
python3 -I -B /opt/af-perf/control/scripts/performance/cli.py executor \
  --config /etc/af-perf/executor.toml status
```

Start with an explicitly selected small core case for an operational smoke, then restore the
complete family. An empty `core.cases` selects a full advisory bootstrap; a finite list selects
partial scope. Shards run sequentially on this one VM under the original total deadline.
Serialization does not enlarge it. The local schedule is planned serially before anything is
reserved: the plan's `build_minutes`, then each shard's whole `shard_sampling_minutes` window
(its start and end health sentinels inside it), then the runner's 30-second finish reserve must
fit the tier's `run_total_minutes` and the executor's `run_seconds`. A submission past that
bound is refused before any original, claim or build, naming the shards that would fit (quick:
1 of 60 minutes; nightly and release: 2 of 120 minutes). No window, sentinel, sample count or
deadline is relaxed to fit, and Actions keeps its own parallel schedule, never a fallback. A
whole family that does not fit one original is a native comparison instead
(`comparison-launch`, below): it partitions the same plan's shards into sequential bounded
originals. A run that fits may still stop at a window or the deadline; its stopping reason and
missing slots remain visible. Builds, preparation and measurement all hold the same machine
exclusion.

Before submitting, run the same checks as the compute account without reserving anything:

```sh
python3 -I -B /opt/af-perf/control/scripts/performance/cli.py executor \
  --config /etc/af-perf/executor.toml preflight --work core \
  --base "$BASE_SHA" --head "$HEAD_SHA" --key "$EXPERIMENT_KEY"
```

It prints `af.perf-executor-preflight/1` and exits 0 when `ready`, or 4 with the exact first
refusal: an unreachable root or mirror through any ancestor, a source that does not resolve, a
reused key, a full queue or journal, missing capacity, the serial bounds above (`serial`), a
profile's diagnostic tools (`tools`, with the host observer's actual capability as
`host_observer`, checked under the machine exclusion) or the configured images (`tools`:
`build_image`, `sample_image`, `image_toolchain`, `sample_runtime`, or `image_admission` with the
refusal). A
ready preflight is not a reservation: submit next.

Preflight, submission and the claim each check the configured `build_image` and
`sample_image` as the compute account: pinned by digest, present on this host (nothing pulls),
Linux and declaring no implicit writable volume. In the build image they run the recipe's own
probes: its exact Rust toolchain and target and, for the musl shipping recipe, the
`zigbuild_bin` directory's cargo-zigbuild and Zig at their pinned versions; in the sample image,
one probe of its own interpreter, tools and containment (`sample_runtime`). These probes start
containers, so preflight and submission take the machine exclusion for them (a busy machine
refuses the check) and the claim runs them under its own. A missing image or tool refuses
before any original, claim or build, naming the image or tool. One that disappears after
submission ends the original `failed` (measurement `unavailable`) before its first build, with
no fence kept. An image whose identity or volume declaration refuses ran only `docker image
inspect` and no probe container, so it keeps no fence either; only a probe container whose
removal is unconfirmed keeps the fence. Nothing is
pulled, provisioned or re-pinned for you: provision the pinned image, then submit again.

Preflight, submission and the claim admit storage with one calculation: the bytes the root
holds now plus the original's native reserve (core and profile: worktree, sources, scratch,
three build artifacts and four copies of every shard bundle; telemetry: 24 GiB of builds and
the profile's `work_bytes`; each plus four `archive_bytes` and one `history_bytes`) must fit
`max_bytes`, and the root's filesystem must have that reserve available to the compute account
now. `max_bytes` is a bound, not space: a 100 GiB bound on a nearly full filesystem is refused,
and a large filesystem never lets the reserve exceed `max_bytes`. A refusal prints the bytes
held, the reserve, their sum, `max_bytes`, what the bound leaves and the filesystem's available
bytes, so a ready preflight is refused at the claim only when the space was taken in between;
the original then stays queued, and nothing is pruned to make room. A
telemetry or profile original is admitted in time the same way, before any claim or build:
the executor's 30-second reserve, 600 seconds of source retention, each telemetry build's
whole 4,800-second cap (or the parent plan's `build_minutes`), 240 seconds of scratch
preparation and cleanup, and the telemetry profile's `timeout_minutes` (or the diagnostic
`total_minutes`) must fit `run_seconds`; the refusal names every step. The `manual` telemetry
profile (240 minutes) needs 24,870 seconds, so `run_seconds = 3600` is refused while 25,200
fits within the 8-hour ceiling. Execution checks the same bounds again after its sources:
too little time left ends the original `failed` with measurement `not_executed` before either
build, nothing measured and no fence kept; an unexpected or unconfirmed cleanup still fences.

Submit telemetry with `--work telemetry` and a new key, then `cycle`. Each side is compiled
through `telemetrybuild`, each execution is contained, the exact configured overhead,
invocation-cost and read-latency settings are used, and the actual installed matching verifier
admits raw documents. A failed or partial native telemetry result can still be a completed
measurement. `--work profile` similarly creates a separate native diagnostic child under the
same exclusion. With `diagnostic.cases` it diagnoses those exact instance IDs from `cli.py
list`. With `cases = []` it diagnoses what a verified parent comparison found: submit it with
`--parent REQUEST_ID` naming a completed local core original of the same exact sources. The
parent is verified offline (`verify`), and its retained plan and `report/verdict.json` are
frozen into the profile (`request.json` `parent`, `parent-verdict.json`): the native selection
takes its regressed cases first, then inconclusive ones, at most `max_cases` (default 3, at
most 8), and records every selected and skipped case with its reason in the profile report.
It cannot alter core verdict samples, and its instrumented timings never enter a verdict.

CPU-profile recipes need the explicitly configured host tools: `diagnostic.perf` (the perf
executable) and `diagnostic.inferno_bin` (the directory holding the pinned Inferno 0.12.8
`inferno-collapse-perf`, `inferno-diff-folded` and `inferno-flamegraph`), canonical absolute
paths, never searched for on `PATH`. Install Inferno from its registry pin as root, for example
`cargo install --locked --version 0.12.8 --root /opt/af-perf/tools/inferno-0.12.8 inferno`, and
leave the directory unwritable by the compute account. Submission and execution, before the
first build, refuse a profile whose recipe tools are missing or not executable.

`diagnostic.observer` chooses where perf runs. Unset, perf runs inside the contained child,
which holds no capability: a kernel that denies every unprivileged perf event
(`kernel.perf_event_paranoid` above 2) refuses the profile before any build, and Docker's
default seccomp profile refuses `perf_event_open` to a container without `CAP_PERFMON`. With
`observer = "host_perf"`, a CPU-profile recipe uses the trusted host observer instead: the
candidate keeps its unchanged container (network none, every capability dropped, no new
privileges, read-only root), and the configured perf runs on the host as the compute account,
under the same authority supervisor, deadline, output bounds and machine exclusion as every
diagnostic tool. Each contained command starts behind a private gate FIFO. The observer finds
the one process of the owned container waiting at that gate (its cgroup names the container's
full ID, its ancestry reaches the container's init, it runs as the compute account), freezes
its host PID and start time, attaches `perf record -p PID` disabled, enables it (a CLI command,
recorded whole) or leaves it disabled until the probe's acknowledged region (a native probe),
waits for perf's acknowledgement, checks the frozen identity again (a recycled PID is refused)
and only then opens the gate. A command that exits in milliseconds is recorded from its first
instruction, and no Docker client or waiting-shell time is attributed to AF. perf leaves when
its target does; its end is confirmed before the container's removal is, and either one
unconfirmed keeps the machine fence. Each perf is admitted for its own bounded lifetime: the
observed command's own bound (a CLI case's `sample_timeout_ms`, a probe's tool timeout) plus
10 seconds for the acknowledgement and 30 for its settlement, never more than the
supervisor's 120-second per-run cap and never past the diagnostic deadline; it is never lent
the whole remaining diagnostic window, which the supervisor refuses. When the attachment fails
after the contained command started (perf refused, the target missing or changed), the command
is cancelled through its supervisor: a cancellation the supervisor confirms releases the
command's scratch and the capture records the observer's own reason, and one it does not
confirm is `cleanup_uncertain` with that reason first, the scratch kept and the fence held.
Each recording keeps an `af.perf-host-observer/1` record
(`observer-*.json`) beside its capture. A host-observed CLI command's region time is its exec to
its exit in that trusted recording (the kernel's exec `COMM` and `EXIT` records of the frozen
process), never the supervised child's wall, which includes the attachment and the wait at the
gate; a recording that does not hold both records stops the capture as
`command_boundary_unknown`, its files kept, and nothing of it is attributed. A CLI capture
lists every repetition's recording and observer record, in order with the region time each
contributed, in its `af.perf-repetitions/1` index (`repetitions.json`), which `profiles.json`
binds by size and SHA-256; admission refuses a missing, changed or unlisted repetition, and
decodes each host-observed recording again to check its region time. Each recording is decoded
by its own `perf script --no-inline`, in that order, under the tool's own time bound and the diagnostic
deadline, and a decoding that fails names its recording and keeps the earlier ones' output.
Every capture, and every capability proof, decodes the same way: function-level stacks, with
the same fields, samples, losses and unknown frames, where an inlined callee is counted in the
function it was inlined into and has no frame of its own. Expanding inline frames cost 4.7
seconds per real fresh-process recording against 0.09 seconds without, which the hundreds of
recordings a CLI capture needs cannot afford within the diagnostic budget. The report says the
stacks are function-level and never shows inline attribution; the retained recordings and
symbols keep it available, by decoding them again with plain `perf script` outside the capture.
The build IDs with hits come the same way from every recording, each listed by its own
`perf buildid-list --with-hits` in that order under the same bounds, so a short first execution
with no AF sample never hides a later one's hit; a listing that fails or is malformed names its
recording and leaves the capture unrendered, every recording kept. When the profiled
executable carries a build ID and no recording hit it, the capture is omitted as
`executable_not_sampled`, every recording kept and nothing rendered from it: what was sampled
is not that executable's code, and `profiles.json` refuses such a capture without its
omission, so a child claiming it usable is not admitted. A musl executable carries no build ID
(`binary_build_id_present` is null): its capture is bound by the executable's exact hash and
retained symbol archive instead, and no build ID is invented for it. perf is never bound into a candidate container: the
host observer runs `perf script`, `perf buildid-list` and Inferno on the host too, reading the
admitted AF binary at the same path the container saw. The capability is checked by doing it,
not from a path or a sysctl: `preflight`, `comparison-preflight` and the profile's execution,
before any build, attach this perf to an owned gated loop and decode its samples; a refusal
names perf's own reason. The executor never changes a sysctl, grants a capability or runs a
privileged container.

Operator setup for the host observer (once, as root, reviewed): install the pinned perf
outside every candidate path, owned by root, group the compute account's group, mode 0750,
with only `cap_perfmon` (DWARF unwinding also reads the target's stack, which the same
capability allows for a process of the same account):

```sh
install -o root -g af-perf-exec -m 0750 perf /opt/af-perf/tools/perf-7.0.14/bin/perf
setcap cap_perfmon=ep /opt/af-perf/tools/perf-7.0.14/bin/perf
getcap /opt/af-perf/tools/perf-7.0.14/bin/perf
```

Then configure `perf = "/opt/af-perf/tools/perf-7.0.14/bin/perf"`,
`inferno_bin = "/opt/af-perf/tools/inferno-0.12.8/bin"` and `observer = "host_perf"` under
`[diagnostic]`. Leave `kernel.perf_event_paranoid` as it is. The compute account already has
Docker access; the observer reads `docker inspect` of its own containers only.

A request has `requests/exp_<opaque>/request.json`, its frozen configuration/plan and
`state.json`. After completion `result.json` binds actual backend, persistent-machine environment,
producer/source identities, observed host/CPU/Python and every retained evidence file by hash.
A core result also names each side's built `af` in `binaries` (see Nightly originals).
Native build manifests hold actual image/tool identities. `sources.git` retains original
commit/tree/blob objects, independent of later mirror GC. Reports are at:

| Work | Retained report/evidence |
| --- | --- |
| Core | `requests/ID/run/`, `requests/ID/report/report.html`, `requests/ID/history/history.html`; shared `core-archive/` |
| Telemetry | `requests/ID/run/report.html`, raw documents and builds; shared `local-telemetry-history/history.html` |
| Profile | `requests/ID/profile/` and its native profile report, separate from verdict samples; `requests/ID/parent-verdict.json` for a parent-selected profile; `observer-*.json` beside each host-observed capture |
| Comparison | `comparisons/CMP_ID/comparison.json`, `plan.json`, `aggregate/` (its run, report and `aggregate.json`); refusals in `comparison-refusals/` |
| Failed/lost controller | Original request and state diagnostic, plus any bounded native partial evidence and recovery paths |

Verify an original locally, without Actions artifacts or network transport:

```sh
python3 -I -B /opt/af-perf/control/scripts/performance/cli.py executor \
  --config /etc/af-perf/executor.toml verify --request-id "$REQUEST_ID"
```

A full release comparison is one durable native lifecycle (below). For a smoke or one
family, combine chosen originals into one overview from their retained native state:

```sh
python3 -I -B /opt/af-perf/control/scripts/performance/cli.py executor \
  --config /etc/af-perf/executor.toml overview --scope full --member "$CORE_ID" \
  --member "$TELEMETRY_ID" --member "$TRACE_ID" --member "$CPU_ID" --output "$OVERVIEW_DIR"
```

It writes a new directory with `overview.json` (`af.perf-comparison-overview/1`),
`overview.md` and `overview.html`, prints the JSON and exits 0 only for a `complete`
comparison, 4 otherwise. Each original keeps four separate judgments: its state with its
terminal detail; its verification, recomputed now by `verify` (`verified`, `refused`, or
`not_terminal` for a queued, claimed, failed, interrupted or uncertain one); its coverage
(core: planned, matched, failed and missing records; telemetry: planned, run, failed and not-run
runs); and its performance verdict with the regressed cases. `--scope` is the operator's declaration
of what the overview compares. Under `full` it is `complete` only when a core, a telemetry, a
trace profile and a CPU profile original of the same exact sources are all complete, verified
and fully covered, the core aggregate result is not `incomplete` (a missing closing health
sentinel or an unstable environment leaves even a fully observed shard ineligible), and every
profile's `--parent` is a core original of the same overview; every other state is `incomplete`
with every reason listed. A `partial` scope, for a smoke or one family, is at best `partial`,
never `complete`. A complete comparison may still hold a real regression, and a refused or
partial original never contributes coverage or a verdict. Links point at the native reports in
the journal, which stays the evidence; copy the overview directory together with the journal
root (`requests/`) it links into, so its relative links keep working.

`status`, `verify`, `overview`, `comparison-status`, `comparison-report` and
`comparison-export` take no journal lock: they read the retained, atomically published state
while a `cycle` holding the journal's exclusive lock executes an original, so a running
comparison can be watched natively. `submit`, `preflight`, `cycle` and the other `comparison-*`
operations still take the lock and report busy when another controller holds it.

Core admission rehashes retained objects, plans and raw evidence and recomputes the native
comparison. Telemetry's matching native verifier is also used at execution/import. Keep the
whole original directory for later admission; archived summaries alone do not replace it.
Local core environment identities differ from hosted profiles. Local telemetry history has its
own root, bound to local request receipts; do not import hosted history as local or copy local
points into a hosted cohort. No local result supplies a passing required check.

## Periodic operation and explicit events

Install the example [source refresh service](../scripts/performance/service/af-perf-source-refresh.service)
and [timer](../scripts/performance/service/af-perf-source-refresh.timer), and the
[executor service](../scripts/performance/service/af-perf-executor.service.in) and
[timer](../scripts/performance/service/af-perf-executor.timer.in). Pin the paths and accounts. Install the two `.in` templates as
`/etc/systemd/system/af-perf-executor@.service` and `af-perf-executor@.timer`; the source
filenames deliberately keep the protected inventory's canonical path alphabet.
Enable source refresh, then `af-perf-executor@core.timer`; optionally enable the telemetry
instance. Each invocation handles at most one queued experiment. The native machine lock
excludes both instances and all manual/core/telemetry/profile/calibration paths. systemd also
keeps one active invocation per instance. Resource/time/log bounds are explicit in the units.
There is no installed Actions runner.

With `poll_main=true`, `service --work core` discovers the latest fetched reviewed main tip
and compares it to its first parent, once per source identity. The telemetry instance submits
that same pair to its separate experiment family. This polls newly observed tips; it does
not claim coverage of intermediate commits that arrived between polls, PR events or releases.
No remote dispatch occurs for local work. The installed control stays pinned as main advances.

For exact PR work use `submit --kind pr --base BASE --head MERGE --pr-head PR_HEAD --key KEY`.
The trusted operator must supply GitHub's exact immutable merge and ordered parent identities;
the controller verifies the Git objects. For a release use `--kind release --tag TAG` and exact
`--base`/`--head`; the existing release/predecessor resolver must agree. Published-release
eligibility is an explicit trusted submission here; the existing hosted release handoff remains
unchanged. This controller does not automatically watch PR metadata or assert a GitHub check.
A nightly uses `--kind nightly` under its own configuration, below.

A journal command can report busy while another holds the journal for a long experiment.
Retry a rejected submission later with the same key. Already queued items persist. The synced journal index detects deleted or unindexed originals;
missing reservations cannot be silently replaced. Global
machine contention returns `busy` before a claim, so the next timer may try that unstarted item.
After a claim there is no retry. Cancellation/deadline/controller loss records or recovers an
uncertain original, and the sibling `/tmp/af-perf-machine.lock.fence` prevents every new launch.
Torn `.pending` receipts also refuse automatic recovery. Never clear these for progress.

Recovery is an operator inspection of the retained request, owned container names, cgroups,
mounts and scratch/build paths. Confirm **all** owned descendants stopped and backing mounts
safe, preserve failed evidence, then settle only the empty sibling fence while holding the
same machine lock through operator maintenance. Do not delete/recreate the lock inode, alter
candidate permissions automatically or rerun the original. A new experiment needs a new key;
a calibration original is never backfilled. This implementation supplies no unsafe automatic
fence-reset command.

## Validate an exact candidate producer

A producer that is not yet accepted main is validated beside production, never in place of it:
its own frozen checkout, its own source mirror and trusted ref, its own configuration and
journal, the machine's one shared lock, and the ordinary schedules paused for the duration.
Production's `/opt/af-perf/control`, `/etc/af-perf/executor.toml`, mirror and journal (and
every original in it) are not touched. The operator's layout: the candidate producer at
`/opt/af-perf/candidates/$SHA`, a separate bare mirror whose
`refs/heads/perf-lab-stability-20261029` admits that SHA, and a candidate data root. `SHA` is
the full 40-hexadecimal producer commit; the paths below are that layout, and the commands are
the operator's to run, not something this change ran.

1. Freeze the candidate checkout, as root, from the exact commit (never a moving branch), with
   no bytecode caches, root-owned and not writable by compute:

   ```sh
   SHA=0123456789abcdef0123456789abcdef01234567   # the full producer SHA, replaced
   install -d -o root -g root -m 0755 /opt/af-perf/candidates
   git -C /var/lib/af-perf-executor/source.git cat-file -e "$SHA^{commit}"
   git -C /var/lib/af-perf-executor/source.git archive --format=tar "$SHA" \
     | (install -d -o root -g root -m 0755 "/opt/af-perf/candidates/$SHA" \
        && tar -x -C "/opt/af-perf/candidates/$SHA" --no-same-owner)
   chmod -R go-w "/opt/af-perf/candidates/$SHA"
   find "/opt/af-perf/candidates/$SHA" -name __pycache__ -prune -print   # must print nothing
   ```

2. Give the candidate its own bare mirror, owned by `af-perf-source` and readable by
   `af-perf-readers`, credential-free like production's, with exactly one trusted ref whose
   ancestry admits the SHA (the controller requires `control_sha` to be an ancestor of
   `trusted_ref`), plus the release objects the comparison measures:

   ```sh
   sudo -u af-perf-source git init --bare /var/lib/af-perf-candidate/source.git
   sudo -u af-perf-source git -C /var/lib/af-perf-candidate/source.git fetch --no-tags \
     /var/lib/af-perf-executor/source.git \
     "$SHA:refs/heads/perf-lab-stability-20261029" \
     "refs/tags/v0.13.0:refs/tags/v0.13.0" "refs/tags/v0.14.0-rc.1:refs/tags/v0.14.0-rc.1"
   sudo -u af-perf-source git -C /var/lib/af-perf-candidate/source.git merge-base --is-ancestor \
     "$SHA" refs/heads/perf-lab-stability-20261029 && echo admitted
   ```

   The controller trusts exactly the configured `repository_path` (`source_repository`'s exact
   `safe.directory` for that one mirror): add no wildcard or global Git trust, and keep every
   credential with the source account, none in compute's environment or the mirror.

3. Write a private configuration, `/etc/af-perf/candidate-$SHA.toml` (root-owned, 0644, no
   group or other write): a copy of production's `/etc/af-perf/executor.toml` (the same
   images, `machine_id`, `zigbuild_bin`, byte caps, `max_requests`, `max_queue`,
   `run_seconds` and `cycle_seconds`, none raised) with exactly the keys below replaced. They
   give the candidate its own fresh data root, journal and mirror, and they freeze the full
   release comparison's workload instead of whatever production runs: production's `tier`
   (`quick` in the example, and the `release` tier holds only 11 cases), `shards` and
   telemetry selection are not that workload. A full comparison measures the whole `nightly`
   family on the shipping musl target in 8 smaller serial shards, and the `manual` telemetry
   profile's three experiments on both sides over 2 rounds. The cases, pairs and warmups are
   the tier's own; the shard count is smaller, and `[core.windows]` declares the 45-minute
   shard sampling window and the 120-minute run deadline of each serial partition, the windows
   the recipe was measured in. The `nightly` tier's own windows are 15 and 180 minutes
   ([ADR-0266](adr/0266-measure-nightlies-as-originals-of-the-independent-executor.md)) and stay
   the ordinary nightly's; a 15-minute window cannot hold this recipe's largest shard (440
   records, which took 30 minutes 52 seconds, all valid, in the operator's run of the
   preliminary PR #473 producer `cbd8661c`, evidence of that producer only; the frozen producer
   `6d8208d`'s shard 0 sealed complete, 440 of 440 records, none failed or unstarted, in
   2032.2 seconds, 33 minutes 52.2 seconds, evidence of that producer only). The tier's own 4 shards
   did not fit either: on the v0.13.0 against v0.14.0-rc.1 pair, a measured first shard of 776 slots
   stopped at 738 inside its unchanged 45-minute window
   ([verification record](performance-local-executor-verification.md#eight-serial-shards-for-the-full-release-comparison-2026-10-30)).
   Smaller shards are an adaptation to that measurement, not a prediction: the preflight checks
   the schedule's worst-case bounds, never how fast a host samples, so a shard can still stop
   at its window on other hardware. The suite's `release` tier (11 cases) and production's own
   configuration stay unchanged:

   <!-- full-comparison-configuration -->
   ```toml
   root = "/var/lib/af-perf-candidate/journal"
   repository_path = "/var/lib/af-perf-candidate/source.git"
   control_sha = "<the full SHA>"
   trusted_ref = "refs/heads/perf-lab-stability-20261029"
   journal_id = "claw_candidate_<first 12 of the SHA>"
   poll_main = false

   [core]
   tier = "nightly"
   environment = "linux_x86_64_musl_claw"
   shards = 8
   cases = []

   [core.windows]
   shard_sampling_minutes = 45
   run_total_minutes = 120

   [telemetry]
   profile = "manual"
   experiments = ["overhead", "invocation_cost", "read_latency"]
   rounds = 2
   ```
   <!-- /full-comparison-configuration -->

   Delete any `pairs`, `scale` or `corpus_runs` line under `[telemetry]`: an override is a
   narrower workload, and its result is another question. Keep production's `zigbuild_bin`
   (the musl target refuses a configuration without it) and its `[diagnostic]` CPU-profile
   tools with `observer = "host_perf"`; the comparison derives each diagnostic lane's recipe
   (`linux_musl_trace`, `linux_musl_cpu_profile`) and selected cases itself. The `root` must
   not exist yet: a fresh journal holds no earlier original under a comparison's keys.

   Create the journal's parent as compute's own, 0700, under the candidate data root:
   `install -d -o af-perf-exec -g af-perf-exec -m 0700 /var/lib/af-perf-candidate`. Scope the
   reviewed `hostprep` sudo policy for loop images and mounts to that root only (a copy of
   production's rule naming `/var/lib/af-perf-candidate/journal`), and remove that rule when the
   validation ends.

4. Keep one exclusion and pause the ordinary schedules. The candidate uses the same
   `/tmp/af-perf-machine.lock` and `/etc/af-perf-machine-lock.json`: provision nothing new.
   Stop the timers and let an executing invocation finish, then confirm nothing holds the lock
   and no fence is present:

   ```sh
   systemctl stop af-perf-executor@core.timer af-perf-executor@telemetry.timer \
     af-perf-source-refresh.timer af-perf-telemetry-collect.timer
   systemctl is-active af-perf-executor@core.service af-perf-executor@telemetry.service
   test ! -e /tmp/af-perf-machine.lock.fence && echo no-fence
   ```

5. Run the validation as the compute account from the candidate checkout, so its installed
   authority is the checkout `control_sha` names. Every command is bounded by the
   configuration's `cycle_seconds` and `run_seconds`, and the comparison by its partitions:

   ```sh
   CAND="python3 -I -B /opt/af-perf/candidates/$SHA/scripts/performance/cli.py executor \
     --config /etc/af-perf/candidate-$SHA.toml"
   $CAND comparison-preflight --kind release --tag "$TAG" --base "$BASE" --head "$HEAD" --key rel_1
   $CAND comparison-launch --kind release --tag "$TAG" --base "$BASE" --head "$HEAD" --key rel_1
   # Check the frozen plan and telemetry workload below before anything executes.
   $CAND comparison-run --comparison "$CMP_ID"     # repeat until it exits 0
   $CAND comparison-status --comparison "$CMP_ID"
   $CAND comparison-export --comparison "$CMP_ID" --output "$BUNDLE_DIR"
   python3 -I -B "/opt/af-perf/candidates/$SHA/scripts/performance/cli.py" executor \
     comparison-verify --bundle "$BUNDLE_DIR"
   ```

   `BASE` and `HEAD` are the exact full SHAs of `v0.13.0` and `v0.14.0-rc.1` in the candidate
   mirror (`git -C /var/lib/af-perf-candidate/source.git rev-parse "v0.13.0^{commit}"`), and
   `TAG` is `v0.14.0-rc.1`. `comparison-launch` retains the frozen plan and queues the
   partitions and the telemetry original; nothing runs until `comparison-run`, since the
   timers are stopped. Before it, check that the retained plan is the whole nightly family
   (33 case instances, 31 judged and 2 exploratory, 2932 scheduled slots in 8 shards: the 2852
   of the tier's 4 shards and the health sentinels of 4 more), that it froze the declared 45
   sampling and 120 run minutes, that the declaration partitions its shards into
   `[[0, 1], [2, 3], [4, 5], [6, 7]]` where every later partition reuses the first one's
   build, and
   that the telemetry original's frozen configuration measures 12 runs (3 experiments x 2
   sides x 2 rounds), with the candidate's own installed code:

   <!-- full-comparison-checks -->
   ```sh
   python3 -I -B - "/opt/af-perf/candidates/$SHA/scripts/performance" \
     /var/lib/af-perf-candidate/journal "$CMP_ID" <<'PYCODE'
   import os, sys
   sys.path.insert(0, sys.argv[1])
   from perflab import contracts, schedule
   root, ident = sys.argv[2], sys.argv[3]
   def read(*parts):
       with open(os.path.join(root, *parts), "rb") as stream:
           return stream.read()
   doc = contracts.read_json(read("comparisons", ident, "comparison.json"), "comparison")
   plan = contracts.read_json(read("comparisons", ident, "plan.json"), "run")
   cases = plan["cases"]
   found = (plan["tier"], plan["profile"]["environment_id"], len(plan["shards"]), len(cases),
            sum(not case["exploratory"] for case in cases),
            sum(case["exploratory"] for case in cases),
            sum(len(schedule.expected_slots(plan, shard["index"])) for shard in plan["shards"]))
   print("plan: tier, environment, shards, instances, judged, exploratory, slots =", found)
   assert found == ("nightly", "linux_x86_64_musl_claw", 8, 33, 31, 2, 2932), found
   windows = (plan["limits"]["shard_sampling_minutes"], plan["limits"]["run_total_minutes"])
   parts = [item["shards"] for item in doc["partitions"]]
   print("windows: sampling, run total minutes =", windows, "partitions =", parts)
   assert windows == (45, 120) and parts == [[0, 1], [2, 3], [4, 5], [6, 7]], (windows, parts)
   assert all(item["build_from"] == doc["partitions"][0]["request_id"]
              for item in doc["partitions"][1:]), doc["partitions"]
   config = contracts.read_json(read("requests", doc["telemetry"]["request_id"],
                                     "configuration.json"), "executor")["telemetry"]
   runs = 2 * config["rounds"] * len(config["experiments"])
   print("telemetry: profile, experiments, rounds, runs =",
         (config["profile"], config["experiments"], config["rounds"], runs))
   assert (config["profile"], runs) == ("manual", 12) and all(
       config.get(name) is None for name in ("pairs", "scale", "corpus_runs")), config
   PYCODE
   ```
   <!-- /full-comparison-checks -->

   A failed check means the configuration is not the full comparison: run nothing, and
   discard that candidate journal (it holds only queued originals of this comparison) before
   a corrected configuration with a new `root` and `journal_id`. The launch's `schedule`
   partitions the 8 shards into 4 serial core originals of 2 shards each; the first builds and
   the later ones reuse its immutable build. After the comparison ends, its report's telemetry
   lane states the same coverage, `required` 12.

6. Restore the ordinary services only after the candidate's cleanup is confirmed: every
   original terminal and none `uncertain` in `comparison-status`, no
   `/tmp/af-perf-machine.lock.fence`, and no owned container, mount or loop device of the
   candidate root left (`docker ps --all`, `findmnt`, `losetup --list`). An uncertain cleanup is
   recovered as above before anything else runs. Then remove the candidate's sudo rule and
   start the timers again:

   ```sh
   systemctl start af-perf-source-refresh.timer af-perf-executor@core.timer \
     af-perf-executor@telemetry.timer af-perf-telemetry-collect.timer
   ```

   Keep the candidate journal and bundle as evidence beside production's; neither is imported
   into production's journal, history or cohort.

## Validate a release comparison

The operator runs these on the VM as the compute account after installing a candidate control;
nothing here was executed by the change that documents it. `CLI` stands for
`python3 -I -B /opt/af-perf/control/scripts/performance/cli.py executor --config
/etc/af-perf/executor.toml`; `BASE`, `HEAD` and `TAG` are the exact full release SHAs and tag
(for example v0.13.0 against v0.14.0-rc.1), and every key is new. Configure `core.cases = []`
(the whole family of the configured tier), keep `core.shards` at the tier's count unless a
measured run showed its shards do not fit their window, and
configure the CPU-profile tools and `observer = "host_perf"` as above. The comparison
measures the configured tier's family, not a fixed one: the full release comparison of the
whole nightly family (33 case instances, 8 serial shards, the 12 manual telemetry runs) uses the
configuration and checks of [Validate an exact candidate producer](#validate-an-exact-candidate-producer)
steps 3 and 5, on its own fresh journal.

```sh
CLI comparison-preflight --kind release --tag "$TAG" --base "$BASE" --head "$HEAD" --key rel_1
CLI comparison-launch --kind release --tag "$TAG" --base "$BASE" --head "$HEAD" --key rel_1
CLI comparison-run --comparison "$CMP_ID"     # repeat until it exits 0; resumable
CLI comparison-status --comparison "$CMP_ID"  # usable while another command executes
CLI comparison-report --comparison "$CMP_ID" --output "$REPORT_DIR"
CLI comparison-export --comparison "$CMP_ID" --output "$BUNDLE_DIR"
python3 -I -B /opt/af-perf/control/scripts/performance/cli.py executor comparison-verify \
  --bundle "$BUNDLE_DIR"
```

`comparison-preflight` prints `af.perf-comparison-preflight/1` and exits 0 when `ready`, 4 with
the first refusal; it reserves nothing, and every refusal (also of `comparison-launch`) is kept
under the journal's `comparison-refusals/`. It declares the comparison (`af.perf-comparison/1`,
its ID `cmp_…`): the exact source pair, one frozen plan of the whole core family, the serial
partition of that plan's shards, the telemetry original and the trace and CPU profile lanes
with the recipe each must use for the plan's target (the shipping musl parent uses
`linux_musl_trace` and `linux_musl_cpu_profile`; no GNU recipe is substituted). It checks every
member as the compute account: each partition's submission, the telemetry configuration, the
diagnostic tools and the host observer's actual capability. A lane that has no recipe for the
target, missing tools or an observer that cannot record refuses the whole comparison.

The partition makes the whole family feasible without relaxing anything: each core original
runs a sequence of the plan's shards, each in its unchanged sampling window with its own health
sentinels, under its own unchanged run deadline. The first partition builds (the plan's
`build_minutes`); every later one runs on that partition's verified, immutable build after a
600-second reuse reserve, so every shard bundle binds one build. A later partition does not
inherit the first one's run deadline: its run bundle seals its own execution authority
(`partitions.json`, `af.perf-partition-execution/1`, [ADR-0269](adr/0269-bind-comparison-partition-execution-to-reused-build-artifacts.md)),
the plan's frozen `run_total_minutes` from its own execution start, bound to the reused
build's exact `builds.json` and its recorded run, which stays unchanged provenance. The runner
reads its shard's window from it and refuses, before anything starts, an authority for another
plan or build, a widened window, one starting before the build ended, or a shard it does not
declare; the original's `run_seconds` still tightens every shard. The aggregate carries every
later partition's authority, and its evaluation checks each shard against its own partition's
window and that no partition's shards overlap a later one's. The preflight's `schedule`
names the shards per original and each partition's worst case; for release, and for the full
comparison recipe's declared `[core.windows]`, 2 shards per original (20 + 2 × 45 + 0.5 of 120
minutes, then 10 + 2 × 45 + 0.5). A plan whose single shard
window cannot fit one original is refused, never split further.

`comparison-launch` retains the declaration and frozen plan under `comparisons/CMP_ID/`, then
submits the partitions in order and the telemetry original. `comparison-run` drives them:
it advances and runs one `cycle` per queued original, reopening the journal each time, until
nothing is queued; its exit is not the comparison's result. When every partition has ended,
`comparison-advance` (which `comparison-run` calls) assembles the declared shards of the
verified partitions into one run of the frozen plan and evaluates it once
(`aggregate/aggregate.json`, `af.perf-comparison-aggregate/1`, with its report and the shared
`core-archive/`): the whole family's denominator, a failed, missing or uncertain partition's
records missing, never a falsely complete result. The aggregate's verdict then selects the
regressed, then inconclusive cases (at most `max_cases`) that the trace and CPU profile
originals diagnose; a profile's parent is the aggregate. A verdict with nothing to diagnose
leaves those lanes `not_applicable`, named in the report. Every original keeps its own record:
a failed or uncertain one is never retried, and a lost controller makes a claimed original
`uncertain` with the fence kept. A termination signal (SIGTERM, SIGINT) to the controller during
a shard cancels the whole controller, not only that shard: the shard seals its `interrupted`
record (even when the signal lands in its final slot and every slot was recorded: the completed
samples are kept, never counted missing, yet the shard is not complete), no later shard of the
original and no later original starts, and the executor removes
the original's scratch. With that cleanup and every owned descendant confirmed, the original
ends `interrupted` with its partial evidence and the fence settles; otherwise it is `uncertain`
and the fence stays. Either way the original is never replayed. A shard run alone, outside a
controller, still just ends `interrupted`.

`comparison-report` writes `report.json` (`af.perf-comparison-report/1`), `report.md` and
`report.html`, prints the JSON and exits 0 only for a `complete` comparison, 4 otherwise. It
verifies every complete original offline now. `complete` needs the core aggregate over every
partition and not `incomplete` (a missing closing sentinel or an unstable environment leaves
it ineligible), every telemetry run measured, and for every case the aggregate verdict
selects a baseline and a candidate trace without omission whose region coverage is complete (a
missing side, an omitted side, a `partial` or a setup-only `region_missing` trace leaves the
trace lane failed with its reason) and a differential flamegraph, and every original verified.
Each profile's own selection must be exactly the one the aggregate's verdict derives (mode,
limit, parent verdict digest, cases and reasons), and its parent receipt the aggregate's
result, plan and verdict; a profile of another case or another aggregate never completes a
lane. Completeness, evidence
verification, the performance verdict and calibration eligibility (never, for a release
comparison) are separate: a complete comparison may hold a real regression. `comparison-status`
prints the same document from durable state without verifying, so it is never `complete`.

`comparison-export` writes a new self-contained bundle: the report, with its links rewritten
into the bundle, and below `evidence/` every native record it names at its journal path (the
declaration, plan, the whole aggregate with its run and report, each original's request,
state, result and configuration, each core original's plan, run bundle and report with its
evidence, the telemetry original's whole run with its manifest, raw documents and logs, and
every profile's plan, evidence and frozen parent verdict), with `manifest.json`
(`af.perf-comparison-export/1`) hashing every file, bounded by `archive_bytes`. An export
whose pages, nested native reports included, link anything it does not hold is refused, never
published. Copy the directory anywhere; `comparison-verify --bundle` needs no journal or
configuration and refuses a changed, missing or extra file, a link of any bundled page that
leaves the bundle or names nothing in it, or a result that is not the one the report names.
It then verifies the native evidence itself: every bundled file of an original must be one
its sealed terminal result names with that hash, each core run (and the aggregate's) is
evaluated again offline and must reproduce its bundled verdict, the telemetry result is
verified again under the installed measurement authority, and each profile's children are
admitted again. Finally it derives the report again from the bundle alone, as
`comparison-report` derives it from the journal: the members the bundled declaration names,
each original's state bound to its sealed result (an original the bundle does not hold is not
submitted), the aggregate's partitions, run, result and coverage recomputed from those
originals and its verdict, and every lane's state, coverage and diagnostic selection. A report
that says anything else is refused, however its hashes and manifest were rewritten: a failed
or missing original relabelled complete, a dropped member, a passed verdict over a
regression, a one-sided, partial or setup-only trace of a selected case, or valid diagnostic
children of a case the aggregate did not select with every receipt resealed. An honest incomplete or running bundle verifies, as what it is. That checks what
the originals sealed; it is not a new performance acceptance. Run it from the trusted control installation that produced the bundle, or a
checkout of the same measurement authority: the telemetry result is refused under any other.

Repeat the comparison with new keys to see run-to-run spread. A same-source A/A comparison
uses `--base "$HEAD" --head "$HEAD"` with a new key; an injected slowdown and the A/A
calibration with qualification use the local cohort below, never an ad hoc run. Telemetry
read latency is judged only over its declared 10,000 corpus runs: a `corpus_runs = 200` result
is valid evidence but `not_judged`, and qualifying it needs `corpus_runs = 10000` that fits the
profile's timeout. Keep the historical failed report unchanged beside new evidence.

Telemetry overhead and invocation-cost runs select each side's fixture contract from the
version its `af --version` prints: v0.13.0 measures under `check-receipt@1` (the same packages
declaring `af/TaskCheckReceipt@1`, re-pinned to the digests the contract declares and its own
`af catalog digest` prints), v0.14.0-rc.1 through v0.14.0-rc.3 under the committed
`check-receipt@2`; a release no contract names (v0.11.9, say) is refused, never measured under
the committed contract. The documents (`af/telemetry-overhead@2`,
`af/telemetry-invocation-cost@2`) record the contract and every package pin.

A submission admits the selected releases' declared workloads before anything is reserved,
claimed or built, from the release each side's retained source declares in its Cargo.lock: a
telemetry original needs a fixture contract of the installed bench script for both sides, and a
core original with CLI cases needs the authority's CLI registry and an adapter of it for both
sides (`af_cli_v1` from 0.12.0-rc.3 on) whose command for every selected case and parameters is
declared and runnable: `af_cli_v1` declares both `tui_open` fixtures pending, so a 0.12 side
with a browser case is refused with that reason while its discovery commands stay admitted. Every
core original also needs, for both sides, a native probe adapter of the authority's
`fixtures/performance/adapters.toml` (`af.perf-adapters/2`) that declares the side's release
(`min_version`, 0.12.0-rc.3 for `native_probe_public_api_v1`) and serves the health
sentinel's contract and every selected library case's (and its seed's): a release no adapter
declares (0.11.9, say) is refused even with library cases only. A pair either refuses is refused at `preflight`,
`submit` and `comparison-preflight`, never at its build. The same admission runs the configured
sample image's own `/usr/bin/python3` once, in a probe container started as every bounded
container is (this account, no network, a read-only root, every capability dropped, no new
privileges, its removal confirmed) under the machine's one exclusion: a sample image without
`/bin/sh`, that interpreter or the PTY driver's modules, or a container that is not so contained
or has no cgroup v2 namespace, refuses before any claim or build. The build still chooses a
side's probe adapter by the first that compiles, but only after the admission above has
established its declared coverage: a compile never stands in for it. A comparison member
already in the journal under its key must have frozen the comparison's very configuration and,
for a core partition, its frozen plan: a narrower same-key original (another telemetry profile
or experiment set, partial core cases) refuses the comparison before any new claim, and
`comparison-status`, `comparison-report` and `comparison-export` mark such an original refused
and the comparison incomplete, never complete over its own run list. Their drain latency runs from the harness's last write to the command's observed
exit on `CLOCK_MONOTONIC` (`harness_last_write_to_command_exit`), never from file timestamps.

The CLI oracle declares each release's own contract from its version (`fixtures/performance/cli.toml`):
`af review report --format json` is `af/review-report@4` through the 0.13 releases
(`af_cli_v1`, `af_cli_v2`) and `af/review-report@5` from 0.14.0-rc.1 (`af_cli_v3`), and
neither accepts the other. Every rule still checks all 100 Findings (identity, status,
severity, location, distinct keys and the summary tally) over the whole document. A CLI
sample keeps stdout within its unchanged `sample_log_bytes` total less a 16 KiB stderr reserve
(245,760 of 262,144 bytes), which holds 0.14.0-rc.1's 234,442-byte report whole; an output
past it is truncated and refused as truncated, never parsed as a prefix. A diagnostic child run
of a CLI case asks the supervisor for the same split: 245,760 bytes of stdout within a 262,144-byte
total log, never that stdout plus another tool's 64 KiB stderr allowance (other diagnostic
tools keep it, under the unchanged 1 MiB per-run log cap).

The 10,000-event seeds (the native `state_projection` seed and the `task show` and Campaign CLI
seeds) are written by each side's own probe through the same protected public writers, but in
a bounded tmpfs at `/perf/prep` (512 MiB, owned by the compute user, inside the container's
memory limit; only seed writers get it): their 10,000 commits no longer wait for a flush of the
scratch disk each. The finished Store is then copied, every file flushed, into the seed's
directory, re-read through a fresh Store and judged by the seed's oracle; the probe's stderr
names both paths. Whether this alone brings each writer under its 120-second bound on Claw is
for that run to show: the per-event protected-writer CPU cost is unchanged.

## Nightly originals

Each nightly is measured as one original of a second executor configuration
([ADR-0266](adr/0266-measure-nightlies-as-originals-of-the-independent-executor.md)). It compares
the frozen main commit the nightly was built from with the highest stable `vX.Y.Z` release tag
reachable from that commit, release candidates excluded, as the `nightly` tier of
`fixtures/performance/suite.toml` defines it. Copy the executor example a second time to
`/etc/af-nightly/executor.toml`, with the same owner and mode rules, and change these keys:

```toml
root = "/var/lib/af-perf-executor/nightly-journal" # its own journal, never the quick one's
journal_id = "claw_nightly_1"
build_image = "..." # the verified digest of the image's musl stage
zigbuild_bin = "/opt/af-perf-tools/bin"
poll_main = false # nightlies are submitted, never polled

[core]
tier = "nightly"
environment = "linux_x86_64_musl_claw"
shards = 4
cases = []

[diagnostic]
recipe = "linux_musl_trace"
```

Keep `machine_id`, the source mirror and the installed control of the quick configuration.
Both configurations take the same machine lock, so a nightly and a quick original never run
at the same time. The musl stage of
[`scripts/performance/images/Dockerfile`](../scripts/performance/images/Dockerfile) puts
`cargo-zigbuild` and `zig` together in `/opt/af-perf-tools/bin`; the operator pins that image's
digest by hand.

Submit a night's core and telemetry originals, then run `cycle` against the same configuration:

```sh
python3 -I -B /opt/af-perf/control/scripts/performance/cli.py executor \
  --config /etc/af-nightly/executor.toml submit --work core \
  --kind nightly --tag "$NIGHTLY_TAG" --head "$MAIN_SHA"
python3 -I -B /opt/af-perf/control/scripts/performance/cli.py executor \
  --config /etc/af-nightly/executor.toml submit --work telemetry \
  --kind nightly --tag "$NIGHTLY_TAG" --head "$MAIN_SHA"
python3 -I -B /opt/af-perf/control/scripts/performance/cli.py executor \
  --config /etc/af-nightly/executor.toml cycle
```

`--tag` must be `nightly-YYYY.MM.DD`, a real calendar date, and there is no `--key`: the key is
`nightly:<tag>:<work>`. `--head` is resolved to one commit once, and both the derived baseline
and the request use that commit. Without `--base` the baseline is derived as above, and the
request's `submission.baseline` records the derived tag and commit; an explicit `--base` is used
as given and recorded as `null`. No reachable stable tag refuses the submission. Submitting the same
night again with the same arguments prints the existing original; another `--head` under the
same tag is refused as a reused key. `--work profile` is refused. A core nightly is refused
unless the configuration's `core.tier` is `nightly`, naming the tier it found, so the quick
configuration cannot take one by mistake.

Shards still run one after another. A core nightly is one build, then four shards of at most
15 sampling minutes each. The plan freezes the tier's 180-minute run deadline, counted from the
start of the build. The build has five bounded steps, the supervisor and each side's `af` and
probe, each within the suite's 20 build minutes: 100 build minutes at worst, then four shards
of 90 seconds of preflight, 15 sampling minutes and a 30-second finish reserve, 68 minutes, make
168, and the other 12 cover the dependency fetch and the archive. (The sampling is a quarter of
a quick shard's per schedule: a deadline counted from the first shard would give it back, a
follow-up.) 180 minutes is the hard maximum of a nightly's run deadline; the hard maxima
in `scripts/performance/perflab/limits.py` are unchanged. A build that runs long leaves the last
shard less time: a shard cut by the deadline, or not started, stays incomplete, and its missing
slots stay visible. Expect about three hours plus source retention and the report;
`run_seconds = 28800` bounds the whole original.

`verify --request-id` prints the result. For a core original, `binaries.baseline` and
`binaries.candidate` hold the `sha256` and `bytes` of each side's built `af`, as the run's
`run/builds.json` names them; `verify` refuses a result whose `binaries` differ from it. If
`binaries.candidate.sha256` equals the SHA-256 of the `af` inside the nightly archive shipped
from the same commit, the measured candidate is that archive's binary; if not, the measurement
used another build of the same source. A side that built no `af` is `null`. Telemetry, profile,
hosted and non-executed results carry `binaries: null`; telemetry builds its own binaries.

## Local calibration declaration and review checkpoint

Declare only after this producer is accepted and committed, installed exactly, and the VM's
actual CPU/image tuple has been observed. Keep the hosted v7 declaration and its histories
byte-identical. Use `core.cases=[]`, the intended tier, local environment, same machine and
journal IDs and exact configured images throughout the new local cohort.

```sh
python3 -I -B /opt/af-perf/control/scripts/performance/cli.py executor \
  --config /etc/af-perf/executor.toml cohort-declare \
  --cohort-id "$NEW_LOCAL_COHORT" --start-date "$FUTURE_UTC_DATE" --days 7 \
  --group local_cpu "$OBSERVED_CPU_MODEL" "$SAMPLE_IMAGE_DIGEST_REFERENCE" \
  --output local-cohort.json
```

Review and commit the exact declaration at `fixtures/performance/local-cohort.json` on main,
then refresh the source mirror. The separate local shape binds the original journal, machine,
environment and producer before any execution. There is no Actions history anchor or run ID.
Submit due members with `executor ... calibration-dispatch --cohort local-cohort.json`, then
run bounded `cycle` invocations until the day's queue drains. Automate those same two commands
with an operator-owned timer for the finite dates; disable ordinary main polling during the
cohort to avoid consuming its daily window. Off-schedule originals, failed/lost claims and
missing evidence remain in the declared denominators. Never retry/backfill originals.

After all 20 A/A and preparation originals, compute the seal:

```sh
python3 -I -B /opt/af-perf/control/scripts/performance/cli.py executor \
  --config /etc/af-perf/executor.toml calibration-prepare \
  --cohort local-cohort.json --output local-calibration-parameters.json
```

Commit and independently review it at `fixtures/performance/local-calibration-parameters.json`
on main **before** submitting any slowdown trial. Refresh the mirror. Dispatch later members
with `--parameters local-calibration-parameters.json`. The controller checks the reviewed
bytes, and every trial request freezes their hash. If review misses the fixed dates, stop:
retain the declaration/originals and seek a new declaration; never shift this one's schedule.
Compute `executor ... readiness --cohort local-cohort.json --parameters
local-calibration-parameters.json --output local-readiness.json` after the finite cohort.
Readiness replays originals and recomputes sealed parameters with the shared statistical code:
seven days minimum, 20 A/A, preparation, 30 descriptive trials and ten whole power trials,
all critical cases, unchanged thresholds and dose bands. A readiness output is not activation.
Hosted declaration/dispatch/readiness commands and their original Actions receipts stay supported.

## Switch back to GitHub

Stop the old hosted dispatch timers before enabling independent compute. Drain in-flight
originals, retain their histories, and check there is no uncertain machine fence. The existing
hosted PR/nightly/release event workflows remain available; this local configuration does not
disable their triggers. To avoid duplicate scheduled comparisons, explicitly disable the
superseded operator timers and coordinate any GitHub scheduled workflow automation with the
operator. Keeping hosted PR/release validation is a separate intentional selection.

To switch the operator controller back, stop its timers and drain its local queue. Set
`backend="github_actions"`; use the hosted environment/configuration and remove telemetry
scale/corpus overrides and any `[core.windows]` table, which the existing workflow cannot
express (unsupported settings refuse).
Supply the token only to the trusted controller/`gh` transport and resume one producer.
The same journal deduplicates a submission key across defaults; queued requests retain their
original backend/settings. Hosted dispatch returns `submitted`, not a fabricated measurement:
ordinary hosted collectors preserve the real run/attempt artifacts and receipts. Ambiguous
POST outcomes become uncertain and are never blindly resent. Historical local results remain
local, and separately declared hosted calibration continues through its existing dispatcher.

## Evidence boundary

The [focused verification note](performance-local-executor-verification.md) records exact
implementation checks and their results.

Focused tests exercise real process contention, Git retention, claims, crash/ambiguity,
configuration, native reports/archives and calibration arithmetic with injected computation.
They do not establish Docker/Claw operation, a live daemon cleanup proof, actual performance,
calibration qualification or rollout. The coordinator must run authentic core and telemetry
experiments, prove live contention/cleanup on the exact VM, verify/export artifact hashes and
reports, and choose the final backend after acceptance and the full native gates/reviews.
