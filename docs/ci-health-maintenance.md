# CI health maintenance: hermetic Phase 1

Phase 1 of the [design](design/ci-health-maintenance.md) observes, diagnoses and reports using
offline GitHub-shaped service fixtures. Its native Scenario identifier is `maintenance-ci-health`.
It runs ordinary pinned, read-only structured Tasks. The only adapter effects are scoped
notifications; remedy selection reports intentions. No Ant, principal, timer, publication grant,
rerun operation, quarantine confirmation or release change is installed.

## Evidence and diagnosis

`custom/CiHealthSnapshot@1` carries `af.ci-health-snapshot/1`, and `custom/CiDiagnosis@1` carries
`af.ci-diagnosis/1`. Their JSON Schemas are published under `schemas/` and by
`af schema show af.ci-health-snapshot/1` and `af schema show af.ci-diagnosis/1`.
Snapshots contain at most 100 failures, 20 log references per failure and 64 KiB total JSON.
Unreadable, missing or excess evidence sets `complete = false` with named `omissions`.
Missing source SHA is a refusal; missing run attempt or job identity is an omission, never green.
A failed nightly receipt without an observed failed job also remains incomplete.
Malformed repository, receipt or omission metadata is refused before recurrence state changes;
it cannot use a green job to close an active signature before the enclosing evidence is rejected.

The observer's `collect` path makes at most the declared number of repository-scoped GETs
(maximum 32), each capped at 1 MiB. It binds main and optional open release PR heads, the
observation window and exact run attempts; truncated run/job lists and exhausted requests become
omissions. `OfflineGithub` supplies the hermetic transport. There is no installed live GitHub
transport or paid model evidence in this phase.

The observer hashes canonical JSON arrays with SHA-256. An occurrence names the exact repository,
workflow, run, attempt, job and head SHA. A signature includes `af.ci-fingerprint/1`, repository,
workflow file, job name, platform and normalized fingerprint. The first failing test is selected
lexically; otherwise the first normalized error line is used in adapter order. Normalization
removes ANSI escapes, timestamps, durations, temporary paths, SHAs and labeled run/job numbers,
while retaining test identity, platform, workflow and assertion text. A missing fingerprint uses
an occurrence-only signature and stays unknown.

The private observer journal folds distinct run attempts transactionally. Re-observing an attempt,
including a replacement job ID on that attempt, does not increase recurrence. A different head
retains 14 days of history. A green observation for an active workflow/job/platform/head closes
the signature and resets its counters. Signature and scope history expire after 14 UTC days.
Both service journals cap their databases at 16 MiB (4096 SQLite pages); retained records have
count and byte bounds, and exhaustion refuses work. They never evict deduplication evidence to
create a new allowance. Keep their private state with the owner's bounded service state; native
Scenario state remains in its registered Store under the existing machine Storage Budget.

A diagnosis has exactly one snapshot in `source_artifact_ids`, its occurrence and signature,
source revision, category, rationale and 1–20 evidence references. Native structured acceptance
checks the exact captured snapshot, every identity and every evidence reference. A reference
must name that snapshot or one of the selected occurrence's log artifacts, always with the same
snapshot identity and the selected failure's `head_sha` as its `source_sha`. The snapshot's
top-level `source_sha` records main; a release job can have a different head. The Task receives
the failed head in its captured Selection Requirements, and its diagnosis, evidence and native
settlement observation must name that revision. Selecting or diagnosing main for that release
failure is refused. These identity fields retain their existing version 1 shapes; the correction
is to their cross-document binding. Foreign, malformed, missing or empty evidence refuses
acceptance. The selected occurrence, signature and revision are fixed by a versioned
`af.ci-diagnosis-selection/1` Requirements specification before Provider admission. Another
occurrence of the same signature does not satisfy that selection. The observer's deterministic
malformed-evidence record selects `unknown`; model prose
cannot repair refused evidence. The native `ci_diagnosis` Task specialization records a durable
refusal as `af/scenario-ci-diagnosis@2`, an observation distinct from accepted Task output. It
retains the original refused Task receipt, exact snapshot and captured selection, failed Attempt
and diagnostic IDs, and at most 2,048 characters of each native diagnostic. The deterministic
diagnosis has category `unknown`, the exact snapshot/occurrence/signature/source identities and
a fixed kernel rationale. An oversized malformed answer cannot amplify the diagnostic prefix.
Provider admission, wall, environment, cancellation and integrity failures keep their ordinary
failure or waiting lifecycle; they do not manufacture diagnosis evidence.
Exact pinned infrastructure and transient signatures bypass the
model in the deterministic diagnosis selector.

