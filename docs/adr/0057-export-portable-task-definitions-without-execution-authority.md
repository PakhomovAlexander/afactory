# ADR-0057: Export portable Task definitions without execution authority

**Status:** accepted; superseded in part by [ADR-0113](0113-ga-reads-only-what-ga-writes.md):
readability of previously captured locks.

## Context

P12 turns an inspected generated Pipeline into a reusable Git package. Exporting the selected
effective closure would copy local Worker replacements and could make a provider account or
originating Task value part of the reusable definition. Package names also form a hierarchy:
placing a parent package directly above its child makes their file digests overlap.

## Decision

Export is an absent-destination file operation, separate from developer decisions, Task execution
and catalog activation. A private inspection compiler restores captured authority, checks the
durable selected Planner proof where present, and recompiles the final plan with its recorded
bindings. This compiler never returns a live host or Provider capability. Current account login
is unnecessary; the recorded compatible engine and original CAS data remain necessary.

Walk the original Pipeline defaults and child Calls, including the selected shared Task-kind
package. Exclude local overrides and preparation machinery. Reject direct defaults in reserved
namespaces when no portable shared default exists. Typed public input ports carry future Task
values; export preserves applicability, coverage and resource declarations. No Task input,
approval or provider binding is serialized into the bundle. Known originating Task references
embedded in definitions or package files cause refusal rather than silent semantic rewriting.

Rename the exported root and generated children consistently, preserve shared dependency names,
and calculate pins from the resulting package bytes. Give each package a separate directory
derived from its complete name. Add the optional shared-catalog `path_base = "manifest"` mode
for movable bundles; omitted `path_base` retains the existing repository-relative meaning.
Imports and package paths share the selected base and remain inside the exact Git repository.

Emit closed contract fixtures and provide a token-free local-Git contract-test command. Its
report distinguishes interface/schema validation and prerequisite discovery from actual Task
acceptance. A later developer reviews, tests, commits and explicitly imports the bundle before
using it as an existing Pipeline. Export never satisfies the originating plan's approval gate.

## Consequences

Generated definitions can become existing catalog fits without another Planner invocation.
Local Worker tuning and provider account state stay outside the shared defaults. Shared verifier
policy digests remain explicit compatibility constraints. A package-name prefix cannot change
another package's digest during export or sync. Existing captured locks remain readable.

The deterministic handoff fixture exports an unapproved nested plan, proves the original Task
unchanged, moves and commits the bundle, checks its contracts, and executes a matching Task in a
second checkout with no Planner call. Separate cases refuse preparation, embedded private Task
text, overwrite and symlink/path escapes. The starter pack remains a separate P12 deliverable.
