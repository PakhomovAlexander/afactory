# Calibration cohort dispatcher

`scripts/performance/cohort-dispatch.py` is the product operator entry point. It retains the
captured Claw `/opt/af-performance/dispatch.py` interface: import the file, set `module.ROOT`
to `state/calibration`, set `module.LAB` to `state`, then call `module.main()` with the process's
`--dry-run` argument. The captured source's SHA256 was
`217991cfa12e24edfa08546449bafafadc2423e3e8026e89d76a02831b2c218f` (audit source `c96e340`);
the repaired file has different bytes. This repair performs no installation or live API access.

## Replacement declaration: quick_20261028_hosted_v7

[ADR-0225](adr/0225-declare-the-replacement-hosted-cohort-from-the-accepted-producer.md)
records the operational replacement under ADR-0181/0182/0184. The frozen
`cohort-quick-20261023-hosted-v6.json` remains unchanged, SHA-256
`62955096a58517dad3544425ead9b18ebe3b85de94b8518d5ce85deb761d5ab7`.
Its malformed groups and mismatched producer make it invalid for dispatch. Keep it disabled;
preserve its history, archive, reader, seeds and every missing/failed original. Never repin,
rewrite, retry, backfill or replace its members.

The new declaration is
[`fixtures/performance/cohort-quick-20261028-hosted-v7.json`](../fixtures/performance/cohort-quick-20261028-hosted-v7.json).
Its canonical digest is
`sha256:71e342c386e44f4efc878fef0fd8b55a10fef0a5eaf11ff0ed9b02d7fd039f88`;
its exact file SHA-256 is
`4f975367d9f27855a1318d3b18a0b0845efa26dd259fa85fe555ce45ae536308`.
This is a declaration only: no service installation, dispatch, passed measurements, sealed
parameters, readiness or enforcement is asserted.

### Producer and supplied observations

Authority `508bf45967b8bdcbfaeaae9de27f5258672a617c`, tree
`ee13c8cd0323e853e98ad930e5d84fc20017751a`, retains the exact accepted output of native Task
`performance-lab-postmerge-perf-cache-repair-20261026`, result
`sha256:ab31221ad9b02ca3061f4216105d63ea4520026ba5a9dfa5492578aa63b07525`.
That Task accepted the implementation-reviewed output Snapshot
`sha256:511dbbaee8a2a35d1cdca2eacfdcc26b84198390dbd539364a3054fb3bb742e5`, Manifest
`sha256:6f416776aa45c0d79b77531d672a5ee84a34cceff2d0fd11aac4bc7b3c3e23c9`.
The retained commit's entire blob bytes and modes were mapped to that accepted output by the
coordinator. This is not a separate whole-source verification Task of the newly created Git
identity. Earlier unsatisfied continuation input is not acceptance evidence.

The Worker verified the public read-only archive
`/var/lib/af-task-performance/dependencies/producer-authority-perf-cache-20261026/producer-authority.tar.gz`:
8,505,279 bytes, SHA-256
`55143a808427e0529d1a7116acf24748212720ca41a3fd59cebeba50f4a4f57f`.
Before extraction under `target/cohort-validation/authority/`, all 14 entries were checked as
bounded ordinary files/directories beneath `producer.git` (8,512,441 expanded bytes), with no
links, special files or escaping paths. Git verification checked HEAD/tree, all 3,024 objects,
the single shallow commit, and absence of remotes, hooks, alternates and ancestor objects;
`git fsck --full --strict` passed. Runtime, workflows and `.af/` bytes/modes match the producer.

The Task supplied a complete fresh history observation for `PakhomovAlexander/afactory`,
`performance.yml`, at `2026-10-26T17:50:12.704103+00:00`: total 118, latest run number 118.
Its evidence file is `/var/lib/af-task-performance/ga/logs/workflow-history-20261026T175012Z.json`,
SHA-256 `8e14c99e76bd64afe940feed30cb56eddb50c8c17138348b96436075a00f7fbc`.
This supplies `after_run_number=118`; every later run, including source-mismatch refusals,
stays in subsequent complete histories. The Worker used this supplied capture, without a live
API call or opening the coordinator's history file.

