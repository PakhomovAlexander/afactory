# Adversarial Phase 0 cases — specified, not captured

Phase 0 asks for adversarial fixtures alongside the behavioral ones: late receipt, malicious
check, hostile Git config, cross-slice interaction. Unlike everything in
[`../synthetic/`](../synthetic/README.md), these **cannot be generated from the shell harness**
— it has no attempt fencing, no sandbox, no snapshot capture, and no scatter. There is nothing
to record.

So they are written as specifications: preconditions, the exact behavior required, and the
acceptance criterion each one discharges. They become executable fixtures in the phase that
builds the mechanism (2–4), not before. Each is deliberately written as a *failing* test to be
inherited — if the kernel does nothing, the case fails.

**Do not weaken a case to make it pass.** Each one exists because the failure it describes is
silent: the run still produces a verdict, and the verdict is wrong.

| Case | Phase | Discharges |
|---|---|---|
| [`late-receipt.md`](late-receipt.md) — **implemented** | 3 | "A receipt from a fenced attempt is quarantined and charged but can never feed downstream" |
| [`malicious-check.md`](malicious-check.md) — **partly implemented** | 2 | "a malicious check/helper cannot touch a host marker, credentials, the canonical checkout, or undeclared network" |
| [`hostile-git-config.md`](hostile-git-config.md) — **implemented** | 2 | "Git hooks, filters, external helpers, and implicit submodules cannot execute during offline capture/materialization" |
| [`cross-slice-interaction.md`](cross-slice-interaction.md) | 4 | "Dynamic scatter requires a whole-target closeout, including a fixture whose defect spans two individually valid slices" |

## Status

`hostile-git-config` is no longer a specification: it lives as
`crates/review-source-git/tests/hostile_git_config.rs`. It failed on its first run — `git status`
hashes worktree files, which executes the candidate's own `clean` filter — and the fix was an
allowlist of git subcommands that cannot transform content. That is what these cases are for.

`malicious-check` is partly implemented: three of its five probes are discharged by the
`trusted_local` sandbox, and the two that need real isolation stay open — with a tested refusal
in place of a weakened test, so a pipeline needing them cannot run on a directory instead.

`late-receipt` is implemented, minus its broker half — there is no broker yet, and the file says
so rather than being marked closed on the strength of the parts that are.

Only `cross-slice-interaction` still describes mechanism that does not exist, and it stays a
specification until scatter lands.
