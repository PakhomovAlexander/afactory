# Non-fixed resolutions are scoped and challengeable

**Status:** accepted (2026-08-20)

`rejected` and `wontfix-tracked` are trusted Resolutions over exact claims, Evidence, Subject
scope, and policy revision, not permanent status toggles. `wontfix-tracked` also records a
severity ceiling, tracking reference, and expiry. A later materially new Evidence digest, higher
severity, out-of-scope Subject, or persisted expiry emits a resolution challenge and moves the
Finding to `contested`; an exact duplicate within scope remains terminal history.

## Consequences

- Resolution requests optimistically name the expected current Finding view and are idempotent.
- Expiry uses persisted policy-evaluation time. Replay of a fixed event set never consults the
  host clock.
- A declined Finding cannot suppress a later blocker merely because both Reports share an
  occurrence key or historical legacy alias.
