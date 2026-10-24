- A usage error under `--json` is the one `af/error@1` document `af help json` promises, with
  clap's message, `exit_code` 2 and `cause: usage`, beside clap's own lines on stderr; `--help`
  and `--version` stay what they are. `af/error@1` now has a schema (`schemas/error-v1.json`), and
  a refusal af classifies adds typed fields a script branches on: `cause`, `next_action` and
  `details`. A node past its recovery allowance is `recovery_exhausted` with the node, how often
  it was abandoned and its allowance; a zero `limits.wall_ms` is `task_file_invalid` naming the
  file and field instead of `--timeout-secs`; an unreadable `--goal` file is `input_unreadable`
  and says how to fix it.
- Every next command af prints for a Task carries the `--repo`, `--state` or `--workspace` the
  command was given, as typed: the `next:` lines of `af task show`, `run` and `list`, the
  `state.next_action` of every `--json` document but `af task report`'s, the preview's approve and
  `Details` commands (no more "use the same selectors" line), a refused plan's `--fit` command and
  a recovery refusal's fix. Run as printed, each acts on the same Store; under the default Store
  nothing is added.
- A refused plan is `af/task-selection@3` (version 2 is refused): it adds `error`, `exit_code` and
  `next_action`, the `--fit` command for an infeasible plan. A refused `af task refresh` says why
  on stderr, and under `--json` is one `af/error@1` with `cause: refresh_refused`, exit 4.
- `af review plan --json` carries `next_action`, the command its text prints, or null; its text
  wraps at the terminal's width, prose under its label and a long command continued with a
  backslash between its arguments. `af task list` moves each row's next command to an indented
  line of its own when the table is wider than the terminal.
- Every `--help` page and `af help` topic fits 100 columns, and every example fits 80: a note
  that did not now sits on its own `#` line above its command. `af task --help` names `run
  --from`, and `af task report --help` counts a gate retry's lease as active time.
- The human Markdown of a Document or report escapes a line's first word only where it could
  open a block, so `-foo`, `+1` and `!wow` read as written; the canonical `af.document/2` bytes
  are unchanged.
