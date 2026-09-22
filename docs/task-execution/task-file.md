# Task-file command walkthrough

The Task-file entry point runs captured Workers through the common Task
runtime. The checked-in `fixtures/task-runtime/pagination` project is a deterministic example:
an implementation Worker adds offset/limit pagination, a check exercises it, and an independent
evaluator verifies the sealed result. It requires Git and `/usr/bin/python3`, with no credentials.

Copy the example into a separate directory and commit its files before running it. Configuration
is read from the committed Authority Snapshot: `.af/task-catalog.toml` pins the Pipeline and
Worker packages, and `.af/code-policy.toml` defines mandatory checks. The local `ticket.json`
provides the Task ID, goal, explicit Pipeline choice and resource bounds. Ticket text is input
data; it cannot change those committed execution permissions.

```sh
af task plan --file ticket.json --state /tmp/pagination-state --json
af task explain pagination-cli --state /tmp/pagination-state --json
af task explain pagination-cli --plan PLAN_ID --state /tmp/pagination-state --json
af task run pagination-cli --execute --state /tmp/pagination-state --json
af task show pagination-cli --state /tmp/pagination-state --json
af task list --state /tmp/pagination-state --json
```

The state directory must be outside the source checkout. Planning persists exact inputs,
contracts, bindings and graph without running a Worker. Running uses those captured bytes,
including after local files change. Inspection remains possible with a newer executable;
execution currently requires the exact recorded engine. A finished Task returns its recorded
result on another `run`, with unchanged Attempt count and spend.

`af task start --execute --file ticket.json` combines planning and execution. Without an explicit Pipeline,
selection uses captured applicability facts, strategy priorities and resource feasibility.
Generation requires a captured Planner and developer signing policy; its exact proposed plan
waits for signed approval before execution. See the public [Task file](../../schemas/task-file-v1.json)
and [Task catalog](../../schemas/task-catalog-v2.json) schemas for the input shapes. An
explicit [Provider admission cost](model-bindings.md) is optional; an omitted one means the
fixed default allowance. Package existence, signing keys and joint
resource feasibility still require admission validation.

```text
ticket + captured S0
        |
        v
implement -> seal S1 -> check S1 --passed--> evaluate S1
                          |                     |
                          +---------------------+
                          |
                          v
              result + verified/unverified S1
                          |
              explicit Task-ID confirmation
                          v
                 new local worktree
```

Failed checks return unsatisfied acceptance (exit 3); unavailable checks return inconclusive
acceptance (exit 4). Both skip the evaluator. Positive acceptance requires the current check
and evaluator receipts. No command changes the original checkout during execution.

Deliver a verified result only while the target repository is clean and exactly matches S0:

```sh
af task deliver pagination-cli --state /tmp/pagination-state \
  --branch task/pagination --worktree /tmp/pagination-delivered \
  --confirm pagination-cli --json
```

The branch and worktree must be absent. Delivery records its preparation before creating them,
verifies the materialized result, and supports retry/recovery with the same command. It makes
no commit, push, PR or remote call. The common Store keeps delivery receipts alongside the Task.

JSON inspection uses [`af/task-inspection@11`](../../schemas/task-inspection-v11.json) and list
entries use [`af/task-list-entry@2`](../../schemas/task-list-entry-v2.json); their
`chargeable_tokens` values are exact unsigned decimal strings. Inspection retains original
execution-record versions, and `explain` includes the captured
[`af.compiled-task/1`](../../schemas/compiled-task-v1.json) graph and
ExecutionPlan. Public schemas describe these shapes without replacing Store replay validation.

Exact plan inspection accepts only a plan recorded in the selected Task's history. It emits
`af/task-plan-inspection@1`, including the original revision, compiled graph, bindings and
recording event IDs; `current_plan` distinguishes the active plan from an earlier one.
Inspecting an earlier generated plan leaves the current revision and approval state unchanged.
