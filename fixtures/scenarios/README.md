# Inert Scenario and Ant examples

These are definition files, not registered or active instances. They contain placeholder Task
plans, machine file roots and service destinations. Nothing installs a schedule or grants
publication authority by reading them.

`issue-fix.json` exercises all five step kinds: an existing Task, a pinned child Scenario, exact
checks, a signed owner decision, an authorized publication adapter, merge observation and safe
owned-file cleanup. The child also records a conditional no-change skip. Completion compares the
actual Task conclusion with `verified`; a successful process alone cannot complete it.
`local-decision.toml` is the equivalent human authoring path, with a signed-decision completion
condition. `inputs.json` supplies typed scalar data. `daily-ant.json` binds the same bundle to an explicit
timezone with skip policies and per-run and rolling limits.

`service.py` is a credential-free adapter fixture, installed only into disposable test machine
directories and pinned there. It models service receipts and exact-head checks. Its deliberate
lost-response switch proves reconciliation through the actual bounded subprocess interface;
it never creates a real PR or follow-up. A production adapter is installed, audited, authorized
and pinned separately under the [bounded adapter contract](../../docs/scenarios.md).

The executable evidence is `crates/af/tests/it/scenario.rs`: it replaces the placeholders with a
private existing Task fixture, runs the real CLI across process boundaries and verifies receipts,
restart recovery, cancellation, stale evidence refusal and preservation of unexpected content.

The optional production adapter, offline installer and explicit private machine configuration are
documented in [GitHub Scenario adapter](../../docs/scenario-github-adapter.md). `github/` contains
inert v1 config/payload examples and a signed publication → green → owner review → merged →
follow-up bundle. The values are test placeholders; it is neither registered nor scheduled.
`github/mock-service.json` and `github-transport.py` are credential-free service mocks used by
Python tests and the real CLI production-adapter tests. Never install the mock transport in
production. Test-installed executables contain all production application bytes plus the mock
transport, and the existing machine policy pins their complete digest.
