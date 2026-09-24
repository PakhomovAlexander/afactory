# ADR-0119: Open a read-first browser on bare `af`

Status: accepted, 2026-09-23.

## Context

[`docs/design/tui.md`](../design/tui.md) decides that `af` with no subcommand opens a full-screen,
vim-shaped browser over the place it was started in, and plans it in packages. This record covers
package M1, steps 1 and 2 of §7:

- the shell: scope resolution, the left bar with its four folders, the Settings pane, the bare
  `af` dispatch, and `q` and `:q`;
- the Providers and Pipelines panes.

The legacy review TUI and its terminal dependency were removed earlier (PR #114). Nothing of it is
ported. Four constraints bind the new surface:

- The browser is a projection. A number that no CLI document carries is not shown, and every
  mutation goes through the path the CLI uses.
- Bare `af` on a pipe keeps its contract: help on stderr and exit 2.
- A pane never writes a Store, and it never turns working-tree bytes into execution authority.
- `make check` runs Cargo with `--locked`.

## Decision

### Dispatch and scope

`arg_required_else_help` leaves the top-level command, and `Af` gains `--repo DIR`. With no
subcommand and stdout not a terminal, `af` asks clap for the same refusal it gave before, with the
help on stderr and exit 2. With stdout on a terminal, `tui::launch` runs. Scope comes from
`config::load`: no git toplevel means the user scope and machine-owned layers only, and a toplevel
means the project scope. `--repo DIR` must name a repository.

### Panes

Each pane reads through the function behind a CLI command:

- **Settings** is built from the scope's one `config::load`. It shows the layer table
  `af config paths` prints, with absent directory layers folded into the nearest one, the file
  `af config edit --layer directory` would create. It also shows the effective values with the
  origins `af config show --origin` prints. The user scope adds the Provider registry. `e` edits
  the highlighted layer through `af config edit`.
- **Providers** comes from `providers::discover_with_cancel`, on a thread the event loop polls.
  It prints the `af provider status` header and row, one bar per `ProviderLimit` next to
  `format_limit`'s text, the note, and, for an ambient candidate, the setup line. `status_table_*`,
  `is_ambient_candidate` and `setup_hint` move into `providers` so the CLI and the pane share one
  spelling. `R` runs the bounded usage probe `--usage` runs, and `<C-c>` cancels it.
- **Pipelines** lists `.af/pipelines/*.toml` and every `pipeline.toml` under
  `.af/task-packages/`. A package's main pane is `task_execution::plan_tree_preview` on a
  synthesized Task file, with the package's first accepted kind, the package selected by name
  with no fallback, and wide limits. That function runs the `af task plan` path with a new
  `Presentation::Silent`, into a scratch Store that it discards, from the committed `HEAD`. It
  returns the text `af task explain --tree` prints: `preview::current_marked` beside
  `preview::current`, together with the line of every Worker row and that slot's binding, which
  the status line shows. A review Pipeline names `af review plan --pipeline` instead. The
  compilation runs off the key loop.
- **Workers** and **Tasks** are folders whose pane says which later package brings it.

### Command line

`:` lines go through `Af::try_parse_from`. `:e LAYER` spells `af config edit --layer LAYER`, and
`:help` spells `af help`. `:q`, `:cd DIR` and `:scope user|project` belong to the browser. A parse
error shows clap's own first line on the status line. In this package the browser runs only
`af config edit`, handing the terminal over, and `af help`. Any other command is named and not
run. `gf` hands the terminal to `$EDITOR` through the `open_in_editor` that `af config edit` now
shares.

### Terminal

The session opens `/dev/tty` in raw mode on the alternate screen. The terminal is restored on
drop, on every hand-off, and from a panic hook that runs before the panic message. Raw mode keeps
`VMIN = 0, VTIME = 1`, so a read waits at most a tenth of a second. That tenth is the event loop's
only clock: background work is polled between reads. Painting writes only the rows that changed,
as printable ASCII. Below 80x24 the screen shows one line naming the minimum. Keys and sequences
come from one table in `keymap.rs`.

The terminal is driven through `rustix::termios`, which is already a direct dependency: the only
change is its `termios` feature. The design names `crossterm`. Re-adding it needs `Cargo.lock`
entries whose registry checksums the change could not produce, and `--locked` refuses a lock
without them, so the backend is isolated in `tui/term.rs` and `paint.rs` for a later swap.

## Consequences

`af` at a terminal opens a browser that cannot disagree with the CLI, because it calls what the
CLI calls. The Pipelines pane compiles a real plan, which costs one engine digest and one Snapshot
capture per package opened. It does that off the key loop, into a scratch Store, with no Worker,
Provider or token spent.

Tests pin the machine layers through `config::load_rooted`, and the Providers golden uses a fixed
inventory. The renders are the same on every machine. Their fixture is `fixtures/consumers/hub`,
layered over the pinned packages of `fixtures/task-runtime/pagination`.

No wire contract, schema, fixture or `--json` document changes. `CONTEXT.md` gains no term.
