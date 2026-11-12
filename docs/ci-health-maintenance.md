# CI health maintenance: inactive Phase 1

Phase 1 of the [design](design/ci-health-maintenance.md) provides a bounded production observer
and hermetic GitHub-shaped service fixtures for observation, diagnosis and reporting. Its native
Scenario identifier is `maintenance-ci-health`.
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

The observer's `collect` path makes at most the declared number of GETs (maximum 32), each
capped at 1 MiB. API requests stay repository-scoped; signed job-log downloads share the
same allowance. It binds main and optional open release PR heads, the
observation window and exact run attempts; truncated run/job lists and exhausted requests become
omissions. `OfflineGithub` supplies the hermetic transport. The explicitly selected production
reader and inactive adapter staging are described below. Tests make no live requests or paid
model calls.

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
The journal's `af.ci-health-state/2` metadata retains only the latest capture's complete service
digest and exact snapshot. Its receipt commits atomically with the capture's recurrence folds.
Identical evidence replays that snapshot, preserving each job's original count even when one
capture contains several attempts of the same signature. A different capture replaces the
receipt and uses the ordinary recurrence and green-closure rules. State version 1 is refused;
discard it on upgrade. The metadata schema is available through `af schema show af.ci-health-state/2`.
The capture digest also binds successful jobs, even though they are absent from the snapshot's
failure list. Reordering otherwise identical GitHub runs or checks preserves the capture.
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

## Production observation, staged only

`scripts/ci-health.py observe-github` explicitly selects `scripts/ci-health-github.py`. The offline
`observe --service` command and fixtures remain available. Production uses the nightly reader's
actual proxy-free HTTPS opener, with its five-second connection and thirty-second read bounds,
API version and no-redirect policy. API GETs stay beneath
`https://api.github.com/repos/<repository>/`; only exact job-log endpoints may lead to the
credential-free signed download described below. The default allowance is 16 physical requests
(at most 32), 1 MiB per response, 8 MiB in aggregate and 60 seconds total, including slow headers and body reads. One failed read or 5xx
can be retried once as in the nightly transport; both attempts spend those same bounds, with no
sleep or refreshed allowance. An optional owner-only token file is read at invocation, never
embedded in executable bytes, passed as an argv value or logged. Server error bodies and transport
diagnostics are not printed, and an answer reflecting this token is refused.

The reader resolves the explicit repository/main-ref and optional open release PR number once.
It reads complete bounded check-run and workflow-run pages at each selected head, then jobs of
each exact run attempt overlapping the explicit UTC window. An attempt-specific GET binds its
start/update interval: an in-window rerun is retained even when the run was originally created
before the window. Missing, mismatched or future attempt metadata is incomplete evidence.
Every job binds its run, attempt, head, check-run URL, check name and conclusion. Sorted runner labels retain the exact platform identity;
missing labels cannot be inferred from a job name. Failed checks lacking a matching job are
omissions. Successful jobs can close recurrence; skipped and neutral jobs cannot. Incomplete,
foreign, truncated or unavailable checks/jobs remain incomplete evidence.

Failure annotations supply bounded error evidence first; their count must match the check's
reported count. Every failed job also attempts a job-log GET, supplying at most 1 MiB of UTF-8 evidence, with at most
100 selected error lines of 4,096 characters each. One API 302 may name an HTTPS signed blob
download on `productionresultssa<digits>.blob.core.windows.net` or
`pipelinesactions<alphanumeric>.blob.core.windows.net`. The URL must carry a read-only blob SAS
(`sig`, `se`, `sp=r`, `sv`, `sr=b`); userinfo, ports, fragments, path escapes and another redirect
are refused. The download uses the same proxy-free no-redirect connector without the GitHub
credential. Its requests, retries and bytes spend the original aggregate request/byte/wall
allowances. Signed URLs and response bodies are never included in diagnostics.

Explicit libtest `test ... FAILED`, nextest `FAIL [...]`, pytest `FAILED ...` and Python unittest
`FAIL:`/`ERROR:` records naming a method and dotted qualified case supply at most 100 distinct
test identities, each at most 2,042 characters. The lexical first identity defines the fingerprint before any generic error line.
Changing diagnostic prose or test record order cannot change that test's signature. Multiline
annotations can also supply explicit test records. The combined distinct identities from
annotations and available logs share the 100-test bound and determine the lexical first test;
partial named annotations cannot hide earlier log identities. Bounded annotation text remains
sufficient fallback when the log is remotely
unavailable. Malformed, oversized or out-of-scope log evidence still refuses completeness.
Test IDs retain their parameter bytes, including SHAs, durations and paths; only log decoration
is removed before extracting them. Content digests name the bounded annotation/log reads.
Missing, malformed or excess evidence retains an unknown failure and a named omission. No full log enters a Task or Scenario input.

`--nightly-receipt` names the actual `builds_root/<date>/publish.json`, validated against the
shipped `af.nightly-publish/1` schema and publication invariants. The reader checks its exact
repository and main head, date/tag, asset order, release URL, notification/pruning outcomes and
publication timestamp in the observation window. It does not substitute a fixture summary or
select an arbitrary newest file. A receipt of an earlier green head cannot attest newer main;
that observation remains incomplete. A missing, malformed, stale or foreign receipt is a named
omission. A failed nightly notification also yields a failure conclusion; only the writer's
exact explicit unconfigured reason permits an absent message without failure. The snapshot
retains its canonical receipt identity and conclusion.

