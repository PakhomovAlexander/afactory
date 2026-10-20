# Security policy

## Supported versions

During the public alpha, security fixes ship in the latest public alpha release; older alpha
releases and release candidates are not patched. Use `af self update` to move forward. A
project's `.af/af.lock` can still pin one of them (anything from 0.8.0 on): `af` installs such a
pin only after you confirm it at the terminal, or under `[self] install_pins = "always"`, and the
prompt says when the pinned release is older than the one running or a pre-release.
A release reads only the state and configuration it writes itself, so a security fix carries no
compatibility guarantee for state or configuration an earlier release wrote
([ADR-0145](docs/adr/0145-read-only-what-this-release-writes-and-version-every-changed-shape.md)).

From 1.0 onward, only the latest `1.x` minor line will receive security fixes, shipped as patch
releases on that line (`1.N.1`, `1.N.2`, …).

| Version                    | Supported |
| -------------------------- | --------- |
| latest public alpha        | yes       |
| older alphas and RCs       | no        |

## Reporting a vulnerability

Report privately through GitHub's vulnerability reporting for this repository:

<https://github.com/PakhomovAlexander/afactory/security/advisories/new>

Do not open a public issue, discussion, or pull request for a vulnerability, and do not
include secrets, tokens, or private repository contents in the report; a description of the
path and a redacted reproduction are enough. There is no email route.

You will get an acknowledgement within 7 days. After that, expect a first assessment of
severity and scope, a fix or a mitigation on the supported line, and credit in the advisory
and the CHANGELOG unless you ask not to be named. Coordinated disclosure is the default: the
advisory is published when the fixed release is available.

## Security model

`af` runs other programs on your behalf — reviewer CLIs, gate commands, provider preflights,
`git`, `gh` — against copies of your code. What it guarantees, and what it does not:

