# ADR-2026-11-06-separate-scenario-domain-from-cli: Separate Scenario domain from the CLI

**Status:** accepted (2026-11-06).

## Context

Scenario contracts, durable Runs, Ant scheduling and execution orchestration lived inside the
`af` binary beside clap parsing and presentation. The domain called concrete Task commands and
machine Store services directly. That coupled deterministic admission and journal replay to the
application even though neither owns command parsing or terminal output.

## Considered options

- Keep the domain in the binary and re-export its modules: rejected because a library cannot
  depend on a binary and the dependency direction would remain implicit.
- Move Task commands, Project services and machine configuration into the Scenario library:
  rejected because they have application responsibilities beyond Scenario coordination.
- Copy native Task parsing and planning into Scenario: rejected because two implementations
  could disagree about native acceptance, authority or diagnostics.

## Decision

1. `af-scenario` owns Scenario and Ant contracts, pins and identity algorithms, durable state
   and journal replay, scheduling, roadmap admission, template instantiation, bounded adapter
   dispatch and execution orchestration. It has no dependency on the `af` binary.
2. An explicit `Host` interface supplies native Task document validation, predecessor and source
   checks, planning, execution, recorded-run cancellation, exact accounting and performance proof
   validation. It also supplies machine Store locations, path resolution, ownership preparation,
   private directories, registration and a collection guard held for the dispatch lifetime.
   Template validation remains required during admission and every journal replay.
3. The binary implements that interface using its existing Task and Storage services. It retains
   clap parsing, terminal and JSON presentation, concrete native Task/application adapters and
   source capture commands. TaskRuntime and Project services keep their existing boundaries.
4. The library retains all pinned machine authority, signature, reconciliation, deadline, budget,
   ownership and publication checks. A host is an application dependency, never an additional
   publication grant. External adapter execution still requires the exact captured machine policy
   and Scenario authority. The low-level effect dispatcher remains crate-private, reached through
   durable runtime orchestration; extraction installs no adapter or credentials.
5. Domain and schema parity tests move with their implementation, using an explicit isolated host
   for native services. End-to-end Scenario CLI tests retain the real native adapter boundary.
   Focused boundary tests check template refusal during admission/replay and recovered accounting.

## Consequences

Runtime behavior, CLI spellings, Scenario/Ant wire shapes, persisted type strings, event names,
schema IDs, signatures and IDs stay unchanged. There is no migration, compatibility reader or
alias. The workspace gains one internal crate and no third-party dependency. Test classification
and protected performance build inventories include the new crate; existing gates and isolation
rules remain unchanged.
