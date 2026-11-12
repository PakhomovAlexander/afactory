# Issue-oriented operator Pipelines

This is executable, staged **consuming repository authority**. A Task Worker cannot edit `.af/`.
The root operator installs it after the candidate verifies. This checkout's existing authority
is deliberately not silently modified. The replacement catalog contains none of the old
`default/*`, `default-light/*`, `pi-light/*`, `warm/*` or operator `kernel/*` choices; it retains
backlog triage as an independent service. Captured historical Tasks keep their original bytes.
The [decision](../../docs/adr/2026-11-12-refresh-issue-workflow-model-families.md) records the boundary.

## Select and inspect

```sh
python3 -I scripts/issue-workflows.py preview
```

| Selection / public implementation Pipeline | Required earlier stage | Implementer | Code judges |
|---|---|---|---|
| `bugfix/light-kimi-terra-sol` | Known cause and narrow deterministic regression | Pi `kimi-coding/k3.1-pro` high | Terra high review; Sol medium evaluation |
| `bugfix/light-codex` | Same light scope | `gpt-6.1-sol` medium | Terra high review; Astra medium evaluation |
| `bugfix/default-codex` | Cause understood | `gpt-6.1-sol` medium | Terra high bugs; Astra high correctness; Astra medium evaluation |
| `bugfix/research-fix-codex` | Accepted diagnosis from Sol high, checked independently by Terra high | Default implementer | Default judges |
| `impl/default-codex` | Requirements understood | Default implementer | Default judges |
| `impl/light-kimi-codex` | Small specified non-authority requirement with deterministic acceptance | Pi `kimi-coding/k3.1-pro` high | Terra high review; Sol medium evaluation |
| `design/astra-kimi` | Original issue contract | Astra medium design author | Terra high challenge; Kimi high independent challenge; Sol medium final acceptance |
| `perf/research-kimi-codex` | Small known bottleneck with measured reproduction; accepted Kimi high research checked by Terra high | Default implementer | Default judges plus Astra medium measurement evaluation |
| `perf/research-experiment-codex` | Accepted Sol high research, checked by Terra high | Default implementer | Default judges plus Astra medium measurement evaluation |

Terra means `gpt-6.1-terra`, Sol means `gpt-6.1-sol`, and Astra means `gpt-6-astra`.
Every Codex binding is `codex-personal`; Pi uses the machine-local alias `pi-personal` for an
admitted `kimi-coding` Provider. No credential is supplied here. There is no advisory/uix Worker.
The shared Review policy retains an optional bugs policy entry to allow light's single bound
reviewer; default graphs bind both reviewers and require every bound result for closure.
Do not describe this as per-Pipeline enforcement of required benchmark Demands.

The native `kernel` check still runs `scripts/verify.sh --profile fast` locally, and Markdown
checks remain mandatory. No check node selects `remote_checks`. Independence is checked against
the source writer. The final `issue-internal/verification` Task has no implementer and is shared
by every selection; it is an internal capability, not nine extra selectable profiles.

`catalog.toml` pins the staged package bytes. `contracts.json` retains native public contracts.
`replacement-catalog.toml` is complete replacement authority; `replacement-af.toml` selects
`bugfix/light-kimi-terra-sol` by default with the existing limits and reserves. This ordinary native
default accepts a Task containing only `task_id`, `kind` and `goal`, without a `standard` selector
fact. Other staged selectors and the launcher's routing attestations remain required. Package definitions,
role choices and pins are versioned here; local bindings may supply only personal account aliases.

## Review and install the hard migration

First record the repository, every Workspace's Store, bindings file and active/pinned Tasks.
Run against each machine-local Store, with the candidate binary explicitly selected:

```sh
python3 -I scripts/issue-workflows.py --af /absolute/path/to/candidate/af \
  --repo /project --state /private/tasks migration > /private/migration.json
```

This reads native Task inventory and prints old package removals, exact desired pins, replacement
files, a catalog file digest and every retained Task ID. `ready: false` identifies active Tasks;
quiesce them before applying the manifest. This is a preview, not a lock or installation command:
repeat it under the operator's exclusive workstream coordination immediately before switching.
Inventory every Store, including explicitly pinned history, rather than relying on one default
Store to discover other machines or hidden paths.

