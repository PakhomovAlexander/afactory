# Generate review authority with `af onboard`

**Status:** accepted (2026-08-27)

Release `v0.3.0` can execute and validate digest-pinned `.af/` review authority, but a new
consuming repository still needs a maintainer to construct the pipeline, Worker packages, and
lockfile out of band. Agents therefore cannot discover the supported setup from the binary and
are tempted to paste prompts, hand-type digests, trust candidate-owned policy, or weaken a Gate
until a first run starts. That is not a viable client onboarding boundary.

Add a deterministic, token-free `af onboard` command to the binary. For a repository without
`.af/`, the command inspects only bounded, conventional project files, selects a real acceptance
Gate or requires an explicit literal Gate, and previews a versioned `multi-review@1` scaffold. An
explicit `--apply` atomically creates an absent `.af/` directory containing project metadata, one
static diff pipeline, two non-overlapping reviewer Worker packages, an exact generated lockfile,
and agent-readable operating instructions. The default mixed runner profile binds correctness to
Claude Opus at high effort and architecture to the machine-configured Codex model in a read-only
runner; explicit `claude` and `codex` profiles remain available for one-provider installations.

For an existing `.af/`, plain `af onboard` validates the selected pipeline, exact pipeline pin,
referenced Worker packages, graph, Gates, convergence policy, and budgets, then prints its built-in
operating guidance. It never silently changes existing authority. `--refresh-lock` is the only
update operation in this slice: it explicitly recomputes the selected pipeline and referenced
Worker pins from their exact current bytes, validates the result, and atomically replaces only the
lockfile. The resulting diff remains project-owned policy that must be reviewed and committed on a
trusted Authority Snapshot.

Onboarding never calls a model, probes or stores credentials, executes a Gate, creates Campaign
state, fetches a PR, commits, pushes, comments, or overwrites an existing `.af/`. Static independent
reviewers may execute concurrently through the existing scheduler; dynamic Planner fan-out remains
M8 work. The generic scaffold is product code only until emitted: project-specific changes and the
emitted authority remain in the consuming repository.

## Considered options

- **Keep maintainer-authored onboarding outside the binary.** Rejected because clients and agents
  cannot reproduce or audit a supported setup from the released product.
- **Teach agents with a large pasted prompt.** Rejected because the instructions drift from the
  binary, provide no exact lock generation, and consume context on every setup.
- **Generate authority with a model.** Rejected because the first trusted policy would depend on
  unpinned inference, spend tokens before a budget exists, and still need deterministic locking.
- **Overwrite or merge an existing `.af/`.** Rejected because a convenience command must not
  silently bless changes to trusted execution policy.
- **Generate one deterministic, absent-only scaffold and validate/repin explicitly (chosen).** It
  gives agents a complete released workflow while keeping every authority change visible.

## Consequences

- A new repository can reach a reviewable, exactly pinned multi-review proposal using only the
  released binary and one explicit apply step.
- Conventional Gate discovery is deliberately small and fail-closed; unusual repositories supply
  `--gate NAME=COMMAND` rather than receiving a guessed command.
- The built-in prompts and runner profiles become versioned product behavior and require the same
  review and release discipline as schemas or CLI contracts.
- `af onboard` makes setup reproducible but does not make generated policy automatically trusted;
  it becomes authority only after the consuming repository reviews and commits it.