## Native Scenario fixture

[`fixtures/scenarios/maintenance-ci-health/`](../fixtures/scenarios/maintenance-ci-health/README.md)
contains the native bundle, read-only Task packages, GitHub/service evidence and adapter fixture.
`configure.py` renders a definition from an exact preobserved snapshot and a native Task target.
It neither plans nor executes a Task. The installed `ci_observe_v1` fixture recomputes that
observation and refuses a difference before diagnosis. Incomplete, healthy, exhausted and
missing-fingerprint snapshots render a report-only path without a model dispatch. Exact pinned
infrastructure/transient signatures also bypass the Task and retain a deterministic diagnosis.
The fixture reserves at most one diagnosis Task per Run. Any further occurrences are explicitly
reported as unknown under `diagnosis_reservation_exhausted`. Every observed failure is sent with
its workflow/run/attempt/job identity, job name, platform, conclusion, head SHA, log artifact
references and release-blocking flag. The findings include an explicit `release_alerts` entry for
every release-blocking occurrence, including those that cannot receive a diagnosis. Findings go
out immediately after observation, before diagnosis or remedy selection. They use
`af.ci-health-notification/2`, with batches bounded by the complete serialized adapter request,
including the escaped body, scope and envelope. Every observed failure and omission is retained
within the existing 64 KiB request and Scenario bounds. Version 1 notifications
are refused; `af schema show af.ci-health-notification/2` publishes the current shape.
The quarantine-intent predicate uses the existing read-only inventory resolver and Q2
scan: two distinct Tasks in its seven-day UTC window on the pinned evidence platform. It never
calls `scan --write`, `confirm` or `apply`.

The accepted-output `task_output_field` binding remains available to ordinary Task steps:

```json
{
  "source": "project",
  "value": {"source": "step", "step": "diagnose", "port": "result"},
  "projection": {
    "kind": "task_output_field",
    "port": "diagnosis",
    "schema": "af.ci-diagnosis/1",
    "field": "category"
  }
}
```

Only `category` and `signature_id` of that schema are allowed. The native Host opens the pinned
Task Store and verifies the exact accepted result, singleton output, envelope and snapshot
before recording the admitted payload once as `af/scenario-task-output-field@1`, inside that
Task step's native intent evidence. Conditions and replay check its exact Task/result/port/artifact
binding, canonical envelope identity and closed diagnosis schema without reopening the Task Store. Ordinary Task collection
therefore cannot erase the recorded scalar evidence. Unsupported schemas/fields, missing/non-scalar
values, changed identities and missing or malformed kernel records fail closed. There is no arbitrary JSON path expression.
The fixture declares repository, main ref, optional release selector (`none` or a PR number),
observation start/end and notification route as typed scalar inputs. Every step and completion
condition checks these values against the exact compiled scope; changing an input cannot reinterpret
the captured observation or dispatch an effect under another scope.

The CI health fixture uses the explicit `af/scenario@6` Task specialization
`{"kind":"ci_diagnosis","target":{...}}`, with one attempt, the same native Task plan authority,
reservation, cancellation lease and exact accounting as an ordinary Task. Its `result` port is
an observation carrying `af/scenario-ci-diagnosis@2`. Accepted settlements retain the accepted
Task receipt and canonical admitted output envelope; refused settlements retain the original
unsatisfied receipt and a visibly deterministic unknown. Neither a literal nor an adapter
observation can supply native settlement projections. `ci_diagnosis_field` allows only `category`
and `signature_id`; `ci_diagnosis_refused` returns a boolean. A notification consumes the
`diagnosis_settlement` observation plus the category and signature text ports.
The refused receipt is never bound through the accepted `diagnosis` Task port.

Refusal provenance retains native 26-character alphanumeric Attempt IDs, alongside the SHA-256
identity of each original diagnostic. `af/scenario-ci-diagnosis@2` replaces version 1, which
incorrectly required Attempt IDs to be artifact digests. Version 1 fails closed. Definitions,
journals, Task receipts and the adapter protocol keep their current versions and shapes.

The changed definitions, bundles, pins, Runs, lists and Ant documents use version 6; the journal
uses `af/scenario-store@7`. Their previous versions fail closed. Machine policy remains version 5
and the adapter request protocol remains version 4 because their typed shapes are unchanged.

