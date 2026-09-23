# `af` TUI — design (rc.7)

Status: accepted 2026-09-23 (bare `af` opens the TUI). Replaces the `af review tui` surface in `crates/af/src/tui.rs`.
Terms follow `CONTEXT.md`; the layout of `.af/` follows `docs/design/config.md` §2.

## 1. What it is

`af` with no subcommand opens a full-screen, vim-native browser over everything af knows about
the place it was started in. It is a **read-first** surface: every pane is a projection of data
the CLI already prints, and every mutation goes through the same path the CLI uses (a subcommand,
or `$EDITOR` on a declared file). The TUI never turns working-tree bytes into execution
authority; that invariant is inherited unchanged from the review TUI.

Non-goals for the first cut: in-TUI editing of TOML, live transcripts, a pipeline DAG editor,
mouse support.

## 2. Entry and scope

| invocation | scope | root of the left bar |
|---|---|---|
| `af` outside a git repository | **user** | `~` — user layer, provider registry, every Task in `$XDG_STATE_HOME/af` |
| `af` inside a repository | **project** | `<toplevel basename>` — project + local layers, `.af/` packages, this repository's Tasks |
| `af --repo DIR` | project | as above, for `DIR` |
| `af` with stdout not a tty | — | prints help, exit 2 (today's behaviour) |

Scope resolution reuses `config::load(repo)`: `toplevel == None` means user scope. The user scope
still shows project Tasks — grouped by repository — because the Store is user-wide; the project
scope filters the same Store by repository identity. `arg_required_else_help` on `Af` goes away;
`Option<Command>::None` dispatches to `tui::launch(scope)`.

`af review tui` is removed with the rest of the `review` namespace. `af task tui` is not added:
the Task pane covers it.

## 3. Layout

```
+--------------------------+-----------------------------------------------------------+
| ~/bs/apkhmv-main/af/afac | PROVIDERS                                          claude-main |
| v afactory   (project)   |                                                           |
|   v providers/           | ID           KIND    STATUS   AUTH        SUBSCRIPTION    |
|     > claude-main        | claude-main  claude  ready    oauth       Max 20x         |
|       codex-main         |                                                           |
|   v workers/             | limit  5h window   [########..........]  41% used  resets in 2h 10m
|       correctness        | limit  7d window   [#############.....]  68% used  resets in 3d 4h |
|       bugs               |                                                           |
|   v pipelines/           | note   weekly limit read from the local /usage screen     |
|       review             |                                                           |
|   v tasks/               |                                                           |
|     v running (1)        |                                                           |
|         task-01J9…  62%  |                                                           |
|     > done (14)          |                                                           |
|                          |                                                           |
+--------------------------+-----------------------------------------------------------+
| NORMAL  providers/claude-main          j/k move  Enter open  R probe  / search  :cmd  ? help |
+--------------------------+-----------------------------------------------------------+
```

Three regions, fixed for the whole session:

- **Left bar** (28 columns, `nvim-tree` style; hidden below 90 columns with `<C-b>` to toggle).
  A folding tree whose depth-1 folders are the four fixed tabs: `providers/`, `workers/`,
  `pipelines/`, `tasks/`. Selecting the root row opens the settings pane for the scope.
- **Main pane**: one renderer per node kind (§5). Scrollable; never wider than the terminal.
- **Status line**: vim mode word, breadcrumb of the selected node, the key legend for the
  focused region. `:` opens a one-line command prompt in the same row; `/` a search prompt.

Glyphs are printable ASCII only (`v`/`>` for folds, `+--`/`'--` for tree branches, `#`/`.` for
bars). This keeps the existing `terminal_data_is_printable_ascii` test meaningful and matches the
CLI's own tree output.

Minimum size 80x24, same refusal message pattern as today.

## 4. Key model

The TUI is a modal, vim-shaped application. Keys are the same in every pane; a pane may add a
few pane-local verbs, listed in its section and in the status line.

**Motion (NORMAL, any region)**

| key | effect |
|---|---|
| `j` `k` | row down / up |
| `h` `l` | in the bar: collapse / expand the folder (or move to parent / first child); in the main pane: scroll horizontally where a table overflows |
| `gg` `G` | first / last row |
| `<C-d>` `<C-u>` | half page |
| `<C-f>` `<C-b>` | full page (`<C-b>` on the bar toggles it instead) |
| `zo` `zc` `za` `zR` `zM` | fold open / close / toggle / all open / all closed (bar) |
| `/` `?` `n` `N` | search forward / backward within the focused region, next / previous |
| `<Tab>` `<C-w>l` `<C-w>h` | move focus bar → main → bar |
| `]]` `[[` | next / previous sibling folder in the bar (providers → workers → …) |
| `Enter` `o` | open the selected node in the main pane |
| `y` | yank the selected id (Task id, plan id, provider id) via OSC 52 |
| `gf` | open the file behind the node in `$EDITOR` (worker prompt, pipeline TOML, config layer); the screen is released and re-entered |
| `R` | refresh the focused pane from disk (providers: run the bounded probe) |
| `?` on the status line focus, `:help` | key help overlay |
| `q` `:q` `ZZ` | quit; `<C-c>` cancels a prompt or a running probe first |

**Command line (`:`)** — a small fixed vocabulary, completed with `<Tab>`:

```
:q                       :help [topic]            :e user|project|local
:task run ID [--confirm-plan PLAN]   :task show ID   :provider setup ID --kind claude|codex
:cd DIR                  :scope user|project
```

Commands are parsed with the same clap definition as the CLI (`Af::try_parse_from(["af", …])`),
so the TUI can never grow a second grammar. A command that would spawn Workers releases the
terminal exactly as the review TUI's `r` does today, runs the ordinary code path, and waits for
Enter before re-entering.

**Prompts**: `INSERT`-like line editing with `<C-a>`/`<C-e>`/`<C-w>`/`<C-u>`, `Esc` cancels.

## 5. Panes

Each pane names the CLI output it mirrors and the loader it reuses. Nothing is re-derived in the
TUI: if the number is not in a CLI `--json` document, the pane does not show it.

### 5.1 Settings (root row)

Mirrors `af config show --origin` and `af config paths`.

```
SETTINGS  project: afactory                                  .af/af.toml, .af/af.local.toml
layer      file                                       state
built-in   (binary)                                   -
system     /etc/af/config.toml                        absent
user       ~/.config/af/config.toml                   present
directory  ~/bs/apkhmv-main/af.toml                   absent
project    .af/af.toml                                present   <- edit with e
local      .af/af.local.toml                          absent

[defaults]
pipeline = "review"                                   .af/af.toml:8
[self]
keep_versions = 3                                     ~/.config/af/config.toml:4
```

User scope shows only the machine-owned layers plus the provider registry path
(`~/.config/af/providers.toml`). `e` opens the highlighted layer file (`config::edit`).

### 5.2 Providers

Mirrors `af provider status`; the left bar lists every entry from `providers::discover`.
The main pane shows the same columns as the CLI table for the selected provider, then one row per
`ProviderLimit` as a bar with `used_percent` and `format_limit`'s reset text, then `detail` as
`note`. Ambient candidates render greyed with the `af provider setup` hint the CLI prints.

`R` runs `start_provider_refresh` (the bounded, possibly charged probe) with a spinner in the bar
row; the existing `finish_provider_refresh` poll keeps working.

### 5.3 Workers

The bar lists Worker packages found in the declared directories, in this order and with the
source folded in as a second-level group when more than one exists:
`.af/workers/` (reviewer Workers), `.af/task-packages/`, `.af/packages/`, `.af/vendor/`.
User scope lists nothing here except catalogs synced under `~/.local/share/af` when present.

Main pane, three sections separated by rules:

1. **Identity** — `name`, `version`, `schema`, source path, lock pin from `af.lock` (or
   `unpinned`), runner (`program` + args as `af.lock` records them), attempt bounds
   (`signature.attempt.tokens`, `wall_ms`), effects and roles.
2. **State** — derived only from Store records for this scope: how many Attempts of this Worker
   are `reserved`, `started`, `settled ok`, `settled failed`, `abandoned`, plus tokens charged
   and wall time across those Attempts. Empty when the Store has no Attempt naming the Worker.
3. **Prompt** — the package's instruction file (`reviewer.md` today; whatever the package
   declares as its prompt) rendered as plain text, scrollable, with `gf` to edit. If a package
   has no prompt file the section says so instead of guessing.

