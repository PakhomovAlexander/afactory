# Preview and confirm a Task plan

`af task start --file` captures the Task and stops before dispatching any Worker, including with
`--json`. Planning may check local Provider identity, but performs no paid capability admission
or Worker inference.

```sh
af task start --file ticket.json
# Read the displayed Task, Pipeline, stages, bindings, effects and limits.
af task explain TASK_ID --tree
# Copy the complete sha256:... identity from PLAN, not a shortened prefix.
af task run TASK_ID --confirm-plan PLAN_ID
```

Use the same `--repo` and `--state` selectors for all commands. `af task plan --file ticket.json`
remains the explicit plan-only entry point. A changed captured plan refuses confirmation before
dispatch, including a change before acquiring the Task writer lease. Confirmation does not grant
extra effects, budget, a fresh deadline or permission to deliver the result.

## Compact and tree views

The default human output renders a compact ASCII flow from the captured graph, with the selected
Pipeline package/version, complete plan ID, effective Worker model/effort/Provider alias, public
input/output names, total token allowance, protected verification reserve, Attempt cap, remaining
time and captured effects/data destinations. Command Workers are labelled as such. Private
principal identities and credentials are not printed.

The compact flow folds internal sealing, receipt assembly and selection nodes and reports the
count. `?` marks conditional work, not guaranteed execution. Arrows show dependency-compatible
order; they do not promise concurrent execution or a timing estimate. Long flows/binding lists
show an explicit omitted count. `task explain TASK_ID --tree` expands child Pipeline calls and
shows every execution step and its conditions. `--json` retains the existing machine-readable
inspection schema and complete graph. `--tree` and `--json` are mutually exclusive.

Both views read recorded artifacts, never modified live Pipeline files. Terminal controls and
non-ASCII display text are replaced; long rows wrap at 96 columns. This is a rendering boundary,
not a modification of the captured Task text. Exact historical inspection remains available with
`task explain TASK_ID --plan PLAN_ID`; a historical view does not advertise an execution action.

## Claude and Codex

Use the same CLI output in either conversation. The agent workflow is:

1. Run `af task start --file ticket.json` without `--execute`.
2. Show its ASCII preview verbatim in a code block. Offer the tree view on request.
3. Ask the user to approve the displayed plan. Do not invent buttons, Worker identities, costs,
   timing estimates or claims that the checks have already passed.
4. After approval, call `af task run TASK_ID --confirm-plan FULL_PLAN_ID` with the same selectors.
   If the plan changed, display the new preview and obtain a new decision.

The CLI does not inject messages into either host application or authenticate who typed a local
confirmation flag. Host approval remains the agent's responsibility. This confirmation prevents
accidental first execution and stale-plan execution; it is separate from cryptographic approval
of generated plans.

## Automation, resume and generated plans

Automation explicitly opts in with `af task start --execute ...` or
`af task run TASK_ID --execute`. `--execute` and `--confirm-plan` cannot be combined on run.
No implicit approval is inferred from `--json`, a pipe or the absence of a terminal. Existing
execution walkthroughs using `--execute` are automation examples. The `af review run` surface
retains its existing explicit-execution behavior.

Admitted Tasks can resume with `af task run TASK_ID`; finished Tasks return their recorded
result without spending again. There is no cross-Task trust cache: each new Task/plan
needs confirmation or explicit automation. Editing a live shared definition does not replace
an already captured plan; refresh/replanning produces the identity that must be confirmed.

For no-fit Tasks, the first preview describes the fixed Planner preparation. Confirming it
permits only that bounded planning work. The resulting generated execution plan still stops for
its [signed developer decision](generated-plans.md). Neither `--confirm-plan` nor `--execute`
can replace that signature. Approval waiting remains inside the original Task deadline.
