# A payload shape change bumps the event type version

**Status:** accepted (2026-08-20)

`RunReport@1` persists `format!("{verdict:?}")` and `format!("{reason:?}")` — Rust `Debug`
output — into the append-only event log, and `reviewctl run` reads the verdict back with
`.starts_with("Incomplete")` to decide how many rounds have closed. A Rust enum variant name is
therefore load-bearing for convergence arithmetic, through a `Debug` impl, in the system of
record. Renaming `RunVerdict::Incomplete` would make incomplete rounds start *closing* rounds and
consuming `max_rounds`, with no compile error anywhere.

Fixing that means giving the payload a defined shape, which is the first payload change the
kernel has ever made: all fourteen event types are `@1` and the version suffix has never been
exercised. We decided that a payload shape change bumps the version — `RunReport@2` carries the
typed verdict, `RunReport@1` is frozen with its legacy shape, and readers handle both. The event
type enum from the M3.8 work lists both.

## Considered options

- **Tolerant reader on `RunReport@1`.** One type, a payload that is a string on old events and an
  object on new ones. Rejected: `@1` would denote two incompatible shapes, which is exactly what a
  versioned contract exists to prevent, and the schema would have to permit both — so it could
  reject neither. That is the same criticism the README already levels at one-directional schema
  tests: "a schema that accepts everything passes".
- **Break in-flight campaigns.** Refuse to replay a campaign written by an older `reviewctl`, and
  require open campaigns to be finished or abandoned before upgrading. Cleanest code by far — one
  shape, one schema, no legacy arm. Rejected because it sets the precedent that "append-only and
  replayable" holds only within a release, and it lands the cost on an operator who is mid-way
  through triaging a review.
- **Bump to `RunReport@2` (chosen).** Uses the mechanism the type names have carried from the
  start. Every campaign on disk keeps replaying, nothing is migrated or rewritten, and the
  convention is established once for every payload change that follows.

## Consequences

- The event type vocabulary now carries versions, and grows by one entry every time a payload
  moves. That is the intended cost of an append-only log.
- The `@1` reader arm is **permanent**. A log is append-only, so readers for old shapes live as
  long as the logs do; they may not be deleted when they stop being written.
- Each event type needs a payload schema for this to mean anything. `RunReport@2` gets one now;
  the others are defined as they are touched, which is the plan the README already stated for the
  type vocabulary.
- The round counter reads `verdict.kind` structurally on `@2` and keeps the prefix match only on
  the `@1` arm, where the string is frozen and can no longer drift.
- A test must pin both arms against a stored fixture of each shape — a reader that only ever sees
  the new shape proves nothing about the old one.
