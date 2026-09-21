# Security policy

## Supported versions

Only the latest `0.x` minor line receives security fixes. A fix ships as a patch release on
that line (`0.N.1`, `0.N.2`, …); older minors are not patched, and `af self update` moves an
installation forward.

| Version                    | Supported |
| -------------------------- | --------- |
| latest `0.x` minor         | yes       |
| earlier `0.x` minors       | no        |

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
process is spawned with `env_clear()` and an explicit allowlist, so API keys and other
variables in your shell never reach a child process unless the allowlist names them.
Provider authentication directories (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`) are handed to the
reviewer CLI as *paths*. Those granted path values are redacted from captured stdout and
stderr before anything is stored. That redaction covers the granted paths only — it does not
recognise arbitrary token contents, and it is not a substitute for the environment isolation.

**Git never runs your hooks or your config.** Snapshot capture invokes `git` with
`env_clear()`, `core.hooksPath=/dev/null`, `core.fsmonitor=false`, and
`GIT_CONFIG_GLOBAL=/dev/null`, so a hostile checkout or a global config cannot execute code
through capture.

**`trusted_local` is a filesystem boundary, not security isolation.** A `trusted_local`
sandbox is a materialised copy of a snapshot in a temporary directory. It keeps the input
immutable and captures every mutation; it does not stop a process running as your user from
reading what you can read, writing to absolute paths, or opening sockets. The README says
the same.

**`container` is the isolation boundary.** The `container` sandbox runs
`--rm --network=none --env-file /dev/null --user uid:gid` with only the sandbox root mounted.
A runtime that is installed but unusable reports no isolation and refuses to run rather than
falling back to the host. Pipelines that require `container` isolation are refused otherwise.

**No network code of its own.** The workspace contains no HTTP crates. Network egress happens
only through the `gh` and `git` CLIs and through the reviewer CLIs themselves, each under the
environment rules above. The workspace forbids `unsafe_code`.

**Releases are signed.** Every release's `SHA256SUMS` is signed with minisign; the public key
is committed at `crates/af/keys/release.pub` and embedded in the binary, and `af self`
verifies the signature before trusting a checksum. `install.sh` verifies the signature when
`minisign` is on `PATH` and the checksum otherwise.

**`af` never publishes.** It does not commit, push, or open pull requests on your behalf. A
verified result is delivered as uncommitted work in a new local worktree for a human to
inspect (ADR-0031).

### Out of scope

- **What a reviewer writes.** A malicious or compromised reviewer model can write anything
  into its sandbox: a bad patch proposal, a misleading finding, a planted file. The kernel
  refuses to publish it and records what happened; it does not judge the content. Reviewing
  the proposal is still your job.
- **The reviewer CLIs and the runtimes they need.** `af` inherits their behaviour, including
  whatever they do with the credentials in the directories you grant them.
- **`trusted_local` escapes.** Treat anything a `trusted_local` gate could reach as reachable.
  Use `container` when the code under review is untrusted.
- **Your own environment.** A compromised host, a modified `gh` or `git`, or a tampered
  `.af/` policy committed to the repository are outside what `af` can defend against.
