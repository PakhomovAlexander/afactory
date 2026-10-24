# ADR-0204: Make the first run say what to do next

**Status:** accepted (2026-10-23).

## Context

A fresh-machine run-through on macOS and Debian 12 found the install itself solid — eight seconds,
signed, idempotent — and everything after it mute. The installer's last line named shell setup
and `af self status`, no product step, and its PATH hint was a bash line while `claude` from its
native installer lives in `~/.local/bin` too. Bare `af` without a terminal printed its usage on
stderr and exited 2 with nothing to start from; at a terminal, the browser opened on Settings
with no sign that no Provider was registered and the repository was not onboarded. `af onboard`
always proposed a mixed Claude and Codex profile, never said which Provider kinds the Workers
needed against which were registered, and its lock warning said `release v0.11.0 has no
SHA256SUMS` when the fetch had failed. `af --help` opened with kernel words it never defined and
listed the commands twice, differently; `af task plan --help` had six undocumented options. The
README needed some 160 lines before a first review, never showed a first Task, and named
`minisign` only after the one-liner that refuses without it. On one VM the Gate's prerequisites
were found one failed paid run at a time.

The binding constraints are older: the installer stays signed by default (ADR-0045); a Provider
login happens only at a human's terminal (ADR-0112); a command's `--help` is its contract
(ADR-0146); `af onboard` is token-free and never reads a credential (ADR-0032); every printed
document has a `kind@version` (ADR-0145).

## Considered options

- **A guided `af init` wizard that installs, registers and onboards in one go.** It would prompt,
  which nothing may do without a terminal, and it would have to start a Provider login, which
  only a human at a terminal may do (ADR-0112). Rejected: each surface the user already meets
  says what comes next, and every step stays one command a human or an agent can run.
- **Hints only in the README.** The run-through showed the README is read after something went
  wrong, from the output that went wrong. Rejected: the binary's own output carries the next
  step; the README repeats the same four steps.
