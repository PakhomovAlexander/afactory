# Non-goals

Capabilities that are deliberately not built, with the reasoning that keeps them out. Each entry
names the decision that would have to change first.

## Review

### Running a subset of a pipeline (`--only <node>`)

A pipeline defines what a review *is*; running a subset of it produces a different review
wearing the same Campaign's name. Findings fold into the Ledger during the graph run, before
convergence is computed, so a one-reviewer probe would advance the news Round and leave the other
reviewers' prior Findings unconfirmed while correctly failing to close the Round. The supported
cheap-iteration path is a separate pipeline file (`--pipeline quick.toml`) with one reviewer, its
own budgets, its own Campaign and its own Ledger.

### Applying Proposals from the kernel (`af review apply`)

`af review export <proposal-id>` followed by `git apply` keeps the kernel's boundary literally
true: nothing in it mutates a repository, and Git handles a stale patch better than the kernel
could. Export refuses stale Proposals by default; applying one with `--allow-stale` and
`git apply --3way` is an explicit human decision
([ADR-0010](adr/0010-proposals-are-exported-by-id-and-base-bound.md)).

### Dropping out-of-set Findings

A Finding outside the current Report Scope is information the operator paid for. It is retained
and marked non-blocking rather than discarded; a blocker found and thrown away must never look
like a blocker never found ([ADR-0011](adr/0011-silence-is-not-a-drop.md),
[ADR-0013](adr/0013-scope-is-evaluated-per-active-claim.md)).

### Re-keying Findings on rename

Finding identity is path-independent, so a rename changes a Report's location and Scope but never
its Finding, and ambiguous duplicates use recorded Grouping
([ADR-0006](adr/0006-finding-identity-is-path-independent.md)).

### Adding `diff` to the safe Git subcommand allowlist

The allowlist is checked against the first argument alone, and the generic `bytes`/`text`/`line`
invocation helpers accept arbitrary argv, so admitting `diff` would admit every form of it
workspace-wide, including the worktree-versus-index form that runs the candidate's clean filter.
Tree diffs go through one typed method instead
([ADR-0001](adr/0001-tree-diff-behind-a-typed-method.md)).

### Automatic Integration for static review graphs

Automatic Integration is admitted only behind a captured Slicer/Scatter closure route; a static
review graph gets no `[integration]` without whole-Subject and semantic closure. Generalizing
closure to static graphs would be new work; bypassing closure is not on the table
([ADR-0039](adr/0039-own-dynamic-shards-inside-a-typed-scatter-node.md),
[ADR-0040](adr/0040-promote-only-checked-derived-snapshots.md)).

### Host cache passthrough for safe pipelines

A safe pipeline never receives direct host-cache access; caches are sandbox-local snapshots
([ADR-0008](adr/0008-safe-caches-are-sandbox-local-snapshots.md), superseding
[ADR-0003](adr/0003-gate-caches-pass-through-to-the-host.md)).

### Reviewing code the operator does not trust

Reviewer-authored Proposals raise what a compromised reviewer can place before an operator for
application. That risk is accepted while the tool reviews first-party code only; the containment
probes that remain open are recorded as open in
[`security/containment-probes.md`](security/containment-probes.md) rather
than claimed covered. Pointing the tool at untrusted code requires revisiting that acceptance
first ([ADR-0010](adr/0010-proposals-are-exported-by-id-and-base-bound.md)).

## Tasks

### Delivery that commits, pushes or opens a pull request

Delivery creates only a new local branch and linked worktree containing the verified Snapshot as
uncommitted work. Overwriting the current checkout would make unrelated human work part of
recovery; publishing is a human action and would widen the credential and remote-authority
boundary; delivering an unverified Snapshot would bypass the acceptance Gate and evaluator
contract ([ADR-0031](adr/0031-deliver-verified-tasks-to-new-local-worktrees.md)).

### Model output as plan approval

Every generated plan requires an authorized developer's signed approval binding the exact plan,
Task revision and authority. Model output never provides it, and Afactory never holds the signing
key ([ADR-0046](adr/0046-add-versioned-task-contracts-with-exact-plan-approval.md),
[ADR-0056](adr/0056-share-planning-accounting-and-authenticate-generated-plan-decisions.md)).

### Local bindings that weaken mandatory acceptance

A machine-local bindings file can replace Workers and Provider aliases, but it cannot overwrite
the committed policy, change mandatory checks, weaken evidence or retention requirements, widen
effects or make a verification Worker dependent on a source-writing Worker
([ADR-0052](adr/0052-capture-local-bindings-and-compose-review-acceptance.md)).

### A second budget for child work

Embedded Review, bounded repair, Planner bootstrap and Provider admission all consume the
original Task's allowance. Neither a child call nor a retry creates another budget, and a
transition cannot reset spent tokens, Attempt counts or the deadline
([ADR-0049](adr/0049-run-task-workers-through-shared-durable-attempts.md),
[ADR-0056](adr/0056-share-planning-accounting-and-authenticate-generated-plan-decisions.md)).

## Distribution

### Vendoring the kernel into consuming repositories

A consuming repository pins one `af` release in its `af.lock` and dispatches to it; it does not
vendor this workspace ([ADR-0044](adr/0044-af-manages-itself-and-dispatches-to-the-pinned-release.md),
[ADR-0045](adr/0045-one-release-train-and-a-pin-that-binds-bytes.md)).
