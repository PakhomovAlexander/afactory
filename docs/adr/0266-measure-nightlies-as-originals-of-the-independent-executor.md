# ADR-0266: Measure nightlies as originals of the independent executor

**Status:** proposed (2026-10-29).

## Context

The owner's nightly train (decided 2026-10-29) measures every nightly on the Claw VM through
the independent executor of
[ADR-0240](0240-execute-performance-experiments-independently-on-a-locked-vm.md). The `nightly`
tier of `fixtures/performance/suite.toml` compares the frozen main commit a nightly was built
from with the highest stable release reachable from it (`gitrefs.nightly_baseline`), on profile
`linux_x86_64_musl_claw`. The executor accepted only `--kind manual|pr|release`: a nightly had
to be submitted as a manual experiment with a hand-chosen baseline and key, nothing stopped the
quick-tier executor on the VM from taking it, and a result did not say which binaries it
measured. The musl build image also kept cargo-zigbuild in cargo's bin directory and zig in
`/usr/local/bin`, so no single `zigbuild_bin` could name both.

## Considered options

1. Keep submitting nightlies as manual originals. Rejected: the baseline rule, the key and the
   tier would rest on the operator's care, and a resubmission under a new key would measure the
   same night twice.
2. Run nightly shards concurrently to fit the tier's budget. Rejected: the lab's machine lock
   refuses concurrent shards on purpose (ADR-0240).
3. Raise the nightly run deadline above 180 minutes so that longer serial shards fit after the
   build. Rejected: 180 minutes is the hard maximum of a non-quick tier's run deadline, and the
   hard maxima stay as they are.
4. Add a nightly kind with its own rules to the executor, cut the nightly tier to fit serial
   shards inside the hard maxima, and name the measured binaries in the result. Chosen.

## Decision

`cli.py executor … submit --kind nightly --tag nightly-YYYY.MM.DD --head SHA [--base SHA]`
accepts `--work core` and `--work telemetry` and refuses `--work profile`, a tag that is not
`nightly-` and a calendar date, and an explicit `--key`. `--head` is resolved to one commit
once; the baseline is derived from that commit and the request freezes the same one, so a ref
that moves during the submission cannot split them. Without `--base` the baseline is
`gitrefs.nightly_baseline(git, head)`: the highest stable `vX.Y.Z` tag reachable from `head`,
release candidates excluded; no such tag refuses. A core nightly is refused unless the
configuration's `core.tier` is `nightly`, with a message naming the tier found. The nightly
runs under a second configuration with its own root and `journal_id`, tier `nightly`,
`environment = "linux_x86_64_musl_claw"`, `shards = 4` and `zigbuild_bin`, under the same
machine lock. Shards stay serial and the shard runner is unchanged.

The nightly tier (`[tiers.nightly]` in `fixtures/performance/suite.toml`) is cut to fit those
serial shards inside the hard maxima: `shards = 4`, `shard_sampling_minutes = 30`,
`run_total_minutes = 180`, `concurrent_jobs` unchanged. The run deadline starts with the build
(ADR-0170), so it holds the build as well as the shards. A core nightly performs one build
before its shards, which compiles the authority's small supervisor and two sides: the stable
baseline and the frozen candidate, whose commits differ, so the candidate never shares the
baseline's build. Each build step is bounded by the suite's `build_minutes = 20`. Counting each
side's build at that bound, 2 × 20 build minutes and 4 × 30 sampling minutes make 160 minutes,
which leaves 20 of the 180 for each shard's preflight and 30-second finish reserve, the
supervisor compile and the dependency fetch. A build that runs longer leaves the last shard
less time: the shard runner stops a shard at the run deadline and starts none after it, and
the missing slots stay incomplete, never a pass. The hard maxima in
`scripts/performance/perflab/limits.py` are unchanged: 180 minutes is
`LONG_RUN_TOTAL_MINUTES_MAX`, 30 sampling minutes is below `LONG_SHARD_SAMPLING_MINUTES_MAX`
(90), and the committed suite/4, run/7, shard/4 and builds/6 schemas already admit both. The
release tier and every other tier keep their numbers.

