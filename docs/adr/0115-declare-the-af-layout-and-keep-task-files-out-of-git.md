# ADR-0115: Declare the `.af/` layout once and keep Task files out of git

Status: accepted, 2026-09-22.

## Context

A consumer's pull request carried 244,008 added lines, of which roughly ninety per cent were
candidate patches, reviewer results, logs and captured Task inputs written under `.af/tasks/` by
the coordinator driving `af`. The kernel wrote none of them — `af` already refuses a state
directory inside a checkout and keeps every recorded artifact in the Store under
`$XDG_STATE_HOME/af` — but Snapshot capture includes every tracked and untracked-not-ignored path,
so the files then rode along in every later candidate.

Two gaps made that the path of least resistance. Nothing declared what `.af/` may hold: `af
onboard` writes a handful of files and any other path under `.af/` was equally unknown, so nothing
could warn. And nothing said where a Task file lives; the kernel's own campaigns kept theirs under
`.af/tasks/`, which reads as an endorsement.

## Options

- **Refuse a Task file inside the repository.** Rejected: a Task file in the checkout is a
  workflow smell, not a safety violation, and refusing would break every consumer mid-campaign for
  a problem a sentence of advice solves. Requests are captured, never trusted, so nothing about
  admission changes with the file's location.
- **Strip undeclared paths out of the captured Snapshot.** Rejected: Snapshot identity is the
  kernel's foundation. Removing bytes from a Snapshot to flatter a diff would make the reviewed
  object differ from the repository, and the reviewed object is the whole point.
- **Gitignore `.af/tasks/` from `af onboard`.** Rejected twice over: `af onboard` writes nothing
  outside `.af/` by [ADR-0032](0032-generate-review-authority-with-af-onboard.md), and blessing a
  directory for Task files inside the checkout entrenches exactly the habit that caused the
  problem.
- **Write the layout in the documentation only.** Rejected: prose cannot be checked. The
  documentation already claimed `af init` gitignores `af.local.toml`, and there is no `af init`
  and no command that writes a `.gitignore`. A list nothing verifies drifts.
- **Declare the layout as data in the code, render it everywhere, and warn.** Chosen.

## Decision

`review_config::layout::LAYOUT` is the single declared list of every canonical entry directly
under `.af/`. Each entry carries its segment, whether it is a file or a directory, its writer of
record, whether git versions it, and what it holds. `layout::classify` turns any
repository-relative path into `Outside`, `Root`, `Declared` or `Undeclared`; a declared directory
covers everything beneath it and a declared file covers exactly itself. A synthetic entry is in
the table because the kernel names it, but it exists only inside an in-memory authority Manifest,
so on a repository path it classifies as `Undeclared`: a tracked copy of `task-compat/` is as
stray as any other tree.

There is no second list. `af help config` renders the table and one paragraph from `LAYOUT`,
the design notes that once quoted it were retired at GA, so the help text is the one rendered
home and a test asserts it carries every entry. A further test walks every production source file under `crates/*/src` — each module's
inline `#[cfg(test)]` block and its separate `tests.rs` excluded — and fails on any `.af/` path the
table does not declare. The table therefore cannot fall behind the code that resolves it.

Declaring the table against the code, rather than against the plan's shorthand, added five entries
the kernel demonstrably names and the plan's enumeration had missed: `document-policy.toml` (the
document Task policy, written by `af catalog init` beside `code-policy.toml`), `packages/` (where
a starter writes its Task packages), `artifact-reuse/` and `cache/` (the only roots an accepted
self-optimization proposal may edit), and `task-compat/` (the legacy Task authority bundle). The
last is the one entry that is never on disk: the kernel builds it inside an in-memory authority
Manifest, so it is marked `synthetic` rather than versioned. `af.local.toml` remains the one
committed-layout entry git does not version, and the project excludes it itself.

`af task plan`, `af task start --file`, `af review plan --file` and `af review run --file` warn on
stderr, once the Task file's bytes are in the Store, when the file resolves inside the repository
and `git check-ignore` does not ignore it. The message names the file, says af captured it, and
names `$XDG_STATE_HOME/af/tasks/`. It is advice: exit codes, stdout and every `--json` document
are byte-identical to what they were, the check is bounded and runs after capture, and git
declining to answer — no binary, no repository, a deadline — produces no warning rather than a
guess.

## Consequences

One table now answers "may this path be here?" for help text, documentation and, from the next
package, classification of a captured Snapshot manifest. Adding an authority path to the kernel
means adding an entry, and the drift test says so with the file that resolved it.

The warning changes no contract. A project that keeps Task files in its checkout keeps working and
hears about it once per plan; a project that gitignores them, or keeps them outside the repository
as the kernel now does, hears nothing.

The kernel's own Task files under `.af/tasks/warm-layers/` are captured in the Store and belong
outside the checkout; `docs/design/worker-warm-layers-plan.md` keeps its historical commands and
says so. Their removal from git is a plain deletion and carries no further decision.
