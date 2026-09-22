# Require typed Generation outputs in pipeline version 2

**Status:** proposed; superseded in part by [ADR-0113](0113-ga-reads-only-what-ga-writes.md): the
untyped port shorthand kept readable for older pipeline files, and `PriorFindings@1` as a
Generation output. A Generation emits an exact `FindingSet@1`, and a diff Subject's `ChangeSet@1`.

Pipeline version 2 introduced typed ports but retained the string shorthand for older pipeline
files. The built-in Generation executor dispatches outputs by artifact contract, so an opaque
string output can load but must fail later during execution. Rejecting that shape at load narrows
the documents accepted under the existing version discriminator.

We will keep pipeline version 2 and require every built-in Generation output to use a typed
`PriorFindings@1` or `ChangeSet@1` declaration. The string shorthand remains readable on other
node kinds. The load error names this version-2 exception and the exact replacement contracts.

## Considered options

- **Continue accepting opaque Generation outputs.** Maximizes parser compatibility. Rejected
  because the executor cannot soundly infer an artifact contract from a port label and paid work
  would fail after load.
- **Introduce pipeline version 3.** Makes the narrowing explicit in the top-level discriminator.
  Rejected for now because no valid version-2 Generation execution is lost: the narrowed shape
  was admitted by the parser but undispatchable by the executor.
- **Keep version 2 with a documented node-specific exception (chosen).** Fails before capture or
  reviewer spend, preserves the shorthand elsewhere, and gives consuming repositories an exact
  migration message.

## Consequences

- A formerly parseable opaque Generation output now fails at definition load.
- Consuming repositories must replace `outputs = ["findings"]` on Generation nodes with explicit
  typed port tables.
- New built-in Generation artifact kinds require an additive executor and validation change; port
  names never substitute for artifact types.