### 5.4 Pipelines

For now the pane is the text `af task explain --tree` prints, produced by
`task_execution::preview::render(…, tree = true)`, verbatim:

```
PIPE  review@1.0.0 (.af/pipelines/review.toml)  [configured]
+-- correctness: slot -> workers/correctness  attempts 1..1
+-- bugs: slot -> workers/bugs  attempts 1..1
'-- verify: call fixture/verify
    '-- gates: checks/test.sh
```

Selecting a pipeline with no captured Task compiles a plan the token-free way `af task plan`
does and renders that; selecting a Task's pipeline from the Tasks pane shows the captured
`ExecutionPlan`. `j`/`k` highlight a row; the status line shows its slot binding and provider.
The ASCII DAG from the review TUI is not carried over; it can return as a `:set dag` view later.

### 5.5 Tasks

The bar groups by state: `running/`, `awaiting approval/`, `done/`, `failed/`; user scope adds
one level for the repository. Row text is `task-id  outcome  progress%` where progress is
`settled stages / stages in graph.order`.

Main pane for one Task, top to bottom:

```
TASK   task-01J9K…  implement: "add --json to af task list"
PLAN   plan-7f3a…   configured           STATE running   started 12:04:31   elapsed 6m 12s
SNAP   snap-9c1…    authority HEAD@f3db3da

PROGRESS   4 / 7 stages
  [ok]  capture            0.4s
  [ok]  correctness        2m 03s   12 480 tok
  [ok]  bugs               1m 41s    9 912 tok
  [..]  implement          2m 30s   running, attempt 2/3
  [  ]  verify
  [  ]  gates
  [  ]  publish

TOKENS   chargeable 22 392   input 18 001   output 3 140   cache read 1 251   reasoning 0
TIME     wall 6m 12s   checks 0.9s   dependency prep 3.1s   verification -

HISTORY  (af task show)
   1 TaskCaptured@1          art-…
   2 TaskPlanCompiled@1      art-…
   …
```

