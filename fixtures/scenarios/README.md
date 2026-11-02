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
