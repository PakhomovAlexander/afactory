# ADR-0232: Measure telemetry interruption from the selected fixture boundary

**Status:** accepted (2026-10-26)

## Context

The five-operation collection interruption fixture used a one-second cycle deadline with a
0.4-second terminal reserve. Git authority expansion, installed verification and retained
history admission could consume the work interval before the selected operation started.
The captured full gate failed the entry assertions for verification, authority retention and
history rendering; a timeout before those boundaries does not test their interruption.

## Decision

Separate preparation from measured work in this fixture with a test-owned clock that stays
at zero until the selected operation is entered. Execute preparation and admission for real.
Only this fixture defers the collection timer until that boundary; the selected blocking
operation uses the original production `Budget.enforce` and real SIGALRM interruption.
The clock then advances with real monotonic time through terminal recording and cleanup.

Keep the one-second budget, 0.4-second reserve and 2.5-second measured elapsed assertion.
Assert the preparation clock, unchanged limits, exactly one entry, an actual interruption
and its recorded diagnostic. Preserve persisted incomplete accounting, partial-import refusal,
fully verified import before history rendering, and subsequent lock acquisition. Existing
whole-cycle tests continue to bound preparation, Git processes, transport and terminal work.
No production deadline, verification, admission, ownership or telemetry semantics change.

## Considered options

- Increase the cycle allowance or retry failures: rejected because entry would still depend
  on machine speed and the original interruption contract would lose coverage.
- Fake successful verification or inject a timeout exception: rejected because neither proves
  real admission followed by actual deadline enforcement at the chosen boundary.
- Pre-import every result: rejected because that bypasses the import boundaries being tested.

## Consequences

Repeated focused runs exercise all five entries without requiring unrelated setup to fit into
0.6 seconds. Their elapsed assertion covers the selected operation and terminal cleanup;
preparation is explicitly outside that measurement. These fixture runs are not full-gate or
hosted performance evidence. The AF kernel runs the complete gate, independent reviews and
acceptance evaluation after sealing the candidate. Protected harness identities are regenerated
with the prescribed generator; `.af/` authority remains unchanged.