The fixture sends findings, an additional diagnosis notification when a Task ran, exactly one
remedy-intent notification when a failure was selected, then the outcome. Each
notification names its Scenario Run and snapshot. Native scalar equality checks category and
signature; each row excludes all earlier matching rows. The ordered outcomes are alert-only,
release alert, infrastructure alert, rerun intended, quarantine evidence intended, repair
intended, and report-only. In Phase 1 none dispatches a rerun, repair or quarantine operation.
The shipped Watch condition is false because Phase 1 opens no PR. An isolated lifecycle fixture
sets it true for a fixture-owned open PR and proves all notifications precede the bounded Wait;
resuming that Wait does not repeat diagnosis or notifications.
After a refused diagnosis, the diagnosis, remedy and outcome notifications carry the
deterministic unknown; the earlier findings retain the original observed evidence. Only row 1
(`alert_only`) runs, and Watch is skipped even if the fixture requested it. Reporting does not
rerun, repair, quarantine or publish anything.
Each resume makes one merge observation under the same durable Wait intent and consumes one
dispatch from the Scenario's existing allowance. The dispatch counter counts observations,
including unsatisfied polls; it is not a count of distinct intents or notification effects.

## Independent watchdog

`scripts/af-maintenance-watchdog.py` is the executable `af-maintenance-watchdog` implementation.
It is independent of the Scenario's Report and never drives or ticks a Run. It reads the native
checksummed `runs/scenarios.sqlite` journal with a read-only connection. Its reviewed configuration
pins the Ant identity/revision, writer release, timezone transitions, daily time, start day,
notification destination, optional installed notifier digest and a bounded invocation wall.
`schemas/maintenance-watchdog-config-v1.json` defines the complete configuration.

After two hours of grace, missing admission and missing durable skip evidence produce
`missed_occurrence`. A failure before a durable outcome notification produces
`run_failed_before_report`; a Run still active after its pinned wall produces `run_stalled`.
A waiting PR does not satisfy a new daily occurrence. A durable busy skip does. A visible pause
or disable suppresses future due times; it does not erase an occurrence already due.
The watchdog scans at most 4096 expected dates and retains at most 16384 alert identities.

The separate outbox persists one identity per Ant, expected occurrence and alert kind before
notification. Default execution is dry-run: it prints pending `af.maintenance-watchdog-alert/1`
records. `--notify` requires an explicit owner-controlled, digest-pinned notification adapter.
It uses the bounded native adapter request/response protocol, reconciles before execute, and
retains uncertainty. A lost response never authorizes a second notification effect with a new
identity. `--ack ALERT_ID` explicitly acknowledges an alert the external notification service has
retained. Neither dry-run nor notification mode reads credentials from Scenario inputs.

An offline observation can be run without af or any Provider:

```sh
python3 -I scripts/ci-health.py observe \
  --service fixtures/scenarios/maintenance-ci-health/failing-service.json \
  --state "$TMPDIR/ci-health-private/observer.sqlite"
```

Create the private state directory first. The example writes only its service state and stdout.
The watchdog needs explicit reviewed configuration, native journal and private state paths:

```sh
python3 -I scripts/af-maintenance-watchdog.py \
  --config /private/maintenance/watchdog.json \
  --journal /private/af-store/runs/scenarios.sqlite \
  --state /private/maintenance/watchdog.sqlite
```

No systemd unit is installed. Deployment, the first live tick, reruns, standing publication,
repair/quarantine PRs and automatic `confirm`/`apply` remain outside Phase 1. The nightly chain's
independent service-health alert remains responsible for a missed watchdog timer.

## Hermetic receipts

- `scripts/test-ci-health.py`: contracts, exact bounded GETs, normalization, recurrence, closure,
  evidence refusal, deterministic diagnosis and every feasible overlapping remedy-predicate pair.
- `scripts/test-af-maintenance-watchdog.py`: no tick, skips with/without durable reason,
  failure before Report, PR waiting across a heartbeat, pause/disable and lost notification response.
- `af::it task_custom::ci_health_*`: real native Task/Scenario execution with a fake Provider,
  field projection, foreign and malformed evidence refusal, deterministic unknown reporting,
  first-row routing and notification-before-Watch.
- `af-scenario tests::ci_health_*`: projection and settlement wire/schema parity, refusal origin
  checks, deterministic unknown routing that skips a requested Watch, and the watchdog reading
  an actual native journal without changing it.
- `af-contracts::schema_parity ci_health::*` and `af-store ci_health::*`: contract parity, source
  evidence binding and Python/native identity parity.
- `af-runtime task::structured::tests::ci_health_*`: the exact native Worker context fits its
  existing reservation, accepts the captured snapshot and refuses foreign diagnosis evidence.

These are functional/lifecycle receipts, not live GitHub, paid model or deployment evidence.

