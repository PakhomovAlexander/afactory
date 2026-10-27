# ADR-0256: Carry the recovery of an interrupted command as a typed field

**Status:** accepted (2026-10-26). Applies [ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md)
to the error document; extends [ADR-0129](0129-stop-task-workers-when-af-is-interrupted.md) and
[ADR-0209](0209-recover-a-lost-or-interrupted-attempt-without-charging-its-reservation.md).

## Context

A command SIGINT or SIGTERM stops (ADR-0129) exits 130 or 143. Under `--json` its stdout is one
`af/error@1` document, and the command that continues the work, `af task run TASK_ID` with the
Store selectors the command was given or `af task start --file TASK_FILE` once a node used up
its recovery allowance (ADR-0209), was only in the `error` sentence. Every other recovery a
script acts on is a field: `af/error@1` carries `cause`, `next_action` and `details` for the
refusals af classifies, and an agent is told to branch on fields, never on prose (`af help
json`, `af help agents`). GitHub issue #331 item 10 asks for the same here.

`af/error@1` cannot carry it as it stands. Its `cause` is a closed vocabulary of five, and its
schema requires a `cause` beside any `next_action`, so the field needs a sixth cause. A closed
vocabulary that gains a value is a changed shape: a script that validates against
`schemas/error-v1.json` would refuse the new document.

## Considered options

- **Add `interrupted` to `af/error@1` in place.** Rejected: ADR-0145 clause 2 gives every
  changed printed shape a new version, and a document with an unknown cause would still claim
  version 1.
- **Put `next_action` in the interrupted document without a `cause`.** Rejected: version 1
  requires a cause beside a next command, and a next command without a cause is the orphan
  the schema exists to refuse.
- **Reuse `recovery_exhausted`.** Rejected: an interrupted node that still has a recovery left
  is not exhausted, and that cause fixes the exit code at 1.

## Decision

1. The error document is **`af/error@2`** (`schemas/error-v2.json`). Version 2 is version 1 with
   one more cause, `interrupted`: exit code 130 or 143, `details` with the `signal` (`SIGINT`
   or `SIGTERM`) and the `task` the command was running (null when it ran none), and
   `next_action` the command that continues it, the one the sentence names: `af task run
   TASK_ID` with the command's `--repo`, `--state` or `--workspace` as typed, `af task start
   --file TASK_FILE` past the recovery allowance, the same command line again when the command
   ran no Task, and the `af provider auth status` command of the same login for an interrupted
   `af provider auth begin`. It is null only when no such line can be pasted: a word with a
   control character, or a line past the schema's 4096 characters.
2. Every command prints version 2; `af/error@1` is neither printed nor described, and its
   schema is removed. Every other field and cause keeps its meaning.
3. The text a person reads is unchanged: stderr still ends with the sentence that names the
   same command.

## Consequences

- A script that resumes an interrupted run reads `next_action` and runs it; it no longer parses
  the sentence.
- A script that compared `schema` with `af/error@1` sees `af/error@2`; one that validated
  against `error-v1.json` validates against `error-v2.json`. The parity test refuses a version
  1 document and an interruption with another cause's facts.
- The ADRs before this one keep naming `af/error@1` as the record of what they decided; the
  current shape is this ADR's.

## Amendment (2026-10-27): one rule for every writer, and lines that fit

The review of the first implementation found the rule of decision 1 applied by one writer only
and printed commands still wider than the terminal. These four clauses complete the decision.

1. The `next_action` rule of decision 1 is enforced where the document is written, so it holds
   for every writer: an interrupted Task's `af task run TASK_ID --state DIR` with a tab, a
   newline or a delete in its selector, or past 4096 characters, is null, like the same
   command line again. A cause whose fix the schema requires keeps one: an exhausted
   recovery's `af task start --file TASK_FILE` drops the selectors no line can carry, and its
   sentence's `fix:` completes it. Every `af/error@2` document validates against
   `schemas/error-v2.json`.
2. A printed next command fitted to a terminal has no line wider than it: the program's own
   path and any argument wider than a line are cut into quoted pieces joined by a backslash and
   a newline, a label that leaves the command no room (`Task ID: STATE; next:`) is wrapped
   first, and a continuation is never indented past half the width. The log a detached run that
   ended at once replays is fitted like the run's own. Off a terminal every line stays whole.
3. `af doctor` offers a fix only when running it can clear its check: `af provider detect
   --apply` only where that command would register a login now, an unregistered auth directory
   whose CLI of its kind is on PATH and reports it logged in (a login marker alone, a logged-out
   context or a CLI found only off PATH registers nothing), and for the project row only a login
   of the kind its Worker needs; an install wherever the default link is missing, the directory
   on PATH or not, by this binary's own path when no `af` is on PATH, that path cut into pieces
   its shell joins, in fish's quoting under fish, and `&&` ending the line before the PATH line.
   The elvish PATH line, too long for a line, lists its pieces for `str:join`.
4. The project row's hint names the file `af onboard` stops at by taking its steps in its order:
   the project file, the selected pipeline and the lock parsed as their types, the lock's pins,
   the pipeline's pin, its Worker packages through the registry reader (the pin before the typed
   `reviewer.toml`), then each routed pipeline. A package is found by its validated name, never
   by the pipeline's string as a path, and no path outside `.af/` is named. A file that no
   longer matches its pin gets the `af onboard --refresh-lock` that re-pins it.
