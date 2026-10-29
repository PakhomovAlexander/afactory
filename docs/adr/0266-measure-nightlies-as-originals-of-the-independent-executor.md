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
3. Add a nightly kind with its own rules to the executor, and name the measured binaries in
   the result. Chosen.

## Decision

`cli.py executor … submit --kind nightly --tag nightly-YYYY.MM.DD --head SHA [--base SHA]`
accepts `--work core` and `--work telemetry` and refuses `--work profile`, a tag that is not
`nightly-` and a calendar date, and an explicit `--key`. Without `--base` the baseline is
`gitrefs.nightly_baseline(git, head)`: the highest stable `vX.Y.Z` tag reachable from `head`,
release candidates excluded; no such tag refuses. A core nightly is refused unless the
configuration's `core.tier` is `nightly`, with a message naming the tier found. The nightly
runs under a second configuration with its own root and `journal_id`, tier `nightly`,
`environment = "linux_x86_64_musl_claw"`, `shards = 4` and `zigbuild_bin`, under the same
machine lock. Shards stay serial and the shard runner is unchanged.

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
and refusing them would make `status` and `verify` fail on every journal the VM already holds.
Request/1 originals keep their identities, so their keys still deduplicate. Every older version,
and every newer one, still fails closed (ADR-0145); nothing writes an earlier version.

The musl stage of `scripts/performance/images/Dockerfile` adds `/opt/af-perf-tools/bin` holding
`cargo-zigbuild` and `zig` as symbolic links and checks both versions through it, so
`zigbuild_bin = "/opt/af-perf-tools/bin"` names both. The existing paths stay. The operator
re-pins the image digest by hand; no committed configuration's digest changes.

## Consequences

The [operator guide](../performance-local-executor.md#nightly-originals) gains the second
configuration, the submit lines, the derived baseline, the duration and how to read
`binaries`. A core nightly is one build, then four serial shards of at most 45 sampling minutes
each, all inside the tier's frozen 120-minute run deadline counted from the build's start; a
shard that deadline cuts stays incomplete and visible as missing slots, and `run_seconds`
(28800 in the installed configuration) bounds the whole original. This change enlarges no
budget. Like PR and release originals, a nightly plan is frozen with the planner's manual
trigger; the request's submission carries what makes it a nightly.

Existing manual, main, PR, release and calibration submissions keep their key, identity and
rules; only the version of the request document they are written as changes. The nightly
build, the daily report, publishing, systemd units for the second configuration, the suite's
budgets and the hosted nightly workflows are unchanged.

Lab tests (`scripts/performance/tests/test_executor.py`, `test_contracts.py`) prove the
submission rules through the real CLI, journal and a Git history with `v0.13.0`, `v0.14.0-rc.3`,
an unreachable `v0.14.0` and a nightly tag; the request and result fixtures; `verify` and
`status` on a request/1 and result/2 original; and the Dockerfile's tool directory, read as text.
They run no Docker, network or benchmark.