**Child processes start from an empty environment.** Every reviewer, gate, and provider
process, and every `git` and `gh` of a Remote Check gate, is spawned with `env_clear()` and an
explicit allowlist, so API keys and other variables in your shell never reach a child process
unless the allowlist names them. A Worker's or a local check's `PATH` holds no empty or
relative entry. Provider authentication directories (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`)
are handed to the reviewer CLI as *paths*. Both granted path values are redacted from captured
stdout and stderr before anything is stored, as granted and in their canonical spelling. A
Claude Worker also receives your real `HOME` and `USER`, which Claude Code needs to find its
Keychain login; those are not redacted. A Codex Worker's `HOME` is an empty directory af creates
for the Attempt, never the code under review. That redaction covers the granted paths only — it
does not recognise arbitrary token contents, and it is not a substitute for the environment
isolation.

**Private-host login is an explicit capability.** On Linux, `af provider auth` consumes two
validated anonymous pipes separate from ordinary streams, binds one action-time host permission
to a Provider/context/requester and deadline, and keeps challenge/code material only in private
bounded memory. A separate lifetime guard retains the native auth-context lock until its login
process group is stopped, including when the coordinating af process dies. Durable recovery
state contains no challenges or account identities. The host is responsible for authenticating
the requesting human and private delivery; same-user host compromise is outside this boundary.
Generic `setup --login` remains terminal-only. See [the host contract](docs/provider-auth-host.md).

Recognized authentication failures and obvious credential-bearing failed native output are
replaced with a closed summary before capture; exact parsed usage remains separate. This is a
bounded auth-error policy, not a promise to identify every secret in arbitrary successful output.

**Git never runs your hooks or your config.** Snapshot capture invokes `git` with
`env_clear()`, an empty `HOME`, `GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_NOSYSTEM=1` and
`-c` overrides that include `core.hooksPath=/dev/null` and `core.fsmonitor=false`, and only
plumbing that applies no filter, so a hostile checkout or a global config cannot execute code
through capture. The browser that bare `af` opens and the advice about a Task file inside the
checkout run `git` the same way, and the browser compares working-tree files with `HEAD` itself
rather than through `git diff` or `git hash-object`. A tree that names a path git refuses to check
out (`.git` in any case or NTFS/HFS+ spelling, a symlinked `.gitmodules`) is refused at capture,
materialization and Task binding, so no sandbox receives a `.git/config` from the tree under
review.

**`trusted_local` is a filesystem boundary, not security isolation.** A `trusted_local`
sandbox is a materialised copy of a snapshot in a temporary directory. It keeps the input
immutable and captures every mutation; it does not stop a process running as your user from
reading what you can read, writing to absolute paths, or opening sockets. The README says
the same. On Linux a shell Attempt or a local check ends, besides its process group, every
descendant that left the group with `setsid` before its tree is sealed; on macOS such a
descendant can outlive it and write into its tree.

**`container` is the isolation boundary.** The `container` sandbox runs
`--rm --network=none --env-file /dev/null --pids-limit 4096 --cap-drop=ALL
--security-opt=no-new-privileges --init --user uid:gid` with only the sandbox root mounted.
`--env-file /dev/null` keeps your environment out; it does not clear the `ENV` the pinned image
declares. Memory and CPU are unlimited unless the `[gate]` binding sets `memory` and `cpus`.
A runtime that is installed but unusable reports no isolation and refuses to run rather than
falling back to the host. Pipelines that require `container` isolation are refused otherwise.

**No network code of its own.** The workspace contains no HTTP crates. Network egress happens
only through the `curl`, `gh` and `git` CLIs and through the reviewer CLIs themselves, each under
the environment rules above. The workspace forbids `unsafe_code`.

**Releases are signed.** Every release's `SHA256SUMS` is signed with minisign; the public key
is committed at `crates/af/keys/release.pub` and embedded in the binary, and `af self`
verifies the signature before trusting a checksum, on every install path. An install that a
project lock asks for needs the archive to match both the lock's digest for this target and the
signed `SHA256SUMS` (the receipt records `verified_by = "lock+signature"`); a release without
signature material is refused, with or without a lock. A pin that is not installed is installed
on demand only as `[self] install_pins` allows: `"ask"` (the default) asks at the terminal and,
with no terminal, installs nothing and runs the current `af`; `"always"` installs; `"never"`
never does. `af review plan` never installs. `install.sh` fetches over HTTPS only and verifies
the signature; without `minisign` it refuses unless `AF_INSTALL_ALLOW_UNSIGNED=1` accepts the
checksum alone, which only catches a corrupt download, and a re-run verifies the installed
binary again.

**Self-management talks to GitHub, and remembers pinned projects.** After an eligible command,
at most once per `[self] check_every` (24 hours by default), a detached `af self refresh-check`
asks the release repository for its releases: anonymously over HTTPS with `curl`
(`api.github.com` and `github.com`, which see your IP address and the `curl` user agent), and,
only when that is refused, through `gh` under your own GitHub login. `[self] update_check =
false`, `auto_update = "never"`, `CI` or `AF_SELF_OFFLINE=1` turn it off. Each project lock that
dispatch honours is remembered in `$XDG_STATE_HOME/af/self.toml`, the canonical path of the lock
and the version it pins, up to the 64 most recent, so `af self remove` and `prune` keep those
versions; `af self status --json` prints the list, entries whose lock is gone are dropped, and
`af self uninstall --purge` removes the file.

**`af` never publishes.** It does not commit, push, or open pull requests on your behalf. A
verified result is delivered as uncommitted work in a new local worktree for a human to
inspect (ADR-0031).

**af's state is private to you.** Every directory af creates for its own state — the state and
telemetry roots, campaign and Task Stores, their `cas/` and `git-home/` — is created `0700`, and
every file af writes there `0600`, the event log's SQLite `-wal` and `-shm` files included, whatever
your umask; a root af owns that is open to group or others is narrowed to `0700` when af opens it.
af never changes the process umask, so what it writes into your checkout, such as the `.af/` files
of `af onboard`, keeps the modes your umask gives it. An explicit `--state` directory must be
absent, empty or already an af Store, since it becomes af-owned; af removes such a Store whole only
while it carries the owner marker af wrote when it created it and holds nothing else, and otherwise
removes only its own entries ([ADR-0144](docs/adr/0144-hold-afs-disk-use-to-a-machine-budget.md)).

### Out of scope

- **What a reviewer writes.** A malicious or compromised reviewer model can write anything
  into its sandbox: a bad patch proposal, a misleading finding, a planted file. The kernel
  refuses to publish it and records what happened; it does not judge the content. Reviewing
  the proposal is still your job.
- **The reviewer CLIs and the runtimes they need.** `af` inherits their behaviour, including
  whatever they do with the credentials in the directories you grant them.
- **`trusted_local` escapes.** Treat anything a `trusted_local` gate could reach as reachable.
  Use `container` when the code under review is untrusted.
- **Your own environment.** A compromised host, a modified `curl`, `gh` or `git`, or a tampered
  `.af/` policy committed to the repository are outside what `af` can defend against.