After verification and review, the operator:

1. Copies `task-packages/` to `.af/task-packages/`, replacing the catalog and settings from the
   two `replacement-*.toml` files. Installs `approved-code-policy.toml` and
   `approved-report-policy.toml` byte for byte as `.af/code-policy.toml` and
   `.af/report-policy.toml`. These versioned files preserve the current local kernel command,
   required Markdown check, bounds and measurement definitions; review and commit them together.
2. Removes the old selectable package entries by replacing the catalog, rather than merging old
   choices back. Removes superseded slot overrides and workstream pipeline names from local
   bindings; only personal account aliases remain. No captured Task is edited, replanned,
   deleted or treated as a new catalog choice. Unreferenced old package directories confer no
   selection authority; their removal is optional and is not Store collection.
3. Runs `af catalog test --source . --manifest fixtures/issue-workflows/catalog.toml --json`,
   plans the selected bounded pilot, checks its effective roles, and commits reviewed authority.
   Preserves the old exact authority commit for already captured Tasks.

The launcher `plan` refuses an installed catalog or policy file that differs from staged authority.
`compose` checks those exact files at every pinned authority commit. `plan`, `check-plan` and
reconciliation follow the native captured authority in the Store and check the effective code or
report policy identity and the exact captured catalog bytes, and refuse remote check selection.
Code Tasks must retain the complete native Review policy: severity gate, reviewer requirements,
clean and maximum rounds, targeted repair setting and check policy. Report Tasks retain their
native report-only policy authority; missing required policies, explicit nulls and extra domain
policies are refused. Installed and pinned catalogs must match the staged bytes exactly. Missing
Store metadata fails closed. The launcher also
refuses local slot/package overrides and nonpersonal aliases. Native direct planning is a lower
level operator interface; it does not enforce the launcher's routing attestations. Never claim
that docs or labels enforce scope on a caller bypassing this interface.

## Native stage composition

The public Pipeline is each selection's implementation entry point. For research/design, its
`research_report` port and the implementer's corresponding port are required `af/Document@1`
inputs. A raw artifact cannot bind that root: native admission requires a recorded Task output.
The supported complete workflow is the native Scenario emitted by `compose`:

```text
[issue-internal/report or issue-internal/kimi-report, independently accepted]
    report -> public implementation Pipeline.research_report
    snapshot -> issue-internal/verification.source
```

Implementation consumes the accepted Document on its declared Worker port and retains the same
artifact ID in candidate/report provenance. Research prose carries evidence, not execution
approval. Report operators do not run inside an implementation profile.

Copy example Task files to a private directory. Set implementation and final verification to
identical `goal`, `requirements`, `acceptance` and `issue` values (including absence).
For pilot reconciliation, set `requirements.issue_url` to the exact GitHub issue URL in the
pilot manifest. This structured field is captured in native `af/Requirements@1`; a top-level
Task `issue` or manifest URL alone cannot establish the binding. Local Tasks and composition
may omit `requirements.issue_url`, but those Tasks cannot be reconciled as linked pilots.
`requirements.acceptance` must contain the original issue's nonempty acceptance criteria.
Research/design keeps its own stage goal and acceptance: it judges report completeness, never
claims to have implemented the fix. Its `requirements.original_issue` must contain exactly
`goal`, `requirements` and `acceptance`, copied from the implementation's actual fields
(use JSON `null` for omitted top-level `acceptance`). This includes all original structured
requirements, including `issue_url` when present, alongside the full goal and acceptance. Do not wrap an unrelated goal in a common identity.
Every report pipeline sends this same native `af/Requirements@1` to their author and independent
verifier; their staged prompts require checking the report against the embedded issue details.
The original issue-contract examples describe the concrete same-source issue-contract substitution defect;
replace that whole contract consistently for a different issue. This contract-sensitive example
uses the default known-fix route; select light only for a different, eligible narrow issue. Set verification's `base.commit`
to the original source commit. Pin each stage from
that **same committed source** with `af scenario template-pin`. For example:

