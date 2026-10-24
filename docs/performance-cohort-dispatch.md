# Calibration cohort dispatcher

`scripts/performance/cohort-dispatch.py` is the product operator entry point. It retains the
captured Claw `/opt/af-performance/dispatch.py` interface: import the file, set `module.ROOT`
to `state/calibration`, set `module.LAB` to `state`, then call `module.main()` with the process's
`--dry-run` argument. The captured source's SHA256 was
`217991cfa12e24edfa08546449bafafadc2423e3e8026e89d76a02831b2c218f` (audit source `c96e340`);
the repaired file has different bytes. This repair performs no installation or live API access.

## Declare a new cohort after the repair is committed

The frozen `cohort-quick-20261023-hosted-v6.json` remains unchanged, SHA256
`62955096a58517dad3544425ead9b18ebe3b85de94b8518d5ce85deb761d5ab7`.
Its groups contain the parser defect, and its authority's workflow is different from the
current producer. Re-pinning an operator hash cannot repair either fact. Do not edit its groups,
identity, membership, missing/failed samples or history, or declare a replacement against its
authority during this Task. No old-cohort readiness or completed calibration is claimed.

After review and integration, the coordinator must:

1. Record the full **committed repair SHA** as the new producer authority. Both its
   `.github/workflows/performance.yml` and its complete `scripts/performance` tree must remain
   byte-identical at the dispatch head and trusted control respectively, as ADR-0184 requires.
2. Refresh the complete workflow history through its current newest run. The previously
   recorded `2026-10-23T11:37:26Z` observation (anchor 28, latest 64) is stale. Select the actual
   latest workflow run number as the fresh declaration anchor; retain every later run, including
   source-mismatch refusals. Do not reset any old journal or hide a raced original.
3. Declare a **separate new cohort identity and file**, with a fresh future UTC start date that
   leaves time for review and merging. Use the repaired authority's `cohort-declare` command
   from [the CI runbook](performance-ci.md#calibration-cohort). No replacement is produced here.
4. Declare exact CPU/image tuples from retained shard observations, with separate CLI values,
   for example the observed CPU `AMD EPYC 9V74 80-Core Processor` and full image
   `ghcr.io/pakhomovalexander/afactory-perf-gnu@sha256:26b0c708b9cdffdeb4731da33161f148b419af3c9e656ae9dc51d318215ae8b2`.
   A CPU such as `Intel(R) Xeon(R) Platinum 8370C CPU @ 2.80GHz` stays one quoted argument.
   Verify every allowed tuple against actual retained evidence; do not copy the old six
   malformed groups or infer unobserved CPUs. Different image repository names or digests
   are different groups.
5. Keep the statistical policy unchanged: cases, N/K, seeds, health rule, five-day minimum,
   20 A/A members, preparation, 30 descriptive trials, ten whole power trials and all dose
   limits and denominators. All 20 A/A and at least nine whole power trials must qualify;
   missing, failed and invalid originals still count. Commit and review the declaration
   before any member, then use a merged trusted control containing its exact bytes.

## Install exact reviewed bytes, leaving dispatch disabled

Installation belongs to the coordinator after review. Keep the hourly collector on its existing
pinned lane and preserve archived runs and failed collections. Do not enable or temporarily
start a real dispatch timer for a preflight. Use a separate dispatcher state root if the
existing collector also reads `collector-manifest.json`; do not repoint the collector's manifest.
The layout below is compatible with the wrapper's existing `ROOT` and `LAB` assignments.

From a trusted checkout, set `PRODUCER` to the full committed repair SHA and `STATE` to the
operator's absolute, dispatcher-only state directory. Export the complete installed library,
including vendored Tomli, from that exact commit; never import a mutable worktree. These local
staging commands do not dispatch or enable a timer:

```sh
PRODUCER=FULL_COMMITTED_REPAIR_SHA
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
