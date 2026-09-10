# Task contract fixtures

`v1/` is the additive Task contract corpus introduced by ADR-0046. Positive payloads are
identified by `content-ids.json` using the existing canonical JSON content digest. The IDs inside
these shape fixtures are synthetic references; they do not claim Store admission or execution.

`negative.json` records forbidden cases. `shape` cases must fail both the JSON Schema and Rust
admission. `semantic` cases must fail Rust cross-field validation; JSON Schema alone cannot
prove those invariants. The tests also cover exact approval matching, canonical identity,
independence, review exit precedence and current-Subject repair assessment.

The existing synthetic review corpus remains unchanged. Later runtime packages must add their
own durable admission, crash/replay, budget and compatibility evidence.