For a later reviewed observation, select every scope and path explicitly (this example would
perform live GETs if invoked; it is not run as part of staging or tests):

```sh
python3 -I scripts/ci-health.py observe-github \
  --repository OWNER/REPOSITORY --main-ref main --release-pr 123 \
  --since 2026-11-08T04:00:00Z --captured-at 2026-11-09T04:00:00Z \
  --nightly-receipt /private/nightly/builds/2026-11-08/publish.json \
  --state /private/maintenance/observer.sqlite \
  --github-token-file /private/maintenance/github-read-token
```

Stage the native `ci_observe_v1` executable once with its repository, main ref, destination,
private state/token paths, request/wall bounds and an absolute private occurrence directory:

```sh
python3 -I scripts/ci-health.py stage-adapter \
  --repository OWNER/REPOSITORY --main-ref main \
  --destination github:OWNER/REPOSITORY --output /private/staging/ci-observer \
  --occurrence-dir /private/maintenance/occurrences \
  --state /private/maintenance/observer.sqlite \
  --github-token-file /private/maintenance/github-read-token
```

Staging exclusively creates an absent 0700 executable. It makes no requests, reads no credential
or receipt and touches no observer state. The executable embeds the exact helper and schema
bytes and machine scope. Its digest stays pinned across daily occurrences; `--since`,
`--captured-at`, `--nightly-receipt` and `--release-pr` are refused by `stage-adapter`.
Changing code, repository, main ref, destination, state/token paths or occurrence directory
requires a newly reviewed executable pin. No machine policy is changed here.

For each owner-reviewed occurrence, use the captured `af.ci-health-snapshot/1` document selected
for the diagnosis Task and notification bodies. The occurrence directory must already exist,
owned by the invoking user with mode 0700. This command stages only selectors:

```sh
python3 -I scripts/ci-health.py stage-occurrence \
  --snapshot /private/maintenance/selected-snapshot.json \
  --since 2026-11-08T04:00:00Z \
  --nightly-receipt /private/nightly/builds/2026-11-09/publish.json \
  --occurrence-dir /private/maintenance/occurrences
```

It exclusively creates `<snapshot-content-digest-hex>.json` with mode 0600, carrying
`af.ci-observe-occurrence/1`: `snapshot_digest`, `since`, `captured_at`, `nightly_receipt` and
`release_pr`. The end time and release PR number come from the selected snapshot; the owner
explicitly supplies the start time and absolute receipt path. The document is at most 8 KiB,
the nonempty UTC window at most one day, and the release PR is absent or a positive bounded
integer. The schema is also available through `af schema show af.ci-observe-occurrence/1`.
A missing, foreign, malformed, oversized, symlinked or non-private selector document is refused
before any GitHub request. Directory traversal uses held directory descriptors and refuses
symlinks. The adapter reads only this exact filename, never an arbitrary newest receipt or
caller-supplied path. The owner can stage later occurrences without changing executable bytes.

The staged adapter accepts only native `af/scenario-adapter-request@4` observation requests
with its exact destination and repository, and a condition
`ci_observe_v1:sha256:<snapshot-content-digest-hex>`. The Scenario compiler derives this digest
from the selected snapshot payload, separately from its native Task artifact ID. Plain
`ci_observe_v1` is refused. The adapter reads that digest's owner-authorized selector document,
collects current evidence and compares main SHA with the request revision. It evaluates the capture
against a private in-memory journal copy, replaying the latest receipt for identical service evidence
or folding a different capture. It refuses a preview digest mismatch before
changing recurrence or satisfying the Wait. Changed jobs, attempts, logs, receipts or omissions
cannot authorize diagnosis or notifications from a stale selected snapshot, even at unchanged
main. The accepted result is checked again against the same digest inside the journal transaction,
before its recurrence folds and capture receipt commit.

Native observation evidence remains the versioned `af.ci-health-snapshot/1` document; the adapter
response names its canonical content identity and returns the exact requested condition. Production
capture bounds the complete serialized response, including the escaped snapshot, envelope and final
newline, before committing recurrence and the capture receipt. Excess evidence is dropped with
`64 KiB adapter response bound exceeded` in the incomplete snapshot's omissions; retained failures
still reach findings and release alerts. Missing main SHA or changed revision/snapshot refuses
observation. An incomplete snapshot remains visibly incomplete
even when its exact bounded observation completed. Every Action, notification and request-chosen
selector is refused. Capturing each snapshot for its pinned diagnosis Task and approving a live
machine policy remain separate owner steps; this change activates nothing.

## Native Scenario fixture

