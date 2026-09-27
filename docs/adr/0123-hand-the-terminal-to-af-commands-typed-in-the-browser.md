# ADR-0123: Hand the terminal to `af` commands typed in the browser

Status: accepted, 2026-09-27.

## Context

[`docs/design/tui.md`](../design/tui.md) §4 plans the browser's `:` line as the CLI's own
grammar, and §5.5 has a Task run and delivered from it. §7 step 5 delivers this as package M5.
Until now, [ADR-0119](0119-open-a-read-first-browser-on-bare-af.md) ran only `af config edit` and
`af help` from that line. Any other command was named and refused.

Six facts shape the answer:

- The browser is read-first. A mutation must go through the CLI's own path, never a second
  implementation of it inside the browser.
- `selfmgmt::maybe_dispatch` execs the release a project's `af.lock` pins, unless
  `AF_DISPATCHED_FROM` is set. A lock that changes while the browser is open, for example
  after `af onboard --refresh-lock`, would otherwise send a later command to another release.
- The browser's session runs in raw mode, with signals off, on the alternate screen. A child
  that inherits the browser's process group shares its `<C-c>`. With the terminal back in
  cooked mode, `SIGINT` would reach both of them, and the browser would die without restoring
  anything.
- On macOS, `std::env::current_exe` canonicalizes the path it returns at every call. After
  `af self` switches the link to another release, a later call names that release.
- Delivery accepts only a verified Task, meaning a finished Task whose result's acceptance is
  `satisfied`. It also asks for the Task ID again, as `--confirm`, as its explicit confirmation
  ([ADR-0031](0031-deliver-verified-tasks-to-new-local-worktrees.md)). The Tasks pane already
  groups a finished Task as `done` exactly when its acceptance is `satisfied`.
- The clap definition `cli::Af::command()` lists every subcommand at every level, and marks the
  hidden ones.

## Options

- **Call the command's code in-process**, the way `:e` calls `config::edit`. This was rejected.
  Every command would need a variant that returns instead of exiting and that does not print
  onto the painted screen. A command's panic or `exit` would also take the browser down with
  it.
- **Spawn `af` from `PATH`.** This was rejected, because it may be another release than the
  browser's own.
- **Spawn the running executable, resolved once when the browser starts, in its own process
  group that owns the terminal's foreground, and wait for Enter after it.** This was chosen.

## Decision

### Routing

A `:` line is split with `shell_words`:

- `:q`, `:cd`, `:scope`, `:e` and `:help` keep their behaviour. `:e` and a typed
  `config edit` edit in-process, and `:help` and a typed `help` show help in the main pane.
- Any other line goes to `cli::Af::try_parse_from(["af", …])`. A parse error is clap's first
  line on the status line, as before.
- A line that parses without a subcommand, such as `:--repo DIR`, would open a second browser.
  It is refused on the status line, which points at `:cd`.
- Every other parsed line is handed off.

### The hand-off

The child is the running `af` executable, with exactly the parsed words as its arguments. The
path is resolved once, when the browser launches. The child's working directory is the scope's
root: the repository toplevel in a project scope, and the home directory in the user scope.
The child inherits the browser's environment, plus `AF_DISPATCHED_FROM`. That variable keeps
the value the browser itself was dispatched with, and otherwise holds this release's version.
Self-management therefore never sends the child to another release partway through a session.

The terminal is handed over in this order:

1. `Host::release` leaves the alternate screen, shows the cursor, and goes from raw mode
   straight to cooked mode without `ISIG`, in one `tcsetattr`: until the child's group owns the
   foreground, `<C-c>` is a byte, not a signal to the browser's group. A release that fails
   (the leave sequence cannot be written, or the terminal refuses the mode) is an error, and
   the browser re-enters the screen before it reports it; nothing runs on a terminal that did
   not leave. Closing the browser restores the saved mode best effort.
2. `Host::run` reads the terminal's foreground group first; a terminal that will not say is
   refused, because without the browser's own group nothing keeps `<C-c>` from it. It spawns
   the child with inherited stdin, stdout and stderr, in a new process group.
   It gives that group the terminal's foreground with `tcsetpgrp`, then sends the group
   `SIGCONT`. Only then does it turn `ISIG` on, with `SIGTTOU` blocked because the browser is
   now in the background, so `<C-c>` reaches the command from its first moment on the
   foreground. A child that touched the terminal before its group owned the foreground was
   stopped by `SIGTTIN` or `SIGTTOU`, and continues now. `<C-c>`, `<C-\>` and `<C-z>` reach
   the child's group, never the browser. A command may end before its group takes the
   foreground, and then `tcsetpgrp` fails because the group is gone. The browser, which never
   gave the foreground away, reaps that child and reports its own exit, not a failed hand-off;
   only a child still running is killed as one.
3. The browser waits with `waitpid(WUNTRACED)`. A child stopped by `<C-z>` is continued at once,
   because the browser has no job control and nothing else would resume it.
4. When the child ends, the browser turns `ISIG` off and only then takes the foreground back,
   both with `SIGTTOU` blocked. If the terminal refuses to turn `ISIG` off, the browser never
   takes it back: it ends without touching the terminal, which the shell reclaims as it does
   from any finished job, and `af` reports why. `Host::pause` reads Enter in raw mode and returns to cooked mode
   without `ISIG`, and `Host::reenter` goes raw. So from the release to the re-entry the browser
   never owns the foreground while `ISIG` is on; every change of mode is one `tcsetattr`, not a
   pass through the saved mode, which is restored only when the browser closes.
