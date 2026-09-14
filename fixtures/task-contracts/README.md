# Task contract fixtures

`v1/` is the additive Task contract corpus introduced by ADR-0046. Positive payloads are
identified by `content-ids.json` using the existing canonical JSON content digest. The IDs inside
these shape fixtures are synthetic references; they do not claim Store admission or execution.

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

The existing synthetic review corpus remains unchanged. Later runtime packages must add their
own durable admission, crash/replay, budget and compatibility evidence.
