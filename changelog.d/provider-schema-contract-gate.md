- Fix const/enum type inference for strict Worker provider schemas, including nullable custom
  triage fields. Preflight invalid schemas and stop automatic retries of native schema rejections
  while preserving original kernel validation, evidence and usage accounting.
- Add an offline inventory-backed `make contract-check` covering public and shipped Worker
  contracts, whole-request projection, parity and current Store refusal boundaries. See the
  [decision](docs/adr/2026-11-17-preflight-provider-schemas-and-inventory-contracts.md)
  and [gate documentation](docs/contract-gate.md).
- Refresh the reviewed release validation catalog pin for the added functional contract coverage;
  retain exact-source refusal checks for workflow, resolver and catalog drift.
- Preserve native authentication redaction before and after deterministic schema rejection,
  while retaining parsed usage and suppressing same-schema retries.
- Match the kernel's JSON Schema numeric equality when checking const/enum contradictions,
  including nested literal data and distinct large integers. Make the Linux measurement argv
  contract test explicitly simulate Linux on other hosts while retaining its account assertions.
- Refresh the staged protected-harness digest and inventory for the portable argv test;
  preserve historical receipts and installed authority.
- Preserve enclosing union constraints and legal nulls during strict reply normalization;
  keep pattern-property maps explicitly unsupported. Give native stderr and unframed schema
  rejections deterministic priority while retaining rejection evidence after privacy filtering.
- Canonicalize the Wake recorder fixture path on macOS and assert unsupported-platform
  admission refuses without creating provenance; retain Linux private-key lifecycle coverage.
- Inspect local reference targets under shared schema bounds, decoding URI fragments before
  pointer unescaping. Preserve impossible alternatives and optional omissions with kernel-only
  projection, require explicit object closure, and redact combined authentication/schema failures.
- Align inventory URI fragment decoding with the kernel and retain legal nullable values
  declared only within union alternatives, including nested arrays and reversed branch order.
- Preserve original native evidence for transport failures during OAuth refresh; explicit
  authentication signals remain redacted alongside network and deterministic schema failures.
- Keep untyped object/array constraints, including nested union branches, in explicit kernel-only
  mode. Preserve authentication sensitivity independently of retry classification across Claude
  native envelopes, unframed output, stderr and shared telemetry capture.
- Keep unions with sibling object/array types in whole-request kernel-only mode to avoid
  conflicting required/nullable rewrites; preserve standalone typed unions and scalar projection.
