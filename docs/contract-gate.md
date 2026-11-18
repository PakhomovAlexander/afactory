# Fast contract gate

Run `make contract-check` from the repository root with the Rust toolchain pinned in
`rust-toolchain.toml`, cargo-nextest and Python 3. This is an offline contract check with no
provider admission, model calls, remote checks or Store migration.

The gate runs discovery/ownership and injected negative tests, then a filtered workspace nextest
selection under profile `ci`. The Rust selection includes the existing `af-contracts` and
`af-check` schema parity registries, actual shipped Worker output sets, the strict provider
projector and dialect validator, Codex fake-adapter schema files, plan-time preflight and a
current-source Store corruption/version refusal test. Normal functional validation includes the
same tests. `release-check`, shared by normal fast/full local and CI base checks, always runs the
cheap inventory checks even when classes are excluded.

## Inventory and ownership

`scripts/contract-inventory.json` is an authored inventory, not a schema generator. Each family
names schema documents, reader/writer files, semantic-validation owners and existing test evidence.
Whole Worker request entries name every physical model-authored output, its canonical schema,
projection mode and reason. Kernel-captured candidates/development caches and intentionally
incomplete negative fixtures have explicit exclusions. A newly declared port without its schema
or an explicit reviewed exclusion fails discovery.

Discovery covers public `schemas/*.json`, package input/output schema files, staged and installed
fixture copies (including nested `.af`), onboarding templates, and embedded structured-kind
schemas by exact JSON Pointer. Three existing public conformance corpora remain evidence for the
independent parity tests; their payload data is not treated as nested schema nodes. Canonical IDs
are unique across public resources; package copies retain their independent local reference roots.
When upstream replaces a schema version, update its inventory membership and any canonical port
mappings together, retaining the current reader/writer and parity evidence. The injected replacement
test refuses retired entries, stale mappings and stale Worker schema copies independently.
Reference discovery visits schema keywords, never unreferenced literal object keys inside `const` or
`enum`. Inventory fragment resolution percent-decodes UTF-8 before splitting pointer segments and
unescaping tildes, matching the kernel resolver.

The Python check refuses omissions/orphans, duplicate inventory ownership/IDs, broken refs,
missing owner/evidence files, whole-port drift and unexplained projection exclusions. The Rust
check compiles every inventoried schema and validates actual complete projected requests against
the pure bounded dialect validator. Canonical copy equality and the independent schema/serde,
semantic-validation and identity tests remain separate checks.

## Provider behavior

The central projector infers scalar const/enum types, preserves explicit restrictions and nullable
or mixed scalar values, and handles nested arrays/unions. It leaves bounds, conditionals and
semantic validation to the original kernel validators. Optional nulls accepted by the original
schema survive normalization, including enclosing union constraints and alternative-only properties;
valid alternative normalization retains the most original nulls. Pattern-property maps retain
explicit unsupported projection. Unknown ports survive normalization so the kernel can refuse them.
Const/enum compatibility checks use the kernel's JSON Schema numeric equality, including nested
literal values; equivalent integer/decimal spellings agree without rounding distinct large integers.
Impossible literal restrictions select kernel-only projection, including inside alternatives and
optional properties. Objects are open unless `additionalProperties: false` explicitly closes them.
Object and array keywords without an explicit type retain kernel-only projection, including
nested union branches: those keywords also permit scalar instances. Compatible scalar literals
can still establish a type without narrowing the captured contract.
Unions sharing an object or array type at the same schema node retain kernel-only mode:
independent required/nullable rewrites of sibling structural constraints and closed branches
can conflict. This boundary applies recursively and disables projection for the whole output
request; standalone typed union branches and scalar unions still project.
Local references make their targets schema nodes even under annotations or literal locations;
cycle detection and shared depth/node bounds apply to those targets. URI fragments are percent-decoded
before JSON Pointer unescaping, matching the pinned resolver. Unreferenced literals stay data.
Authentication sensitivity is tracked independently of deterministic schema rejection, including
mixed native stderr, unframed output and combined or separate native errors in both adapters.
Saved stderr excerpts and CLI repair hints receive the same independent authentication sensitivity
before any startup, timeout or transport-failure diagnostic is constructed.
Shared telemetry capture also detects authentication independently; successful model prose does
not become a native error, and ordinary OAuth refresh network failures retain their evidence.

`Result<Option<Schema>, Error>` has three meanings: a valid emitted strict schema, an intentionally
unsupported projection retaining kernel-only validation, and an invalid contract/projection
refused before invocation. Unsupported whole requests include boolean schemas, references, open
maps, untyped arrays and literal object/array enumerations. Every port is checked for invalidity
before another port can select unsupported mode. No native rejection triggers an unconstrained
retry. Native `invalid_json_schema` rejection from framed errors, native stderr or unframed stdout is
deterministic; raw bounded evidence and reported
usage are retained, and unreported usage remains unknown.

## Timing evidence

Local macOS arm64 development observation, Rust 1.88.0, 17 November 2026: a warm
`make contract-check` took 10.93 s wall time, including Cargo's separate incremental build/check
of 0.11 s. Discovery checked 708 schema locations in 11 families in 0.402 s; the seven Python
injection tests took 0.322 s. The filtered nextest run passed 210 tests in 8.905 s. The initial
test build in this continuation took 57.74 s, reported separately; it is not a warm gate timing.
These are local observations, not a baseline/candidate comparison, speedup claim, provider
compatibility certification or a timing threshold.

The fake adapter inspects the actual `--output-schema` file for the shipped custom triage,
report author/verifier and evaluator contracts, verifies no spawn on preflight refusal, and
proves one call on synthetic schema rejection even after a transient event. The Store negative
uses a disposable current-source Store with an outstanding reservation: truncated/corrupt CAS
bytes and unknown plan types refuse without rewriting evidence, appending a reset or changing
accounting. It adds no historical reader.

[Decision](adr/2026-11-17-preflight-provider-schemas-and-inventory-contracts.md).
Compatibility and representation consolidation remain deferred to #1116 and #1117.
