# ADR-0141: Let a pinned `ci`-tagged root Pipeline send a changed workflow

Status: accepted, 2026-10-05. Amends
[ADR-0140](0140-run-a-declared-check-through-a-gate-pull-request.md) option 4 (a candidate that
changes `.github/`): it stays refused unless the exception below grants it. Everything else in
ADR-0140 is unchanged.

## Context

ADR-0140 refuses a Remote Check (`remote_candidate_changes_ci`) for any candidate whose
`.github/` differs from the Task's source, because the changed workflow decides what judges the
candidate. That rule keeps a Worker from rewriting its own gate. It also means a Task whose job
is to change the CI workflow can never be checked remotely. On a host too small to build the
repository, that leaves no Gate at all for such a Task.

Some Tasks are meant to change CI: a reviewed catalog Pipeline that a project keeps for workflow
maintenance. The project, not the Worker, decides that such a Pipeline may change what judges
it. The kernel needs to recognise that decision from authority the Worker cannot touch.

## Options

1. **Where the authority comes from.**
   - *A tag on the selected root Pipeline's pinned package* — chosen. The Pipeline is a reviewed,
     digest-pinned package on the Authority Snapshot, captured into the Task's run authority
     before any candidate exists. Its exact tag `ci` is a project decision recorded in bytes a
     Worker cannot edit.
   - *The Pipeline name, a job label, a substring of either, a Task-file boolean or an environment
     variable* — rejected. A name, label or substring is data anyone can choose to look like CI.
     A Task-file field is Task input, which grants no execution authority. An environment variable
     is global state that no record binds. None of them is reviewed and pinned.
2. **Which Pipeline may grant it.**
   - *Only the selected root of the admitted plan* — chosen. Selection is recorded authority:
     the plan's `pipeline_id` and the compiled graph's root call.
   - *Any tagged Pipeline in the closure, including embedded and called children or another
     pinned catalog Pipeline* — rejected. A child cannot widen what its untagged caller
     declared, and a Pipeline the Task never selected has said nothing about this Task.
3. **Generated Pipelines.**
   - *Never grant* — chosen. A Planner's proposal is model output. Developer approval admits it to
     run; it does not make it reviewed catalog bytes. A plan with any generated origin, and a
     Planner bootstrap, grants nothing whatever its tags say.
4. **How the grant reaches the executor and the record.**
   - *A typed capture rebuilt from the Store for every domain, named in the evidence and
     recomputed by every reader* — chosen.
   - *A boolean on the domain or the executor* — rejected. A direct API caller could set it,
     and replay could not tell which authority granted it.

## Decision

### Tags

`af.pipeline/1` gains an optional `tags` set: at most 16 tags, each 1 to 64 ASCII letters,
digits, `.`, `_` or `-`, starting with a letter or digit. The TOML parser sorts the set and
refuses duplicates. The set is empty by default and left out of the serialized form when empty,
so every existing Pipeline keeps its exact bytes, package digest and plan identity. Tags match
exactly and case-sensitively. The kernel reads exactly one tag, `ci`. `CI`, `cicd`, `ci-trusted`
and every other tag are only labels.

### The capture

`review_pipeline::task::remote_check::TrustedCiPipeline::capture(cas, plan_id)` is the only
constructor of the exception. Its fields are private. It reads only recorded artifacts and
grants only when all of these hold:

- The `af/ExecutionPlan@1` validates, is not a Planner bootstrap and has no generated origin.
- Its `af/TaskRevision@1` names the same Task authority.
- That authority is a captured `af.task-run-authority/2` with a code policy.
- The compiled graph's `root` call names a Pipeline that the authority's `packages` pin, at
  exactly the plan's `pipeline_id` and its root dependency.
- The pinned `af/TaskPackage@1` is re-verified against its pinned digest and parsed by the
  compiler's own package reader. Its `pipeline.toml` carries the tag `ci`.

Anything else returns no exception. A record that does not read back as the type it claims is an
error. The coordinator (`af task start --authority …`, the `af review --file` wrapper, which
starts the same way, and `af task run` on resume) captures it again from the Store each time it
builds a code or Review domain for the admitted plan. Every Attempt, retry and reopen therefore
uses a capture of the same recorded authority. `CodeTaskDomain::with_trusted_ci` and
`ReviewTaskDomain::with_trusted_ci` refuse a capture whose code policy or compiled graph is not
the domain's own. A domain built without a capture, as any direct API caller builds it, refuses
the candidate as ADR-0140 does. Its reader also refuses every record that claims the exception.

### The phase and its evidence

The check operator gives the executor the capture only for an invocation of the plan it was
captured for. The executor compares `.github/` exactly as before. When the candidate differs,
it sends the candidate only with a capture. Without one the result is still
`remote_candidate_changes_ci`, and the refusal now also names the tagged-root route. Mapping,
opt-in, local-first order, branch ownership, push and pull request rules, the merge-ref proof,
latest-attempt selection, required-job matching, log excerpts and cleanup are unchanged.

`af/RemoteCheckEvidence@1` gains an optional `trusted_ci` object. It is present exactly when the
candidate's `.github/` differed and the exception sent it, in every record of that phase,
including later refusals such as a failed local check. The object carries `tag` (always `ci`),
`authority_id` (the Task revision's captured run authority, not just its code policy),
`plan_id`, `pipeline` and `pipeline_id`. Evidence without it is byte-identical to ADR-0140's.
Evidence with it cannot carry `remote_candidate_changes_ci`. The schema states both rules.

The receipt reader no longer takes the source from the evidence. It recomputes the candidate's
root ancestor along `parent_snapshot_id` and whether `.github/` differs from it. It refuses
evidence that names another source. It refuses evidence that claims the exception when the
candidate changes no workflow, when the claim names another plan than the receipt's, or when the
claim is not exactly the reader's own capture. It also refuses evidence that published or
observed a changed workflow without the exception. `af task show` names the granting Pipeline
under the check.

## Consequences

- A project can keep a reviewed, pinned workflow-maintenance Pipeline tagged `ci`. Tasks that
  select it can hold a remote Gate on the workflow they change. Every other Task keeps the
  ADR-0140 refusal.
- A changed workflow is trusted code. It runs with the repository's secrets and permissions,
  and it decides which jobs exist and what they do. A green run of an altered workflow shows
  that the altered workflow passed. It is not a universal quality guarantee and proves nothing
  about the checks the source's workflow would have run. Tagging a Pipeline `ci` is the project's
  statement that its Tasks may be judged that way.
- The required-check contract stays frozen policy. The declared `workflow` path and `required`
  job names come from the captured code policy and cannot be changed by the candidate. A
  changed workflow that drops or renames a required job leaves the check `remote_check_missing`.
  One that skips a job is inconclusive, and one whose job fails fails the check.
- A candidate cannot grant itself the exception. Tags it writes into its own tree, a catalog it
  edits, and a Pipeline a Planner generates are not the pinned bytes of the selected root.
- Rollback is removing the tag from the Pipeline and re-pinning it. Tasks admitted after that
  refuse again. Evidence already recorded keeps naming the authority that granted it.