- **Make `af doctor` part of `af self status` or `af provider status`.** Each answers for its own
  namespace; the first-run questions cross them (is the bin directory on PATH, is git new
  enough, does the Gate's `make` exist, is there a Provider for each Worker kind). Rejected for
  one top-level command that reads what those commands read.
- **Let `af doctor` fix what it finds.** A health check that installs, logs in or rewrites
  configuration is no longer safe to run anywhere. Rejected: it reads only, and every finding
  carries the one command that fixes it.
- **Probe Providers in `af onboard` by default.** The status probe starts each Provider's CLI;
  onboarding has always been a pure read of the repository and the registry. Rejected for a
  registry read by default and `--probe` to add the login state.
- **Put the providers block in `af onboard --json`.** The onboard document has no version yet,
  and changing its shape here would be a second change of contract inside a first-run package.
  Rejected for now: the block and the runner's reason are text only, and the JSON document keeps
  its shape.

## Decision

1. **The installer's ending.** `install.sh` ends with at most six lines on stdout: the installed
   version and its path, `next:`, three steps (`af provider detect` — find and connect your
   claude/codex logins; `af onboard` — in a repository, preview `.af/` token-free;
   `af self setup-shell --write` — completions and man pages), and, only when the bin directory
   is not on `PATH`, the line for `$SHELL`: `fish_add_path DIR` for fish, `export PATH="DIR:$PATH"`
   with `~/.bashrc` or `~/.zshrc` named for bash and zsh, a generic line otherwise. The binary's
   own activation line is not repeated. `AF_INSTALL_CHANNEL=rc` or `--rc` installs the newest
   pre-release, a release ranking above its own pre-releases and `rc.N` by N, as
   `[self] channel = "rc"` does; any other channel or argument is a usage error (exit 2). The
   refusal without `minisign` names the install line for the package manager it finds (brew,
   apt, dnf, pacman, apk). The installer stays signed by default.
2. **Bare `af`.** Without a terminal it keeps its usage on stderr and exit 2, after two lines: that
   the browser opens only at a terminal, and `first run? af provider detect · af onboard · af help
   start`. At a terminal with no registered Provider it opens on the Providers pane, whose first
   row says `no Provider registered yet -- :provider detect scans this machine (read-only);
   :provider detect --apply registers what it finds`; in a repository without `.af/`, the
   Settings pane's `project` row reads `absent  <- af onboard`. The browser draws ASCII only.
3. **Onboarding names the Providers.** Without `--runner`, a new bundle's runner is `claude` or
   `codex` when that is the only Provider kind registered and `mixed` otherwise; the `profile`
   line says which and why. Between `reviewers` and `gates` the preview prints a `providers`
   block: per model Worker, the kind it needs and the registered Providers of that kind with the
   `--provider NODE=ID` binding to pass, or `none registered` with `af provider detect` and the
   `af provider setup <kind>-main --kind <kind> --login` line. It reads the registry only;
   `--probe` adds each Provider's login state from the token-free status probe. After `--apply`
   the next steps say to register a Provider when the block says none; validating existing
   authority prints the `af review plan` line with the registered IDs filled in. The generated
   `.af/README.md` step 2 names `af provider detect` and `af provider setup … --login`, and the
   private-host protocol moves to a closing "Agents without a terminal" note. A successful
   `af provider setup` or `add` ends with `next: af onboard (in a repository)`.
4. **Warnings say what happened.** A lock written without the release's checksums says `could not
   fetch SHA256SUMS of vX (REASON); the lock records this target only (TARGET)`, where REASON
   carries the `gh` failure and the anonymous route's own (`the anonymous route failed: HTTP
   404`). A build without a receipt names the receipt path it looked for and `af self install`.
5. **`af doctor [--json] [--repo DIR]`.** A top-level, token-free check whose text starts with
   `token-free; the charged end-to-end Provider check is af provider doctor`, then one line per
   check — `binary`, `path`, `shell`, `update`, `tools`, `providers`, `storage`, and `project`
   inside a repository — each `ok`, `warn` or `fail` with a one-line `fix:`. Exit 0 when nothing
   failed, 1 when a check failed. `--json` prints one `af/doctor@1` document
   (`schemas/doctor-v1.json`): `checks[] {id, status, detail, fix}`, `fix` null exactly when the
   check is `ok`. It never calls a model, logs in, installs or writes a file; it is exempt from
   pin dispatch and schedules no update check. The processes it starts are version checks, the
   Provider status probe for registered Providers, and, while update checks are on, one
   anonymous HTTPS request to the release source whose answer it discards. The Gate's tools are
   read from its commands and the Makefile or script they name.
6. **Help.** `af --help` opens with one sentence of what af is, the four steps and `af doctor`,
   then one command list, in the order a first run needs the commands (provider, onboard,
   review, task, self, doctor, help, then the rest). `af help start` and `af help concepts`
   (Task, Worker, Attempt, Snapshot, Round, Campaign, Store, Provider, Gate, Pipeline, Candidate,
   two sentences each with the command that shows it) are topics, and `af help` lists every
   topic once with the description the topic registry holds: one source. Every argument of every
   visible command has a description, which a test walking the clap definition enforces.
   `af task output` writes to stdout without `--output`.
7. **Shell setup and removal.** `af self setup-shell --write` ends with what is left to do:
   `done — fish picks this up in a new shell`, or `done — add the rc line above, then open a new
   shell`;
   `--remove` deletes exactly the completion file and the af man pages it writes. Without
   `$SHELL` the message lists all five supported shells. `af self uninstall` names the completion
   files and man pages it leaves, with the `rm` line that removes them; `--purge` removes them.
8. **Documentation.** The README's Quickstart is four numbered steps with one command each — the
   install with the `minisign` line above it, `af provider detect --apply`, `af onboard && af
   onboard --apply && git add .af && git commit`, then `af review plan` and `af review` with
   `--provider` bindings — and `af doctor`, followed by what just happened, agents and CI,
   updating and removing, and a first `af task`. Install mechanics and signing live in
   `docs/self-management.md`, the Provider details in `docs/providers.md`, and the Requirements
   list each Gate prerequisite with its install line. A test runs the README's token-free
   commands against a fixture repository.

## Consequences

- Every first-run surface — the installer, bare `af`, the browser, `af onboard`, `af provider
  setup` — ends by naming the next command, and `af doctor` answers "what is missing here" in
  one token-free command, so an agent without a terminal can find its way and a human is asked
  only for what only a human may do.
- `af provider detect` is named by the installer, the hints, the browser, onboarding and the
  doctor's fixes. It is delivered by its own package; until a build carries it, those lines name
  a command that build does not have.
- `af onboard --json` is unchanged: the providers block and the runner's reason are text. Giving
  them to agents is a versioned onboard document of its own, later.
- `af doctor`'s tool scan is a reading of the Gate's commands, not an execution: a Gate that
  reaches a tool only through layers of scripts may need a tool the doctor did not name. The
  first failed check names it.
- `af doctor` adds one schema, `af/doctor@1`; a changed shape is a new version (ADR-0145).