The failed-head and release-alert correction adds four Rust tests in existing binaries and two
Python test methods. The native fixtures contain three cases: accepted release-head evidence,
refused main-head evidence, and an unselected release alert with diagnosis exhausted. They use
two fake admissions and two fake Workers in total; the exhausted case invokes no Provider.
The installed-notification Python fixture adds two release cases and checks reconciliation does
not duplicate delivery. The batch test retains every observed failure, omission and release alert
within the existing 64 KiB adapter request and 20-dispatch bounds, including Unicode job names
and full log-reference lists. No test adds a sleep, real-time wait, retry or new binary.
Runs that diagnose now execute one additional notification Action so findings can precede the
Task and diagnosis can follow its settlement. Deterministic-only Runs retain their prior Action
count unless evidence needs multiple bounded findings batches.

In the implementing sandbox, both new schema tests passed (0.252 and 0.120 seconds), the CAS
failed-head test and native category-routing test passed, and all 18 Python CI health tests
passed. The new binary fixtures reached planning but the existing Provider disable-state
authority check refused `/opt`, owned by uid 65534 rather than uid 1000 or root. Their native
end-to-end assertions still require a host that admits the unchanged private fixture paths;
these failures are not passing receipts and no authority check is relaxed.
The broader schema/Scenario selection ran 294 tests: 269 passed and 25 failed. All schema parity
tests passed; seven native Task tests stopped at the Provider ancestor check and eighteen native
Scenario state tests refused the sandbox's `/tmp/.git` marker. The pinned fast gate built and
linted successfully, then stopped in `test-performance-scenario.py` with 20 repository-boundary
errors among 49 tests. The gate therefore did not complete its functional, e2e or uix validation.

The refusal regression adds two tests in existing binaries: one in-process schema/provenance
test and one native Scenario test, initially with two fake-Provider cases. Each case reserves the
unchanged 5,096-token Task allowance and executes one fake admission and one fake diagnosis; there are no
paid model calls, sleeps, retries or new binaries. The category-routing unit test also covers a
refused settlement alongside all five accepted categories. In the implementing sandbox, the
new unit test took 0.049 seconds and the six-case routing test took 0.519 seconds. The native
test stopped during Provider admission because `/opt` was presented as owned by uid 65534;
its full gate cost and lifecycle assertions therefore still require the enclosing Task's gate.

### Read-only check source regression

The native fixture must create its disposable snapshot with writable permissions while preserving
its exact input bytes. `std::fs::copy` preserves the source mode; a Task check materializes that
source as `0444`. The shared tree-copy helper already restores owner-write permission for copied
Task packages, but the separate snapshot copy bypassed that helper. Both
`ci_health_incomplete_snapshot_reports_without_a_task_remedy_or_watch` and
`ci_health_malformed_snapshot_identity_is_refused_before_provider_admission` then failed when
editing their copied snapshot, before reaching their assertions.

On 2026-11-10, a temporary diagnostic ran both exact tests inside a local Task's native check,
using the ordinary `Sandbox::materialize(Mode::ReadOnly)`, check runtime and Task check operator.
The denied write was `<private-test-HOME>/<fixture>/repo/ci-health/snapshot.json`, mode `0444`;
its parent was `0755`. The check's `TMPDIR`, compiled `CARGO_TARGET_TMPDIR` and private test HOME
were distinct, and fixture directory creation succeeded. The reported `task_custom.rs:15:69`
was the unchanged JSON write helper, not `Fixture::new`. A freshly rebuilt test binary reproduced
the same failure with path diagnostics, so the evidence did not justify a warm-cache or temporary
root change. All temporary instrumentation was removed.

The fixture now reads the source bytes once and writes them into a new disposable snapshot file.
`ci_health_fixture_allows_snapshot_mutation_from_a_sealed_source` exercises that same constructor
with an actual read-only sandbox: the copied bytes match exactly, owner mutation succeeds, and the
source bytes, mode and seal stay unchanged. It failed on the original copy operation and passed
with this change. It adds one test, with no model calls, waits or new test binary; its measured
nextest duration was 0.034 seconds.

The diagnostic rerun after the fix passed the malformed-identity test. The incomplete-snapshot
test advanced past snapshot mutation but could not plan in the managed Worker environment:
Provider disable-state validation refused `/opt`, owned there by uid 65534 rather than uid 1000
or root. That refusal also occurred on the unmodified tree's local focused run. This is not a
passing lifecycle receipt; the enclosing Task must still run its normal gate and independent
judgments on the sealed candidate. No Provider security check or sandbox boundary was changed.

