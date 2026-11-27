# Offline agent-navigation evidence verifier

`scripts/analyze-agent-bench.py` is a bounded, credential-free structural verifier and descriptive
report generator for offline v2 evidence. The [installed credential-free benchmark](../agent-bench.md)
replays its own native Task receipts.
It starts no processes or model calls and writes only its report to stdout:

```sh
python3 -I scripts/analyze-agent-bench.py fixtures/agent-bench/evidence.json
python3 -I scripts/test-agent-bench.py
```

The committed evidence and analysis fixtures are synthetic conformance examples. Their token and
time values are invented test inputs, never observations of navigation or A/A validity. No live
smoke, A/A or A/B has been executed by this component.

## Wire protocol

The input is one UTF-8 JSON document, at most 8 MiB, with schema
`af.agent-bench-evidence/2`. Duplicate JSON keys, non-finite numbers, unsupported versions,
unknown typed fields and missing typed fields are refused. The wire schema is
[`agent-bench-evidence-v2.json`](../../schemas/agent-bench-evidence-v2.json). Its definitions
specify schedules, slots, receipts, invocations and grades. Semantic relationships and content
identities are checked by the executable verifier; schema validation alone is insufficient.
Opaque protocol, oracle-package, source, model, collector, answer and grade-evidence payloads are retained
as JSON objects/values. Their internal admission contracts are outside this initial component.

`objects` maps `sha256:` identities to retained JSON values. The identity hashes UTF-8/ASCII bytes
from Python JSON encoding with sorted keys, ASCII escaping, no whitespace separators and no
non-finite numbers. It describes these canonical values, not original provider stream bytes or
an AF CAS artifact identity. Implementations must reproduce this encoding before comparing hashes.
Do not convert native integers through floating point. Individual counters are nonnegative
integers at most 9007199254740991; booleans are refused as counters. Derived sums use exact integers.

The envelope names one schedule and arrays of receipt and grade identities. The schedule pins
protocol, case and oracle packages, base/head source payloads, model and collector payloads. It
contains an ordered list of at most 512 slots. The retained case package is typed as
`af.agent-bench-cases/1`: a nonempty `cases` array of unique `id`, `family` and nonempty `prompt`
records. Each slot binds an exact admitted case ID and positive integer repetition. Each pair
has exactly one base and one head slot with the same case, repetition and family; slot IDs and
case/repetition pairs are unique. `aa` requires identical source references, `exposed_ab`
requires distinct references, and `smoke` permits either. The ordered slots preserve supplied
execution order. Smoke admits at most four slots. A/A and exposed A/B require exactly twelve
cases and the reviewed 72-slot order, reconstructed by `frozen_slots`: for repetitions 1–3, sort
case IDs by SHA256 of UTF-8 `aa-order-v1`, NUL, `agent-bench-aa-20261112-v1`, NUL, decimal
repetition, NUL, case ID. The first six pairs run base first, the last six head first, with
adjacent sides. This validates order but cannot establish when the schedule was frozen.
Every slot requires exactly one terminal receipt, including slots never started. Foreign,
duplicate, missing, hash-mismatched or unreferenced objects are refused. Array ordering remains
part of identity. No reference is interpreted as a filesystem path or fetched over the network.

Receipts bind the exact schedule and slot, one terminal status, nullable monotonic elapsed
nanoseconds, timing completeness, an ordered list of native invocations, a nullable answer
reference and navigation-trace completeness. Calls carry input, output, cached-input,
reasoning-output and AF-chargeable counters separately, each nullable, plus usage completeness.
The collector must normalize provider events before writing these per-invocation records; this
reader rejects duplicate invocation IDs across the entire schedule but cannot detect an event
counted twice under different IDs or prove that an invocation was omitted. Complete usage requires
both input and output.
Cached input cannot exceed input; reasoning output cannot exceed output. Unknown subset semantics
must remain null. Missing counters are never zero measurements. A nonnull charge must reconcile
exactly to input minus cached input plus output, with all three counters present. An unstarted
receipt must have no calls, answer or elapsed time, and both completeness flags must be false;
even a zero elapsed value would assert activity.

Grades bind schedule, slot, exact answer, pinned oracle package, verdict and retained grade
evidence. A grade with another answer or oracle is refused. Missing and ambiguous grades remain
unresolved. These relationships establish structural provenance only: they do not authenticate a
grader, prove blinding or establish independence. A subject's self-grade must never authorize a
benchmark claim merely because this reader accepts its shape.