```sh
af scenario template-pin --file /private/pilot/diagnose.json --repo /project \
  --task-state /private/tasks --authority-commit EXACT_COMMIT --json > /private/pilot/research.pin.json
af scenario template-pin --file /private/pilot/research-fix.json --repo /project \
  --task-state /private/tasks --authority-commit EXACT_COMMIT --json > /private/pilot/code.pin.json
af scenario template-pin --file /private/pilot/verify.json --repo /project \
  --task-state /private/tasks --authority-commit EXACT_COMMIT --json > /private/pilot/verify.pin.json
python3 -I scripts/issue-workflows.py compose bugfix/research-fix-codex \
  /private/pilot/research.pin.json /private/pilot/code.pin.json /private/pilot/verify.pin.json \
  --routing /private/pilot/routing-diagnosis.json \
  > /private/pilot/workflow.json
```

For design use `design.json` with `design/astra-kimi`: it composes from **one** report pin and
ends at an accepted reusable design, with no implementation or code verification stage.
Astra medium authors the original design; Terra high and Kimi high independently challenge that
same sealed Document. Sol medium final acceptance receives the original Document, original
issue requirements and both required typed `af/ReportEvaluation@1` challenge inputs. Failed,
inconclusive, duplicate, missing or stale challenges refuse final dispatch. Report acceptance
retains the final evaluation whose provenance includes both challenge IDs. The retired
`impl/design-codex` name has no alias.

To reuse that accepted design, select `impl/default-codex` and give `compose` three pins in
order: `design/astra-kimi`, `impl/default-codex`, shared verification. The optional
`research_report` input is delivered from the accepted design Task, with exact original issue,
source and producer checks; omitting that pin keeps the ordinary two-stage route. Native direct
planning can bind `inputs.research_report = {"task":"DESIGN_TASK_ID","port":"report"}`;
use composition for the launcher's additional original-issue validation. Research prose remains
evidence, never execution authority.

Broad performance uses `issue-internal/report` and `perf/research-experiment-codex`;
small known bottlenecks use `issue-internal/kimi-report` and `perf/research-kimi-codex`.
Simple selections take only code and shared verification pins.
`compose` checks routing facts and the exact catalog and approved policies at the pinned Git commit. It refuses missing
stages, missing or differing original research/design issue contracts, differing implementation
and verification goals or structured requirements/acceptance, differing source
commits/Stores, substituted pipelines and
nonpersonal bindings. Each stage retains its own native authority ID; profiles legitimately
capture different authority objects on the same commit.

Create an owner-only `af/scenario-policy@5` file outside the checkout, containing exactly those
`template_id` values under `task_templates`, and empty `adapters`, `task_plans`, `decision_keys`
and `file_roots`. Validate using `af scenario validate --file workflow.json --authority policy.json
--json`. Review the exact pin before `af scenario start ... --confirm-pin PIN --execute`.
No Action or remote adapter is present. Native dispatch authenticates completed accepted
predecessors in the same Store, checks research's original source and 32 KiB bound, and refuses
missing, rejected, changed-source or oversized evidence before implementation. Scenario receipts
and the native Task ledger own cumulative stage reservations and costs. No paid probe is needed
to construct, compile or validate these fixtures.

Performance measures the committed `release_build` workload and `release_build_time` objective:
baseline, implementation using accepted research, full code Review, candidate measurement,
deterministic Comparison and comparison-aware acceptance. Required goal coverage ends at that
acceptance. Missing or non-improving comparison cannot satisfy it. This initial entry is not a
general native lab roadmap or a claim about a different workload. No performance publication
Action is composed. Research precedes the experiment's baseline; no model invents Measurements.

## Bounded pilot recipes

For a small known-fix pilot, choose an issue with an actual deterministic failing reproduction,
known cause and narrow scope. Record those facts in a copy of `routing-known.json`. Authority,
contract, security, accounting, persistence or uncertain concurrency work is ineligible for light.
`pipeline/light`, `pipeline/default` and `pipeline/design` triage labels are routing hints, not
executable names or semantic proof; select the concrete catalog entry and review its facts.

For direct single-stage planning of a simple selection:

```sh
python3 -I scripts/issue-workflows.py --af /absolute/path/to/candidate/af \
  --repo /project --state /private/tasks plan bugfix/default-codex \
  --file /private/pilot/known-fix.json --routing /private/pilot/routing-known.json
```