### Native refused Attempt identity regression

The native Task ledger emits 26-character alphanumeric Attempt IDs. The first refusal settlement
schema and validator instead required a SHA-256 artifact digest. Its in-process fixtures used
synthetic digests, so they passed while a real refused diagnosis failed with
`CI refusal lacks exact native diagnostic evidence`, before reporting could run. Replacing the
fixture digest with a native-shaped ID reproduced that validation failure in process.

The settlement writer, reader, shipped schema and installed fixture adapter now use
`af/scenario-ci-diagnosis@2`; version 1 is refused. The provenance test checks the Task execution
contract, settlement validator and wire schema against the same valid and invalid Attempt IDs.
The end-to-end test covers refused foreign evidence, malformed evidence, foreign signatures and
foreign occurrences. Each must retain the original unsatisfied Task result and diagnostic, record
the deterministic unknown, notify findings, alert-only remedy and outcome, and skip Watch even
when requested. Replay must repeat no notification or Worker call.

This correction adds no test or binary. It adds seven Attempt-identity parity cases to the existing
in-process test and two cases to the existing native Scenario test, each with one fake admission
and one fake diagnosis. There are no paid model calls, new waits, retries or remedy effects.

The implementing sandbox's CI-profile selection ran all eight CI-health integration tests plus
CI-health contract, Store, runtime and Scenario tests and the shipped-schema tests: 18 passed
and six failed. The corrected provenance test, including the seven identity cases, took 0.073
seconds; the six-case routing test took 0.651 seconds. Five integration tests, including the exact
refusal regression, stopped before Provider admission because `/opt` was owned by uid 65534.
The watchdog test refused a temporary Store beneath the sandbox's `/tmp/.git` ancestor.
Neither refusal was bypassed. The two additional native fixture cases still need a full-path
timing receipt from the enclosing Task's gate; this sandbox cannot measure that cost.

### Release-head context admission regression

The release-head identity checks were already correct, but the complete Worker request exceeded
its reservation before the Worker could answer. The observer retains main in the snapshot's
`source_sha` and the release revision in the selected failure's `head_sha`. The Task captures
that exact observation artifact and Selection Requirements, without a repository checkout.
Native admission validates the diagnosis and evidence against the selected failure, and the
Scenario preserves that selection in both accepted and deterministic-refusal settlements.

The distinct-release fixture rendered 4,025 bytes (1,007 estimated tokens), exceeding the
unchanged 1,000-token Worker reservation. This was a context refusal, not foreign evidence.
Worker package version 1.0.3 compacts its instructions by 49 bytes: the same request now renders
3,976 bytes (994 estimated tokens). The fixture catalog pins the new package bytes. No evidence,
schema, reservation, deadline, acceptance check or native fallback changes.

`ci_health_fixture_prepares_exact_snapshot_within_reservation_and_refuses_foreign_diagnosis`
now checks both main and distinct-release observations through native Worker request rendering
and reply admission. Its added release case failed with the original instructions:
`CI context needs 1005 tokens, but Worker reserves 1000`. It also checks that the rendered
Selection names the failed head, accepts matching evidence, and refuses main and foreign
occurrence, signature or log references. The existing native Scenario regression retains both
the accepted release-head case and the refused main-head case, with the whole settlement printed
on a provenance assertion failure.

This adds one in-process case to an existing test: one additional private CAS, one context
render and five native reply checks. The expanded test took 0.048 seconds in the focused
CI-profile run. It adds no test, binary, process, model call, wait or retry, and no work to kernel
startup, capture or replay. Native end-to-end timing still requires the enclosing Task's gate:
this sandbox refuses Provider planning beneath `/opt`, owned by uid 65534, before the diagnosis
runs. The separate
watchdog fixture also refuses notifier authority beneath `/tmp/.git`. Neither boundary is
bypassed; these refusals do not prove the native Scenario's acceptance assertions passed.

The focused CI-profile selection ran all CI-health integration, schema-parity, Store, runtime
and Scenario tests, plus `schema_show_prints_every_id_af_help_json_lists`: 17 passed and eight
failed. Seven native Task tests, including the exact release-head and release-alert regressions,
stopped at the `/opt` ownership refusal; the native watchdog test stopped at the checkout
boundary. All 18 Python CI-health tests passed; nine of ten Python watchdog tests passed, with
the remaining notifier-authority case refused beneath `/tmp/.git`. Formatting, workspace
Clippy with warnings denied, Markdown lint, test-class audit and changelog validation passed.
The pinned fast gate and independent review/evaluation remain the enclosing Task's checks.
