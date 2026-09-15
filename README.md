# Afactory

[![CI](https://github.com/PakhomovAlexander/afactory/actions/workflows/ci.yml/badge.svg)](https://github.com/PakhomovAlexander/afactory/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/PakhomovAlexander/afactory)](https://github.com/PakhomovAlexander/afactory/releases/latest)
[![Licence](https://img.shields.io/badge/licence-Apache--2.0-blue.svg)](LICENSE)

`af` is a multi-agent coding factory for Git repositories. Its first capability is a deterministic
Review Kernel: `af review` runs a sandboxed, budgeted pipeline of model and command reviewers
against a pinned Snapshot and folds what they return into a findings ledger with convergence.
`af task` runs implementation Tasks the same way. One invariant holds everywhere: reviewers and
implementers only ever mutate a sandbox and return typed artifacts; only the kernel integrates;
publishing to a branch or pull request stays an explicit human action.

**Status:** pre-1.0. The 0.8.x line is usable and released; the `.af/` authority format and the
persisted artifact types may still change before 1.0, and every change that affects committed
`.af/` policy is announced in [`CHANGELOG.md`](CHANGELOG.md) under *Authority compatibility*.

## Install

```sh
curl -fsSL https://github.com/PakhomovAlexander/afactory/releases/latest/download/install.sh | sh
af self setup-shell --write     # completions + man pages for your shell
af self status                  # what is installed, the default, the release key, the pin here
af self update --check          # exit 10 when a newer release exists; `af self update` installs it
```

The installer places the newest release in the self-managed layout (`$XDG_DATA_HOME/af/versions/<v>/`,
default symlink at `~/.local/bin/af`) and `af` takes over from there. Every release ships
per-target tarballs, a `SHA256SUMS` file, and `SHA256SUMS.minisig` signed with the release key in
[`crates/reviewctl/keys/release.pub`](crates/reviewctl/keys/release.pub); the binary embeds that
key and refuses an unsigned or badly signed release. `install.sh` verifies the archive digest and,
when `minisign` is on `PATH`, the signature too.

Supported targets: `aarch64-apple-darwin`, `x86_64-unknown-linux-musl`, and
`aarch64-unknown-linux-musl` (static: no glibc floor). On x86_64 macOS, or anywhere else, build from
source:

```sh
cargo install --path crates/reviewctl --locked
```

## Quickstart

```sh
af onboard                          # preview .af/ and the exact apply command; spends no tokens
af onboard --runner codex --gate 'check=make check' --apply  # use the real Gate for this repository
af provider setup codex-main --kind codex  # authenticate and register one explicit Provider
af provider status                 # verify registered and ambient Claude / Codex contexts
af review plan --policy-rev origin/main --base origin/main --uncommitted \
  --provider correctness=codex-main --provider architecture=codex-main
af review run --campaign pr-123 --policy-rev origin/main --base origin/main --uncommitted \
  --provider correctness=codex-main --provider architecture=codex-main --json
```

`af onboard` previews a deterministic authority bundle and writes nothing without `--apply`; it
never executes a Gate or a model, reads a credential, or overwrites existing policy. Its preview
prints a copyable apply command carrying the exact detected or explicit Gate; replace `make check`
above when the repository uses another deterministic acceptance command. `af provider setup`
runs the Provider CLI's official interactive login when needed, verifies it, then writes only an
ID, Provider kind, and auth-directory path to the machine-local registry; credentials remain owned
by that CLI. `af provider add` registers an already authenticated context without opening a login.
Ambient IDs shown by `status` are discovery labels and cannot be selected directly. `af review
plan` resolves policy, Base, candidate, the exact Change Set, the selected route, Gates, budgets,
and required Provider bindings without Campaign state, external calls, or tokens — the route line
reads like `route    route => .af/pipelines/docs.toml (docs); 3 changed path(s)`. `af review run`
opens or resumes the Campaign, runs the Gate, admits the Providers, dispatches the Workers, and
prints the verdict; `af review report` shows the Campaign's wall-clock, per-Attempt Provider usage,
and every Finding's disposition afterwards.

Campaigns are light by default: one closed Round. If it finds defects, fix them and run the
project's deterministic gate; do not start another Campaign. `--heavy` keeps the pipeline's full
convergence window and is for when a human explicitly asks for deep convergence review
([ADR-0037](docs/adr/0037-default-campaigns-to-one-round-light-review.md)). Campaign state lives
under the XDG review-state root and grows with every Subject materialized:

```sh
af review campaigns                            # every Campaign, its verdicts and Round history
af review gc --older-than 14 --keep 5          # preview: what would go, and how much
af review gc --older-than 14 --keep 5 --apply  # remove those Campaign directories
```

## What it does

- **Review campaigns with light-by-default convergence.** One Campaign owns one event log and one
  Ledger across as many Rounds as policy allows; Findings keep their identity across reviewers and
  Rounds, and a Round closes only on a real verdict.
- **Implementation Tasks delivered to new local worktrees.** `af task start --kind implement`
  lets one implementer edit a sandbox, read-only acceptance gates inspect the sealed result, an
  independent evaluator approves a content-addressed Snapshot, and `af task deliver` — only after
  explicit Task-ID confirmation — creates a new branch and linked worktree. It never commits, pushes,
  opens a PR, or touches the source checkout.
- **Deterministic gates that reuse CI checks.** A Gate is whatever the pipeline declares — usually
  the project's own `make check` — executed through an admitted provider in a disposable clone. A
  check that could not run is not a pass, and neither is a Gate with no required checks.
- **Per-Attempt token budgets and Provider admission.** Budgets reserve before dispatch, scopes
  nest, and the tightest one refuses by name. Every packaged model Worker needs a named Provider
  from the machine-local registry, admitted by a charged preflight before any dispatch.
- **Routing pipelines by changed paths.** `.af/af.toml` routes a docs-only change to a cheaper
  pipeline and an oversized Diff to a Scatter pipeline; selection is token-free and pinned in the
  Campaign Manifest so later Rounds never re-route.
- **Signed, self-managed releases with a byte-binding lock.** `.af/af.lock` pins the release that
  wrote it and its archive digest per target; inside such a project any `af` on `PATH` execs that
  version, installing it on demand only when the bytes match.

## Requirements

- Git, and a Rust toolchain at or above 1.88 for source builds (`rust-toolchain.toml` pins it).
- The `claude` and/or `codex` CLIs for model Workers. `af provider setup` owns interactive login,
  verification, and registration in one command without reading credentials; `af provider status`
  shows what each context can do. Command Workers need neither.
- Optional: an OCI container runtime (Docker or compatible) for `provider = "container"` Gates.
  Detection runs the runtime's own `info`; an unusable runtime is refused, never silently downgraded.
- Optional: `minisign`, for verifying release signatures at install time.

## Configuration

`af onboard --apply` creates `.af/` at the repository root:

```text
.af/
  af.toml        project policy: defaults, Worker bindings, routing
  af.lock        the release pin (bytes per target) and every Worker package digest
  pipelines/     pipeline definitions (this repository: review.toml, implement.toml, …)
  workers/       Worker packages: <name>/reviewer.md (prompt) + reviewer.toml (manifest)
```

Configuration merges built-in → `/etc/af` → `~/.config/af` → every `.af/af.toml` above the
repository → `.af/af.toml` → `.af/af.local.toml` → `AF_<TABLE>__<KEY>`; `af config show --origin`
names where each value came from. Provider bindings such as `claude-main` and `codex-main` live in
a machine-local registry, never in the repository. `af help layers`, `af help self`, and `af help
exit-codes` explain the rest; every namespace and command has its own `--help`. Pipelines, routing,
and budgets are described in [`docs/architecture.md`](docs/architecture.md).

## Documentation

- [`docs/README.md`](docs/README.md) — map of everything below.
- [`docs/architecture.md`](docs/architecture.md) — how the kernel is built, boundary by boundary.
- [`CONTEXT.md`](CONTEXT.md) — the vocabulary: Snapshot, Subject, Campaign, Round, Finding, Attempt.
- [`docs/adr/README.md`](docs/adr/README.md) — the binding design decisions.
- [`docs/tasks.md`](docs/tasks.md) — the `af task` guide.
- [`docs/migration.md`](docs/migration.md) — moving a `.review/` consumer to `.af/`.
- [`docs/design/overview.md`](docs/design/overview.md) — the ported design notes.
- [`CHANGELOG.md`](CHANGELOG.md) — every release and its authority compatibility.

## Development

```sh
make check                              # fmt + clippy + tests + fixture reproduction
fixtures/synthetic/generate.sh --check  # the synthetic corpus still reproduces byte-for-byte
make pilot-check                        # deterministic Task start/deliver/recovery smoke
make review-kernel-container-probes     # live container probes; needs a usable runtime
cargo run -p reviewctl --bin af -- review tui
```

The toolchain is pinned, the lockfile is committed, and `unsafe_code = "forbid"` is set
workspace-wide. See [`CONTRIBUTING.md`](CONTRIBUTING.md) before opening a pull request, and
[`AGENTS.md`](AGENTS.md) if you are an agent working in this repository.

## Releasing

`make release VERSION=0.9.0 COMPAT="…"` bumps the workspace version, writes the `CHANGELOG.md`
section, and opens the release pull request. Merging it tags the commit, runs `make check` on Linux
and macOS, builds every target, signs `SHA256SUMS`, and publishes the release.

## Security and licence

Report vulnerabilities as described in [`SECURITY.md`](SECURITY.md). Afactory is licensed under
the Apache License, Version 2.0; see [`LICENSE`](LICENSE).