[`fixtures/scenarios/maintenance-ci-health/`](../fixtures/scenarios/maintenance-ci-health/README.md)
contains the native bundle, read-only Task packages, GitHub/service evidence and adapter fixture.
`configure.py` renders a definition from an exact preobserved snapshot and a native Task target.
It neither plans nor executes a Task. The installed `ci_observe_v1` fixture recomputes that
observation and refuses a difference before diagnosis or notifications. Its Wait condition binds
the selected snapshot content digest, including when main SHA is unchanged. Incomplete, healthy,
exhausted and missing-fingerprint snapshots render a report-only path without a model dispatch. Exact pinned
infrastructure/transient signatures also bypass the Task and retain a deterministic diagnosis.
The fixture classifies every occurrence deterministically before selecting the first unresolved
occurrence for its one diagnosis Task reservation. An earlier pinned infrastructure or transient
signature leaves that reservation available to a later unresolved occurrence. Only further
unresolved occurrences are reported under `diagnosis_reservation_exhausted`; deterministically
classified occurrences do not exhaust a diagnosis reservation. Every observed failure is sent with
its workflow/run/attempt/job identity, job name, platform, conclusion, head SHA, log artifact
references and release-blocking flag. The findings include an explicit `release_alerts` entry for
every release-blocking occurrence, including those that cannot receive a diagnosis. Findings go
out immediately after observation, before the diagnosis Task or its remedy selection. They use
`af.ci-health-notification/5`, with batches bounded by the complete serialized adapter request,
including the escaped body, scope and envelope. The batches' `deterministic_results` carry
every available deterministic diagnosis, with the first matching report-only
remedy's `row` and `intended`. The diagnosis retains the exact snapshot, occurrence, signature,
failed head and evidence references. Further unresolved occurrences receive a deterministic unknown
and alert-only intent naming the exhausted reservation; the selected Task's pending result is
absent from this array. Summaries and results can occupy separate batches when the combined
request would not fit; findings omit the repeated single `deterministic_diagnosis` field.
Every lifecycle notification (findings, diagnosis, remedy and outcome) binds the repository through
`repository_id` and the transport subject's canonical repository digest. The notification reader
checks the digest against its pinned snapshot, restores the exact repository in its durable record
and refuses a conflicting body repository. This avoids
spending the dispatch allowance on repeated scope bytes for large valid repository inputs.
Every observed failure and omission is retained
within the existing 64 KiB request and 20-dispatch Scenario bounds. Notification versions 1–4
are refused; `af schema show af.ci-health-notification/5` publishes the current shape.
The quarantine-intent predicate uses the existing read-only inventory resolver and Q2
scan: two distinct Tasks in its seven-day UTC window on the pinned evidence platform. It never
calls `scan --write`, `confirm` or `apply`.

The pinned diagnosis Task's Selection Requirements must name that first unresolved occurrence,
its signature and its failed head before planning; rendering does not rewrite an approved Task
plan. When every occurrence is classified deterministically, the first supplies the remedy/outcome
context and no diagnosis Task is dispatched. All occurrences' diagnoses and remedy intents still
appear in findings. The selected occurrence retains its separate remedy/outcome notifications.

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

At the adapter boundary, native notifications transport `diagnosis_settlement` as text containing
`af/scenario-ci-notification-evidence@2`. This bounded projection carries the exact Task, plan,
revision and result IDs, snapshot and selection, category, accepted output artifact ID or every
refused Attempt and diagnostic ID. `settlement_id` is the canonical content digest of the full
`af/scenario-ci-diagnosis@2` record retained in the Run. `omitted` explicitly records that receipt
result details, diagnosis details, the accepted envelope and diagnostic prefixes remain in that
native settlement. These references do not grant an adapter ambient Store access.

The encoder first checks that the original observation's subject equals the notification's exact
repository. Every fixture notification transports its subject and `repository_id` as the canonical string digest;
the installed reader checks them against its pinned snapshot and restores the exact repository
in the durable notification. The original inputs and intent digest remain unchanged. The kernel
checks the complete encoded request against the existing 65,536-byte bound.

Notification version 5 adds `repository_id`, `findings_reference` and durable `native_evidence`. Lifecycle
bodies reference the snapshot details already delivered in findings instead of repeating the potentially large omission
list. The reader restores that list from the exact snapshot; unselected occurrences stay explicit
in every lifecycle body. The complete native evidence projection is retained in the durable notification, including
its explicit omission marker. Evidence version 1, the earlier observation transport and notification
versions 1–4 fail closed. Ordinary adapter requests and native journal records keep their versions.

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

Reviewed, inactive definitions live in
[`scripts/maintenance/af-maintenance-watchdog.service`](../scripts/maintenance/af-maintenance-watchdog.service)
and [`.timer`](../scripts/maintenance/af-maintenance-watchdog.timer). The timer checks daily at
06:05 UTC: the example's 04:00 UTC expected Ant occurrence, two-hour grace and five minutes for
receipts. `Persistent=true` checks after downtime. Neither unit depends on a successful Ant tick,
nightly unit or Scenario Report, so those failures cannot suppress the watchdog.

The oneshot runs as the dedicated `af-maintenance` account, with a 0700 StateDirectory, 0077
umask, a read-only native Store, private systemd configuration credentials and only its outbox
writable. Its 75-second service bound contains the configuration's at-most-60-second invocation;
control-group termination, memory, process, file and log limits bound the process and notifier.
`--notify` requires the exact private configuration's owner-controlled, digest-pinned notifier.
Only that notifier may read an optional separate notification credential; the watchdog has no
GitHub mutation or nightly-publish credential.

Staging for later owner review means copying the definitions and script/helper bytes into an
owner-controlled installation outside every checkout, preparing the private account/state,
granting read access to the native journal, and reviewing the exact Ant revision, writer release,
zone, expected time, notification destination and notifier digest in the configuration. The
service reads `/etc/af-maintenance/watchdog.json` through `LoadCredential=watchdog-config`; its
`%d/watchdog-config` argument is a private path, never a token. An optional notifier credential
line is commented. Keep the staged service and timer disabled and unstarted. A different pinned
expected time requires a reviewed timer change after its two-hour grace. No command here installs,
enables or starts a unit, and no installer references these definitions.

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
  reservation through the full 64-KiB, 100-failure bound, accepts the captured snapshot and
  refuses foreign diagnosis evidence.

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

