# Export and reuse a Pipeline

`af task export` writes a reusable catalog from a captured Task plan. It leaves the Task's
plan, decisions and execution history unchanged, including when the Task is still waiting for
developer approval. A preparation plan that has not produced a final definition cannot be exported.

```text
Developer A                                Developer B
-----------                                -----------
Task -> Planner -> exact plan
                    |
                    +-> approve -> execute
                    |
                    +-> export -> review/test -> Git commit
                                                  |
                                      explicit catalog sync
                                                  |
                                      new Task -> select existing
                                                  |
                                      execute (zero Planner calls)
```

Export into an absent project-relative directory, then review and commit the emitted files:

```sh
af task export TASK_ID --name team/pagination --destination shared/pagination --json
git add shared/pagination
git commit -m 'Share pagination Pipeline and contract fixtures'
af catalog test --source . --revision HEAD --manifest shared/pagination/catalog.toml --json
```

The bundle contains `catalog.toml`, `contracts.json`, and exact Pipeline, default Worker and
selected Task-kind packages. Every public artifact input remains a typed parameter. Applicability
facts, output contracts, coverage bindings and Attempt limits remain explicit; export does not
remove a constraint to make a definition appear more general. The command recomputes package pins
from the emitted bytes. Generated child names are nested under the chosen shared name, and Calls
are updated consistently.

The bundle uses `path_base = "manifest"`, so moving the entire directory preserves its pins.
Physical package directories use package-name hashes to keep parent and child names from
overlapping. Public names remain in the catalog and Pipeline definitions. Existing catalogs keep
repository-relative paths when `path_base` is omitted.

Export selects the original shared Worker defaults even when the Task ran with local replacements.
A definition that directly requires a `local/*` default needs a reviewed shared default before it
can be exported. The bundle does not contain the originating Task, its inputs, signed decisions,
provider account bindings or local replacement packages. Recognizable originating Task identities
or literal Task text embedded in a package cause refusal. Shared Worker package files remain part
of the reviewed bundle; inspect their instructions and command dependencies before sharing them.
Exact verifier-policy digests remain compatibility constraints for the importing project.

Export validates captured packages and recompiles the recorded plan using recorded binding data.
It requires the recorded compatible engine, but does not contact a Provider or require a current
account login. Recorded bindings used for this inspection cannot dispatch a Worker.

Import the committed catalog into another developer's project:

```sh
af catalog sync --source ../developer-a-project --revision HEAD \
  --manifest shared/pagination/catalog.toml --destination .af/vendor/pagination --json
```

Review the import, add `.af/vendor/pagination/catalog.lock.json` to the project's catalog imports,
and commit that configuration. Remove duplicate package entries instead of shadowing them. Keep
the project's verification, independence and local Provider settings. A matching new Task can now
select `team/pagination` as an existing Pipeline; generating it again is unnecessary. Export itself
does not change the active catalog or approve execution.

## Contract checks and prerequisites

```sh
af catalog test --source . --manifest shared/pagination/catalog.toml \
  --pipeline team/pagination --json
af catalog test --source . --manifest shared/pagination/catalog.toml \
  --worker fixture/evaluator --json
```

The command reads one exact local Git revision, validates package pins and dependency closure,
checks Worker payload schemas, and compares the complete catalog with its closed contract
fixtures. It refuses missing fixtures, changed coverage, unknown package selectors and unsafe
paths. `--fixtures` can select another fixture file in the same commit. Without that option,
`contracts.json` is read beside the catalog manifest.

The report lists required command programs or Provider/model/effort bindings. Program discovery
does not execute a binary or prove environment admission. Model entries require local admission
when planning a real Task. The report records zero Attempts and `business_acceptance =
"not_executed"`: interface checks are separate from Task execution and its independent acceptance.
The credential-free reuse integration test exercises actual implementation, checks and evaluation
after export, Git capture and import.
