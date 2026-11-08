# ADR-2026-11-06-name-shared-libraries-for-af-responsibilities: Name shared libraries for AF responsibilities

**Status:** proposed (2026-11-06).

## Context

Afactory's libraries now serve common Task execution as well as Review. Their `review-` package
names imply narrower ownership than their responsibilities. In particular, contracts are shared
across Task kinds, the runtime executes Tasks, configuration captures Pipeline authority, and the
external ticket source captures issues rather than executing Tasks.

This is an implementation naming decision. The protected identities and vocabulary in
[ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md) remain
unchanged, as do the dependency boundaries, including the process leaf in
[ADR-0026](0026-share-process-supervision-through-a-leaf-crate.md).

## Considered options

- **Keep the existing library names.** Rejected: they obscure the common Task responsibilities
  and suggest that neutral infrastructure belongs to reviewer execution.
- **Replace only the prefix with `af-`.** Rejected: `core`, `pipeline`, `config` and `source-task`
  still leave the contracts, runtime, Pipeline configuration and issue capture roles unclear.
- **Keep compatibility aliases or shim crates.** Rejected: these internal workspace names have
  no compatibility obligation, and duplicate names would obscure ownership and dependency audits.

## Decision

Rename each library package and its directory together, updating Rust imports and all active
build, feature, test, performance and documentation references:

| Previous package | Package |
| --- | --- |
| `review-core` | `af-contracts` |
| `review-graph` | `af-graph` |
| `review-config` | `af-pipeline-config` |
| `review-pipeline` | `af-runtime` |
| `review-attempt` | `af-attempt` |
| `review-runner` | `af-runner` |
| `review-runner-claude` | `af-runner-claude` |
| `review-runner-codex` | `af-runner-codex` |
| `review-runner-pi` | `af-runner-pi` |
| `review-source-git` | `af-source-git` |
| `review-source-task` | `af-source-issue` |
| `review-check` | `af-check` |
| `review-sandbox` | `af-sandbox` |
| `review-store` | `af-store` |
| `review-process` | `af-process` |
| `review-parallel` | `af-parallel` |
| `review-telemetry` | `af-telemetry` |

The `af` package and binary stay named `af`. Rust import names use Cargo's corresponding
underscores. No library is split, merged or given a new dependency edge by this decision.
Closed workspace inventories, test selectors and protected performance harness paths use the
renamed packages; generated harness fixtures are regenerated through the existing digest tool.
CI selection, coverage, budgets and isolation retain their existing semantics.

## Consequences

Source navigation and local Cargo invocations use the responsibility names. Existing accepted
ADRs, historical measurement records and published release notes keep their historical names.
Active unreleased change notes and the `default/implementer` and `default-light/implementer`
instructions use the renamed crates. Those two Worker packages receive patch version updates
and refreshed catalog pins from `af catalog digest`; their signatures, authority, budgets and
selection rules stay unchanged. Integration with current main retains its GPT-6.1 bindings and
advances those two instruction-bearing packages to `default/implementer@1.0.2` and
`default-light/implementer@1.0.4`, with native digest refreshes over the combined bytes. Newly
added owner-default instructions already use current navigation and retain their upstream pins.
Unrelated Task packages retain their current-main bytes and pins.

Runtime behavior, CLI commands, persisted type strings, events, schema IDs and JSON shapes do
not change. In particular, `review.kernel/*` and `af/*` identities remain exact. There is no
migration, compatibility reader, alias, shim crate, third-party dependency or release change.