### Reviewed Phase 1 continuation

`correctness-1` is fixed in `remedy`: diagnosis revision matches the selected failure's
`head_sha`. `test_bundle_distinct_release_head_pinned_classifiers_select_row_two_release_alert`
failed on the reviewed candidate with `remedy-1` for both pinned transient and infrastructure
signatures; it now checks the rendered row 2 `release_alert`, exact release-head diagnosis and
main-head refusal through `configure.bundle`.

The evaluator's missing production reader and receipt path are implemented by
`ci-health-github.py`, `observe-github` and `stage-adapter`. `ProductionTests` exercises realistic
REST check/run/job/log responses, exact main/release heads, real nightly-publish receipts,
annotations, missing/malformed/foreign evidence, request/response/aggregate/wall caps, retries,
signed-log downloads and foreign-redirect refusal, credential handling and the self-contained
installed entry point with an in-memory opener. Tests do not open a socket. The evaluator's missing independent systemd units
are supplied under `scripts/maintenance/`; the watchdog unit test binds their private authority,
bounds and timer to the existing 04:00 UTC fixture and two-hour grace. They remain inactive.
All prior native contracts, Task packages, budgets, Scenario refusals and fixtures are preserved.

This continuation adds nine Python test methods in the two already-classified script suites:
eight functional CI-health tests and one e2e watchdog test. The production tests add two short
Python staging subprocesses; all transport calls are in process. No Rust test, new binary, model
call, sleep, timeout-driven assertion or startup/replay work is added. Full suite timings and any
host refusals are recorded in the Task report; the independent native check and review/evaluation
remain the enclosing Task's acceptance receipts.

The supplementary correctness review also reproduced two production-reader edge cases before
fixing them: an in-window rerun was lost when its original run creation preceded the window,
and a failed nightly Telegram delivery was mapped to success. The realistic REST/receipt tests
now retain that rerun's release failure using exact attempt metadata and distinguish the nightly
writer's explicit unconfigured reason from a delivery failure. The watchdog definitions needed
no further correction. The independent supplementary evaluation found the requested inactive
reader, receipt path and units present; authoritative review remains the enclosing Task's.

The continuation's full CI-health Python suite passed 26 tests. Its watchdog suite passed ten
of eleven; the notifier fixture was refused beneath `/tmp/.git` with `Notifier authority must
be outside every Git checkout`. The initial native schema/CI-health/Scenario selection ran 289
tests: 264 passed and 25 were refused (seven Provider ancestor-owner checks under `/opt` and
eighteen Scenario-state checks beneath `/tmp/.git`). Every schema-parity test passed. The fast
gate completed its build, release checks, formatting and linting, then its functional class
stopped in the unchanged performance-Scenario suite with 20 repository-boundary errors among
49 tests. It therefore supplied no completed functional, e2e or uix receipt. No boundary was
bypassed and these failures are not reported as passing or as timing flakes.

The nine added Python methods contributed 1.099 seconds in the recorded full script runs,
including the two staging subprocesses; the new watchdog unit method contributed 0.017 seconds
of that total. Workspace Clippy, cached Markdown lint 0.22.1, schema parity, test-class audit and
change-note checks passed. The worker received the reviewed Snapshot without usable Git metadata
or a `../base` mount; supplementary review compared untouched starting-file copies. A complete
native baseline/diff Subject and the authoritative independent judgments remain the enclosing
verification Task's responsibility.

### Production evidence continuation

The production observation now binds the exact Scenario snapshot content digest and uses bounded
owner-authorized per-occurrence selectors under one pinned executable. Hermetic installed-entry
point tests cover changed jobs at unchanged main and two successive daily windows, receipts and
release PR numbers. The native Scenario regression verifies stale evidence reaches neither
notifications nor Provider admission. Signed log downloads preserve the original bounds and
withhold the GitHub credential; supported explicit test failure records take precedence over
preceding generic diagnostics. The occurrence writer/reader/schema parity and malformed,
non-private, oversized and symlink refusal cases are exercised without network access. No live
Ant, timer, publication permission or credential is installed.

This continuation adds nine functional Python test methods in the existing classified suite and
one native Scenario integration test in the existing `af::it` binary. The nine new Python methods
ran in 0.777 seconds together; their only new subprocess work is four short selector-staging CLI
invocations. The native regression reuses the existing fixture and isolated HOME/XDG helper; its
recorded 0.463-second execution stopped at the host Provider-directory ownership refusal before
Scenario admission, so it does not measure successful execution cost. No new test binary,
paid Provider call, sleep, timeout-driven assertion or startup/replay work is added.

All 35 CI-health Python tests pass, including the installed production adapter, occurrence schema
writer/reader, signed-download caps and identity edge cases. Independent correctness review and
evaluation reproduced additional annotation/parameter collisions, which the named regressions now
cover; final re-review reported no remaining findings. The native CI-health/Scenario/schema
selection compiled: 268 of 294 tests passed, including all four shipped-schema unit tests, and
26 failed. Eight Task fixtures were refused because `/opt` is owned by uid 65534 rather than the invoking user or root; eighteen
Scenario fixtures were refused beneath `/tmp/.git`. Every schema-parity test passed. The watchdog
suite passed ten of eleven, with its notifier fixture refused by the same checkout boundary.
The additional GitHub-adapter, Git-workspace and merge-watch Python suites encountered installer
boundary refusals (25 failures and 86 errors) and forbidden Unix-socket binding (21 errors).
These are failed sandbox receipts, not passes or timing flakes; no boundary was weakened.