The supplied verified shard observation is from `2026-10-24T20:15:26.993549+00:00`, archive
`/var/lib/af-performance/archive`, raw observation SHA-256
`8be3a5bc86291d84d3f944218287a8276414cc1bf686afe887ce8a8c34289a44`.
All 655 verified shards use the full image
`ghcr.io/pakhomovalexander/afactory-perf-gnu@sha256:26b0c708b9cdffdeb4731da33161f148b419af3c9e656ae9dc51d318215ae8b2`.
The exact tuples below are the supplied observations, not new Worker measurements or a claim
that every case has prepared coverage. Each archive index identifies its run bundle.

| Group ID | Exact observed CPU model | Shards | Example index / shard / Actions run |
| --- | --- | ---: | --- |
| `intel_xeon_8573c` | `INTEL(R) XEON(R) PLATINUM 8573C` | 50 | `000000000003.json` / `shards/0/shard.json` / `39633795685` |
| `amd_epyc_7763` | `AMD EPYC 7763 64-Core Processor` | 312 | `000000000003.json` / `shards/1/shard.json` / `39633795685` |
| `amd_epyc_9v74` | `AMD EPYC 9V74 80-Core Processor` | 130 | `000000000006.json` / `shards/0/shard.json` / `39637277881` |
| `amd_epyc_9v45` | `AMD EPYC 9V45 96-Core Processor` | 128 | `000000000008.json` / `shards/0/shard.json` / `39646860626` |
| `intel_xeon_6973p_c` | `Intel(R) Xeon(R) 6973P-C` | 21 | `000000000014.json` / `shards/0/shard.json` / `39659018121` |
| `intel_xeon_8370c` | `Intel(R) Xeon(R) Platinum 8370C CPU @ 2.80GHz` | 14 | `000000000019.json` / `shards/4/shard.json` / `39663723586` |

### Exact declaration command

Run from the source root with Python 3.14, after verifying and safely extracting the named
archive above. The existing CLI reads the registry from the extracted authority repository;
each group supplies three separate arguments, preserving CPU `@` and full OCI image identity.

```sh
python3 -B scripts/performance/cli.py cohort-declare \
  --repo target/cohort-validation/authority/producer.git \
  --authority 508bf45967b8bdcbfaeaae9de27f5258672a617c \
  --cohort-id quick_20261028_hosted_v7 \
  --tier quick --environment linux_x86_64_gnu_hosted --recipe linux_gnu_native_release \
  --group 'intel_xeon_8573c' 'INTEL(R) XEON(R) PLATINUM 8573C' 'ghcr.io/pakhomovalexander/afactory-perf-gnu@sha256:26b0c708b9cdffdeb4731da33161f148b419af3c9e656ae9dc51d318215ae8b2' \
  --group 'amd_epyc_7763' 'AMD EPYC 7763 64-Core Processor' 'ghcr.io/pakhomovalexander/afactory-perf-gnu@sha256:26b0c708b9cdffdeb4731da33161f148b419af3c9e656ae9dc51d318215ae8b2' \
  --group 'amd_epyc_9v74' 'AMD EPYC 9V74 80-Core Processor' 'ghcr.io/pakhomovalexander/afactory-perf-gnu@sha256:26b0c708b9cdffdeb4731da33161f148b419af3c9e656ae9dc51d318215ae8b2' \
  --group 'amd_epyc_9v45' 'AMD EPYC 9V45 96-Core Processor' 'ghcr.io/pakhomovalexander/afactory-perf-gnu@sha256:26b0c708b9cdffdeb4731da33161f148b419af3c9e656ae9dc51d318215ae8b2' \
  --group 'intel_xeon_6973p_c' 'Intel(R) Xeon(R) 6973P-C' 'ghcr.io/pakhomovalexander/afactory-perf-gnu@sha256:26b0c708b9cdffdeb4731da33161f148b419af3c9e656ae9dc51d318215ae8b2' \
  --group 'intel_xeon_8370c' 'Intel(R) Xeon(R) Platinum 8370C CPU @ 2.80GHz' 'ghcr.io/pakhomovalexander/afactory-perf-gnu@sha256:26b0c708b9cdffdeb4731da33161f148b419af3c9e656ae9dc51d318215ae8b2' \
  --start-date 2026-10-28 --days 7 --preparation-members 16 --preparation-iterations 1000000 \
  --repository PakhomovAlexander/afactory --branch main --after-run-number 118 \
  --output fixtures/performance/cohort-quick-20261028-hosted-v7.json
```

