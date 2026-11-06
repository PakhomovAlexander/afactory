# Implementation through human merge

A Pipeline describes one Task's execution and typed acceptance. The reusable `impl` Scenario
orchestrates accepted Tasks, owned candidate preparation, signed PR publication, completed CI,
actual human merge and owned cleanup. Models run only inside Tasks. There is no verification-only
Task, automatic merge or scheduler installation.

`scripts/scenario-impl.py` is an offline authoring recipe. It creates packages in a consuming
repository, using the existing native package manifests and `af catalog digest`; it installs
nothing into Afactory's catalog. Supply the team's runners, Provider bindings, Attempt budgets
and native independence policy in a JSON file:

```json
{
  "workers": {
    "implementer": {
      "runner": {"kind": "model", "provider_kind": "codex", "model": "YOUR_MODEL", "effort": "medium"},
      "provider": "YOUR_CONFIGURED_PROVIDER",
      "attempt": {"tokens": 100000, "wall_ms": 900000}
    }
  },
  "independence": {
    "command_workers_by_package": true,
    "distinct_principals": false,
    "distinct_providers": false,
    "distinct_models": true
  }
}
```

The [complete inert shape](../fixtures/scenarios/impl/team.example.json) supplies all four roles
with placeholder models and no live bindings. The abbreviated example above must be completed with `correctness`, `bugs` and `evaluator` entries of
the same shape. Choose each runner and reservation explicitly; command runners use the native
`command` object and zero tokens. Personal accounts, models and effort choices belong in the
consumer's workstream configuration. Both reviewers are required and independent of the
implementer; the evaluator is also independent. Native admission enforces the supplied independence
policy. Distinct principals, providers or models can be required by the consuming policy.

From the consumer repository (output must be a new directory immediately below
`.af/task-packages/`):

```sh
python3 -I /path/to/afactory/scripts/scenario-impl.py packages \
  --output .af/task-packages/my-team --namespace my-team \
  --af /private/tools/af --team /private/workstream/team.json --check project-gate
```

Review the generated `catalog.toml`, merge its entries into the project's catalog, and commit
the packages and policy together. Keep the project's local checks under `.af/code-policy.toml`.
The generated catalog is a complete minimal example, not permission to overwrite existing
configuration. Recompute pins after any package edit. No personal pipeline is shipped in AF.

The generated implementation Pipeline routes optional typed continuation `findings` to the
implementer. `af task run NEW --from OLD` uses the consumer's `[task.defaults]` Pipeline and
carries native findings and candidate provenance. The focused regression actually plans and
executes this continuation with inert Workers; a bundle's structure alone proves no such support.
The correctness reviewer has `read-source` and `execute-checks`, returns required exact-Snapshot
`ImplementationTestEvidence@1`, and must build, privately install or stage the candidate in the
project's appropriate isolated test environment, and execute meaningful tests and acceptance cases.
The evaluator receives that evidence, local checks, requirements and the reduced Review Round
through explicit typed edges. Failed or unavailable execution cannot become passing acceptance.

Author a native implement Task using `my-team/implementation`, `verification: "review"`,
`facts: {"standard": true}` and adequate Task/verification limits for the team's declared
reservations. Review the exact native plan before execution. Pin the committed Task template
with `af scenario template-pin` as described in [Scenarios](scenarios.md), then build the bundle:

```sh
python3 -I /path/to/afactory/scripts/scenario-impl.py bundle \
  --pin template-pin.json --destination github:OWNER/REPOSITORY \
  --title 'Implement the requested change' --body-file pr-body.md \
  --repairs 2 --owner owner --output impl-bundle.json
```

The recipe accepts any reviewed implement template; inspect its pinned package contracts before
granting machine authority. The Scenario name is `impl`; its Pipeline name is configurable.
Template pinning, policy installation, Run creation and signing remain operator-controlled.
Use the separately installed [Git workspace capability](scenario-git-workspace.md) under adapter
name `workspace`, granting only its preparation, publication, observation and cleanup operations
at the exact destination. Its exclusive owned writer restriction and all signed publication,
lease, recovery and owned-resource checks remain binding. The
[executable-review guide](task-execution/executable-review.md) explains evidence limits.

`--repairs` permits zero, one or two fresh implementation Tasks. Only exact completed failing CI
(`checks_completed` with `checks_state = repairable`) selects a repair. Its typed inputs carry
the accepted predecessor Task, completed observation and preparation Action; native admission
adds CI evidence to requirements. Each repair repeats reviewers and evaluator, prepares with the
previous owned receipt, and requires a fresh exact signed Decision before updating the same PR.
Lazy selection avoids consuming skipped receipts. Pending, missing or unknown CI never starts
repair. After the finite repairs, successful checks still precede a separate actual `merged`
Wait and cleanup. Closed-unmerged PRs cannot complete. The Run has finite token, elapsed-time and
dispatch limits; repeated pending observations consume dispatch allowance and can exhaust it.

The current generic evidence contract admits CI outcomes only. Review comments/change requests
and remote base/conflict bytes are unavailable as automatic repair inputs. Human review and
merge remain external actions; CI readiness proves neither. Handle conflicts through a separately
captured source and explicit operator action. Task Stores are retained; cleanup removes only the
workspace capability's explicitly owned resources after its independent actual-merge readback.

Run the focused native tests without live services or models:

```sh
cargo test --locked -p af --test it generated_impl
cargo test --locked -p af --test it production_workspace_scenario_repairs_same_pr_and_cleans_only_after_merge
```

The focused generated recipe tests use private homes, command Workers and an inert workspace
outcome service. They cover package pins/planning, another configured team without execution,
native continuation with actual findings delivery, and zero/one/two CI repairs with fresh
signatures, lazy skipped receipts, same-PR identity, separate merge waiting and cleanup. This
outcome fixture models the lifecycle; it does not prove production adapter composition.

The installed production adapter regression
`production_workspace_scenario_repairs_same_pr_and_cleans_only_after_merge` independently
exercises that boundary with real Git commits, worktrees and bare remote refs, real accepted
command-backed Tasks and native signed Decisions. Only external HTTPS/GitHub transport is mocked.
It covers ready and pending CI, stale-head refusal, completed failing CI admitting a fresh repair
with predecessor/source provenance, a second signature and the same PR with exactly one creation
POST. An actual local merge precedes removal of owned worktrees, build roots and local/remote
refs; the merged base remains. It uses the reusable capabilities directly, separately from the
generated recipe fixture.

See [generic capability coverage](scenario-git-workspace.md#recovery-and-bounds) for production
transport/recovery, dirty/foreign cleanup refusals and the ordinary native repair regression,
including `python3 -I scripts/test-scenario-git-workspace.py`. None of these tests claims live
publication.
