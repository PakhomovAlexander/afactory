# Package-defined structured Task examples

These are inert staged packages. They install nothing into the operator's configuration and
change no committed `.af/` authority. `triage` has one typed external `issue` input and one GPT-6.1 Sol
medium Worker on `codex-personal`, followed by kernel structural acceptance. It has no report,
Document or evaluator node. `capitalize` proves the same profile for another business kind.

The [profile reference](../../docs/task-execution/structured.md) explains the structural guarantee
and limits. The older `fixtures/issue-triage` report flow remains a historical two-model runner
prototype; it is not the single-model custom Task recipe here.

These packages require a client supporting `af.structured-contract/2`: one model call per
Worker Attempt, with any additional call owned by an explicit scheduler retry. Older clients
refuse the package version before model invocation.

## Install after review

Review each kind, Pipeline, Worker and schema. In a consuming repository copy the selected
`kind/`, `worker/` and `pipeline/` directories to its package location, then compute each pin:

```sh
af catalog digest fixtures/custom-tasks/triage/kind
af catalog digest fixtures/custom-tasks/triage/worker
af catalog digest fixtures/custom-tasks/triage/pipeline
```

Add those exact `name`, `version`, `digest`, `path` entries to the consuming repository's
`[packages]` table, and these mappings to its Task catalog, preserving its other authority:

```toml
[kinds]
triage = "custom/triage-kind"

[providers]
"custom/triage-worker" = "codex-personal"
```

Review and commit the authority before execution. The triage profile supplies its installed
structural policy from the captured kind contract, without requiring a code or report policy.
For capitalization use the corresponding `custom/capitalize-*` names and `capitalize` kind.
No credentials ship in either package.

## One captured issue

The issue example imports the actual #432 body from the checked-in 2026-10-30 capture and
records its exact imported body hash. It deliberately records absent repository excerpts and
accumulated knowledge. The original capture timestamp is unknown and recorded as such; the owner comment is a bounded
excerpt with explicit truncation and its original digest. Refresh the issue identity and supply bounded
cited evidence for a new run. `knowledge.status`, `as_of`, `freshness` and cited entries keep
absence and age explicit. The Worker reads these as untrusted claims. Instructions carry the
repository's class/size/pipeline rubric; missing evidence requires questions and null guesses,
and a security/held issue stays held. The `custom/IssueTriage@2` result is a single-model
proposal with source identity, confidence, rationale and needs-info; it cannot serve as the
older two-model merged publication record.

```sh
af task plan --file fixtures/custom-tasks/triage/task.json --input-json issue=issue.json --json
af task run triage-one --confirm-plan sha256:... --json
af task output triage-one --port triage --format json

af task plan --file fixtures/custom-tasks/capitalize/task.json --input-json note=note.json --json
af task run capitalize-one --confirm-plan sha256:... --json
af task output capitalize-one --port capitalized --format json
```

Each Attempt reserves 40000 tokens and 500000 ms. The example Task allows 150000 tokens,
eight Attempts and twenty minutes, including normal Provider admission and bounded retry.
A schema or transport refusal is a failed Attempt, not a valid classification. Structural
acceptance checks schema and input provenance, not whether the model's judgement is correct.

## Native Scenario

`triage/scenario.json` consumes one existing triage Task, projects its exact `triage` output
artifact and result ID, and requires conclusion `structurally_validated`. Replace its placeholder
absolute repository/Store paths and exact plan/revision IDs with the values printed by the
reviewed Task plan. Put the same plan ID in the Scenario's authority and an owner-only external
machine policy's `task_plans`; keep adapters, grants, decisions and file roots empty.

```sh
af scenario validate --file triage-scenario.json --authority ../scenario-policy.json --json
# inputs.json is {}. Confirm the exact Scenario revision just reviewed.
af scenario start one-issue --file triage-scenario.json --inputs inputs.json \
  --authority ../scenario-policy.json --confirm-pin sha256:... --execute --json
af scenario show one-issue --json
```

A finished Task is reused without another Worker Attempt. A failed or cancelled Task cannot
satisfy Scenario acceptance. This example has no publication, scheduling, queue, dynamic Task
creation, report or evaluator nodes. It writes no labels or GitHub comments.
