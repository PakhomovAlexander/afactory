# Implementation through human merge

The reusable native Scenario `impl` runs an implementation Task, prepares its accepted candidate,
obtains an exact signed publication decision, publishes one PR, waits for CI and review, observes
its human merge, and removes its explicit owned worktrees, builds and branches. Completion requires
both the merge observation and successful cleanup. It installs no scheduler and performs no merge.
The [lifecycle decision](adr/2026-11-05-bind-implementation-lifecycle-to-native-scenario-authority.md)
records the separate machine authority.

The initial implementation Task contains its verifier as the required correctness reviewer. There
is no verification Task. The reviewer receives the already applied candidate in a disposable
sandbox, has `read-source` and `execute-checks`, builds and privately installs the CLI, and executes
meaningful tests and installed-binary acceptance cases. Source changes are refused when that
sandbox finishes. Missing execution evidence must block the reviewer and independent evaluator.
The reviewer also returns the required public `af/ImplementationTestEvidence@1` artifact beside
its unchanged flat Reviewer Result. It records the exact Snapshot and build/install/test commands,
exit codes and bounded evidence, including failed or unavailable results. The evaluator's new
`af/ImplementationEvaluationInput@1` contract consumes that exact artifact; the kernel refuses a
passed evaluation for failed, unavailable or stale testing. The existing native execute-checks
reviewer boundary grants no host publication.

The model team is fixed to `codex-personal`: `gpt-6.1-sol` medium implements,
`gpt-6-sol` high performs executable correctness verification, `gpt-6.1-terra` high reviews bugs,
and `gpt-6-astra` medium evaluates acceptance. Both reviewers are required. The native planner
checks the effective bindings, reviewer capability and local-only Gate graph before dispatch.
The standalone project may be named `devrec`; this Scenario adds no `af devrec` command.

## Author the consuming repository

Run the package builder from the consuming repository with a current native af executable. Its
output directory must be absent. The builder copies the reusable native interfaces, supplies
project-neutral instructions and exact model defaults, and computes every pin with the native
`af catalog digest`. It launches no models, changes no shipped package and publishes nothing.

```sh
python3 /path/to/afactory/scripts/scenario-impl.py packages \
  --af /path/to/installed/af --output .af/task-packages/impl --check local
```

Review the generated packages and versioned native `catalog.toml`; use it for a new consuming
repository or merge its entries into that repository's catalog,
using repository-relative package paths. Move the generated catalog to `.af/task-catalog.toml`
or remove the intermediate copy after merging, before committing the authority bundle. Its `[providers]` bindings name `codex-personal` and its
review policy makes bugs and correctness required. Its explicit independence policy permits the
same Provider, account and model across roles; native per-slot isolated Attempts and private
context remain mandatory. Configure a real local `local` check in that
repository's code policy. No remote check mapping is needed. Multiple `--check` arguments select
multiple declared local checks. No package pin is a placeholder.

Copy `fixtures/scenarios/impl-task.json`, replace its goal and requirements with the actual change,
and review its finite Task/verification reservations. Commit the authority bundle and source.
Capture the exact native template; existing pinned Tasks remain unchanged:

```sh
 af scenario template-pin --file impl-task.json --repo /project \
   --task-state /private/tasks --authority-commit COMMIT --json > template-pin.json
python3 /path/to/afactory/scripts/scenario-impl.py bundle --pin template-pin.json \
  --destination github:owner/project --title 'Implement the requested CLI' \
  --body-file pr-body.txt --repairs 2 --output impl-bundle.json
```

The bundle builder retains the native template digest; native Scenario validation rehashes the
complete template and refuses a mismatch before any dispatch. It writes `af/scenario-bundle@4`
with root `impl`; no hand-edited definition pin, account setup, gate publication or public PR is
part of authoring. The PR title/body are fixed before preparation and remain the same during
repairs. Include the consuming repository's required PR evidence in the body.

## Install bounded machine authority

Use `schemas/scenario-impl-config-v1.json` for the complete `af/scenario-impl-config@1` machine
configuration. Keep it owner-only outside every checkout. It declares:

- The canonical source `repo`, external native `task_store`, private `worktree_root` and new
  private `state_dir`, all disjoint; exact initial `source_commit` and `source_content_digest`
  from the native template; `base` and an owned `branch_prefix`.
- The GitHub repository name and numeric ID, authenticated actor's numeric ID/login, private
  `token_file`, required check identities and explicit required reviewer identities. An empty
  reviewer list means the accepted native reviews and eventual human merge provide review
  authority; external changes-requested reviews still block readiness and admit bounded repair.
- Exact native `af` and `git` executable paths and SHA-256 digests; a private SSH configuration
  and its digest; the commit author name/email. Push uses only
  `ssh://git@github.com/OWNER/REPOSITORY.git`, noninteractive strict host checking and that SSH
  configuration. No workflow supplies a command, remote URL, environment or credential.

The source repository must have no executable Git filters, includes or URL rewrites in local
configuration. Standard core identity settings, user name/email, remote URL/refspec entries and
branch tracking settings are admitted; other local keys are refused. Global/system Git
configuration, hooks and fsmonitor are disabled. Prepare uses
native raw Snapshot-to-Git objects, so no candidate attributes control staging.

```sh
python3 /path/to/afactory/scripts/install-scenario-impl-adapter.py \
  --config /private/impl-config.json --output /private/impl-adapter
```

