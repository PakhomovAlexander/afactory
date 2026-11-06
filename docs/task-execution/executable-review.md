# Executable implementation review

Opt in with ordinary typed Worker and Pipeline contracts. The new public schemas are
[ImplementationTestEvidence@1](../../schemas/implementation-test-evidence-v1.json) and
[ImplementationEvaluationInput@1](../../schemas/implementation-evaluation-input-v1.json).
Worker-local schema copies omit standalone `$id` and `$schema` metadata, following the existing
local-only Worker schema validator. The [decision](../adr/2026-11-06-bind-executable-review-evidence-to-evaluation.md) states their limits.

1. Keep the correctness reviewer required in the consuming review policy and independent of the
   implementing slot. Declare `read-source` and `execute-checks`, without `write-source`.
2. Alongside its unchanged `review.kernel/ReviewerResult@2` result, declare a required `testing`
   output of `af/ImplementationTestEvidence@1`, cardinality `one`, affinity `same_as source`.
   Its `retains.testing` includes `source` and `checks`. Run the exact sealed candidate in the
   reviewer's ephemeral clone: build it, install privately below scratch, and exercise the installed
   executable. Record actual commands, exit codes and bounded public observations; null exits
   describe commands that could not run. Do not claim tests that were not run.
3. Set the evaluator's `worker_input_type` and slot `input_type` to
   `af/ImplementationEvaluationInput@1`. Declare required source, requirements, checks and testing
   inputs; testing has cardinality `one` and `same_as source` affinity. Its evaluation result retains
   `testing`, `checks` and `requirements`. Bind `testing` to that reviewer's output and source to the
   same sealed candidate. Across a child pipeline call, expose and wire the evidence explicitly.
4. Use existing `verify` and `accept` nodes and existing independent local checks. The evaluator
   judges exact Requirements and evidence; its passing result cannot override failed or unavailable
   testing. Bind optional `acceptance` when the Task uses acceptance items and judge each by ID.

The evidence payload uses `schema = "af/implementation-test-evidence@1"`, `snapshot_id`, `status`
(`passed`, `failed`, `unavailable`), nonempty `reason`, and at most 32 `commands`. Each command has
`purpose` (`build`, `install`, `test`), `command`, `exit_code` and `evidence`. A pass requires all three
purposes and every exit zero. Evidence is a Worker-authored claim admitted against a Snapshot;
AF does not independently attest the truth of the command transcript. Private scratch never enters
the candidate. Source edits by executable reviewers fail their Attempt.

Consuming packages choose their own providers, models, efforts and independence settings through
Worker manifests and captured operator bindings. No particular Provider, model, account, role name
or pipeline name enables this capability. Pipelines without the opt-in keep their current contract.
These receipts neither replace CI nor authorize publication or merge.

The [small executable fixture](../../fixtures/executable-review/README.md) and native regression
exercise the reviewer-to-evaluator edge without real model calls or external services.
