# ADR-0126: Keep the captured native executable when its launcher moves

**Status:** accepted (2026-10-01). Supersedes in part
[ADR-0090](0090-recheck-native-task-provider-identity-before-private-invocation.md): the
requirement that `PATH` still resolve to the captured executable before each invocation.

## Context

An `af` process resolves a Provider's command on `PATH` once, follows its symlinks, and invokes
that absolute path for every Worker it runs. ADR-0090 rechecks the Provider before each private
invocation. One of its comparisons resolved `PATH` again and refused when the result differed
from the captured path.

Claude Code installs each version as its own file, `~/.local/share/claude/versions/<version>`,
behind a launcher symlink, `~/.local/bin/claude`. It updates itself without notice by repointing
the launcher. The captured version file usually stays where it was; the updater may delete old
version files later. A package manager that upgrades a client, such as Homebrew, removes the old
versioned path at once.

An update during a Task therefore changed what `PATH` resolved to. The next Claude Worker or
Provider admission of that Task was refused with `Captured Task Provider identity is no longer
current or could not be verified before invocation`, and the Task ended incomplete. This
happened when Claude Code went from 2.1.284 to 2.1.285 while a Task was running. A Task that
runs for hours is likely to cross an update.

The second resolution did not protect what runs: every invocation already used the captured
absolute path. It only reported that the installation had changed.

A person who leaves their client's updates on must not lose a Task to an update.

## Considered options

- **Adopt the newly resolved executable as soon as the launcher moves.** Rejected: the Task's
  Provider admission ran on the captured client. While that client is still there, changing it
  in the middle of a Task changes behaviour for no need; 2.1.285 itself changed how usage is
  reported
  ([ADR-0125](0125-charge-a-claude-model-breakdown-that-covers-the-top-level-summary.md)).
- **Refuse when the captured executable is gone.** Rejected: the Task then fails because of an
  update, which is the failure this record removes.
- **Compare only the account when only the version changed.** Rejected: `af` cannot tell a
  version update from any other replacement of the launcher without a rule for each client's
  installation layout.
- **Require native auto-updates to be off, and say so in the refusal.** Rejected: a Task then
  fails because of another tool's setting, which other sessions on the machine can trigger.
- **Keep the captured executable while it is there, and move to the installed client when it
  is gone (chosen).**

## Decision

While the captured absolute path is still an executable regular file, every invocation runs
it, and `PATH` is not resolved again.

When the captured file is gone, the process resolves the Provider's command on `PATH` once more
and uses that executable for the rest of its Workers. The replacement receives private input
only after it passes the recheck below; in particular it must report the captured principal and
authentication method.

Every other comparison of ADR-0090 stays, and runs before each private invocation on the
executable in use: the configured Provider, its authentication directory and selector, the
sanitized `PATH` value, `HOME`, `USER`, and the principal and authentication method.

The executable in use can also disappear while an invocation is being prepared: during its
identity recheck, or between the recheck and its start. In both cases the invocation moves to
the installed client and repeats the recheck on it, within the original deadline and at most
three times. A program that could not be started received no input and spent nothing, so this
is still the one Attempt, not a repeat of paid work. The native adapters report that case apart
from every other failure. An executable that is still there and cannot start fails as before.

When the captured file is gone and `PATH` resolves no client, the invocation refuses before any
probe, with known-zero usage and its own diagnostic: `Captured Task Provider executable was
removed and no installed client replaces it`. A resolved client that cannot be run under the
captured grants refuses the same way with `Installed Task Provider client cannot replace the
removed executable`.

A new `af` process still resolves `PATH` when it starts, so a resumed Task runs the client
installed at that time.

## Consequences

A Task keeps running across a native client update, whether or not the update removes the old
version. All Workers of one `af` process run one client version while its file exists; after it
is removed, the remaining Workers run the installed version. An updated client can behave
differently from the one the Provider admission ran on, and the account it uses is still proven
before every invocation.

A different executable placed earlier in the same `PATH` directories during a Task is no longer
refused; the Task does not run it while the captured file exists. A file replaced in place at
the captured path is not detected, as before: executable identity remains its resolved absolute
path, not a content digest.

The model adapter contract gains `invoke_started`, which tells a program that could not be
started apart from every other return. Its default never reports one, so other adapters are
unchanged, and `invoke` returns what it returned before.

Four CLI fixtures update their client during a Task: during Provider admission, keeping or
deleting the old version file, and during the next Worker's identity recheck, with that recheck
answering or failing. Each checks that both later Workers complete, with no failed Attempt, on
the expected executable. Unit tests cover the replacement and the uninstalled client.
