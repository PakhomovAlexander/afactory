# ADR-0216: Construct reference fixture bytes without per-byte Python loops

**Status:** proposed (2026-10-24); renumbered from unmerged ADR-0201 after published main's
ADR-0204. Implements package
`telemetry-performance-verification-throughput` under the
[continuous performance design](../design/continuous-performance.md), retaining
[ADR-0163](0163-measure-native-workloads-through-a-public-api-probe-and-independent-golden-oracles.md)'s
independent reference and
[ADR-0164](0164-judge-only-declared-probe-requests-against-a-reference-checked-golden.md)'s
fresh check against the exact registry.

## Context

The integrated telemetry verification reached the unchanged one-hour Task check deadline.
Diagnostic profiling identified reference generation as a substantial cost: Python iterated
over each letter, padding byte and source-file fill byte, and the encoder iterated over every
character to validate its restricted ASCII domain. Profiled costs identify work to inspect;
they are not an unprofiled baseline or additive time savings.

## Decision

Construct each deterministic sequence by repeating a literal period and taking its remaining
prefix. Rotate the source-file alphabet by the file index before filling. Retain the integer
index and negative-count behavior of `range`, the exact file header and final newline, and the
current fixture size. Check strings with a compiled scan for any character outside printable
ASCII excluding double quotes and backslashes. Empty strings remain admitted; control bytes,
DEL, Unicode and lone surrogates remain refused with the same diagnostics.

Every `golden_document` still independently derives the complete document from the supplied
registry and fixture inputs. There is no new cache, validation memo, product dependency or
expected-answer read. The existing per-document envelope reuse remains scoped to that one
derivation. Typed contracts, persisted shapes and schema versions do not change.

Re-pin the protected inventory and staged policy only with `harness-digest --write`, after
the protected edits. Installing authority remains outside this implementation Task.

## Evidence and limits

[The focused measurement report](../performance-reference-throughput.md) retains the exact
command, all six elapsed samples, source hashes and equal output digests. On this host the
median time for two full reference-document generations and their checked serialization fell
from 3.934396375 s to 0.987163375 s. This measures reference generation only; it predicts no
complete-gate runtime or product speedup.

Regression tests compare letters, padding and both source-tree sizes with independent
per-position definitions; exercise ASCII boundaries, Unicode, invalid counts and fixture-size
changes; and check repeated calls and changed registries. A CLI integration fixture regenerates
a changed package size, then refuses a schema-valid forged seed even when its semantic digest
is internally consistent. Existing native-instance, seed-writer, cap-boundary, committed-golden
and oracle refusal tests continue to apply. The complete sealed gate and independent reviews
remain the coordinator's acceptance work.
