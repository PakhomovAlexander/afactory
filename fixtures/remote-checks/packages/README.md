# Remote twins of this repository's pipelines

The pipelines that run this repository's `kernel` check through a gate pull request instead of on
the planning machine
([ADR-0139](../../../docs/adr/0139-run-a-declared-check-through-a-gate-pull-request.md), package
RC3 of [`docs/design/remote-checks.md`](../../../docs/design/remote-checks.md)). They are staged
here because a Task Worker may not write `.af/`; installing them is a reviewed hand edit.

| Remote twin | Local original | What differs |
| --- | --- | --- |
| `kernel/gate-bench-remote` | `kernel/gate-bench` | name; `kernel` moved from `checks` to `remote_checks` |
| `kernel/review-code-remote` | `kernel/review-code` | name; `kernel` moved from `checks` to `remote_checks` |
| `kernel/implementation-reviewed-remote` | `kernel/implementation-reviewed` | name; calls `kernel/review-code-remote`, which owns its check node |
| `kernel/verification-reviewed-remote` | `kernel/verification-reviewed` | name; calls `kernel/review-code-remote`, which owns its check node |

Every other byte is the original's, versions included: `markdownlint` still runs on the planning
machine, first. The originals stay installed: a Task file names the local or the remote pipeline,
and there is no per-machine switch.

## Install

1. Append [`code-policy-remote.toml`](code-policy-remote.toml) to `.af/code-policy.toml`. This
   declares `[checks.kernel.remote]` and changes the code policy's identity; re-pin any Worker
   package whose `[signature.evidence]` names the old policy digest.
2. Copy `task-packages/kernel/gate-bench-remote`, `task-packages/kernel/review-code-remote`,
   `task-packages/kernel/implementation-reviewed-remote` and
   `task-packages/kernel/verification-reviewed-remote` to `.af/task-packages/kernel/`.
3. Add each `[packages."…"]` entry of [`catalog-fragment.toml`](catalog-fragment.toml) to
   `.af/task-catalog.toml` that the catalog does not already have. The pins are the staged bytes'
   digests and the paths are the ones step 2 creates.

Installing twice changes nothing: the policy already declares the table, the copies are the same
bytes, and every pin is present. `crates/af/tests/it/task_remote_checks.rs` checks that each twin
is its original with only the differences above, performs these steps twice on a copy of `.af/`,
and plans each root twin with zero Attempts against this repository's policy plus the `remote`
table, expecting `publish-gate` and the mapped `github:` destination in every plan.

## Use

A machine that runs a remote twin needs a push target for this repository in its mapping
(`$XDG_CONFIG_HOME/af/remote-checks.toml` or `AF_TASK_REMOTE_CHECK_POLICY_FILE`), written as
[`docs/task-execution/remote-checks.md`](../../../docs/task-execution/remote-checks.md) shows;
without one, `af task plan` refuses the twin and names the mapping and the repository identity.