Re-running this command to scratch reproduced the declaration byte-for-byte. Typed admission,
case/pair counts, deterministic new-identity seeds, group matching and formulas are checked
against the unchanged producer. The accepted registry supplies policy digest
`sha256:81b54f4b13d6a8568a87c4b0f7dbc531ce2c6a27ac9511c6530c156d4d6ac1ab`;
this is its current identity, not the old cohort's policy digest. The statistical settings below
remain identical to v6. The quick registry fixes 28 critical cases, K=28: eight cases
use N=40 pairs and twenty use N=20 (720 measured pairs per member). Health remains provisional
`sentinel_start_end_v1`, five start/end pairs, 64 batch repetitions, floor 1 and both maximum
ratios 0.2. Preparation uses 1,000,000 iterations per case. There are ten descriptive injection
members at each of 2%, 5% and 10%, and ten whole power members. No policy is retuned.

### Seven-day UTC schedule and sealing checkpoint

| Day | UTC date | Members |
| ---: | --- | --- |
| 0 | 2026-10-28 | 10 A/A: odd `aa_01` through `aa_19` |
| 1 | 2026-10-29 | 10 A/A: even `aa_02` through `aa_20` |
| 2 | 2026-10-30 | 16 preparation: `prep_01` through `prep_16` |
| 3 | 2026-10-31 | 9 descriptive + 3 power: suffixes 01, 05, 09 |
| 4 | 2026-11-01 | 9 descriptive + 3 power: suffixes 02, 06, 10 |
| 5 | 2026-11-02 | 6 descriptive + 2 power: suffixes 03, 07 |
| 6 | 2026-11-03 | 6 descriptive + 2 power: suffixes 04, 08 |

Trial suffixes apply separately to `inj2`, `inj5`, `inj10` and `power`. All 76 seeds are derived
with the existing `member_seed(quick_20261028_hosted_v7, member_id)`, not copied from v6.

**Separate checkpoint:** after the 20 A/A and 16 preparation originals, collect their complete
original artifacts, run `calibration-prepare`, then seal, commit and review the parameters and
retain their exact digest in the trusted control **before any slowdown or power trial**.
Missing coverage cannot be filled by inventing parameters. No parameters are sealed here.
If review cannot finish before the assigned trial days, keep dispatch disabled; never move
members, backfill days or replace originals. A future operational decision must declare another
cohort if this fixed schedule cannot be met.

The unchanged targets are `e = max(r, a/A0)`, `p = max(1/10, 2*e)` and
`3/4 * p*A0 <= median measured injected time <= 5/4 * p*A0`. Readiness requires all 20 A/A
within tolerance in every critical case and at least nine of ten **whole** power trials each
showing a valid-dose regression in every critical case. Missing, failed, interrupted,
unhealthy and invalid originals stay in every denominator; descriptive tallies never replace
whole-trial qualification. First dispatch, attempt 1, remains the original even if unreadable.

The coordinator must publish with a normal merge retaining the actual producer SHA as an
ancestor. Squash/rebase would invalidate that reachability. Before dispatch, trusted control
must contain the exact cohort and unchanged producer scheduler, and main must retain identical
producer workflow/runtime bytes. Install and inspect disabled preflight separately as below;
this declaration authorizes no host changes or activation.

## Install exact reviewed bytes, leaving dispatch disabled

Installation and Claw scheduling belong to the coordinator after exact-source acceptance and
publication. Keep the hourly collector on its existing
pinned lane and preserve archived runs and failed collections. Do not enable or temporarily
start a real dispatch timer for a preflight. Use a separate dispatcher state root if the
existing collector also reads `collector-manifest.json`; do not repoint the collector's manifest.
The layout below is compatible with the wrapper's existing `ROOT` and `LAB` assignments.