Save the plan response, inspect the Task with the same binary, repository and Store, and
validate the exact effective plan before authorizing it:

```sh
af task explain TASK_ID --repo /project --state /private/tasks --json > /private/pilot/inspection.json
python3 -I scripts/issue-workflows.py --state /private/tasks check-plan bugfix/default-codex /private/pilot/inspection.json
af task run TASK_ID --repo /project --state /private/tasks --confirm-plan EXACT_PLAN_ID
```

Use the same explicitly selected candidate binary for the native commands above. Review the
exact `plan_id` in the inspection, the local kernel/Markdown checks and absence of remote checks.
Before any owner-authorized execution, admit at most six concurrent owner runs and require at
least 12 GiB free on every execution volume. These are operator admission conditions, not checks
implemented by this planning-only launcher. Do not launch automatically or use local model,
slot or package overrides. For composed workflows, validate and confirm the exact Scenario pin
as described above; retain all child Task accounting. This committed entry point replaces private
`step.py` and `mk_task.py` selection logic.

Use native composition for final independent verification, and for every staged selection.
`--state STORE check-plan SELECTION inspection.json` checks each declared role’s exact Worker,
effective model/effort/Provider, approved policy identity and the exact
native package closure from `af task explain TASK --json`. It validates data; it does not
approve execution. Native gates and exact-plan confirmation remain mandatory.

There is one implementation Attempt per Task, no automatic repair or backlog dispatch. The
launcher refuses light continuation. If the owner authorizes one manual repair, retain the
original reviewed candidate and cumulative chain with native continuation, keep the original
Base and issue acceptance, and run shared default verification over the **entire final change**.
A second repair exits the light policy and requires a new reviewed default/research decision;
never reset spending by reporting just its final delta.

For `impl/light-kimi-codex`, routing additionally requires `specified_requirements` and
`deterministic_acceptance`, alongside `narrow_change` and no sensitive flags. It does not require
an existing failing bug or known bug cause. For `perf/research-kimi-codex`, require
`known_cause`, `known_bottleneck`, `measured_reproduction`, `narrow_change` and no sensitive flags; broad or
unknown cases go to `perf/research-experiment-codex`. These booleans are explicit operator
attestations, not independently measured evidence. The performance Pipeline still measures only
`release_build` / `release_build_time` using the unchanged native baseline/comparison policy.

For a hard diagnosis pilot, use `routing-diagnosis.json`, state the competing causal hypotheses
and exact reproduction requirements in `diagnose.json`, and compose research-fix. A rejected
report is a failed pilot, not permission to implement a guessed cause. Design-first uses the same
bounded recipe with interface/authority invariants and adversarial design verification.

Copy `pilot.example.json`, replace its synthetic issue/source/Task identities consistently with
the implementation requirements and embedded research/design `original_issue`, and record the PR
URL only after an operator publishes separately. Put the original implementation and any repair
in `tasks`, research Task IDs in `research_tasks`, and final shared verification in
`verification_task`. Reconcile read-only:

```sh
python3 -I scripts/issue-workflows.py --af /absolute/path/to/candidate/af \
  --repo /project --state /private/tasks reconcile /private/pilot/pilot.json
```

Reconciliation reads native checked inspections, retains plan/revision/source bindings, sums each
Task's cumulative charge once, refuses skipped continuation ancestry and preserves unknown usage
separately. It reads content-checked native CAS metadata named by inspections: the original
Snapshot origin must name the manifest commit and the invoking repository's exact native
repository identity (sorted root commits). Every implementation must select the declared
Pipeline. Recorded `research_report` bindings require exactly their accepted report/design
producers, original source and report output/revision/result identities. Reconciliation also
reads each producer's native Requirements and compares `specification.original_issue` against
the original implementation's recorded goal, specification and native acceptance items.
It refuses accepted reports about another issue, even on the same source and Pipeline; stage
acceptance never substitutes for implementation acceptance. All required charges
are counted once. Routes without research refuse research declarations. Final verification
must bind the last candidate, retain the original Base and preserve the original goal and
structured requirements/acceptance. Missing native metadata fails closed. The manifest issue must exactly match
`specification.issue_url` in the original implementation's native Requirements. A declared PR
must name the same GitHub owner/repository as the issue (case-insensitive). The script's
`af.issue-workflow-reconciliation/2` output labels `pull_request_association` as `unverified`
(or `absent` for a null PR) and records `remote_lookup_performed: false`. Native evidence does
not establish the external PR's existence, contents, issue linkage or relation to the candidate;
reconciliation does not look up GitHub or authenticate remote issue state. The pilot manifest
shape and native runtime schemas are unchanged. The absence of
final verification is reported explicitly; reconciliation alone is not an acceptance receipt.
Do not infer live quality, economic savings or throughput from deterministic planning tests.

