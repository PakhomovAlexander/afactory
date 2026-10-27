<picture>
  <source media="(prefers-color-scheme: dark)" srcset="brand/banner/banner-dark.svg">
  <img alt="afactory: agent pipelines made fast" src="brand/banner/banner-light.svg" width="100%">
</picture>

[![CI](https://img.shields.io/github/actions/workflow/status/PakhomovAlexander/afactory/release.yml?branch=main&style=flat-square&labelColor=0F0F0F&color=36EEA8&label=CI)](https://github.com/PakhomovAlexander/afactory/actions/workflows/release.yml)
[![Latest release](https://img.shields.io/github/v/release/PakhomovAlexander/afactory?style=flat-square&labelColor=0F0F0F&color=5195F5&label=release)](https://github.com/PakhomovAlexander/afactory/releases/latest)
[![Licence](https://img.shields.io/badge/licence-Apache--2.0-EE366A?style=flat-square&labelColor=0F0F0F)](LICENSE)

`af` runs coding agents as pipelines over a Git repository. `af review` reviews a change;
`af task` makes one and verifies it. Every Worker, a Claude or Codex agent or a plain command,
works on a copy of your repository in its own sandbox and on its own token budget, and the
pipeline is a handful of files you commit in `.af/`. Committing, pushing and opening the pull
request stay your call ([Boundaries](#boundaries)).

The review pipeline `af onboard` drafts when the machine has both a Claude and a Codex login:

```text
                             +--> correctness  (Claude) --+
your change --> your checks -+                            +--> verdict and findings
                             +--> architecture (Codex)  --+
```

- **Read the plan before anything spends.** `af review plan` shows the checks, the reviewers,
  the login each one runs under and the budgets, token-free and without a login.
- **Models review only what passes your checks.** The Gate, your project's own check command
  such as `make check`, runs before any reviewer; a check that could not run is not a pass.
- **A budget on every Worker.** Each run of a Worker reserves its tokens before it starts, and
  the tightest limit refuses by name; `af onboard` drafts 300,000 tokens per reviewer run and
  1,000,000 per review. A limit stops new work, not a call already running, and the report shows
  what each run spent.
- **The pipeline is shared like code.** af reads `.af/` only from a commit and pins every Worker
  and its own release by digest, so the whole team runs the same review and a change to it is a
  diff you review.
- **Agents can drive it.** Most commands print one JSON document with `--json`, exit codes are
  stable, and a Task's state names the next command: see [For agents](#for-agents).

**Status:** public alpha. A release reads only the state and configuration it writes itself, so
finish or discard running Campaigns and Tasks before you upgrade. Each release's entry in
[`CHANGELOG.md`](CHANGELOG.md) says, under *Authority compatibility*, whether committed `.af/`
needs `af onboard --refresh-lock` or a hand edit
([ADR-0145](docs/adr/0145-read-only-what-this-release-writes-and-version-every-changed-shape.md)).

## Quickstart

Four steps, one command each. `af help start` prints them too, and `af doctor` checks where you
are at any point, token-free.

<a id="install"></a>

1. **Install** af. The installer verifies the release signature with `minisign` and refuses
   without it, so install that first: `brew install minisign` on macOS,
   `sudo apt install minisign` on Debian and Ubuntu.

   ```sh
   curl --proto '=https' --tlsv1.2 -fsSL https://github.com/PakhomovAlexander/afactory/releases/latest/download/install.sh | sh
   ```

   Releases are built for macOS on Apple silicon and for Linux on x86_64 and arm64.
   `curl … | sh -s -- --rc` (or `curl … | AF_INSTALL_CHANNEL=rc sh`) installs the newest
   release, pre-releases included, and `curl … | AF_INSTALL_VERSION=X.Y.Z sh` an exact one. The
   installer ends with the next steps and, when its bin directory is not on `PATH`, the line that
   puts it there for your shell. Signatures, updates, source builds and removal are in
   [docs/self-management.md](docs/self-management.md).

2. **Connect a Provider**, the `claude` or `codex` CLI login that model Workers run under:

   ```sh
   af provider detect --apply
   ```

   It finds the logins on this machine and registers them. With no login yet, run
   `af provider setup claude-main --kind claude --login` (or `codex-main --kind codex`) yourself,
   at your own terminal, never through an agent. af keeps no credential: the registry holds an
   ID, a kind and a directory ([docs/providers.md](docs/providers.md)).

3. **Onboard a repository.** In a clean checkout you trust, preview `.af/`, write it, and commit
   it. On a first run, enter the commands one at a time and read `git diff --cached -- .af`
   before the commit; this line does all four without a pause:

   ```sh
   af onboard && af onboard --apply && git add .af && git commit -m 'Configure Afactory review authority'
   ```

   The preview names the reviewers, the Provider each one needs, the Gate it detected (the
   repository's own acceptance command, such as `make check`) and the budgets; it is token-free
   and writes nothing. Pass `--gate 'check=make check'` when the detected Gate is not the one
   you want.

4. **Review a change.** Make one, plan its review, then run it with the `--provider` bindings
   the onboard preview printed:

   ```sh
   af review plan --policy-rev HEAD --base HEAD --uncommitted
   af review --policy-rev HEAD --base HEAD --uncommitted --provider correctness=PROVIDER --provider architecture=PROVIDER
   ```

   PROVIDER is the ID of a Provider step 2 registered, such as `codex-main`: `af onboard` prints
   the exact command, each reviewer bound to a Provider of the kind it runs, and `af help start`
   prints it for the Providers registered on your machine.

   The plan is token-free and needs no login: it shows the Change Set, the route, the Gate and
   the bindings the run needs. The run executes the Gate, then the reviewers, and prints the
   verdict, the Findings and the tokens it spent. Name the review with `--campaign NAME` and
   `af review report --campaign NAME` prints it again.

Stuck at any step? One token-free command checks the machine and the repository and names the
fix for whatever is missing:

```sh
af doctor
```

### What just happened

`.af/` is the repository's review authority: the pipeline, the Worker packages (a prompt and a
manifest each) and a lock that pins them and the af release that wrote them. af reads it only
from a commit, so nothing you have not reviewed and committed ever governs a review; that is why
`--policy-rev` names a revision. A review captures that revision and your change as immutable
Snapshots, runs every Worker in a sandbox over them, and folds their typed results into a Ledger
of Findings.

A review is one Round by default. When it finds defects, fix them and run your Gate. That ends a
light review: do not open another Campaign to check the fix. `--heavy`, for when a human asks
for convergence review, lets the same Campaign review your fixes in further Rounds until the
pipeline's convergence policy is met or its Round limit is reached
([ADR-0037](docs/adr/0037-default-campaigns-to-one-round-light-review.md)). `af help concepts`
explains Task, Worker, Attempt, Snapshot and the other words in two sentences each.

### Updating and removing

```sh
af self update --check   # exit 10 when a newer release exists; af self update installs it
af self uninstall        # --purge also removes config, state, Stores, completions and man pages
```

`af storage` shows everything af keeps on this machine against its budget and
`af storage prune --apply` reclaims it; `af review gc --older-than 14 --keep 5 --apply` removes
old review Campaigns. More in [docs/self-management.md](docs/self-management.md).

### A first Task

`af task` turns a Task file into a verified change. The software starter is a small Python
project with a pagination Task whose Workers are deterministic commands, so it runs without a
login and spends no tokens (it needs `python3`):

```sh
af catalog init --profile software --destination task-demo && cd task-demo
# Task files are requests, not source: they live outside the checkout.
task_files=$(mktemp -d "$(dirname "$PWD")/af-task-requests.XXXXXX")
mv implementation-*.json review-*.json planning.json "$task_files/"
git init -q && git add . && git commit -qm 'Task starter'
af task plan --file "$task_files/implementation-small.json"
af task run implementation-small --execute
af task show implementation-small
af task deliver implementation-small --branch pagination --worktree ../pagination --confirm implementation-small
```

`plan` captures the Task and prints its plan without running anything; `run --execute` runs that
plan (`--confirm-plan PLAN_ID` instead confirms the exact plan you read); `show` prints how it
ended; `deliver` writes the verified result to a new local branch and worktree, never to your
checkout. The starter's README walks through the rest, and [docs/tasks.md](docs/tasks.md) is
the guide.

## What it does

- **Review Campaigns.** A Campaign owns one Ledger across its Rounds. A Finding keeps its
  identity across reviewers and Rounds, and a Round closes only on a real verdict: a reviewer
  that did not answer leaves it incomplete, never clean.
- **Implementation Tasks.** `af task` runs a Task file through a Pipeline pinned in the
  committed Task catalog: an implementer edits a sandbox, read-only checks inspect the sealed
  result, an independent evaluator approves an exact Snapshot, and `af task deliver`, once you
  confirm the Task ID, creates a new branch and worktree for it.
  `af catalog init --profile software --destination DIR` writes a runnable starter.
- **Gates and Provider admission.** A Gate runs in a disposable copy of the Snapshot (a Task's
  Remote Check runs in your CI instead), and a Gate with no required checks does not pass.
  Every model Worker needs a named Provider from the machine-local registry, admitted by a
  short, charged preflight before anything is dispatched.
- **Routing by changed paths.** `.af/af.toml` can send a docs-only change to a cheaper pipeline
  and an oversized diff to one that splits it; the choice is token-free and pinned for the
  Campaign's later Rounds.
- **Signed releases, pinned per project.** `.af/af.lock` pins the af release by archive digest,
  and inside the project `af` runs that release. One that is not installed yet is installed only
  when its archive matches both the lock and the release's signed `SHA256SUMS`, and only as
  `[self] install_pins` allows: by default it asks at the terminal. If you decline, or no
  terminal can ask, an older pin runs on the current `af` with a warning and a newer one is
  refused.

## Boundaries

- **Your checkout stays yours.** af never applies a sandbox's changes to your checkout: Workers
  work on copies, and a Task's result goes to a new branch and worktree. af writes into a
  checkout only when you point a command at it, such as `af onboard --apply` (its `.af/`) or
  `af task materialize --into DIR`.
- **You publish.** af does not commit, push or open a pull request. The one exception is opt-in:
  a Task pipeline that runs a check as a Remote Check pushes exactly two `af-gate/<task-id>/`
  branches and opens one draft pull request between them so your CI can run that check. The
  machine's operator authorizes it by mapping the repository to a push target. The plan names
  the destination for you to confirm, but `--execute` skips that confirmation, so map a
  repository only on a machine where every Task may push its gate branches
  ([docs/task-execution/remote-checks.md](docs/task-execution/remote-checks.md)).
- **No stored credentials.** A Provider is the official `claude` or `codex` CLI's own login:
  af's registry holds its ID, kind and directory, and `af provider setup --login` runs only at
  an interactive terminal.
- **Empty environments.** Every Worker, Gate and Provider process starts from an empty
  environment plus an allowlist, so your shell's API keys and other variables stay out unless the
  allowlist names them.
- **`trusted_local` is not security isolation.** The default sandbox keeps the Snapshot
  immutable and captures every change, but a process in it runs as you and can read and write
  whatever you can, your checkout included. Run code you do not trust under `container`
  isolation ([SECURITY.md](SECURITY.md)).

## For agents

Rules for a coding agent, or a script, that drives af for someone:

- **Plan first.** `af review plan …` and `af task plan --file …` are token-free. A review plan
  needs no login; a Task plan with model Workers checks that their Providers are logged in.
  Show the plan before anything spends.
- **Read JSON, not prose.** Most commands take `--json` and print one document on stdout, with
  progress on stderr; `af review report` and `af review campaigns` take `--format json`, and a
  command's `--help` names its output flags. Branch on the document's `schema`: most refusals
  are `af/error@1`, while `af provider` commands and a refused Task plan print their own
  documents (`af help json`). `af telemetry tail --json` prints one object per line, and
  `af config show --json` a document with no `schema`.
- **Branch on the exit code.** `af review run` exits 0 pass, 3 fail, 4 incomplete. A foreground
  `af task run` exits 0 verified, 3 changes requested, 4 cancelled, exhausted or waiting for a
  person, 5 gate failed, 6 Provider failed, 7 expired. With `--detach`, 0 only means the run
  started: `af task wait` returns the outcome, or 124 when its `--timeout` passes. 1 is an error
  and 2 a usage error (`af help exit-codes`).
- **Follow `next_action`, with care.** The documents that describe a Task's state
  (`af task show`, `status`, `wait`, `findings` and the others `af help json` lists) carry
  `state.outcome` and `state.next_action`, the command to run next with the `--repo`, `--state`
  or `--workspace` you passed (a `report`, written for a pull request, leaves them out).
  `next_action` is null when there is none, a `TASK_FILE` in it is yours to fill in (with a new
  `task_id` once the Task ran), and a `--login` step belongs to the human. `diff` and
  `materialize` print no state: run `af task status` after them.
- **Never log in for a human.** `af provider setup --login` runs only at an interactive
  terminal, because the OAuth URL and code it prints are credentials in transit; without one it
  exits 3 with the command for the human to run
  ([ADR-0112](docs/adr/0112-refuse-agent-mediated-provider-logins.md)).
- **One review, then fix.** A review that finds defects exits non-zero by design: fix the
  Findings, run the project's Gate, and stop. Do not start another Campaign to check your fix;
  `--heavy` is for when a human asks for convergence review.
- **Ask the doctor.** `af doctor --json` prints one `af/doctor@2` document: every check with its
  status, a `fix` command where one command fixes it, and a `hint` otherwise.

The whole Task loop (plan, run or `--detach`, wait, findings, diff, materialize, deliver, and
`report --pr-body` for the pull request) is in [docs/agents.md](docs/agents.md), which
`af help agents` also prints. In CI, the `CI` variable turns the background update check off.
Working on af itself? Read [`AGENTS.md`](AGENTS.md).

## Requirements

`af doctor` checks these on the machine and in the repository it runs in, including the usual
tools a Gate calls, but not `gh` or its login for a Remote Check; it does not run the Gate.

- macOS on Apple silicon, or Linux on x86_64 or arm64; on an Intel Mac, build from source.
- Git 2.30 or later, `curl` and `minisign` for the installer, which refuses without `minisign`
  unless `AF_INSTALL_ALLOW_UNSIGNED=1`. `gh`, logged in, for a Remote Check, and for `af self`
  downloads from a release repository that is not public.
- The `claude` and/or `codex` CLI for model Workers; command Workers need neither.
  `af provider status` shows what each registered login can do.
- Whatever your Gate's local checks run, installed on this machine: they run here, so a missing
  tool fails the first paid run at the Gate. A Remote Check needs its tools on the CI runner.
- On macOS, Spotlight kept out of af's build and sandbox roots, which `af doctor` checks;
  [docs/README.md](docs/README.md#macos-spotlight-and-gatekeeper) says how.
- Optional: Docker or another OCI container runtime for `container` Gates. An unusable runtime is
  refused, never silently swapped for the host.

## Configuration

`af onboard --apply` creates `.af/` at the repository root:

```text
.af/
  README.md      the generated guide to reviewing a change in this repository
  af.toml        project policy: minimum af release, default pipeline, routes
  af.lock        the af release pin (an archive digest per target) and every Worker package digest
  pipelines/     pipeline definitions; af onboard writes review.toml
  workers/       Worker packages: <name>/reviewer.md (the prompt) and reviewer.toml (the manifest)
```

Configuration merges, lowest to highest: built-in defaults, `/etc/af`, `~/.config/af`, every
`.af/af.toml` above the repository, the repository's `.af/af.toml`, its gitignored
`.af/af.local.toml`, and `AF_<TABLE>__<KEY>` variables (`af help layers`).
`af config show --origin` names where each value came from. Provider bindings such as
`claude-main` and `codex-main` live in a machine-local registry, never in the repository.
[docs/architecture.md](docs/architecture.md) describes pipelines, routing and budgets.

## Documentation

- [docs/README.md](docs/README.md): a map of everything, by question, with troubleshooting.
- [CONTEXT.md](CONTEXT.md): the vocabulary, from Snapshot and Campaign to Finding and Attempt.
- [docs/providers.md](docs/providers.md): getting model access onto a machine, safely.
- [docs/tasks.md](docs/tasks.md): the `af task` guide; [docs/agents.md](docs/agents.md): the
  loop an agent drives.
- [docs/project.md](docs/project.md): `af project`, the repository's Project and its Workspaces.
- [docs/self-management.md](docs/self-management.md): installing, updating and removing af.
- [docs/architecture.md](docs/architecture.md): how the kernel is built, boundary by boundary.
- [docs/adr/README.md](docs/adr/README.md): the binding design decisions.
- [docs/values.md](docs/values.md): the engineering values, in priority order.
- [CHANGELOG.md](CHANGELOG.md): every release and its authority compatibility.

## Development

af is built with af: changes to this repository are made and verified by af Tasks, and every
pull request apart from Dependabot's and the release pull requests carries their
`af task report` ([ADR-0142](docs/adr/0142-carry-the-af-task-report-in-every-pull-request.md)).

```sh
make prereqs                         # what make check needs, with the command for each missing tool
make check                           # preflight, fmt, clippy, the test suite and the release checks
make review-kernel-container-probes  # live container probes; needs a usable runtime
```

The toolchain is pinned in `rust-toolchain.toml`, the lockfile is committed, and
`unsafe_code = "forbid"` is set workspace-wide. Read [CONTRIBUTING.md](CONTRIBUTING.md) before
opening a pull request, and [AGENTS.md](AGENTS.md) if you are an agent working here.

### Releasing

`make release VERSION=X.Y.Z COMPAT="…"` opens the release pull request: the version bump and the
`CHANGELOG.md` section, written from the merged pull requests and their `changelog.d/` notes.
Merging it tags the commit, runs `make check` on Linux and macOS, builds every target, signs
`SHA256SUMS` and publishes the release
([ADR-0045](docs/adr/0045-one-release-train-and-a-pin-that-binds-bytes.md)). A `pin` job then
moves this repository's `.af/af.lock` to the new release, so `af` here runs the newest one;
`make check` fails when the pin is more than one release behind
([ADR-0138](docs/adr/0138-the-repository-pins-its-newest-release.md)). The job pushes to `main`
with a deploy key that bypasses the branch ruleset, running the release binary it has just
verified against the signed `SHA256SUMS`: that signature check is what stands between a hostile
release and `main`
([ADR-0152](docs/adr/0152-push-the-release-pin-with-a-deploy-key-that-bypasses-mains-ruleset.md)).

## Security and licence

Report vulnerabilities as described in [`SECURITY.md`](SECURITY.md). Afactory is licensed under
the Apache License, Version 2.0; see [`LICENSE`](LICENSE).
