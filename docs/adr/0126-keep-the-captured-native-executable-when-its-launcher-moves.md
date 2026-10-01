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
the launcher. The captured version file stays where it was.

An update during a Task therefore changed what `PATH` resolved to. The next Claude Worker or
Provider admission of that Task was refused with `Captured Task Provider identity is no longer
current or could not be verified before invocation`, and the Task ended incomplete. This
happened when Claude Code went from 2.1.284 to 2.1.285 while a Task was running. A Task that
runs for hours is likely to cross an update.

The second resolution did not protect what runs: every invocation already used the captured
absolute path. It only reported that the installation had changed.

## Considered options

- **Adopt the newly resolved executable.** Rejected: the Task's Provider admission ran on the
  captured client. Another client in the middle of a Task changes behaviour that no admission
  covered; 2.1.285 itself changed how usage is reported
  ([ADR-0125](0125-charge-a-claude-model-breakdown-that-covers-the-top-level-summary.md)).
- **Compare only the account when only the version changed.** Rejected: `af` cannot tell a
  version update from any other replacement of the launcher without a rule for each client's
  installation layout.
- **Require native auto-updates to be off, and say so in the refusal.** Rejected: a Task then
  fails because of another tool's setting, which other sessions on the machine can trigger.
- **Keep the captured executable for the life of the process, and require only that it is
  still there (chosen).**

## Decision

The recheck no longer resolves `PATH` again. It requires the captured absolute path to still be
an executable regular file. Every other comparison of ADR-0090 stays: the configured Provider,
its authentication directory and selector, the sanitized `PATH` value, `HOME`, `USER`, and the
principal and authentication method, probed through the captured executable.

A captured executable that is gone refuses before the identity probe, with known-zero usage and
its own diagnostic: `Captured Task Provider executable is no longer available; a native client
update may have removed it`. Running the Task again resolves the installed client.

A new `af` process still resolves `PATH` when it starts, so a resumed Task runs the client
installed at that time.

## Consequences

A Task keeps running across a native client update. All Workers of one `af` process run one
client version, even after a newer one is installed.

A different executable placed earlier in the same `PATH` directories during a Task is no longer
refused; the Task does not run it. A file replaced in place at the captured path is not
detected, as before: executable identity remains its resolved absolute path, not a content
digest.

A CLI fixture updates its client during Provider admission and checks that both later Workers
complete on the captured executable. A unit test covers the removed executable.
