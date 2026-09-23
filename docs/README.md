# Documentation

Where to look, by question. The [README](../README.md) covers installing, the quickstart, and the
`.af/` layout; the CLI documents itself with `af help layers`, `af help self`, `af help exit-codes`,
and `--help` on every namespace and command.

## Start here

1. [`../CONTEXT.md`](../CONTEXT.md) — the vocabulary. Everything else assumes it.
2. [`architecture.md`](architecture.md) — the boundaries and the tests that enforce them.
3. [`adr/README.md`](adr/README.md) — why each boundary is where it is.
4. [`providers.md`](providers.md) — getting model access onto a machine, safely, before running
   anything that spends tokens.
5. [`tasks.md`](tasks.md) — the `af task` walkthrough, once review makes sense.

## By document

| Document | Read it when you want to know |
|---|---|
| [`architecture.md`](architecture.md) | How the kernel is built: crates, contracts, the store, source capture, Check nodes, reviewer adapters, sandboxes, the pipeline graph, pipeline definitions, routing, and Attempt budgets. Each section names the test that pins it. |
| [`../CONTEXT.md`](../CONTEXT.md) | What a word means. Snapshot, Subject, Base, Campaign, Round, Report, Finding, Attempt, Provider and the rest, each with the nearby term it must not be confused with. Read it before arguing about behaviour. |
| [`providers.md`](providers.md) | How a machine gets model access: registering a Provider, why an interactive login is opt-in and terminal-only, what `status` checks versus what `doctor` proves, and every `af provider` exit code. |
| [`tasks.md`](tasks.md) | How to run implementation Tasks with `af task`: start, inspect, evaluate, and deliver to a new local worktree. |
| [`task-execution.md`](task-execution.md) | The Task runtime reference: fixed product decisions, contracts, fixtures, and walkthroughs for the common Task execution abstraction. |
| [`task-execution/task-inputs.md`](task-execution/task-inputs.md) | The Task file's `inputs` table: binding a root input port to a recorded Task's output instead of exporting it to a file, what resolution records, and how a binding is shown. |
| [`non-goals.md`](non-goals.md) | What the kernel deliberately does not do, so a missing feature can be told from a refused one. |
| [`security/containment-probes.md`](security/containment-probes.md) | What a hostile check is contained by, which probes prove it, and which containment probes are still open. |
| [`values.md`](values.md) | The eight engineering values in priority order and the three product values, each with the tests that decide whether a change honours it. Binding. |
| [`design/`](design/README.md) | The two designs still in flight: [warm layers](design/worker-warm-layers.md) with its [package plan](design/worker-warm-layers-plan.md), and the [self-optimizer](design/self-optimizer.md). Direction, not shipped behaviour; where a note and an ADR disagree, the ADR wins. |
| [`adr/`](adr/README.md) | The binding decisions, one per file, with the options that were rejected and why. The index in `adr/README.md` lists every one. |
| [`../CHANGELOG.md`](../CHANGELOG.md) | What each release changed and whether committed `.af/` policy keeps working, needs `af onboard --refresh-lock`, or needs a documented hand edit. |

## Reading and writing ADRs

An ADR is numbered (`NNNN-kebab-case-title.md`, the next number after the highest in
`docs/adr/`), opens with a status line that names the status and date, states the context, lists
the considered options with the reasons each was rejected, records the decision, and ends with
its consequences. Accepted ADRs are immutable; a change is a new ADR that says what it
supersedes. A partially superseded ADR gains a status-line note linking the new one, and that
status line may be restated when the new ADR spends the transition wording it carried; a fully
superseded ADR is deleted, and git history keeps it.
Links to a deleted ADR, or to an internal record deleted at GA, are rewritten to point at the
superseding ADR or to plain text; this is the only edit allowed in another accepted ADR's body
(ADR-0113 clauses 6 and 8). Propose one by copying the shape of a recent ADR, adding it to the
index in [`adr/README.md`](adr/README.md), and opening a pull request.

## Conventions

- The words in `CONTEXT.md` are used with their defined meaning everywhere else. If a document
  needs a new term, define it there first.
- A claim about behaviour cites the test or fixture that pins it; a claim about a boundary cites
  the ADR that fixed it.
- Project-specific pipelines, Worker packages, and Campaign state belong in the consuming
  repository, not here. This repository's own `.af/` is an example that a test asserts still loads.