The pinned fast gate completed its build, release checks, formatting and Clippy, then stopped in
the unchanged performance-Scenario Python suite with twenty repository-boundary errors among 49
tests. It supplied no completed functional/e2e/uix receipt. Cached Markdown lint 0.22.1,
test-class audit, schema generation parity and changelog checks pass. Passing native and fast-gate
receipts remain required in an environment that satisfies the repository's existing boundaries;
the enclosing Task retains authoritative review and evaluation.

### Exact capture and partial annotation regressions

The staged-adapter regression captures two distinct attempts with the same signature into the
adapter's own journal, stages that exact snapshot, and replays its original counts of 1 and 2.
It accepts unchanged and reordered evidence after reopening the journal, and refuses changed
logs and green evidence without changing the journal bytes. The existing stale-evidence test
also refuses a changed successful job whose identity is absent from the failure list.
The state writer/reader/schema regression checks version 2, replacement by a later capture,
reopening, and refusal of version 1 and malformed records. Transaction tests check rollback on
both a digest mismatch and a failed capture-receipt write.

The partial-annotation regression combines annotation `z::last` with log failures `a::first`
and `z::last`, selecting `a::first` with a stable signature after log reordering. It also checks
an earlier annotation identity, exactly 100 combined identities, refusal of 101 identities,
unavailable-log fallback and malformed-log refusal. The observer always attempts the bounded
log read, including when annotations name a test; every request still spends the original bounds.

This correction adds four functional Python methods in the existing suite, measured together at
0.227 seconds. They use in-process transport and staged-entry execution, with no new subprocess,
wait, model call or test binary. Existing schema parity validates one additional schema; the
existing CLI parity test launches one additional isolated `af schema show` command. No kernel
startup, Store replay or source-capture path changes.

All 39 CI-health Python tests pass. The native CI-health, complete schema-parity and Scenario
selection ran 295 tests: 269 passed, with eight Task tests refused by the `/opt` ancestor-owner
check and eighteen Scenario tests refused beneath `/tmp/.git`. Every schema-parity test passed.
The fast gate passed its build, preflight, formatting and both Clippy modes, then stopped in
`test-performance-scenario.py` with twenty repository-boundary errors among 49 tests. It supplied
no completed functional, e2e or uix receipt. Independent review and evaluation remain the enclosing
Task's checks; no authority, bound or fixture is relaxed to bypass these sandbox refusals.

## Classification and response-bound corrections (2026-11-11)

The compiler now classifies the entire observation before allocating its one diagnosis Task.
The native regression
`ci_health_native_classifies_first_failure_before_diagnosing_and_settling_second` supplies a
pinned transient or infrastructure first failure and a distinct unresolved release failure. Its
Task Selection, Worker request, accepted settlement and diagnosis notification must all name
the second occurrence. The Python classifier regression also checks additional deterministic
occurrences, a genuinely exhausted unresolved occurrence, and an entirely deterministic capture.

The installed production observer sizes the actual native response before committing its capture.
The escaped/Unicode regression starts with eight failures whose snapshot JSON fits 64 KiB while
its response does not. The Unicode case retains seven failures in a 64,671-byte response,
including its final newline, and records `64 KiB adapter response bound exceeded`. Installed
notification delivery retains every remaining finding, omission and release alert. Replay leaves
the bounded journal unchanged; omission-only overflow and impossible-bound rollback are checked
too. The native regression
`ci_health_installed_production_adapter_bounds_wire_evidence_and_scenario_reports_retained_findings`
consumes that installed response through the Scenario and checks the resulting findings and outcome.

This continuation adds two functional Python tests and two tests in the existing `task_custom`
e2e module, with no new binary, wait or retry. The Python pair ran in 0.67 seconds. The native
tests add three Scenario fixtures: two classifier cases, each with one fake Provider admission
and one fake diagnosis, and one incomplete production observation with no Provider dispatch.
The production fixture uses 26 GETs within the existing configurable 32-request maximum. No
reservation, protocol cap, schema shape, sandbox boundary or publication authority changes.
There is no change to kernel startup, Store replay or source capture.

All 41 CI-health Python tests pass, as do formatting, all-target Clippy, the test-class audit,
change-note validation and cached markdownlint 0.22.1 on the four changed Markdown files.
The native CI-health, complete `task_custom`, both schema-parity binaries and complete Scenario
selection ran 306 tests: 270 passed and 36 failed. All 223 schema-parity tests passed.
Eighteen Task tests, including the two new native tests, stopped before Provider admission because
the sandbox's `/opt` ancestor is owned by uid 65534. Eighteen Scenario tests, including the native
watchdog, refused Scenario state beneath `/tmp/.git`. The Python watchdog suite passed ten of eleven tests; the remaining
notifier fixture refused that same checkout boundary. These are failed receipts, not timing
flakes or measured native end-to-end passes. Complete native execution cost remains unmeasured
here. The pinned fast gate and independent review/evaluation remain the enclosing Task's checks
after sealing, as required by the Worker instructions; no duplicate whole-suite gate was launched.

## Multi-occurrence context admission

