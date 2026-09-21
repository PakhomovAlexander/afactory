# Documentation

Where to look, by question. The [README](../README.md) covers installing, the quickstart, and the
`.af/` layout; the CLI documents itself with `af help layers`, `af help self`, `af help exit-codes`,
and `--help` on every namespace and command.

## Start here

1. [`../CONTEXT.md`](../CONTEXT.md) — the vocabulary. Everything else assumes it.
2. [`architecture.md`](architecture.md) — the boundaries and the tests that enforce them.
3. [`adr/README.md`](adr/README.md) — why each boundary is where it is.
4. [`tasks.md`](tasks.md) — the `af task` walkthrough, once review makes sense.

## By document

| Document | Read it when you want to know |
|---|---|
| [`architecture.md`](architecture.md) | How the kernel is built: crates, contracts, the store, source capture, Check nodes, reviewer adapters, sandboxes, the pipeline graph, pipeline definitions, routing, and Attempt budgets. Each section names the test that pins it. |
| [`../CONTEXT.md`](../CONTEXT.md) | What a word means. Snapshot, Subject, Base, Campaign, Round, Report, Finding, Attempt, Provider and the rest, each with the nearby term it must not be confused with. Read it before arguing about behaviour. |
| [`tasks.md`](tasks.md) | How to run implementation Tasks with `af task`: start, inspect, evaluate, and deliver to a new local worktree. |
| [`task-execution.md`](task-execution.md) | The Task runtime reference: fixed product decisions, contracts, fixtures, and compatibility checkpoints for the common Task execution abstraction. |
| [`migration.md`](migration.md) | Moving a repository from the retired `.review/` authority layout to `.af/` with `af onboard --migrate --apply`. |
| [`non-goals.md`](non-goals.md) | What the kernel deliberately does not do, so a missing feature can be told from a refused one. |
| [`design/`](design/overview.md) | The design notes the implementation was ported from: [overview](design/overview.md), [entities](design/entities.md), [state machines](design/state-machines.md), [config](design/config.md), [store](design/store.md), [research](design/research.md), [values](design/values.md), [Task execution](design/task-execution.md) and its [examples](design/task-execution-examples.md). Their examples are YAML where the shipped format is TOML; the shape is the same. |
| [`adr/`](adr/README.md) | The binding decisions, one per file, with the options that were rejected and why. The index in `adr/README.md` lists every one. |
| [`../CHANGELOG.md`](../CHANGELOG.md) | What each release changed and whether committed `.af/` policy keeps working, needs `af onboard --refresh-lock`, or needs a migration. |

## Reading and writing ADRs

An ADR is numbered (`NNNN-kebab-case-title.md`, the next free number), opens with a status line
that names the status and date, states the context, lists the considered options with the reasons
each was rejected, records the decision, and ends with its consequences. Accepted ADRs are
immutable; a change is a new ADR that says what it supersedes. A partially superseded ADR gains a
status-line note linking the new one; a fully superseded ADR is deleted, and git history keeps it.
Links to a deleted ADR are rewritten to point at the superseding ADR, or to plain text; this is the
only edit allowed in another accepted ADR's body. Propose one by copying the shape of a recent ADR,
adding it to the index in [`adr/README.md`](adr/README.md), and opening a pull request.

## Conventions

- The words in `CONTEXT.md` are used with their defined meaning everywhere else. If a document
  needs a new term, define it there first.
- A claim about behaviour cites the test or fixture that pins it; a claim about a boundary cites
  the ADR that fixed it.
- Project-specific pipelines, Worker packages, and Campaign state belong in the consuming
  repository, not here. This repository's own `.af/` is an example that a test asserts still loads.
