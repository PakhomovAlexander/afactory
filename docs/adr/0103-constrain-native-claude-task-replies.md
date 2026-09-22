# ADR-0103: Constrain native Claude Task replies

Date: 2026-09-15
Status: Accepted (2026-09-23); acceptance recorded in [ADR-0113](0113-ga-reads-only-what-ga-writes.md)

## Context

A completed Claude invocation can put valid-looking Worker JSON inside Markdown fences in its
textual `result`. The strict typed Worker validator correctly refuses those bytes. Prompt-only
format instructions do not establish a reliable native transport contract.

## Decision

For a rendered `af.worker-request/1`, the generic Claude Task adapter constructs a native
`--json-schema` argument from the exact captured output schemas. The outer object requires
`schema: af.worker-reply/1` and `outputs`; declared ports contain arrays of at most 1,024 payloads.
Unknown outer fields and ports are forbidden. Port presence and business cardinality remain
subject to the existing compiler and Kernel acceptance checks.

The payload schema is preserved. An adapter-generated local URN `$id` on each object schema
keeps its local references scoped to that payload when nested in the transport wrapper. This
avoids changing reference strings or literal `$ref` values inside `const` or `enum`. Captured
contracts already forbid supplied resource IDs and external references. Boolean schemas retain
their original meaning. No schema resolver, authority input, or native tool permission is added.

The native CLI accepts an inline schema, so its serialized argument is bounded to 64 KiB before
spawn. The existing 1 MiB Worker input and output bounds remain. The captured request is sent
unchanged on stdin. Oversized or unsupported typed requests refuse before provider spend.

Typed invocations require an object in the native `structured_output` field. Textual `result`
is never a fallback, and fences are never stripped. The exact raw native envelope is retained;
only its designated structured value is serialized for the existing strict Worker validator.
Native execution failure, incomplete accounting, unexpected model activity, and capture failure
still refuse output while retaining the available usage and raw evidence. Historical legacy
prompts continue to use textual `result`; legacy Review transport is unchanged.

The [native CLI reference](https://code.claude.com/docs/en/cli-usage) documents `--json-schema`;
[programmatic execution](https://code.claude.com/docs/en/headless) documents `structured_output`.
Installed Claude 2.1.272 help exposes this flag. Static inspection of that installed binary shows
schema validation creates a read-only `StructuredOutput` tool after initial tool selection and
emits its accepted value in the native result envelope. Safe mode, restricted mode, explicit
read/write role grants, permission mode, MCP isolation, personal auth grants, and title
suppression remain unchanged. This inspection is not a live-provider success claim.

Native structured output can reprompt internally when validation fails. Afactory adds no retry
loop: its existing invocation deadline, capture supervision, reservation and complete native
usage accounting still govern the call. The native schema facility is not trusted to replace
Kernel output validation, domain acceptance or semantic review.

## Consequences

New behavior is bound by the captured engine binary identity. Existing Tasks, failed native
results and their receipts remain immutable and cannot be resumed under the changed engine.
A future invocation requires a fresh capture. No failed output is retroactively accepted.

Synthetic child-process tests inspect actual arguments and unchanged stdin, exercise both
permission roles, structured field refusals and schema-invalid payloads, retain usage on failed
output, and prove legacy compatibility. Schema validation checks preserve independent local
reference roots and literal values. These tests require no authentication or native inference.