From a trusted checkout, set `PRODUCER` to the full committed repair SHA and `STATE` to the
operator's absolute, dispatcher-only state directory. Export the complete installed library,
including vendored Tomli, from that exact commit; never import a mutable worktree. These local
staging commands do not dispatch or enable a timer:

```sh
PRODUCER=508bf45967b8bdcbfaeaae9de27f5258672a617c
STATE=/opt/af-performance/cohort-state
export PRODUCER STATE
python3 -B - <<'PY'
import hashlib, json, os, pathlib, re, subprocess
sha = os.environ['PRODUCER']
assert re.fullmatch('[0-9a-f]{40}', sha)
state = pathlib.Path(os.environ['STATE'])
assert state.is_absolute()
paths = subprocess.check_output([
    'git', 'ls-tree', '-r', '--name-only', sha, '--', 'scripts/performance/perflab'
]).decode().splitlines()
files = []
contents = {}
for path in sorted(paths):
    relative = path.removeprefix('scripts/performance/')
    data = subprocess.check_output(['git', 'show', sha + ':' + path])
    contents[relative] = data
    files.append([relative, hashlib.sha256(data).hexdigest()])
assert files
identity = hashlib.sha256(json.dumps(files, separators=(',', ':')).encode()).hexdigest()
folder = state / 'collector' / identity
assert not folder.exists()
folder.mkdir(parents=True)
for relative, data in contents.items():
    target = folder / relative
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(data)
manifest = state / 'collector-manifest.json'
assert not manifest.exists(), 'preserve an existing pin; review installation separately'
manifest.write_text(json.dumps({'collector_sha256': identity, 'files': files}, indent=2) + '\n')
(state / 'calibration' / 'cohorts').mkdir(parents=True)
operator = subprocess.check_output([
    'git', 'show', sha + ':scripts/performance/cohort-dispatch.py'])
(state / 'cohort-dispatch.py').write_bytes(operator)
print('operator SHA256', hashlib.sha256(operator).hexdigest())
print('installed library', identity)
PY
```

Review the emitted file hashes and manifest. Install `STATE/cohort-dispatch.py` as the deployed
`/opt/af-performance/dispatch.py` only after comparing its bytes with
`git show "$PRODUCER:scripts/performance/cohort-dispatch.py"`. Keep the existing service wrapper's
import-and-call interface; bind its `state` directory to the dispatcher-only `STATE` above.
The wrapper and installed file must be operator-owned. To install only the verified entry point:

```sh
git show "$PRODUCER:scripts/performance/cohort-dispatch.py" | cmp - "$STATE/cohort-dispatch.py"
install -m 0644 "$STATE/cohort-dispatch.py" /opt/af-performance/dispatch.py
```

The wrapper must still set `ROOT` and `LAB` before calling `main()`; invoking the deployed file
directly would use its own directory for state. This is a local deployment step,
not a change to any GitHub workflow, scheduler or collector pin.

Copy the new committed cohort byte-for-byte into
`STATE/calibration/cohorts/COHORT_ID/cohort.json`. Write `STATE/calibration/config.json` with:

```json
{
  "schema": "af.perf-local-cohort-dispatcher/1",
  "enabled": false,
  "cohort_id": "NEW_COHORT_ID",
  "cohort_digest": "sha256:CANONICAL_DIGEST_PRINTED_BY_COHORT_DECLARE",
  "cohort_path": "fixtures/performance/NEW_COHORT_FILE.json",
  "control_sha": "FULL_MERGED_CONTROL_SHA",
  "workflow_sha256": "SHA256_OF_EXACT_AUTHORITY_WORKFLOW_BYTES"
}
```

Substitute reviewed values, not these placeholders. Compute `workflow_sha256` from
`git show "$PRODUCER:.github/workflows/performance.yml" | sha256sum`; do not re-pin it to a
changed workflow. The control must contain the exact cohort and authority's scheduler tree.
For slowdown trials only, add `calibration_path` and `calibration_digest` and copy the exact
previously sealed parameters to that cohort directory's `parameters.json`; the control must
contain those bytes too. A path alone cannot bypass typed parameter validation.