The first unresolved occurrence was already selected correctly after deterministic classification,
but its Task could stop after Provider admission: the full two-failure Worker request needed
4,725 bytes (1,182 estimated tokens), exceeding the unchanged 1,000-token reservation. The
Worker now uses its compact package instructions directly. Kind version 1.0.1 and Worker version
1.0.4 carry an equivalent compact diagnosis schema, pinned by `af catalog digest`. The same
request retains the complete observation and exact Selection in 3,991 bytes (998 estimated tokens).
That correction kept the then-current 5,096-token, two-Attempt Task bound. The full evidence-range
reservation below supersedes that allowance; contexts exceeding their reservation are still refused.

The local schema uses `minProperties` equal to the number of declared properties in each closed
object, which still requires every field. Its nonempty text patterns and singleton source-ID array
already imply the omitted redundant keywords. The published artifact schema and all accepted
payload shapes remain unchanged. Native Codex projection recognizes those required fields, and
normalization preserves required nullable values. The canonical and compact diagnosis schemas
produce exactly the same native response constraint; unsupported references and open maps retain
their existing behavior.

`ci_health_embedded_contracts_preserve_published_constraints_without_remote_resolution_metadata`
expands only these proven equivalences and compares every published validation constraint.
`ci_health_compact_worker_schema_keeps_native_response_constraints_and_refusals` compares the
native response projections and rejects missing/replaced fields and wrong types through both
validators. `closed_property_count_keeps_required_nullable_fields_and_optional_fields_stay_optional`
checks required null retention and the boundaries of the closed-object rule.

At that point, the expanded
`ci_health_fixture_prepares_exact_snapshot_within_reservation_and_refuses_foreign_diagnosis`
uses the real two-failure `release-snapshot.json`, shared with the Python observer regression.
It retains both failures, selects the release occurrence, fits the reservation, admits its answer
and refuses a valid answer about the first occurrence, main-head evidence and foreign metadata.
The existing native Scenario regression additionally requires completed execution, the full
snapshot in the Worker request, two settled known charges, and no false reservation-exhausted
notification for either pinned deterministic classifier. Its replay still dispatches nothing.

This correction adds two functional unit tests in existing binaries and one case in an existing
in-process context test. The new tests ran in 37 ms together; a diagnostic run measured the
additional context case at 19 ms. The native assertions add two read-only accounting projections
across the existing classifier fixtures; their successful-path cost cannot be measured in this
sandbox. No new subprocess, paid model call, wait, retry or test binary is added. The response
projector and normalizer add a property lookup per visited schema object; schemas without
`minProperties` return immediately. Kernel startup, source capture and Store replay code are unchanged.

The CI-profile selection of all CI-health tests plus the structured-runtime and reply-schema
modules ran 35 tests: 24 passed and 11 failed. Ten native Task fixtures, including the exact
multi-occurrence regression run alone, were refused before Provider admission because `/opt`
is owned by uid 65534 rather than the invoking user or root. The native watchdog refused Scenario
state beneath `/tmp/.git`. All 41 Python CI-health tests passed; the watchdog suite passed ten of
eleven, with its notifier case refused by that checkout boundary. These are failed environment
receipts, not passing end-to-end checks or timing flakes. The pinned fast gate and independent
review/evaluation remain the enclosing Task's checks after sealing.

## Full evidence-range diagnosis reservation

Worker package 1.0.5 reserves 20,000 tokens for one diagnosis Attempt. The pinned Task minimum is
24,096 tokens and two Attempts: 4,096 tokens and one Attempt for mandatory Provider admission,
then 20,000 tokens and one Attempt for diagnosis. Native planning with 24,095 tokens or one
Attempt still refuses before Provider dispatch. The Task target, generated Scenario and inert
`scenario.json` use the same 24,096-token cap. No retry allowance is added.

The evidence bounds remain 100 failures and 65,536 bytes of compact serialized snapshot JSON.
`fixtures/scenarios/maintenance-ci-health/max-snapshot.json` reaches both bounds with a complete
observation, correct occurrence/signature identities, ordinary failure records, UTF-8 and escaped
text. File whitespace is excluded from this payload measurement. The complete native Worker
request is 67,614 bytes: the snapshot plus 2,078 bytes for both artifact wrappers, exact Selection
Requirements, pinned instructions, reply protocol and output schema. Native context accounting
uses the ceiling of bytes divided by four, so the largest input needs 16,904 estimated tokens.
The snapshot is embedded as JSON, so its own serialized bytes are counted once and the overhead
is fixed for this pinned Task. The Worker allowance leaves 3,096 tokens beyond that input estimate
for the answer. These are reservations and an admission estimate, not a claim about measured
Provider usage. Native context, charge and Attempt bounds still apply.

`ci_health_fixture_prepares_exact_snapshot_within_reservation_and_refuses_foreign_diagnosis`
now renders this maximum observation through the real Worker contract, checks the complete byte
and token counts, admits the selected occurrence's reply and retains the foreign-evidence
refusals. The previous 1,000-token Worker allowance cannot admit its 16,904-token context.
`ci_health_maximum_complete_snapshot_reaches_worker_and_returns_accepted_selected_diagnosis`
executes the pinned Task through the binary and fake Provider, requires the exact full snapshot
and Selection to reach the Worker, checks completed and satisfied acceptance, reads the diagnosis
from CAS, and verifies both settled Attempt reservations and dispatch-free replay.
`ci_health_minimum_budget_plans_and_refuses_one_less_token_or_attempt_before_admission` retains
both minimum-budget refusal cases with the new exact allowance. The Python
`test_maximum_complete_snapshot_has_100_failures_and_exact_64_kib_serialized_bound` validates the
same fixture and refuses one additional serialized byte.

