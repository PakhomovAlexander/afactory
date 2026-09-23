# ADR-0118: Let a review Worker that declares `execute-checks` run in an ephemeral clone

Status: accepted, 2026-09-23.

## Context

The `af` TUI plan ([`docs/design/tui.md`](../design/tui.md) §7, package M0) reviews every package
with a UIX reviewer. That reviewer builds `af`, writes its own pseudo-terminal harness, and drives
the shipped panes key by key. A model reviewer could do none of that. Its Worker package could
already declare `effects = ["read-source", "execute-checks"]`, and the catalog and plan compiler
admitted the declaration. But the declaration had no effect:

- The source environment (`crates/review-pipeline/src/task/source.rs`) materialized every Worker
  without a kernel-captured `candidate` port as `Mode::ReadOnly`, whatever it declared. It
  refused readable Review inputs in any other mode.
- The host told the adapter one bit, `writable = effects.contains("write-source")`. The Claude
  adapter then granted `Read,Glob,Grep`, adding `Edit,Write` for a writer. Codex ran
  `-s read-only` or `-s workspace-write`.

So a reviewer's shell was either absent (Claude) or pointed at a directory whose tree was
read-only (Codex). Four existing rules constrain any answer, and none of them may be relaxed:

- Runner adapters own their security flags. Candidate settings, Hooks and packages cannot widen
  reviewer authority ([ADR-0042](0042-require-provider-bindings-and-isolate-claude-reviewers.md)).
- A Worker without a candidate port must leave its declared source exactly as it found it. Only
  the installed seal operation establishes derived Snapshot lineage.
- Readable Review inputs (`.af-review-inputs/`) belong to the disposable baseline and never flow
  through candidate capture.
- Process supervision, including the process-group kill on deadline and cancellation, lives in
  `review-process` ([ADR-0026](0026-share-process-supervision-through-a-leaf-crate.md)).

## Options

- **Let a package name its tools, for example `tools = ["Bash"]` in `worker.toml`.** Rejected. It
  is exactly the package-supplied authority that ADR-0042 removed, and a second vocabulary beside
  effects that a lock digest would have to police.
- **Grant `Bash` to every model Worker that declares `execute-checks`.** Rejected. Outside review,
  nothing seals such a Worker's sandbox back, and a non-review Worker has no plan that needs a
  shell. The grant stays as narrow as the one consumer that motivates it.
- **Give the reviewer a read-only tree and point build output at a directory outside the
  sandbox.** Rejected. It depends on every build tool honouring an environment variable, and the
  harness still needs somewhere to write. The kernel would also own a second, unsealed directory
  per Attempt.
- **Materialize an ephemeral-write clone, and seal it back unchanged.** Chosen. It is the clone
  the AF-owned preparation path already uses, and the kernel already knows how to seal and compare
  it.

## Decision

### One derivation for sandbox and tools

`review_runner::task::WorkerAccess` replaces the adapter's `writable: bool`. It has three values:
`ReadOnly`, `ExecuteChecks` and `WriteSource`. `review_pipeline::task::source::worker_access`
derives it from the captured `OperatorSignature` alone:

- `write-source` gives `WriteSource`;
- otherwise `execute-checks` on a Worker whose `roles` contain `review` gives `ExecuteChecks`;
- anything else gives `ReadOnly`.

The source environment picks its sandbox mode from that value. The host passes the same value to
the model adapter. A non-review Worker that declares `execute-checks` keeps its read-only source
and read-only tools. The mode and the tools cannot disagree, and nothing else feeds either one.

### The sandbox

`ExecuteChecks` materializes the source in `Mode::EphemeralWrite`, with the same readable Review
inputs a read-only reviewer gets. The guard in `add_review_inputs` now asks whether the tree will
be captured as a candidate, instead of whether it is writable, and still refuses the one case that
matters. `finish` seals nothing back. Every Snapshot entry must seal byte-identical, and so must
every declared Review input, because both are baseline entries. A path added at the root, or
beneath a top-level name the Snapshot holds, is a source edit too.

Only paths beneath a new top-level directory the materialized tree did not have are scratch, for
example an ignored `target/` that holds build output and the harness. Scratch is discarded with
the clone, so it never reaches a candidate, a Proposal or a delivered tree. Any source edit fails
the Attempt with `Execute-checks reviewer changed its declared source: <paths>`, naming up to 20
paths and counting the rest. A read-only Worker's failure now names its paths the same way.

### The adapters

The Claude adapter derives its tool list from the access:

| Access | `--tools` and `--allowedTools` |
|---|---|
| `ReadOnly` | `Read,Glob,Grep` |
| `ExecuteChecks` | `Read,Glob,Grep,Bash` |
| `WriteSource` | `Read,Glob,Grep,Edit,Write` |

It keeps `--safe-mode --restricted --permission-mode dontAsk --strict-mcp-config` after the
package's model flags. Codex runs `-s workspace-write` for either writable access and
`-s read-only` otherwise. Both run with the sandbox root as working directory, and Codex also
passes it as `-C`. Package runner arguments stay limited to one model and one effort, so a package
still cannot supply a tool, permission, sandbox or MCP flag.

The Campaign Review host keeps its native profile ADR-0042 recorded: Claude `ReadOnly`, and Codex
writable for sandbox Proposals, now spelled `WriteSource`.

### Bounds and children

The Attempt's wall clock and token reservation are unchanged. `ModelRunner` gains
`killing_process_group_on_exit`, which selects `review-process`'s existing
`ExitPolicy::KillProcessGroup`. Both adapters set it for `ExecuteChecks`. A deadline or a
cancellation already killed the group. Now the group is also killed when the model process exits,
before it is reaped, so a background shell child cannot outlive the Attempt or write into the
tree being sealed.

## Consequences

A UIX reviewer can build and drive the real candidate under either adapter, and its Findings rest
on screens it captured rather than on source it read. The cost is one more ephemeral clone per
such Attempt, and a seal walk over its build output, which the seal already skips hashing because
added paths are never compared by content.

The trust boundary does not move. A reviewer with a shell under the `trusted_local` provider can
reach anything its user can, exactly as a Codex reviewer with `workspace-write` or a command
Worker always could. A project that needs more demands container isolation in its captured policy,
as before. What the kernel guarantees is unchanged: the declared source seals unchanged or the
Attempt fails, and nothing from the clone is captured.

Every existing Worker keeps its access. Read-only reviewers and writers derive exactly what they
had. No wire contract, schema, fixture or `--json` document changes, because `WorkerAccess` is an
in-process adapter argument and not a recorded artifact. `CONTEXT.md` gains no term.
