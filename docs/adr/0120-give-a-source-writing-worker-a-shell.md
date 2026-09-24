# ADR-0120: Give a source-writing Worker that declares `execute-checks` a shell

Status: accepted, 2026-09-24.

## Context

Every implement Task of the `af` TUI campaign failed its checks the same way: the implementer
wrote correct code that `cargo fmt --check` rejected, because a source-writing Worker got
`Read,Glob,Grep,Edit,Write` and nothing to run. It could not format, lint or test what it wrote,
so each run ended `changes_requested` after the check stage and the operator formatted the
candidate by hand. [ADR-0118](0118-let-review-workers-execute-checks-in-an-ephemeral-clone.md)
gave a *review* Worker a shell through `execute-checks`; `worker_access` gave a writer that
declared the same effect nothing more than `WriteSource`.

Two rules constrain the answer:

- The adapter derives tools from captured effects alone
  ([ADR-0042](0042-require-provider-bindings-and-isolate-claude-reviewers.md)); a package still
  cannot name a tool, a permission mode or an MCP server.
- A writer's sealed tree becomes its candidate. A shell leaves scratch in that tree (build output
  under `target/`, tool caches, dotfiles a tool writes into `HOME`), and none of it is what the
  Worker wrote.

## Decision

`worker_access` maps `write-source` plus `execute-checks` to a new
`WorkerAccess::WriteSourceWithShell`. The Claude adapter grants `Read,Glob,Grep,Edit,Write,Bash`
under the same `--safe-mode --restricted --permission-mode dontAsk --strict-mcp-config`; Codex runs
`-s workspace-write` as for any writer. A shell's process group dies with the Attempt, as for an
execute-checks reviewer (`WorkerAccess::has_shell`).

At `finish`, candidate capture skips shell scratch: a path under a top-level name the source
Snapshot did not hold, or a new top-level dotfile. Every other addition, edit and deletion is the
candidate. `SealedSandbox::capture_snapshot_where` filters by Manifest path and never reads or
publishes an excluded entry. A writer without `execute-checks` keeps its old contract: the whole
sealed tree.

The kernel's own `kernel/implementer` package declares `execute-checks` and its instructions ask it
to run `cargo fmt --all`, clippy and the tests it touched before replying.

## Consequences

- An implementer can meet the repository's format and lint gates on its first Attempt.
- A shell-enabled writer cannot add a new top-level directory or a new top-level dotfile: both are
  discarded as scratch. Its instructions say so; a package that must add one does not declare
  `execute-checks`.
- The shell runs inside `trusted_local` sandboxing, which is not security isolation. The Task's
  captured isolation requirement still decides admission, as for ADR-0118.
