# Hermetic CI health Scenario

This is the inert Phase 1 `maintenance-ci-health` vertical slice. The
[operator guide](../../../docs/ci-health-maintenance.md) explains the contracts and boundaries.
`scenario.json` is an `af/scenario-bundle@6` example with placeholder native Task identities;
`inputs.json` supplies its exact typed repository/ref/release/window/notification scope.
It does not register an Ant, install systemd, dispatch a remedy or grant publication.

`failing-service.json` contains bounded observation evidence; `github-service.json` supplies the
exact GET routes for collecting the same observation. `snapshot.json` is their deterministic
`af.ci-health-snapshot/1` result. `task/` contains an ordinary structured read-only diagnosis kind,
Worker and Pipeline, with the published validation constraints preserved in local schemas.
The diagnosis schema uses closed-object property counts and omits redundant nonempty/uniqueness
keywords to reduce request overhead; schema
parity and native response-projection tests prove the constraints remain equivalent.
`$id` and `$schema` resolution metadata are removed for the local-only Worker validator.
A consuming catalog must pin
all three package digests and explicitly bind the Worker Provider. `task/catalog.toml` records
those pins; a test checks them against the exact `af catalog digest` command output. No digest
is hand-invented.

`release-github-service.json` has distinct main and release heads. Its main failure sorts first,
followed by a release-blocking macOS job with separate log evidence. `release-snapshot.json` is
their deterministic two-failure observation, shared by the Python observer and native context tests.
Native tests also make main
green so the release job alone reaches the Task: Selection, diagnosis and evidence must name
the release head, and main is refused for that occurrence.

The diagnosis Task reserves exactly 24,096 tokens and two Attempts: the mandatory Provider
admission reserves 4,096 tokens and one Attempt, and the diagnosis Worker reserves 20,000 tokens
and one Attempt. The native Task target and Scenario token caps retain that same allowance.
The fixture adds no retry allowance. Native planning refuses 24,095 tokens or one Attempt before
Provider admission. The evidence bounds and admission cost are unchanged.

`max-snapshot.json` contains 100 failures in a complete observation with exactly 65,536 bytes of
compact serialized JSON, including UTF-8 and escaped text. The full native request adds 2,078 bytes
for the pinned instructions, protocol, output schema, Selection Requirements and artifact wrappers:
67,614 bytes, or 16,904 estimated tokens at the native ceiling of bytes divided by four. Snapshot
JSON is embedded as a value, so this overhead is independent of its contents. The 20,000-token
Worker reservation leaves 3,096 tokens beyond the largest admitted input estimate for its answer.
This is a bounded reservation, not measured Provider usage. Native context checks still refuse
requests exceeding it. The context regression renders and admits this exact input; the binary
execution regression requires the selected occurrence's accepted diagnosis and settled accounting.

`configure.py` renders the complete bundle from an exact snapshot artifact ID and already pinned
native Task target. It selects report-only steps for healthy, incomplete or unknown-signature
observations. Exact pinned infrastructure/transient signatures retain a deterministic diagnosis
without a Task. Classification runs over every occurrence before the first unresolved occurrence
reserves the one diagnosis Task. Only additional unresolved occurrences are reported as unknown
because the diagnosis reservation is exhausted. The native `ci_diagnosis`
Task specialization records accepted output or a separate deterministic unknown containing the
original refused Task receipt and bounded diagnostic evidence. Native field projections feed category
and signature comparisons. `gate-evidence.json` holds current native Task-list rows and an offline
inventory: the existing Q2 resolver/scan supplies the two-distinct-Task quarantine predicate.
Seven remedy rows exclude all earlier matches, consume the exact
native settlement and only send notifications. Findings are sent immediately before diagnosis.
`af.ci-health-notification/5` batches every observed job's identity, platform, conclusion, head,
log references and release-blocking flag within the existing 64 KiB serialized request bound.
The transport subject and `repository_id` bind the repository by canonical digest; durable reports
restore the full repository. Later phases reference already delivered findings details. Native
settlement evidence version 2 retains exact receipt, output and diagnostic references and explicitly
names the details kept in the native Run. Earlier notification and evidence versions fail closed.
The batches' `deterministic_results` retain every available deterministic diagnosis and its first
matching remedy `row` and `intended`, bound to the exact occurrence, signature, failed head,
snapshot and evidence. Further unresolved occurrences receive a deterministic unknown and
alert-only intent naming the exhausted reservation. The selected Task's pending diagnosis is
absent from that array. Summaries and results can occupy separate batches; findings omit the
repeated single `deterministic_diagnosis` field. The 20-dispatch limit and single Task reservation
are unchanged. Every lifecycle notification carries repository scope once in the native
request's exact `subject`; the reader restores it in the durable notification and refuses a
conflicting body repository.
Its explicit `release_alerts`
include every release-blocking occurrence, even when unselected or diagnosis is exhausted.
Version 1, 2 and 3 notifications fail closed. Refusal always selects alert-only and sends
diagnosis, remedy and outcome with the deterministic unknown. It skips Watch even when requested
by a fixture. Accepted diagnoses retain their accepted Task receipt; refusals never acquire one.
Findings and outcome precede Watch.

`adapter.py` is installed by `configure.adapter_bytes` into a disposable private machine
directory. That function embeds the observer and schema interpreter in one executable, so the
machine policy digest pins every helper byte. The observation Wait condition carries
`ci_observe_v1:<snapshot-content-digest>`; it refuses changed evidence even at unchanged main
before diagnosis or notifications. It recomputes the exact observation, records
idempotent fixture notifications and observes fixture PR state. It has no GitHub transport or
mutation operation. `watchdog-notifier.py` independently models an idempotent notification
service, including losing a response after retaining its receipt.

Executable coverage uses the real af binary, native Task Store and fake model protocol in
`crates/af/tests/it/task_custom.rs`; every af process uses the private HOME/XDG helper. The
Python suites cover the service, normalization, recurrence, all overlapping remedy predicates
and independent watchdog. No live GitHub write or paid Provider call is involved.
