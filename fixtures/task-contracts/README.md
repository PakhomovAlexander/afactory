# Task contract fixtures

`v1/` is the additive Task contract corpus introduced by ADR-0046. Positive payloads are
identified by `content-ids.json` using the existing canonical JSON content digest. The IDs inside
these shape fixtures are synthetic references; they do not claim Store admission or execution.
The unchanged baseline `execution-plan.json` payload is checked against the current
`af/ExecutionPlan@4` schema; Pi's additional captured authority is exercised by the Pi schema
parity cases. The corpus directory name is provenance, not an earlier-version reader.

`negative.json` records forbidden cases as compact mutation recipes: the positive contract
fixture, JSON Pointer, add/replace/remove operation, value, classification and reason. Each
recipe includes the expected fingerprint of its fully expanded invalid payload; expansion
must change the base and preserve that fingerprint. This retains precise examples without
repeating whole contracts in every negative case or Worker review context.

`shape` cases must fail both the JSON Schema and Rust admission. `semantic` cases must pass
the schema and fail Rust semantic admission; unknown classifications fail the test. Invalid
payload fingerprints do not claim that those payloads may enter the Store.

Wire sets must be sorted and unique; `facts` and `covers` are explicit, including empty values.
Tests prove typed serialization preserves every positive fixture's content ID and reject
nontrivial permutations of every set family. They also cover exact approval matching,
independence, receipt-completeness/exit precedence and current-Subject repair assessment.

All three Pipeline fixtures use `af.pipeline/6`. Their directory names retain corpus provenance:
`v1/pipeline-definition.json` is ordinary, `v2/pipeline-definition.json` declares the optional
Project Brief port, and `v3/pipeline-definition.json` declares bounded `recovery_attempts`.
Parity tests preserve the Brief authority and recovery bounds, combine the capabilities, and
explicitly refuse every retired `/1` through `/5` identity. The current compiled graph is
`af.compiled-task/6`; all installed kind profiles use `af.task-kind/3`.

The existing synthetic review corpus remains unchanged. Later runtime packages must add their
own durable admission, crash/replay, budget and compatibility evidence.
