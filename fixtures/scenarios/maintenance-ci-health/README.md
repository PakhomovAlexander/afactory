# Hermetic CI health Scenario

This is the inert Phase 1 `maintenance-ci-health` vertical slice. The
[operator guide](../../../docs/ci-health-maintenance.md) explains the contracts and boundaries.
`scenario.json` is an `af/scenario-bundle@6` example with placeholder native Task identities;
`inputs.json` supplies its exact typed repository/ref/release/window/notification scope.
It does not register an Ant, install systemd, dispatch a remedy or grant publication.

`failing-service.json` contains bounded observation evidence; `github-service.json` supplies the
exact GET routes for collecting the same observation. `snapshot.json` is their deterministic
`af.ci-health-snapshot/1` result. `task/` contains an ordinary structured read-only diagnosis kind,
Worker and Pipeline, with the published constraints copied exactly (only `$id` and `$schema` resolution metadata
are removed for the local-only Worker validator). A consuming catalog must pin
all three package digests and explicitly bind the Worker Provider. `task/catalog.toml` records
those pins; a test checks them against the exact `af catalog digest` command output. No digest
is hand-invented.

`release-github-service.json` has distinct main and release heads. Its main failure sorts first,
followed by a release-blocking macOS job with separate log evidence. Native tests also make main
green so the release job alone reaches the Task: Selection, diagnosis and evidence must name
the release head, and main is refused for that occurrence.

The diagnosis Task reserves exactly 5,096 tokens and two Attempts: the mandatory Provider
admission reserves 4,096 tokens and one Attempt, and the diagnosis Worker reserves 1,000 tokens
and one Attempt. The native Task target and Scenario token caps retain that same allowance.
The fixture adds no retry allowance; the contract and admission costs remain unchanged.

`configure.py` renders the complete bundle from an exact snapshot artifact ID and already pinned
native Task target. It selects report-only steps for healthy, incomplete or unknown-signature
observations. Exact pinned infrastructure/transient signatures retain a deterministic diagnosis
without a Task. Other complete evidence reserves one diagnosis Task; further occurrences are
reported as unknown because the diagnosis reservation is exhausted. The native `ci_diagnosis`
Task specialization records accepted output or a separate deterministic unknown containing the
original refused Task receipt and bounded diagnostic evidence. Native field projections feed category
and signature comparisons. `gate-evidence.json` holds current native Task-list rows and an offline
inventory: the existing Q2 resolver/scan supplies the two-distinct-Task quarantine predicate.
Seven remedy rows exclude all earlier matches, consume the exact
native settlement and only send notifications. Findings are sent immediately before diagnosis.
`af.ci-health-notification/2` batches every observed job's identity, platform, conclusion, head,
log references and release-blocking flag within the existing 64 KiB serialized request bound.
Its explicit `release_alerts`
include every release-blocking occurrence, even when unselected or diagnosis is exhausted.
Version 1 notifications fail closed. Refusal always selects alert-only and sends
diagnosis, remedy and outcome with the deterministic unknown. It skips Watch even when requested
by a fixture. Accepted diagnoses retain their accepted Task receipt; refusals never acquire one.
Findings and outcome precede Watch.

`adapter.py` is installed by `configure.adapter_bytes` into a disposable private machine
directory. That function embeds the observer and schema interpreter in one executable, so the
machine policy digest pins every helper byte. It recomputes the exact observation, records
idempotent fixture notifications and observes fixture PR state. It has no GitHub transport or
mutation operation. `watchdog-notifier.py` independently models an idempotent notification
service, including losing a response after retaining its receipt.

Executable coverage uses the real af binary, native Task Store and fake model protocol in
`crates/af/tests/it/task_custom.rs`; every af process uses the private HOME/XDG helper. The
Python suites cover the service, normalization, recurrence, all overlapping remedy predicates
and independent watchdog. No live GitHub write or paid Provider call is involved.