## Deterministic analysis

The output schema is `af.agent-bench-analysis/2`, specified in
[`agent-bench-analysis-v2.json`](../../schemas/agent-bench-analysis-v2.json); it binds the canonical
evidence and schedule identities. All scheduled slots remain in denominators. Correctness counts require a `correct`
grade and an `ok` terminal status. Failures remain failures even if an answer was graded correct.

Primary native tokens are input plus output, with no additional cache or reasoning charge.
Known token floors and known AF charges are separate from complete totals. Any incomplete call,
including an unknown first retry followed by a fully reported retry, keeps native totals unknown.
Charge totals require a charge counter for every call; a reported AF charge is not evidence of
native-token completeness. Resource totals require complete timing and usage for every slot on
that side. This conservative rule withholds both resource totals when either is incomplete.
No-call/not-started slots have unknown usage rather than measured zero.

Pair deltas are head minus base and appear whenever both sides have complete resources,
including wrong, ambiguous and failed outcomes. `eligible` denotes complete paired resources;
`correct_eligible` separately identifies the resolved-correct subset. Every pair remains listed
with its family, exact case and repetition. Correctness never selects the primary resource sample.

The `resources` endpoints report exact signed relative differences `(head-base)/max(base, 1)`:
average repetitions within case, cases within family, then equal-weight families. Each rational
is represented by its signed numerator and positive denominator. Zero baselines remain listed
separately. An incomplete pair withholds the endpoint aggregate and its family summaries.
`within_bounds` means the absolute signed mean is at most 20%; `outside_bounds` means greater
than 20%. These descriptive screens confer no execution or A/A readiness authority. They do not
establish containment, grading independence or authenticated collection. No confidence intervals,
equivalence tests or improvement inference are produced.
The report always says `qualification: structural_only` and
`conclusion: no_improvement_claim`, including for A/A. Reanalysis is deterministic and changes
neither evidence nor receipts. Repairs require a separately retained evidence envelope; originals
must not be overwritten by a future collector.

## Remaining execution and acceptance work

The current accepted research input is `af/Document@1`
`sha256:8bf9fbce95f3671ea79108b06ae557c4cfacf3ccbd9771333da87aec8bdf3353`, bound to source
`sha256:8ae8f505268f334111d227a8d6b011eca882bfd5876062037cf5e7fcb584c756`.
Its recommendation is installed `af agent-bench plan` and `af agent-bench report` commands with
`af task run` through ordinary Task accounting, restricted host transport, a source-only container
broker, raw native call collection
before normalization/retry aggregation, independent blinded grading and immutable Store receipts.
The handoff also requires the four analyzer repairs, case/repetition binding, exact paired
schedules and family-weighted analysis implemented here. The external cases and oracle answers
in the Task's context are not committed into this repository.

This component supplies the repaired structural analysis. The
[source-navigation broker](agent-bench-broker.md) provides source-only Docker tools. The
[installed benchmark](../agent-bench.md) uses those tools from native Tasks with synthetic
inference or a credential-free Codex fixture. Both smoke modes use synthetic grading.
It retains native Task receipts and has its own report contract.
Old evidence/schedule version 1 and analysis version 1 are retired; no migration or fallback
reader is supplied, following [ADR-0145](../adr/0145-read-only-what-this-release-writes-and-version-every-changed-shape.md).

Still required before trials: independent package validation; reserved confirmation families;
read-only subjects with private sessions/HOME and enforced network/oracle isolation; exact
provider/account/model/tool pins; frozen randomized paired schedules; shared monotonic deadlines
and descendant cleanup; raw native call observations before aggregation; immutable CAS/Task
receipts; independent blinded semantic grading; persistent account/run/disk admission and the
100000-token stop-before-next-call rule across retries. Existing aggregate Task receipts cannot
prove complete navigation traces or native usage for every call. Ordinary zero settlement under
[ADR-0143](../adr/0143-charge-zero-and-record-unknown-usage-when-no-usage-is-reported.md)
remains separate from unknown measurement.

Authenticated Task/Provider integration, live error/containment conformance, family-aware statistical
analysis, A/A validity screens, authorized smoke and exploratory A/B evidence, and independent
whole-change AF verification remain outstanding. Exposed cases can only support exploratory
analysis. There is no result here about speed, precision, correctness of live answers, model
readiness or improvement. No new execution/publication authority is introduced; the required runtime
addition remains implementation work under the captured Task authority.
