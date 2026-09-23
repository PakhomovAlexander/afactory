# Report authority failures explicitly

**Status:** accepted (2026-09-23); superseded in part by
[ADR-0113](0113-ga-reads-only-what-ga-writes.md): the permanent `RunReport@1` and `@2` readers,
and `RunReport@3` itself; its `authority_unavailable` reason is part of `RunReport@6`.

Report Scope depends on durable Subject and Report artifacts. If either authority is unreadable,
the affected claim remains active with unknown Scope and convergence fails closed. `RunReport@2`
could persist only `not_converged` or `exhausted`, however, so replay could not distinguish an
ordinary unresolved finding from missing authority. The CLI warning was useful at the time of the
run but was not a durable explanation.

We decided that new conclusions use `RunReport@3`, whose failure-reason vocabulary adds
`authority_unavailable`. The convergence projection records the number of authority failures in
the active clean window. A non-exhausted failure uses `authority_unavailable` only when that count
is nonzero and no real blocking Finding, new Finding, or failed Gate is already the immediate
cause; otherwise it remains `not_converged`. Existing `RunReport@1` and `RunReport@2` readers
remain permanent and unchanged. A Report whose round disagrees with the active Subject binding is
recorded as a distinct round-binding authority failure, not as an unavailable Subject artifact;
it still counts in the same fail-closed convergence total.

## Considered options

- **Mutate `RunReport@2` to add a reason.** Smallest code change, but an accepted value set is part
  of the payload contract. Rejected because ADR-0002 requires payload-shape changes to bump the
  event type and old consumers must keep interpreting @2 exactly as published.
- **Keep `not_converged` and print a warning only.** Preserves the schema, but loses the cause when
  output is not retained and leaves replay unable to explain the verdict. Rejected because the
  event log is the source of truth.
- **Add authority diagnostics directly to the report payload.** More detailed, but duplicates the
  existing authority-failure ledger and couples the conclusion contract to diagnostic shape.
  Rejected until a concrete consumer needs that detail; the stable reason plus ledger evidence is
  sufficient.
- **Write `RunReport@3` with an explicit reason (chosen).** Additive, replayable, and compatible
  with permanent older readers.

## Consequences

- New runs write `RunReport@3`; @1 and @2 remain accepted on replay.
- `authority_unavailable` is distinct from scheduler incompleteness: node failures still produce
  an incomplete verdict, while missing Scope authority is a completed run that fails closed.
- Exhaustion remains the terminal reason at the configured round limit. The authority-failure
  count and warning still identify why convergence could not be reached before exhaustion.
- Real Finding and Gate failures retain precedence, so an accompanying diagnostic cannot replace
  the actionable durable cause with `authority_unavailable`.
- Operator diagnostics distinguish an unreadable Subject or Report artifact from an inconsistent
  Report-to-Round binding; all three leave affected Scope unknown and block convergence.
- Report readers, the event vocabulary schema, campaign terminal validation, and receipt checks
  must recognize all three report versions.
