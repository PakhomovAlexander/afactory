# ADR-2026-11-05-version-the-single-call-structured-contract: Version the single-call structured contract

**Status:** proposed (2026-11-05).

## Context

The structured profile now enforces one model invocation per Worker Attempt, as described in
[the single-call decision](2026-11-05-limit-structured-worker-attempts-to-one-model-call.md).
The initial internal policy changed no payload fields, but leaving the package contract at v1
would let a client predating enforcement admit a package whose author expects single-call
semantics. A package identity must express that client requirement before model execution.
[ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md) requires
current-only readers and coordinated version identities.

## Considered options

- Keep v1 and rely on deployment instructions. Rejected: older clients would accept packages
  whose invocation semantics they cannot enforce.
- Add a v1 compatibility reader or an optional policy flag. Rejected: this leaves an ambiguous
  current contract and a fallback that ADR-0145 forbids.
- Version the structured contract, retaining its exact data bounds. Selected: existing closed
  version checks refuse unsupported semantics before Provider admission.

## Decision

The current package contract is `af.structured-contract/2`, represented by
`StructuredContractV2` and `structured-contract-v2.json` with schema identity
`urn:af:schema:structured-contract:2`. Its fields, port restrictions, exact schema validation,
provenance rules and resource bounds remain unchanged. Its semantics require one model call per
structured Worker Attempt, including malformed replies, schema failures and transient failures.
Further semantic calls require explicit native scheduler retries with per-Attempt accounting.
Provider Admission is a separate operation; non-structured recovery is unchanged.

All current fixtures, readers and schema parity checks use v2. Current clients refuse v1 and
unknown future versions, without migration, alias or fallback. The validation evidence remains
`af/StructuredValidation@1`: its payload shape is unchanged and its `contract_id` binds the
exact captured package, including the versioned contract bytes.

Both `af catalog digest` (`local_package`) and planning (`capture_package` and
`restore_package`) call `TaskPlanCompiler::parse_package`. Structured kind admission calls
`parse_contract`, whose typed validator requires the exact supported version. Clients with the
previous v1 validator therefore reject v2 at these same boundaries, before Provider admission;
a correct raw digest cannot bypass the version check. The native regression exercises digest
and planning refusal for both retired and future versions, including correctly repinned bytes
and zero adapter invocations. This is structural version-guard evidence, not a fabricated
execution receipt from a historical executable.

## Consequences

Consumers must upgrade to an AF client supporting v2, review their contract and repin packages.
Finish or abandon in-flight v1 Tasks with the original client before upgrading. Changing only
a package's version string cannot give an older executable single-call support. Current v2
packages retain strict one-call and scheduler-retry regression coverage. No implementation
lifecycle features or consuming-repository operational packages are introduced.