The change adds one native execution test in the existing `af::it` binary, one in-process context
case, and one Python boundary test. The execution test uses the existing disposable repository
and fake Provider helpers; it adds one fake admission and one fake diagnosis, with no paid calls,
sleep, wait or new test binary. No production startup, Store replay or source-capture path changes.
The enclosing Task runs the pinned fast gate and independent review/evaluation after sealing;
this Worker runs the focused native, schema, Scenario and Python selections without a duplicate
whole-suite gate. No live call, schedule activation or publication is performed.

In this sandbox, the expanded context test passed in 0.153 seconds for all four cases; this is
an upper bound on the added in-process case's cost. With the former reservation restored just
for the regression check, it failed with
`maximum: CI context needs 16904 tokens, but Worker reserves 1000`; the pinned Worker bytes were
then restored. The new Python boundary test passed in 0.016 seconds, and all 42 CI-health Python
tests passed. The package-pin check passed against the exact native `af catalog digest` output.
Formatting, workspace/all-target Clippy, the test-class audit and cached Markdown lint passed.

The complete focused native/schema/Scenario selection ran 84 tests: 55 passed and 29 failed.
Eleven binary fixtures, including the new maximum-input execution and minimum-budget tests,
were refused during planning by the Provider disable-state ancestor check: `/opt` was exposed as
owned by uid 65534 rather than the current uid 1000 or root. Eighteen Scenario tests refused
state beneath the sandbox's `/tmp/.git` marker. The watchdog Python suite passed ten of eleven;
its notifier test was refused by that same checkout boundary. These are failed environment
receipts, not passing execution evidence or timing flakes. The maximum-size file and payload
both fit the native 64-KiB input cap, but its accepted execution and full-path added gate cost
remain unmeasured on this host. No authority check was bypassed.

## Complete deterministic occurrence reporting

The occurrence-reporting continuation keeps the approved 20,000-token Worker and 24,096-token
Task/Scenario reservations unchanged. The native
`ci_health_native_reports_deterministic_remedy_and_settles_only_first_unresolved_occurrence`
requires exact deterministic reporting alongside one accepted Task, with infrastructure before
or after the unresolved occurrence. The native
`ci_health_native_reports_every_deterministic_diagnosis_and_first_remedy_without_a_task` requires
every exact diagnosis and ordered remedy without Provider dispatch, including overlapping pinned
classifiers and release precedence. Both require dispatch-free replay.

The Python maximum-snapshot regression retains all 100 deterministic results, or 99 exhausted
results beside one pending Task, within the unchanged request and Scenario bounds. The installed
adapter regression also exercises one failure with a 40,008-character repository and 45 failures
with a 30,508-character repository. Repeating scope and diagnosis bytes originally broke these
request/dispatch boundaries; the latter now uses two findings requests, at most 65,247 bytes,
and five actual dispatches. Exact summaries, diagnoses and remedies survive. Reader tests refuse
foreign repository/occurrence/evidence, duplicate occurrences, mismatched remedy rows and missing
lifecycle diagnosis evidence. Schema parity accepts 100 distinct results and refuses 101 and old
notification versions. Independent correctness review and acceptance inspection found no
remaining implementation gaps after these boundary corrections.

This adds two Python methods in the existing functional script suite and one native test plus
one case in an existing native test: two additional Scenario executions through shared fixtures.
The Python changes add nine short installed-adapter invocations; the two new methods took
4.126 seconds together. The new native test stopped at planning in 1.461 seconds, so its
successful-path cost remains unmeasured. No new test binary, paid call, wait or retry is added.
Production startup, source-capture and Store-replay paths are unchanged.

All 44 Python CI-health tests passed. The complete focused native, structured-runtime,
reply-schema, schema and Scenario selection ran 344 tests: 280 passed and 64 failed. All 218
schema parity tests, four shipped-schema tests and eleven focused runtime/runner/Store tests
passed. Twenty binary fixtures were refused by `/opt` Provider-state ancestry ownership;
twelve Scenario fixtures refused outside-checkout authority; twenty-six refused outside-checkout
state; five adapter installers refused private paths; and one Scenario performance fixture
refused the sandbox's 4.0-GiB free `/tmp` against its unchanged 10-GiB disk floor. The watchdog
suite passed ten of eleven, with the notifier refused by `/tmp/.git`. These failures prevent a
successful native execution receipt here; none is reported as a timing flake or a measured pass.
Formatting, all-target workspace Clippy, the test-class audit and cached Markdown lint passed.
The enclosing Task runs the pinned fast gate after sealing, as required by the Worker protocol;
this Worker does not duplicate that whole-suite gate. No live call, activation or publication
is performed.

## Lifecycle notification request bounds

The native regression
`ci_health_large_scope_completes_native_findings_diagnosis_remedy_and_outcome` uses a complete,
valid snapshot with a 40,008-character repository. It executes findings, remedy and outcome
through the installed adapter and the native Scenario, both for an exact deterministic
infrastructure classification and for an accepted fake-Provider diagnosis. Every durable
notification retains the exact repository, Run, snapshot, occurrence, signature and failed head;
resuming the completed Run adds no calls or notifications. The adapter's 65,536-byte request
bound, 20-dispatch Scenario limit and single diagnosis Task reservation stay unchanged.
For that deterministic infrastructure case, the complete serialized reconcile requests are
42,672 bytes for findings, 42,124 for remedy and 42,070 for outcome. The previous remedy and
outcome requests were 82,152 and 82,098 bytes respectively; the snapshot itself is 41,091 bytes.

