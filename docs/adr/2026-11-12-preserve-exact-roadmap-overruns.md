# ADR-2026-11-12-preserve-exact-roadmap-overruns: Preserve exact roadmap overruns

Status: proposed
Date: 2026-11-12

## Context

The [within-Run repair](2026-11-12-keep-within-run-accounting-wide.md) left a reachable
composition boundary: a goal-claimed Run settling 17 tokens and recovering u64 maximum
rolled back in roadmap reconciliation. Roadmap validation also rejected individual actual
charges above the JSON safe-integer bound. The recorded failure remains historical evidence.

## Options considered

- Clamp actual charges, discard receipts or release other unknown reservations: rejected because
  these lose spending or uncertainty.
- Raise budgets or bypass replay validation: rejected because recovery grants no dispatch authority.
- Widen only Run accounting: insufficient; roadmap progress and its admission windows consume it.
- Persist exact roadmap totals as canonical decimal u128 and compare wide totals: selected.

## Decision

Roadmap progress stores `actual_tokens` as canonical decimal u128. Individual native charges
remain exact u64 counters, including recovered overruns; their schemas describe that existing
writer range. Reservations and configured limits remain bounded safe integers. Validation
recomputes actual charges and accepted receipt sums in checked u128, preserving all provenance.
The remaining loop allowance subtracts actual spend in u128 before narrowing the bounded remainder.
Unknown Task reservations remain independently held even after another charge exceeds the loop.

Total, UTC day and rolling-window comparisons use checked u128 sums of actuals and reservations.
Overruns remain replayable and block subsequent admission; expired windows cannot reset cumulative
spend. Revisions cannot lower or reset committed accounting. Goal-claimed Runs remain protected
from retention, preserving their native receipts; ordinary Ant retention remains unchanged.

Under [ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md),
the roadmap and list outputs become version 3 and the enclosing Scenario Store becomes version 9,
including its ownership marker. Previous versions fail closed without migration. Proposal and
revision authority retain version 2 because their shapes and content identities are unchanged.
Progress schema definitions move into the version 3 roadmap schema. Public inspection emits
canonical decimal totals, including zero; native individual counters remain JSON u64 integers.
Consumers must preserve those integers exactly.

## Consequences

A private-Store native regression exercises fresh Task planning, known settlement, lost response,
recovery, replay, retained unknown usage, manual and Ant refusal, and retention protection.
A single-charge regression crosses the safe-integer boundary. Existing within-Run, across-Run,
roadmap window and ordinary settlement tests remain required. This repair grants no publication,
Provider or benchmark authority and makes no performance claim.
