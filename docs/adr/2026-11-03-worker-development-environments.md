# ADR-2026-11-03-worker-development-environments: Bind private Worker development environments

Status: proposed; accepted on merge.
Date: 2026-11-03.

## Context

Issue #584 requires local compile/test environments for implementers and independent reviewers,
without embedding Cargo concepts in the runtime or making a Worker declaration host authority.
Existing package pins, Execution Bindings, private sandboxes, CheckRuntime directories and
process supervision already own the relevant lifecycle. Native model harnesses do not expose a
common protocol that enforces and records every internal command.

## Decision

A pinned Worker package may add a versioned `development.toml` companion,
`af.worker-development/1`, without changing the `af.worker/1` manifest shape. The companion's
strict contract declares symbolic paths, pinned tools, environment, private-state policies,
writable roots, available-only dependencies, denied network and resource requests. The existing
package digest pins these bytes and their requested exact machine-binding identity.

The machine binding, `af.worker-development-binding/1`, is selected only by the operator through
`AF_WORKER_DEVELOPMENT_BINDING_FILE`. It names a local runtime, immutable credential-free image,
checksum utility, tools and ceilings, never a shared writable host cache. Planning and dispatch
both refuse a different binding identity or a wider request. A bounded image-tool identity probe
runs before the harness and never executes an untrusted project build.

The initial backend runs captured command Workers through the existing container provider and
supervisor. Model adapters and unsupported aggregate disk quotas are refused explicitly. CPU,
memory, processes, wall and output have enforced bounds. Source/state/scratch/build/cache/
knowledge are distinct, Attempt-private roots. Reviewer experiments use a second clone and
cannot reach the candidate seal. Unconfirmed daemon cleanup preserves every exposed root.

One `af.worker-command-evidence/1` raw record is retained through the existing Attempt evidence
path for each dispatched outer harness. It identifies actual source/experiment Manifests,
command, environment, binding, probe, timing, exit, available resources and bounded output.
Redaction, truncation and unavailable internal observations are explicit. It records the exact
stdout returned to the command reply parser; it makes no claim about output a nested model saw.
Worker caches and receipts do not replace the project Gate or acceptance.

## Considered options

- Add Cargo-specific runtime paths and inherited host caches. Rejected: neither is a
  language-agnostic capability, and a host cache may contain credentials and mutable authority.
- Treat a writable checkout as containment. Rejected: the existing trusted-local provider
  explicitly supplies no such guarantee.
- Add environment fields to the existing Worker manifest without changing its version.
  Rejected: ADR-0145 versions changed persisted shapes. An independently versioned companion
  can use the existing exact package pin while leaving the manifest's shape unchanged.
- Assume native model CLI output reconstructs every command and returned excerpt. Rejected:
  unavailable observations cannot become evidence. These adapters are unsupported until they
  have an enforceable, host-owned protocol.
- Add another executor or cache/session runtime. Rejected: reuse existing lifecycle owners;
  later acquisition and warm carry must extend Cache Snapshots, Build Caches, Notes and Warm Sets.

## Consequences

Undeclared Workers retain their existing behavior. Companion-enabled Workers require a usable
local container runtime and an exact reviewed machine binding. There is no network or host-run
fallback. Aggregate disk requests fail closed instead of becoming a false quota claim.
The [design](../design/worker-local-development.md) distinguishes this first step from future
trusted acquisition and node-private warm carry. Session support remains removed under ADR-0145.
