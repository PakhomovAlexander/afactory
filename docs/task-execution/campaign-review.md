# Campaign frontend retirement

The installed `af review` interface uses native Task review and the common Task Store. There is
no Campaign execution fallback, captured Review host, phase translator or separate Campaign UI
pane. Campaign selectors such as `--campaign`, `--policy-rev`, `--pipeline` and `--heavy` are
refused on Review run/plan; their authority cannot be inferred from an old `.af/pipelines/` file. The retired `.af/workers/`
Registry root is undeclared; current packages belong in the Task catalog
and its declared package directories.

Use a Review Task file and committed `.af/task-catalog.toml` authority. Select its trusted commit
with `--authority`, its candidate with `--candidate` or `--uncommitted`, and the Diff Base with
`--base`. The Pipeline must declare the required source ports. An empty Diff is refused before
Gates, Provider admissions and Workers.

| Command | Native path |
| --- | --- |
| `af review plan --file FILE` | Capture and compile a common Task plan without beginning an Attempt |
| `af review run --file FILE` | Capture and execute native Review under one Task allowance |
| `af review inspect ID` | Common Task explanation, graph, bindings, limits and history |
| `af review show ID` | Common Task state and check visibility |
| `af review ledger ID --json` | `af/review-ledger@2`, complete canonical Findings and original Report provenance, including missing reviewer coverage |
| `af review report ID --json` | `af/task-report@3`, exact cumulative Task accounting |
| `af review resume ID --confirm-plan PLAN_ID` | Resume the original common Task plan |
| `af review continue ID --from PRIOR_ID` | New native Task plan with exact prior Review history and Base |
| `af review export ID PROPOSAL_ID` | Validate and print the exact sealed proposed repair |
| `af review decision-context ID --json` | Checked canonical identities, Demand lifecycle, current resolutions and policy time |
| `af review decide NEW --from ID --file BATCH` | Plan an exact zero-token human decision Task; `--pipeline` selects its committed native Task package |

`af review ledger ID --format sarif` emits the common SARIF profile. `inspect --plan PLAN_ID`
reads a recorded plan. `--repo`, `--state` and `--workspace` use the same selectors as Task
inspection; the Store stays outside the repository. The terminal browser exposes Review in
`tasks/`, alongside implementation and other Task kinds.

`af provider doctor ID --confirm-plan PLAN_ID` executes only that captured Task's bounded
Provider admission nodes. Their selected receipts and charges are the same ones execution uses.

`continue` plans a new authorized Task; `resume` resumes the original exact plan. A standalone
Review continuation restores the previous reviewed SourceTree and its exact Base, including a
Base originally supplied by `--base`, and binds the completed canonical ReviewHistory. It does
not require an implementation candidate or an implementation-only findings port. Prior Reports,
Finding identities and corroborating claims remain in the history. Each Task reports its own
exact cumulative charges; continuation does not reset or rewrite its predecessor's accounting.
The captured Review policy's existing Round bound still applies across this history. A heavy
Round inside one Task uses that Task's allowance and bounded convergence policy.

A sealed Proposal can be inspected/exported even when its whole Review requests changes or its
author did not nominate it for automatic Integration. Its original selected Reviewer Result,
Report indices, Finding provenance, complete candidate diff and original base are validated.
`--allow-stale` is explicit authority to print an older-base patch; it grants no promotion,
delivery, branch, push or publication authority. Automatic Integration retains its separate
credential and eligibility restrictions.

Export defaults to the current plan. `--plan PLAN_ID` selects a plan from this Task's recorded
current-format history, validated through an exact log prefix. The current authoritative head
still controls stale-base refusal. No archived inspection restores execution approval.

See [Review Tasks](review-task.md) for the executable token-free fixture and canonical reduction,
[the capability record](../design/current-contracts-and-task-review.md) for concrete native owners and regressions, and [bounded repair](bounded-repair.md) for native repair operators.
Generic storage inventory, registered Store retention and writer locks remain supported,
including directories whose storage category is named Campaign. Historical release dispatch,
installation, rollback, signing and byte pins are unchanged.
