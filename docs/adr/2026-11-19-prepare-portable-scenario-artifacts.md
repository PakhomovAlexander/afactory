# ADR-2026-11-19-prepare-portable-scenario-artifacts: Prepare portable Scenario artifacts

**Status:** proposed (2026-11-19).

## Context

A native fresh Task template carries an absolute checkout, a local Task Store and durable
authority from that Store. This makes an exact run replayable on its receiver. Sharing that
template with another developer would also share machine-specific paths and identities.

## Considered options

- Distribute native pins. Each receiver would need the author's Store and source identities.
- Install an artifact's packages in the receiver catalog. This would require a registry change
  and would make the receiver's other Tasks select from the imported packages.
- Resolve the artifact into native authority during explicit local preparation. This reuses the
  Task capture and Scenario execution protocol and keeps the receiver catalog intact.

## Decision

Introduce `af/scenario-artifact@1` for a bounded local report Scenario. The definition declares
a fresh report Task, its named result, limits, and typed named inputs. The artifact directory
carries the Task kind, Pipeline, model Workers and their schemas, pinned by native package
digests. Separately named schema sources have byte digests. Preparation reads the committed
receiver tree and resolves dependency paths below the artifact directory through its Manifest.
Missing files, symbolic links, mismatched digests and escaping paths refuse preparation.

`af scenario prepare` uses the receiver's selected checkout and Task Store. It stages exactly
the artifact's package closure for this template and retains receiver report policy, Provider
aliases, admission costs and independence rules. Explicit local bindings may supply Provider
aliases. Workers are read-only models, and the Pipeline uses native report operators.
Preparation seals the exact caller sources as native report inputs and records the normal
template pin. It spends zero model tokens. The prepared bundle includes the request for
template authority. The receiver's owner-only machine policy must grant it, and the operator
must confirm the native Scenario revision before starting the Run.

The prepared bundle uses the existing native Scenario, pin, Run and Store versions. Each
receiver captures its own source and packages. Native Task planning, Provider admission,
execution limits, report checks, acceptance, and result projections govern the Run.

## Consequences

The same committed artifact can be prepared with separate checkouts, inputs and Stores. A
prepared bundle belongs to the checkout and Store recorded in its template. Another issue
requires a new prepare and approval. The first version supports one fresh report Task, with
no Scenario actions, scheduled work, or registry changes.