The submission is `{"kind": "nightly", "key": "nightly:<tag>:<work>", "tag": …,
"baseline": {"tag": …, "sha": …} | null}`, `null` when the operator gave `--base`. An original's
identity is the digest of its kind, key and work alone, as it already was for every other kind,
so the same night submitted again returns the existing original, and another head or derived
baseline under the same tag is refused as a reused key. This shape is
`af.perf-executor-request/2`, a closed union by `kind` whose other variants keep
`{"kind", "key"}`; the reader checks that the key is the tag and work and that a derived
baseline names the frozen `baseline_sha`.

`af.perf-executor-result/3` adds `binaries`: for a completed local core original, the `sha256`
and `bytes` of each side's built `af` as its `af.perf-builds` record names them, a side without
one as `null`; every other result has `binaries: null`. `verify` refuses a result/3 core
original whose `binaries` differ from its retained `run/builds.json`. A report compares
`binaries.candidate.sha256` with the `af` of the nightly archive shipped from the same commit
to say whether the measured candidate is that archive's binary.

New originals are written as request/2 and result/3 only. The previous request/1 and result/2
stay readable, with their committed schemas, because an executor journal is append-only: its
index, state and result receipts bind each original by digest, an original is never rewritten,
and refusing them would make `status` and `verify` fail on every original the VM already holds.
The producer check is ADR-0240's and unchanged: `verify` still refuses an original written under
another installed control, so a journal is finished or discarded with its own control before an
upgrade, as before. Request/1 originals keep their identities, so their keys still deduplicate.
Every older version, and every newer one, still fails closed (ADR-0145); nothing writes an
earlier version.

The musl stage of `scripts/performance/images/Dockerfile` adds `/opt/af-perf-tools/bin` holding
`cargo-zigbuild` and `zig` as symbolic links and checks both versions through it, so
`zigbuild_bin = "/opt/af-perf-tools/bin"` names both. The existing paths stay. The operator
re-pins the image digest by hand; no committed configuration's digest changes.

## Consequences

The [operator guide](../performance-local-executor.md#nightly-originals) gains the second
configuration, the submit lines, the derived baseline, the duration and how to read
`binaries`. A core nightly takes at most the tier's 180 minutes from the start of its build,
plus source retention before it and the report and archive after it; a shard the deadline cuts
stays incomplete and visible as missing slots, and `run_seconds` (28800 in the installed
configuration) bounds the whole original. Each nightly shard samples for at most 30 minutes,
so pairs that do not fit in that window stay unstarted and the shard incomplete. Like PR and
release originals, a nightly plan is frozen with the planner's manual trigger; the request's
submission carries what makes it a nightly.

Existing manual, main, PR, release and calibration submissions keep their key, identity and
rules; only the version of the request document they are written as changes. The nightly
build, the daily report, publishing, systemd units for the second configuration, the suite's
other budgets and the hosted nightly workflows are unchanged.

Lab tests (`scripts/performance/tests/test_executor.py`, `test_contracts.py`) prove the
submission rules through the real CLI, journal and a Git history with `v0.13.0`, `v0.14.0-rc.3`,
an unreachable `v0.14.0` and a nightly tag; one head when `--head` names a ref that moves after
the baseline is derived; the tier's budget on a plan frozen from it, run through the real local
path with synthetic builds and shards on a stepped clock: the build fixes the 180-minute
deadline, and after two 20-minute builds the shard runner's own deadline check admits four
shards with their whole 30 sampling minutes, ending at 168 minutes, while a fifth does not fit
and is refused at the deadline; the request and result fixtures; `verify` and `status` on a
request/1 and result/2 original under the same installed control; and the Dockerfile's tool
directory, read as text.
They run no Docker, network or benchmark.
