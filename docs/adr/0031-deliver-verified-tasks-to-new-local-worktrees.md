# Deliver verified Tasks only to new local worktrees

**Status:** accepted (2026-08-27)

Minimal v2 deliberately stops at a verified internal derived Snapshot. That boundary proved the
implementation and verification loop without giving the product authority to mutate a user's
checkout. Trusted design-partner dogfood now needs a narrow way to inspect and continue from that
verified result, while pushes, pull requests, and publishing remain human actions.

Add one local delivery transition after a Task has completed with the exact `verified` outcome.
The operator names the Task, target repository, new branch, and absent worktree path, and confirms
the Task ID explicitly. Delivery is refused unless the target repository is clean and its current
committed Snapshot is exactly the Task's recorded source Snapshot. The branch must not exist and
the destination path must not exist.

Delivery creates only a new local branch and linked worktree, then writes the Task's exact derived
Snapshot there. It never changes the caller's current checkout, commits, pushes, opens a pull
request, invokes a remote, or exports credentials. The resulting worktree intentionally contains
the verified change as uncommitted local work for a human to inspect and commit.

Persist a prepared delivery record before the first Git mutation and a terminal receipt after
verification or rollback. An exact repeated request is inspectable and idempotent. A different
target for an already delivered Task conflicts. If creation fails, remove only the branch and
worktree created by that prepared transition; on restart, reconcile the prepared record with the
local repository before retrying or sealing the terminal receipt. Never remove a pre-existing
path or branch.

This decision extends, but does not supersede, [ADR-0030](0030-complete-minimal-v1-and-v2-before-dogfood.md):
v2 still ends at an internal Snapshot, while this deliberately small delivery capability is the
first v3 slice.

## Considered options

- **Wait for the full M7/M9 export and Integration milestones.** Rejected because a verified v2
  Snapshot is already sufficient for a narrow local pilot, and those later milestones solve
  broader Proposal authority and automatic Integration problems.
- **Overwrite the current checkout.** Rejected because unrelated human work and repository state
  would become part of recovery.
- **Commit, push, or open a pull request automatically.** Rejected because publishing is a human
  action and would widen the credential and remote-authority boundary.
- **Deliver an unverified derived Snapshot.** Rejected because it bypasses the v2 acceptance Gate
  and evaluator contract.
- **Create a fresh local branch and worktree only (chosen).** It exposes the verified result for
  dogfood while keeping the mutation bounded, visible, and recoverable.

## Consequences

- Initial delivery is for trusted local repositories and design partners. Arbitrary untrusted
  execution and client credential brokering still depend on M4/M6 controls.
- The delivery receipt and history become part of Task operations; the original Task outcome
  remains immutable.
- A delivered worktree is not Review Kernel Integration. It is a product-level local write-back
  that leaves commit and publication decisions to the operator.
