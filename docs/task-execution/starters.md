# Shared starter Pipelines and Workers

Create a complete credential-free catalog in an absent directory:

```sh
af catalog init --profile all --destination task-demo --json
cd task-demo
git init
# Review the generated authority and Worker definitions.
git add .
git commit -m 'Configure Task starter catalog'
af catalog test --source . --json
af task start --file implementation-reviewed.json --json
```

The factory runs no Workers and commits nothing. It creates supported typed definitions, actual
package pins, contract fixtures, captured policies and runnable Task files. Git and Python 3 are
the command tutorial's local prerequisites. `--profile software` omits the document definitions;
`--profile document` emits just the document tutorial.

```text
                      shared catalog committed to Git
                 +-------------------+-------------------+
                 |                   |                   |
          implementation        review-light       release-notes
            Pipelines          /            \          Pipeline
                 |       correctness        bugs          |
                 |           Worker        Worker     author/verifier
                 |                                       Workers
          implementer Worker
                 |
                seal
                 |
           call review-light  <--- same shared definition
                 |
         evaluate exact requirements
                 |
       require both acceptance obligations
```

| Definition | Behavior | Maximum Attempts / protected verification |
|---|---|---:|
| `builtin/implementation-small` | Implement, seal, call independent verification | 3 / 2 |
| `builtin/implementation-heavy` | Same verification, with one author retry available | 4 / 2 |
| `builtin/verification` | Check and independently evaluate the exact supplied source | 2 / 2 |
| `builtin/review-light` | One discovery Round with both configured reviewers | 3 / 3 |
| `builtin/review-heavy` | Two declared calls to the same Review definition | 6 / 6 |
| `builtin/implementation-reviewed` | Implement, embed Review and evaluate requirements | 5 / 4 |
| `builtin/implementation-repair-targeted` | One bounded repair, targeted verification and goal evaluation | 8 / 6 |
| `builtin/implementation-repair-heavy` | One bounded repair, fix verification, full S2 Review and goal evaluation | 11 / 9 |
| `builtin/release-notes` | Author, render, check sources and independently verify a document | 3 / 2 |

`builtin/repair-targeted` and `builtin/repair-heavy` are shared three-Attempt child components.
The targeted component exposes its distinct repair guarantee. The heavy component exposes
continuation evidence that the later complete Review consumes. Every child shares the parent
Task allowance. A clean first Review skips repair while retaining the original configured bound.

The command Workers implement the explicit pagination and release-note goals in the supplied
Task files. Evaluators check those requirements against the produced output; reviewers report
actual failures. The fix verifier handles its declared pagination claims and returns inconclusive
for unsupported claims. Configure shared or local Workers with compatible contracts for broader
tasks. These examples establish lifecycle and contract behavior; live model speed and cost remain
separately measured product gates.

The initial pagination source is unfinished. Run an implementation Task and explicitly deliver
its verified result to a new worktree to inspect it. Review Task files can review a captured source
on their own, using the same definition as embedded Review. The generated README contains the
delivery command and expected review behavior. See [local bindings](local-bindings.md),
[bounded repair](bounded-repair.md), [heavy Review](heavy-review.md) and [documents](document.md).

## Generated-plan tutorial

Create a starter with an existing minisign public key assigned to developer `owner`:

```sh
af catalog init --profile planning --developer-public-key /path/to/owner.pub \
  --destination planning-demo --json
```

Review, initialize and commit that directory as above. The supplied `planning.json` changes a
captured applicability fact so none of the existing implementation definitions fits. Its shared
`builtin/planner` Worker runs inside the engine's fixed preparation Pipeline and emits a typed
reviewed-implementation definition. The Task stops at `needs_plan_review` after one Planner
Attempt. Inspect it, create a decision payload, sign it externally and submit the signature using
the [generated-plan commands](generated-plans.md). The original fifteen-minute deadline includes
approval waiting and all later execution.

After approval, execution includes the same shared Review. Export, review and commit the resulting
definition before a second developer imports it. Their matching Task uses the imported Pipeline
with zero Planner calls. Export preserves shared specification checkers byte for byte and refuses
originating execution identities or private Task text in every package file. See [export](export.md) and
[ADR-0060](../adr/0060-generate-working-starters-from-supported-contracts.md).

The software Task files include a machine-readable `requirements` object. It is captured as the
`specification` in the declared Requirements input; the goal remains its descriptive summary. The
command substitutes implement and verify only `tutorial.pagination/1` with the exact documented
fields. Use suitable replacement Workers to interpret broader natural-language requests.
