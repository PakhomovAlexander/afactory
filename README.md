<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/banner/banner-dark.svg">
  <img alt="afactory: agent pipelines made fast" src="brand/banner/banner-light.svg" width="100%">
</picture>

[![CI](https://img.shields.io/github/actions/workflow/status/PakhomovAlexander/afactory/release.yml?branch=main&style=flat-square&labelColor=0F0F0F&color=36EEA8&label=CI)](https://github.com/PakhomovAlexander/afactory/actions/workflows/release.yml)
[![Latest release](https://img.shields.io/github/v/release/PakhomovAlexander/afactory?style=flat-square&labelColor=0F0F0F&color=5195F5&label=release)](https://github.com/PakhomovAlexander/afactory/releases/latest)
[![Licence](https://img.shields.io/badge/licence-Apache--2.0-EE366A?style=flat-square&labelColor=0F0F0F)](LICENSE)

`af` is a multi-agent coding factory for Git repositories. Its first capability is a deterministic
Review Kernel: `af review` runs a sandboxed, budgeted pipeline of model and command reviewers
against a pinned Snapshot and folds what they return into a findings ledger with convergence.
`af task` runs implementation Tasks the same way. One invariant holds everywhere: reviewers and
implementers only ever mutate a sandbox and return typed artifacts; only the kernel integrates;
publishing to a branch or pull request stays an explicit human action.

**Status:** public alpha. See the latest release badge above for the published version;
`af --version` reports the binary you actually run. APIs, configuration and persisted state may
change in any release, and a release reads only the state and configuration it writes itself:
finish or discard in-flight Campaigns and Tasks before upgrading. Every release states in
[`CHANGELOG.md`](CHANGELOG.md), under *Authority compatibility*, which committed `.af/` policy it
accepts and the hand edit or `af onboard --refresh-lock` a change needs; that line describes the
release, not a promise about the next one
([ADR-0145](docs/adr/0145-read-only-what-this-release-writes-and-version-every-changed-shape.md)).

## Quickstart

Four steps, one command each. `af help start` prints them too, and `af doctor` checks where you
are at any point, token-free.

<a id="install"></a>

1. **Install** af. Install `minisign` first: the installer verifies the release signature with
   it and refuses without it (`brew install minisign` on macOS, `sudo apt install minisign` on
   Debian and Ubuntu).

   ```sh
   curl --proto '=https' --tlsv1.2 -fsSL https://github.com/PakhomovAlexander/afactory/releases/latest/download/install.sh | sh
   ```

   `curl … | AF_INSTALL_CHANNEL=rc sh` (or `curl … | sh -s -- --rc`) installs the newest
   pre-release instead. The installer ends with the next steps below and, when its bin directory
   is not on `PATH`, the line that puts it there for your shell. Signatures, updates, source
   builds and removal are in [docs/self-management.md](docs/self-management.md).

2. **Connect a Provider**, the `claude` or `codex` CLI login af's model Workers run under:

   ```sh
   af provider detect --apply
   ```

   It finds the logins on this machine and registers them. With no login yet, run
   `af provider setup claude-main --kind claude --login` (or `codex-main --kind codex`) at your
   own terminal, never through an agent. af never reads a credential; see
   [docs/providers.md](docs/providers.md).

3. **Onboard a repository**: in a clean, trusted checkout, preview `.af/` (the reviewers, the
   Provider each needs, and the repository's acceptance Gate, such as `make check`), write it,
   and commit it:

   ```sh
   af onboard && af onboard --apply && git add .af && git commit -m 'Configure Afactory review authority'
   ```

   `af onboard` alone is token-free and writes nothing. To read the generated policy, Worker
   prompts and lock before trusting them, run the commands one at a time and look at
   `git diff --cached -- .af` before the commit. Pass `--gate 'check=make check'` when the Gate
   it detects is not the one you want.

4. **Review a change**: make one, plan its review token-free, then run it with the `--provider`
   bindings the onboard preview printed:

   ```sh
   af review plan --policy-rev HEAD --base HEAD --uncommitted
   af review --policy-rev HEAD --base HEAD --uncommitted --provider correctness=PROVIDER --provider architecture=PROVIDER
   ```

   PROVIDER is the ID of a Provider step 2 registered, such as `codex-main`: `af onboard` prints
   the exact command, each reviewer bound to a Provider of the kind it runs, and
   `af help start` prints it for the Providers registered on your machine.

   The plan shows the Change Set, the route, the Gate and the bindings a run needs, and needs no
   login. The run executes the Gate and the reviewers and spends tokens within the budgets `.af/`
   sets; `af review report` shows its verdict and Findings again.

At any step, one token-free command checks the machine and the repository and names the fix for
what is missing:

```sh
af doctor
```

### What just happened

`.af/` is the repository's review authority: the pipeline, the Worker packages, and a lock that
pins them and the af release that wrote them. Only committed authority is trusted, which is why
`--policy-rev` names a revision. A review captures that revision and your change as immutable
Snapshots, runs every Worker in a sandbox over them, and folds the typed results into a Ledger
of Findings; nothing is written to your checkout, committed or pushed. Campaigns are light by
default: one closed Round. If it finds defects, fix them and run the project's Gate; do not start
another Campaign, and use `--heavy` only when a human asks for convergence review
([ADR-0037](docs/adr/0037-default-campaigns-to-one-round-light-review.md)). `af help concepts`
explains Task, Worker, Attempt, Snapshot and the other words in two sentences each.

### Findings that look alike

Two reviewers may report the same line with different titles or severities. They stay two
Findings: a Finding's identity comes from its first Report, never from its path or title, and af
never merges claims that only look alike
([ADR-0006](docs/adr/0006-finding-identity-is-path-independent.md)). When they are one defect,
say so after the Round closes. `af review ledger --campaign NAME` lists each Finding's ID, and
`af review group --campaign NAME FROM INTO` lists FROM under INTO as one Finding, with FROM as its
alias, the higher severity and the status that blocks more; every Report and each Report's own
verification stay. `af review show --campaign NAME FROM` shows the group, and
`af review ungroup --campaign NAME FROM INTO` lists them apart again.

### Agents and CI

A command whose `--help` lists `--json` prints one versioned document under it, except
`af telemetry tail --json`: it streams JSON Lines, one `af/telemetry-tail@1` object per line as
each record is written, until the segments it follows are sealed, so read it line by line, never
as one document. A tail that fails, even after records, ends the stream with one `af/error@1`
line and exits nonzero, so tell each line by its `schema`. `af review report` takes
`--format json` instead. Not every reading command has a JSON form: `af review ledger`,
`af review show` and `af config paths`, for example, print text only. Exit codes are stable
(`af help json`, `af help exit-codes`). A Provider login is a human's:
`af provider setup --login` runs only at an interactive terminal, because the OAuth URL and code
it prints are credentials in transit
([ADR-0112](docs/adr/0112-refuse-agent-mediated-provider-logins.md)); without one, setup exits 3
with the command for a human to run. `af doctor --json` (`af/doctor@2`) tells an agent
what is missing. `CI` turns the background update check off.

### Updating and removing

```sh
af self update --check   # exit 10 when a newer release exists; af self update installs it
af self uninstall        # --purge also removes config, state, Stores, completions and man pages
```

Campaign state lives under the XDG state root: `af review campaigns` lists it and
`af review gc --older-than 14 --keep 5 --apply` removes old Campaigns; `af storage` shows
everything af keeps against its budget. More in
[docs/self-management.md](docs/self-management.md).

### A first Task

`af task` runs implementation Tasks the same way. The software starter is token-free: its
Workers are deterministic commands.

```sh
af catalog init --profile software --destination task-demo && cd task-demo
task_files=$(mktemp -d "$(dirname "$PWD")/af-task-requests.XXXXXX")
mv implementation-*.json review-*.json planning.json "$task_files/"
git init -q && git add . && git commit -qm 'Task starter'
af task plan --file "$task_files/implementation-small.json"
af task run implementation-small --execute
af task show implementation-small
af task deliver implementation-small --branch pagination --worktree ../pagination --confirm implementation-small
```

`plan` captures the Task and shows its plan without running anything; `run --execute` runs that
plan (`--confirm-plan PLAN_ID` instead confirms the exact plan you reviewed); `deliver` writes
the verified result to a new local branch and worktree, never to your checkout. The starter's
README walks through the rest; [docs/tasks.md](docs/tasks.md) is the guide.

## What it does

- **Review campaigns with light-by-default convergence.** One Campaign owns one event log and one
  Ledger across as many Rounds as policy allows; Findings keep their identity across reviewers and
  Rounds, and a Round closes only on a real verdict.
- **Implementation Tasks delivered to new local worktrees.** `af task start --file` runs a Task
  file through a Pipeline pinned in the committed Task catalog: an implementer edits a sandbox,
  read-only acceptance checks inspect the sealed result, an independent evaluator approves a
  content-addressed Snapshot, and `af task deliver` — only after explicit Task-ID confirmation —
  creates a new branch and linked worktree. It never commits, pushes, opens a PR, or touches the
  source checkout. `af catalog init --profile software --destination DIR` creates a new starter
  directory with a runnable catalog.
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
  version. A pin that is not installed yet is installed on demand only when the archive matches
  both the lock's digest and the release's signed `SHA256SUMS`, and only as `[self] install_pins`
  allows: `"ask"`, the default, asks at the terminal first (naming the version, the lock, and
  whether it is older than the running `af` or a pre-release) and without a terminal installs
  nothing and runs the current `af`; `"always"` installs; `"never"` leaves it to
  `af self install <version>`. `af review plan` never installs.

## Requirements

`af doctor` checks every one of these on the machine and repository it runs in.

- Git 2.30 or later, `curl`, and `minisign` for `install.sh` (`brew install minisign`,
  `sudo apt install minisign`), which refuses without it unless `AF_INSTALL_ALLOW_UNSIGNED=1`;
  `gh` only for a release repository that is not public.
- The `claude` and/or `codex` CLIs for model Workers. `af provider setup` verifies and registers a
  context without reading credentials, and with `--login` runs that CLI's official login at your
  terminal; `af provider status` shows what each context can do. Command Workers need neither.
- Whatever the repository's Gate runs. A Gate runs on this machine, so its tools must be installed
  before a review starts, or the first paid run fails at the Gate. This repository's own
  `make check` needs:
  - Rust at or above 1.88 with `clippy` and `rustfmt` (`rust-toolchain.toml` pins all three;
    `make prereqs` checks them), from https://rustup.rs:
    `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
  - a C compiler and `make`: `xcode-select --install` on macOS, `sudo apt install build-essential`
  - `cargo-nextest`: `cargo install cargo-nextest --locked`
  - `python3` and `npx`: `brew install python node`, `sudo apt install python3 npm`
  - `minisign`, as above
- On macOS, exclude af's build and sandbox roots from Spotlight, which `af doctor` checks:
  [docs/README.md](docs/README.md#macos-spotlight-and-gatekeeper) says how.
- Optional: an OCI container runtime (Docker or compatible) for `provider = "container"` Gates.
  Detection runs the runtime's own `info`; an unusable runtime is refused, never silently downgraded.

## Configuration

`af onboard --apply` creates `.af/` at the repository root:

```text
.af/
  af.toml        project policy: minimum af release, default pipelines, routing
  af.lock        the release pin (bytes per target) and every Worker package digest
  pipelines/     pipeline definitions (this repository: review.toml, p1-review.toml, …)
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
- [`docs/self-management.md`](docs/self-management.md) — installing, updating and removing af.
- [`docs/architecture.md`](docs/architecture.md) — how the kernel is built, boundary by boundary.
- [`CONTEXT.md`](CONTEXT.md) — the vocabulary: Snapshot, Subject, Campaign, Round, Finding, Attempt.
- [`docs/adr/README.md`](docs/adr/README.md) — the binding design decisions.
- [`docs/tasks.md`](docs/tasks.md) — the `af task` guide.
- [`docs/project.md`](docs/project.md) — `af project`: the repository's Project and its Workspaces.
- [`docs/values.md`](docs/values.md) — the engineering values, in priority order, with their tests.
- [`CHANGELOG.md`](CHANGELOG.md) — every release and its authority compatibility.

## Development

```sh
make check                           # fmt + clippy + tests + release-selection check
make review-kernel-container-probes  # live container probes; needs a usable runtime
```

The toolchain is pinned, the lockfile is committed, and `unsafe_code = "forbid"` is set
workspace-wide. See [`CONTRIBUTING.md`](CONTRIBUTING.md) before opening a pull request, and
[`AGENTS.md`](AGENTS.md) if you are an agent working in this repository.

## Releasing

`make release VERSION=1.0.0 COMPAT="…"` bumps the workspace version, writes the `CHANGELOG.md`
section from the merged pull requests and their notes under `changelog.d/`, and opens the release
pull request. Merging it tags the commit, runs `make check` on Linux
and macOS, builds every target, signs `SHA256SUMS`, and publishes the release. The workflow then
pins that release in this repository's own `.af/af.lock` on `main`, so `af` here always runs the
newest release; `make check` fails if the pin falls behind
([ADR-0138](docs/adr/0138-the-repository-pins-its-newest-release.md)). That `pin` job runs the
release binary it just installed, verified against the signed `SHA256SUMS`, in a checkout that
holds the push-capable `PIN_DEPLOY_KEY`. That is the job's blast radius: a release whose signed
bytes were hostile could push anything to `main` with the key, past the ruleset; the signature
check is what stands in the way, and the script itself only fast-forwards
([ADR-0152](docs/adr/0152-push-the-release-pin-with-a-deploy-key-that-bypasses-mains-ruleset.md)).

## Security and licence

Report vulnerabilities as described in [`SECURITY.md`](SECURITY.md). Afactory is licensed under
the Apache License, Version 2.0; see [`LICENSE`](LICENSE).