5. `Host::pause` prints one line on the released screen, `af LINE: exit N -- Enter returns to
   the browser` (`killed by SIGINT` for a child a signal ended). It starts on a fresh line
   whatever the command left: a line's width of spaces wraps only when the cursor was mid-line,
   and a carriage return follows, so output that ended with a newline gets no blank line. The browser discards any unread input, then reads the terminal in raw mode until
   Enter.
6. `Host::reenter` enters the alternate screen again. Keys the browser had already read in the
   same read as the command line were typed before the command ran; they are dropped, and the
   key decoder starts clean, so nothing typed ahead is replayed in the browser.

A child that cannot be spawned is an error on the status line. The terminal is re-entered
without the wait. The process-group calls are `rustix::termios` and `nix` calls that are
already dependencies of `af`, with no `unsafe` code.

### Editors

`gf` and `:e` hand the terminal to `$EDITOR` the same way: `Host::run`, the editor in its own
process group owning the foreground, so `<C-c>` in the editor reaches the editor and never the
browser. `$EDITOR` is read once when the browser starts. There is no Enter wait, since the
editor's own screen is what the user left, and an editor that exits non-zero or is killed is an
error on the status line. `af config edit` from the shell runs the editor as before.

### What is read again

After a hand-off the browser reads again everything the child may have changed:

- the scope and the settings, as after an editor;
- the Tasks, Workers and Providers panes, whether opened or not, as far as each has read
  (below);
- the opened pane and the pane behind the bar's selection.

Providers are discovered again without the charged probe, which only `R` asks for. The Workers
pane reads nothing until it is first opened, because its STATE scans every Task (ADR-0122). Once
it has read, a hand-off reads it again whether it is opened or not. Until then there is nothing
stale to read again, and its first open reads what the child left. What is opened stays opened, and the bar keeps its
selection where the selected node still exists. A reload of the scope keeps the home directory,
the Provider registry and the Task state root, because they come from the process environment
and a child cannot change them. The status line then says `af LINE: exit N`. A non-zero exit,
or a signal, is shown as an error.

### Completion

`<Tab>` completes the last word of the `:` line from the clap definition, the earlier words
split as the line will parse (a quoted value stays one word). At the first word it
offers the browser's own verbs and the top-level subcommands. At any later level it offers the
visible subcommands of the command the earlier words name, and after `help` it offers the same
subcommands. The completion never uses a hand-written list of commands. When the command takes
a Task ID as its first positional argument (`task_id`: `task run`, `show`, `explain`, `deliver`
and the other `task` verbs declaring one), the ID completes from the Task IDs the Tasks pane
lists for this scope, and only from a project scope's own Store (see Prefills). Options the
command declares may come before the ID, with their values (`task show --repo . ID`); an
option still waiting for its value completes nothing. After a flag, or after a word that names no subcommand, nothing is
guessed. One match is filled in, followed by a space. Several matches are listed on the status
line.

### Prefills

The Tasks pane gains two verbs. They work on the bar's selected Task when the bar has focus and
on the opened Task when the main pane has focus, and the opened pane's legend lists them. With
the bar on a row that is no Task, they act on nothing and say so; they never fall through to a
Task opened in the main pane.

- `r` opens the `:` line holding `task run ID --confirm-plan PLAN`. PLAN is the current
  recorded plan from the same inspection read the pane shows.
- `D` on a verified (`done`) Task opens the line holding
  `task deliver ID --branch af/ID --worktree ../ID --confirm` and a space, and the Task ID is
  left to type.
- `D` on a running, awaiting or failed Task opens nothing. The status line says that only a
  verified Task can be delivered, and why this one is not.

In the user scope the Tasks pane lists every repository's Store, but a handed-off command runs
from the home directory and names a Task only by its ID. The pane knows a Store's repository
only by its opaque state-directory name (ADR-0035), so no line it could prefill would reach that
Task. There `r` and `D` open nothing, and the status line says to `:cd` into the Task's
repository and press the key there; completion offers no Task ID.

Neither verb submits. The user reads the line and presses Enter, or `Esc`. The `--confirm`
value stays untyped because ADR-0031 makes delivery's Task-ID confirmation a deliberate human
act. A prefilled confirmation would reduce that act to one keypress, the same keypress as
reviewing the line. Words that the shell would split are quoted, so the line parses back to the
same arguments.

## Consequences

- A command typed in the browser prints exactly what it prints from the shell. A
  pseudo-terminal test compares `:task show ID` with `af task show ID`, and a failing command's
  stderr with the shell's. A third pseudo-terminal test shows that `<C-c>` ends the child, not
  the browser.
- The browser's `Host` gains `run` and `pause`. A recording `Host` proves the order: release,
  run, pause, re-entry, then the re-read panes.
- No moment of the hand-off lets `<C-c>` reach the browser: `ISIG` is off from the release
  until the command's group owns the foreground.
- A command whose line is wider than the terminal wraps its exit line.
- No wire contract, schema, fixture or `--json` document changes. `CONTEXT.md` gains no term.
