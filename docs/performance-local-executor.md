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
The configuration is `af.perf-executor/2`; misspellings and obsolete v1 configurations refuse.
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
`af.perf-executor-request/2` and `af.perf-executor-result/3`; request/1 and result/2 originals
already in a journal stay readable by `status` and `verify`
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
Serialization does not enlarge it: a large run may be incomplete and its missing slots remain
visible. Builds, preparation and measurement all hold the same machine exclusion.

Submit telemetry with `--work telemetry` and a new key, then `cycle`. Each side is compiled
through `telemetrybuild`, each execution is contained, the exact configured overhead,
invocation-cost and read-latency settings are used, and the actual installed matching verifier
admits raw documents. A failed or partial native telemetry result can still be a completed
measurement. `--work profile` similarly creates a separate native diagnostic child under the
same exclusion, using `diagnostic.cases` as exact instance IDs from `cli.py list`. It cannot
alter core verdict samples. Tool unavailability remains an explicit diagnostic omission.

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
| Profile | `requests/ID/profile/` and its native profile report, separate from verdict samples |
| Failed/lost controller | Original request and state diagnostic, plus any bounded native partial evidence and recovery paths |

Verify an original locally, without Actions artifacts or network transport:

```sh
python3 -I -B /opt/af-perf/control/scripts/performance/cli.py executor \
  --config /etc/af-perf/executor.toml verify --request-id "$REQUEST_ID"
```

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
30 sampling minutes each. The plan freezes the tier's 180-minute run deadline, counted from the
start of the build. The build compiles the baseline and the candidate, each step within the
suite's 20 build minutes: 2 × 20 build minutes and 4 × 30 sampling minutes make 160 minutes,
and the other 20 cover each shard's preflight and finish reserve, the supervisor and the
dependency fetch. 180 minutes is the hard maximum of a nightly's run deadline; the hard maxima
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
scale/corpus overrides the existing workflow cannot express (unsupported settings refuse).
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