This continuation adds one native test with two Scenario executions and expands one existing
Python test by four installed-adapter invocations. The expanded Python test took 3.607 seconds.
The native test stopped at planning in 1.481 seconds, so its successful-path cost remains
unmeasured. No new binary, sleep, retry or paid call is added, and production startup,
source capture and Store replay are unchanged.

All 44 CI-health Python tests passed. The complete focused native, structured-runtime,
reply-schema, schema and Scenario selection ran 346 tests: 281 passed and 65 failed. All 218
schema parity tests, four shipped-schema tests and eleven structured-runtime/runner/Store tests
passed. Twenty-one binary fixtures were refused by the sandbox's `/opt` Provider-state ancestor
ownership; twelve Scenario fixtures refused outside-checkout authority; twenty-six refused
outside-checkout state; five adapter installers refused private paths; and one performance
Scenario fixture refused the unchanged 10-GiB free-disk floor against 4.0 GiB in `/tmp`.
The watchdog Python suite passed ten of eleven, with its notifier refused by `/tmp/.git`.
These are failed environment receipts; successful native lifecycle execution remains unproven
in this sandbox. No authority, fixture, gate or budget was relaxed. Independent read-only
correctness review and acceptance evaluation found no implementation gaps. The enclosing Task
runs its pinned fast gate after sealing; this Worker does not duplicate that whole-suite gate.

The native large-scope regression additionally exercises the Task's diagnosis notification,
whose original full settlement observation repeated the 40,008-character repository already
bound by the request subject. The initial notification evidence transport removed that duplicate
scope; the bounded reference transport described above supersedes it. The existing native refusal and category-routing unit tests check the serialized request
bound, repository binding, unchanged original inputs, exact settlement references and schema parity
for accepted and refused diagnoses. The installed-adapter regression
`test_large_scope_native_notification_transport_retains_refusal_and_rejects_old_evidence`
checks diagnosis, alert-only remedy and outcome with the same large scope and refuses malformed
or earlier evidence envelopes. The actual adapter input bound remains 65,536 bytes.

This adds one functional Python method with seven installed-adapter calls (1.105 seconds alone)
and seven in-process transport cases across two existing Rust tests. Those expanded Rust tests
took 0.629 and 3.097 seconds, including their existing work. No native Scenario execution, test
binary, sleep or paid call is added. All 45 CI-health Python tests passed in 34.549 seconds.
The focused native, contract, runtime, Store and shipped-schema selection ran 36 tests: 22 passed
and 14 failed. Thirteen binary fixtures were refused before Scenario execution because the
sandbox exposes `/opt` as owned by uid 65534, outside the unchanged Provider-state ownership
boundary; the watchdog journal fixture was refused because its state lies under a Git checkout.
The exact large-scope native regression has the same Provider refusal, so a successful complete
native Run remains unproven here. Formatting, workspace all-target Clippy, the test-class audit
and changelog checks passed. Markdown lint could not run offline without the cached npm response.
These are environment refusals, not timing flakes. The enclosing Task still runs its pinned fast
gate and independent review/evaluation after sealing; no live effect or publication is performed.

The final refusal-bound regression
`ci_health_near_maximum_snapshot_and_maximal_refusal_diagnostics_complete_native_lifecycle`
drives the native Scenario runtime and installed adapter with a complete snapshot whose repository
is 61,008 bytes and two maximum 2,048-character refusal prefixes containing JSON escapes. It checks
both reconcile and execute request sizes for diagnosis, alert-only remedy and outcome, completes
the Run, preserves the original settlement through replay and retains the exact native references
in every durable notification. The in-process transport cases additionally cover twenty admitted
refusals and accepted output references; the Python installed-service case rejects old versions,
missing or invalid references, foreign repository bindings and duplicate refused Attempt IDs.

This continuation adds one e2e Rust test with nine installed-adapter invocations and one shared
Python observer/compiler setup; it took 1.022 seconds in the focused run. Two existing Python
methods add six installed-adapter invocations in total and took 2.517 seconds together. Two
in-process maximum-refusal cases extend the existing transport test. There is no new test binary,
model call, sleep or scheduling override. Ordinary adapter requests serialize without cloning;
only CI notification transport is projected. Startup, source capture and Store replay are unchanged.

All 45 CI-health Python tests passed after the reference transport changes. The broader focused
Rust selection ran 335 tests: 278 passed, including all 218 schema tests and the new native
lifecycle regression. Its 57 failures were environment refusals: thirteen Provider-state ancestor
ownership checks, twenty-six outside-checkout state checks, twelve outside-checkout authority
checks, five adapter installations and one unchanged 10-GiB free-disk floor. The watchdog Python
suite passed ten of eleven; its notifier was refused under `/tmp/.git`. Additional Scenario Python
suites encountered the same private-path checks or the sandbox's Unix-socket prohibition.
Workspace all-target Clippy, formatting, test-class audit and changelog checks passed.
Markdown lint could not run offline because the npm package was not cached. The enclosing Task
runs the pinned fast gate and independent review/evaluation after sealing; this Worker does not
duplicate that whole-suite gate. No live call, activation or publication was performed.
