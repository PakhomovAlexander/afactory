# ADR-0204: Preserve model authority in repository planning fixtures

**Status:** proposed (2026-10-24). Applies
[ADR-0198](0198-opt-this-repository-into-distinct-model-worker-independence.md) when reconciling
the fixture adjustments of
[ADR-0201](0201-admit-explicit-ninety-minute-checks-and-isolate-command-catalog-fixtures.md).

## Context

The experiment, report and remote-twin planning fixtures now retain the repository's Model
Workers, with isolated fake Provider probes and exact model identities. The Provider disable
feature had adjusted those same fixtures for earlier Command stand-ins. Applying that adjustment
to the Model fixtures disables the very model-diversity policy their assertions must exercise.
The two branches also assigned different decisions the same ADR numbers.

## Considered options

- Relax the Model fixtures' policy assertions: rejected because they must prove planning under
  the repository's trusted authority, including refusal of same-model and Command replacements.
- Restore Command stand-ins everywhere: rejected because that loses the model identity,
  admission reservation and token-free planning coverage of ADR-0198.
- Scope the existing Command helper to disposable Command authority: chosen; it remains
  available without changing the Model planning fixtures or production policy.

## Decision

Remove the Command-only independence adjustment from the three Model planning fixture setups.
Preserve their model bindings, admission reservations, tool PATH helpers, zero-Attempt planning
and refusal assertions. Exercise the retained Command helper separately on a disposable catalog,
checking that only its independence table changes and that production still requires model
diversity. Production keeps `distinct_principals = false` and `distinct_models = true`.

Keep upstream ADRs 0196 through 0198 unchanged. Renumber the five feature decisions to 0199
through 0203, preserving their content and updating their headings, links and index entries.
No runtime contract, schema, check command, budget, workflow or installed authority changes.
The enclosing Task's native remote gate, independent reviews and evaluator decide acceptance
of the complete merged feature.

## Consequences

Both fixture types retain their intended authority. Model planning continues to test the
repository's real policy without dispatching a Worker, while Command fixtures can explicitly
select package independence in private copies. The ADR index has unambiguous decision numbers.