## Deterministic evidence

`issue_workflows` in the native integration binary compiles all nine public model plans,
plans both research stages and read-only verification, checks models/efforts and local gates,
consumes a real accepted hermetic Report output, refuses missing Documents and old names, and
validates composed native Scenarios using exact captured template pins. Providers are isolated
catalog fixtures, never live model calls. Script tests cover routing, composition, active-history
migration refusal, personal bindings and cumulative-chain reconciliation. The Task gate runs
fast validation after sealing; live bounded pilots and installation remain operator work.

## Root activation after integration verification

This integration candidate also changes staged investigator, designer and report-verifier prompts
to evaluate the exact original issue contract. Their staged catalog pins bind the reviewed bytes.
The root operator must copy the updated staged `task-packages/` and replace
`.af/task-catalog.toml` with `replacement-catalog.toml` in one reviewed authority update;
retain code/report policies and existing Task pins. Replace the three updated report Worker
prompts and pins as well as both
`.af/task-packages/backlog/triage-{author,verifier}/instructions.md` and their catalog pins.
Workers in this Task cannot perform that operation. The installed-catalog audit checks installed
bindings and pins; the rubric audit checks the updated staged authority and active docs/examples.
After that update, remove `.af/task-packages/{default,default-light,pi-light,warm,kernel,owner-default}`.
Generic digest and schema tests use supported staged packages and do not read those directories.

Inspection validation compares the complete supplied plan and graph, with JSON types preserved,
against their content-checked native `af/ExecutionPlan@4` and `af/CompiledTask@4` artifacts. It
checks Task/revision linkage before approved roles, policies and package closure. Installed and
captured template authority must also contain the exact staged `replacement-af.toml` settings,
including the default selector and unchanged limits. Validation grants no execution authority.

The performance preview follows research, baseline, implementation, internal code Review,
candidate measurement, Comparison, measurement acceptance and final shared whole-change
verification. Internal Review and final verification are separate stages; earlier failures may
prevent measurement or acceptance, and no preview establishes a measured improvement.

## Latest families at new planning boundaries

Launcher `plan` and `compose` consult token-free `af provider models ID --json` catalogs for
all selected Worker closures, including each composed research and final verification stage.
Stable numeric versions are compared separately for Sol, Terra, Astra and ordinary Kimi Pro;
`k3.1-pro-highspeed` and other model families cannot supersede ordinary Pro. Astra6 remains
selected because the verified catalog contains no Astra6.1. Exact model pins remain versioned;
no catalog result edits authority or silently substitutes a model. Unavailable, skipped,
ambiguous, duplicate or incomplete catalog data refuses new planning. Catalogs describe exposed
models and effort levels, not paid admission or account usability.

`preview`, `migration`, `check-plan` and `reconcile` do not probe current catalogs or rewrite
historical Tasks. Native direct `af task plan` / `af scenario template-pin` remains a lower-level
interface without the launcher's latest-family guard or routing attestations. Installation and
whole-change independent verification remain root operator work. Neither deterministic tests nor
this catalog refresh establish a measured speedup.

For Tasks captured under an earlier catalog, inspect natively or run that catalog's matching
launcher from its preserved immutable source checkout. The current launcher's exact-authority
checks deliberately refuse old package closures; they do not reinterpret old roles as new ones.

`light-implementation.json` is a non-bug implementation template. `research-kimi.json` and
`performance-kimi.json` share a release-build original issue contract. Fill in the exact narrow
bottleneck and existing measured reproduction before using them, and make a verification Task
with that same complete goal/requirements/acceptance. No sample contains measured timings or
claims success. The routing booleans attest eligibility; they never replace native Measurements.
