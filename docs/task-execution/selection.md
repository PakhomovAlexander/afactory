# Select a captured Pipeline

Task planning selects from the captured catalog before any Planner invocation. The original
Task request stays intact: asking for `project/small` and selecting `project/heavy` records both
names. The selection artifact, original request revision and selected adapter are content
addressed; the final Task revision binds them into the execution plan.

```text
Implement this Jira ticket
         |
         v
Capture Task inputs + facts + mandatory acceptance
         |
         v
Existing Pipeline contracts
  small: known fact contradicts applicability ---> record no_fit
  heavy: inputs + outputs + coverage fit ----------> compile child contracts
                                                        |
                                               resolve captured Workers
                                                        |
                                               check budget feasibility
                                                        |
                                               select existing heavy
                                                        |
                                               persist plan, zero Attempts
                                                        |
                                               run / inspect / resume
```

A Task file may name a preferred Pipeline:

```json
"pipeline": {"name": "project/small", "fallback": "generate"}
```

`refuse` considers only that Pipeline. `select` searches the remaining catalog after the
preferred choice fails. `generate` performs that same search and returns `needs_generation`
only when every candidate has a known semantic mismatch. Missing packages, unavailable
Providers or executables, insufficient budget, missing facts and unresolved ties cannot
trigger generation. Generation itself is described in [generated plans](generated-plans.md).

Omit `pipeline` for automatic catalog selection. The captured project catalog supplies optional
ranking per Task strategy and `no_match = "refuse" | "generate"` (default `refuse`):

```toml
no_match = "refuse"

[selection.small]
"project/small" = 1
"project/heavy" = 2
```

Lower numbers win. An explicit feasible choice takes precedence. Missing rankings have equal
last priority, so distinct equally ranked fitting candidates produce `ambiguous`. Selection
checks all candidates in the current priority group, and advances only if none is feasible.
Lower priority semantic matches remain `compatible`, meaning their capabilities were not
checked. Unused catalog Workers do not cause unrelated account reads.

Compatibility checks Task kind, known facts, public inputs, required outputs and acceptance
coverage. Unknown facts are not inferred. Exact structural compilation then checks the expanded
children, installed signatures, effects, lineage, independent verification and replacement
contracts. Resource admission accounts for mandatory work and protected verification, including
paid Provider admission, before dispatch. The recorded deadline never resets on fallback or
resume. Declared reservations are bounds, not predictions of model latency or success.

Worker executable checks and Provider account identity reads during planning are token-free.
They do not prove that the model service will accept a request. Model capability admission runs
as a paid node under the selected Task budget and can still block business execution. Required
host check programs are checked without running them; relative source programs and container
image tools must be resolved by their actual runtime environment.

`af task plan --file ticket.json --json` shows the selected plan and selection assessment with
zero Attempts. `af task explain TASK_ID --json` exposes the same recorded assessment. A selection
refusal exits 1 with `af/task-selection@1`, the persisted selection and request artifact IDs,
candidate reasons and zero charged Attempts. No execution Task is opened for a refusal. Start
and plan use the same selector. Once selected, resume recompiles the captured root and closure;
it does not reroute against edited working files or a changed live catalog.

The selection regression matrix covers explicit choice, automatic ranking, missing facts,
ambiguity, infeasible preferred definitions, exhausted deadline headroom, missing Provider
bindings, missing executables and permitted semantic no-fit. Its heavy fallback case plans
without Attempts, then executes exactly the three existing implementation/check/evaluation
Attempts after the live catalog and heavy package have been removed. Finished replay preserves
the same selection and spends nothing further. The JSON Schema checks all assessment and
decision variants and rejects unknown fields.