## Preflight and dispatch behavior

Run the existing service wrapper manually with `--dry-run`, preserving its exit status. A
disabled, configured operator performs genuine bounded credential acquisition and read-only
history, merged-control, cohort, parameter and producer checks. It writes `runs.json`,
`status.json` and the lock, but never a dispatch intent or POST. `would_dispatch` identifies
the due member and checked `dispatch_head`; scheduling refusals such as `no_member_due`,
`utc_day_closing` and `sealed_parameters_required` remain explicit. Guard failures return
`refused` and exit 4. A normal disabled cycle remains a no-op. Keep `enabled: false` throughout
this repair's deployment review; future enabling is a separate operator decision.

The operator reads credentials through the bounded `gh auth token` call; tokens stay out of
status, journals, source and benchmark processes. Its installed library is verified against the
bounded manifest before import. One lock excludes concurrent cycles. History and ancestry are
bounded to sixteen pages and the API collector to 120 seconds. No member is sent at or after
22:00 UTC, backfilled on another day, retried or replaced automatically.

An enabled cycle fsyncs intent and its directory before exactly one POST to `ref: main` with
`dispatch_head` set to the full main SHA whose producer was checked. The final main GET is an
early refusal, not an atomic dispatch guarantee. Main can still move after it. At a different
SHA, the repaired workflow names the run `af-perf source-mismatch`, which binds no member, and
fails before checkout, planning, build or measurement. It also checks the same binding inside
`ci-schedule`. Full SHA hex case is immaterial; missing, short and non-hex values are refused.
Dispatching to a raw SHA is not assumed supported.

A source-mismatch run stays in the retained history. The operator's intent stays latched as
`dispatch_uncertain:MEMBER` (exit 4), because no matching original appeared. An ambiguous POST
response likewise leaves its intent; a successful CLI response records only `submitted`, not
member completion. Inspect these conditions with the operator; never reset the journal or
silently resend. Future incompatible workflow changes could change the workflow guard itself:
ADR-0184's unchanged producer verification still refuses their evidence. This repair does not
claim an atomic API, eliminate arbitrary future workflow changes, complete calibration, or
activate enforcement.

A retained uncertain dispatch intent does not skip disabled dry-run preflight. Every producer,
control/cohort, workflow, scheduler and final-main guard still runs. Valid preflight reports
`dispatch_uncertain:<member>`; a failing guard reports refusal. Both preserve the intent journal
byte for byte, create no additional intent and send no POST. Enabled non-dry-run cycles keep
the uncertainty latch and never retry it.

## Claw scheduling and retained reports

Claw is the always-on Linux operator host. Keep the existing hourly collection lane and
`/var/lib/af-performance/archive` intact; do not install a periodic collector on this Mac.
The captured dispatcher wrapper is `/opt/af-performance/dispatch.py`; the dispatcher-only
layout in this runbook uses `/opt/af-performance/cohort-state`, with `ROOT=STATE/calibration`
and `LAB=STATE`. Its separate calibration schedule is an operator-managed timer calling that
wrapper, with one locked cycle at a time and the 22:00 UTC cutoff above. This Task installs or
enables no service. Collector/calibration services use UMask 0077.

Inspect `STATE/calibration/status.json` and service exit/log records, plus each
`STATE/calibration/cohorts/quick_20261028_hosted_v7/runs.json` and `dispatches.json` (when intent
exists); keep failures and uncertain intent intact. The collector's configured `--history`
root supplies `history.html` and `archive-status.json`; retain that existing configured root,
rather than guessing a new deployment path. Actions job summaries and retained `perf-report-*`
bundles provide per-run reports. Follow [the CI runbook](performance-ci.md#calibration-cohort)
for offline `calibration-prepare` and `readiness` reports from retained originals; readiness and
a separately reviewed activation remain later operations. No installed timer, successful
collection or measurement is proved by this declaration.