Installation is offline. The executable embeds the complete shared GitHub transport and lifecycle
application, pins the configuration bytes, and initializes a new private lock/journal. Keep those
bytes and receipts while any Run can resume. Never reset uncertain receipts or reinstall over them.
Authorize adapter `impl` in the external `af/scenario-policy@4` with the printed executable digest,
operations `impl_prepare_v1`, `impl_publish_v1`, `impl_cleanup_v1`, `observe`, and exactly
`github:owner/project`. Use a 30000 ms adapter wall and 65536-byte output cap. Add the exact native
`template_id` and the owner's minisign public key. This is explicit machine-installed authority,
not an authority file supplied by the candidate.

Validate the bundle against that policy, review the entire pin, and confirm it using ordinary
`af scenario start --confirm-pin`. Inputs are `{}`. The initial Task and every admitted repair
use their exact captured plan through `af task run --confirm-plan`.

## Preparation, signing and recovery

`impl_prepare_v1` takes a native accepted `candidate` Task receipt, with its result ID as `subject`
and candidate Snapshot as `revision`, plus versioned publication text in `body`. Native
`af scenario impl-source` reopens the configured Task Store, checks acceptance, exact result,
models, source repository and complete committed source content. It writes private raw Git objects
and a checked `af/scenario-impl-source@1` receipt. A changed result, unverified candidate, foreign
repository or different source commit fails closed.

The adapter imports only those private objects and creates deterministic commit bytes before
signing or publication. `af/scenario-impl-prepared@1` binds the Task, result, source and candidate
Snapshots, checked predecessor provenance, source commit, tree, final commit, repository, branch, owned workspace, publication text,
configuration and preparation intent. Native `impl_subject`, `impl_revision` and `impl_body`
projections expose its exact publication identity, commit and body. Root inputs cannot invent an
Action or Task receipt.

At `publication-owner`, use ordinary `af scenario decision-payload`, sign outside af, and submit
`af/scenario-signed-decision@1` with `af scenario decide`. The native signature binds this Run,
execution, pin, Decision invocation, exact prepared object, commit, owner, choice and deadline.
Each publication has exactly one Decision consumer. `impl_publish_v1` authorizes only its exact
compare-and-create push and PR creation, or a fast-forward repair of its recorded PR. It authorizes
no merge. Cleanup separately requires a machine grant and exact typed `merged` observation.

Lifecycle steps retain a seven-day elapsed recovery window within the Run's overall wall budget;
each adapter subprocess still has a separately enforced at-most-30-second machine wall. A process
restart therefore does not lose recovery authority merely because one invocation ended.
Every Action is reconciled before execute. A retained dispatched lifecycle intent can use the
native bounded `recover` method to finish only its own remaining substeps under the same inputs,
grant and decision. Fresh intents cannot recover; reconciliation and observation execute no
lifecycle substep. Possible PR POSTs are never resent during recovery. The adapter journals exact inputs and intent before
effects, serializes callers with a private nonblocking lock, and fsyncs receipts before stdout.
A lost push is recovered by exact remote-ref readback. A lost PR response is recovered only from
positive marker, actor, repository, content, branch and commit evidence. After possible POST,
remote absence stays pending; it never authorizes another POST. A changed head invalidates CI,
review and merge observations. Closed unmerged PRs fail with workspace and branches retained.
The implementation journal admits at most 32 retained intents and 16 MiB of journal bytes;
preparation supports at most 32768 entries and 64 MiB of candidate content. Tools are read and
hashed in bounded chunks (at most 256 MiB per executable). Native source captures are retained
separately from journal bytes. Partial native captures or incomplete checkouts are preserved for inspection; they are never
reset over uncertain or dirty content.

## Bounded repair and completion

The outcome Wait remains active after green CI/review until human merge, so a later failed check
or changes-requested review can still admit a bounded repair without restarting the Run.
The builder permits zero, one or two implementation repairs, reserved in the Scenario's token and
wall limits. Each is a fresh implementation continuation on the previous accepted candidate,
with bounded `af/scenario-impl-outcome@1` CI/review/conflict evidence in its requirements. It has
the same required executable reviewer and evaluator, and no separate verification Task. A typed
`select` binding follows only the selected available receipt when other repair steps were skipped.
Decisions cannot use selection to alias publication approval.

Native staging retains the continuation's actual source Snapshot and separately checks its
predecessor through native rerooted-source provenance. Each repair stages a new commit whose
exact parent is the previous owned commit. Its separate
signature approves that head. The original PR and branch remain the publication identity; repair
never creates another PR or overwrites a foreign head. Detached owned worktrees retain each
candidate until the final human merge. The adapter refuses a third repair. After the allowance,
unresolved CI/review/conflicts fail readiness and preserve the workspaces; green CI alone cannot
complete the Run.

Conflict observations identify the current PR conflict, but the implementation template does not
capture a newly changed remote base as a native input. A repair that needs unavailable base bytes
must remain unresolved; this implementation does not claim automatic conflict resolution. Capture
new reviewed source/authority through the ordinary native workflow before pursuing that change.

After actual merge, cleanup reobserves the exact recorded PR/head and checks the entire explicit
repair lineage before deleting anything. It refuses dirty source, foreign ignored files, replaced
worktree/build inodes, attached foreign worktrees and changed local/remote branch identities.
Only adapter-created `target/` directories are recursively removed, with symlink-resistant removal.
Local branch deletion compares the expected commit; remote deletion uses an exact commit lease.
Crash recovery retains per-artifact cleanup progress and refuses replacement paths. Native source
captures and audit receipts remain private recovery evidence. No unrelated worktree is enumerated
as a cleanup target.

The tests use real local Git and fake service transport; they never publish. Run
`python3 -I scripts/test-scenario-impl.py` and `python3 -I scripts/scenario-impl-schemas.py`.
Native `scenario::implementation::tests`, `scenario::impl_package_builder_*` and
`task_execution::scenario_template::implementation_tests` cover typed admission and package pins;
the existing execute-checks reviewer tests cover the source-preservation boundary.