Sources: the event list from `TaskStore::events`, `TaskCompleted@1` totals for tokens
(`TaskTokenUsageV3` fields), `TaskExecutionRecord` `Reserved`/`Started`/`Settled` for per-stage
state, attempt counts and charged tokens, `TaskRuntimeSpanV1` for the TIME row. A running Task
re-reads the Store every second while it is selected; nothing is polled otherwise.

Pane-local verbs: `Enter` on a HISTORY row opens that artifact as pretty JSON; `p` jumps to the
Task's pipeline in the Pipelines pane; `:task run ID` and `:task deliver …` release the terminal
as described in §4.

## 6. Architecture

`crates/af/src/tui.rs` (2.9k lines, review-specific) is replaced by a module tree:

```
crates/af/src/tui/
  mod.rs        launch(scope), event loop, terminal session (kept from today)
  keymap.rs     mode + key sequence parser (gg, zo, ]], <C-w>l), one table, unit-tested
  tree.rs       the left bar: Node { kind, label, children, folded }, fold/search/motion
  scope.rs      user vs project resolution, path roots
  panes/
    settings.rs providers.rs workers.rs pipelines.rs tasks.rs
  paint.rs      paint / paint_spans / Paint palette (kept from today)
```

Rendering stays on plain `crossterm` with the existing paint helpers; a widget library is not
worth a new dependency for five list-and-detail panes. Each pane implements one trait:

```rust
trait Pane {
    fn load(&mut self, scope: &Scope) -> Result<(), String>;   // pure read of disk / Store
    fn rows(&self) -> &[Row];                                   // what the main pane paints
    fn key(&mut self, key: KeyEvent) -> Option<Effect>;         // pane-local verbs only
}
```

`Effect` is the small closed set the event loop knows: `Quit`, `OpenEditor(PathBuf)`,
`RunCommand(Vec<String>)`, `Yank(String)`, `Refresh`. Every loader is a function that already
exists behind a CLI subcommand, called with `json = true` and rendered from the document, so
CLI and TUI cannot disagree.

Tests: keymap sequences, tree folding and search, and one golden render per pane from the
`fixtures/consumers/hub` project at 100x30, compared as text.

## 7. Delivery order

0. Kernel: a review Worker may declare `execute-checks` and then runs in an ephemeral-write
   sandbox with a shell (Claude adapter adds `Bash`; Codex runs `workspace-write`), so the UIX
   reviewer below can build the candidate and drive it in a pseudo-terminal. Until this ships, a
   model reviewer has `Read,Glob,Grep` only and a read-only sandbox.
1. Shell: scope resolution, left bar with the four folders, settings pane, `af` no-arg dispatch,
   `q`/`:q`. Remove `af review tui`.
2. Providers and Pipelines panes (both reuse existing loaders and renderers directly).
3. Tasks pane: list, detail, progress, tokens, time, history; running-Task poll.
4. Workers pane: identity, prompt, then the State section once Attempt records are indexed by
   Worker in the Store.
5. Command line and the run/deliver hand-off; `gf`; yank.

## 7a. Review

Every package is reviewed by `kernel/review-light`: `correctness` and `bugs` read the source, and
`kernel/uix` (Claude, opus 5.5, high) builds `af`, writes its own pseudo-terminal harness under
`target/uix-harness/`, drives the shipped panes key by key at 100x30 and 80x24, and compares each
captured screen with §3-§5 and with the CLI output the pane mirrors. Its findings quote the key
sequence, the captured screen and the expected one; a refused build or shell is a `block`, never a
source-only review.

## 8. Open questions

- Worker "state" needs the Store to answer "which Attempts named this Worker"; today that is a
  scan of every Task's execution records. Cheap for one repository, slow for the user scope. An
  index by `worker` in `tasks.sqlite` is the clean fix and belongs to step 4.
- Which file is a package's prompt: the first cut looks for `reviewer.md`, then any single `*.md`
  beside `worker.toml`; a declared `prompt = "…"` field in `af.worker/1` would remove the guess.
