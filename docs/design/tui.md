# `af` TUI — design (rc.7)

Status: accepted 2026-09-23 (bare `af` opens the TUI). Replaces the `af review tui` surface in `crates/af/src/tui.rs`.
Terms follow `CONTEXT.md`; the layout of `.af/` follows `docs/design/config.md` §2.

## 1. What it is

`af` with no subcommand opens a full-screen, vim-native browser over everything af knows about
the place it was started in. It is a **read-first** surface: every pane is a projection of data
the CLI already prints, and every mutation goes through the same path the CLI uses (a subcommand,
or `$EDITOR` on a declared file). The TUI never turns working-tree bytes into execution
authority; that invariant is kept from the removed review TUI.

Non-goals for the first cut: in-TUI editing of TOML, live transcripts, a pipeline DAG editor,
mouse support.

## 2. Entry and scope

Bare `af` targets its first browser status-line frame strictly below 200 ms from process launch
in a 100x30 pseudo-terminal, including a registry of 20 large Task Stores (ADR-0262).

| invocation | scope | root of the left bar |
|---|---|---|
| `af` outside a git repository | **user** | `~` — user layer, provider registry, every Task in `$XDG_STATE_HOME/af` |
| `af` inside a repository | **project** | the `[project] name` it declares, else `<toplevel basename>` — project + local layers, `.af/` packages at `HEAD`, the Project's Tasks across every worktree |
| `af --repo DIR` | project | as above, for `DIR` |
| `af` with stdout not a tty | — | prints help, exit 2 (today's behaviour) |

The project scope's name is the `[project] name` the `.af/af.toml` at `HEAD` declares, its first
line, read through git (one `git show` of that blob) on a thread like the Stores (ADR-0240,
never on the key loop or before the first frame): the load records the read, the first poll
after the first frame starts it, and `R` at the root starts it again; until it answers the name
shown stands, the toplevel's basename at first. The working tree's copy of the file, a
`.af/af.local.toml` and an `AF_PROJECT__NAME` change the effective configuration, never the
name, so every checkout of the Project is named alike and an edit names it once committed;
without one the toplevel's basename stands. A name the read finds changed renames the bar's
root and every pane's title when it answers, nothing read again (ADR-0261).

Scope resolution reuses `config::load(repo)`: `toplevel == None` means user scope. A toplevel is
an ancestor whose `.git` git itself would open (a directory holding `HEAD`, or a linked
worktree's `gitdir:` file naming one), never a mere path called `.git`. A reload that finds the
place has become another scope enters that scope the way `:cd` does, dropping the old one's
entries. The user scope
still shows project Tasks — grouped by repository — because the Store is user-wide; the project
scope filters the same Store by repository identity. `arg_required_else_help` on `Af` goes away;
`Option<Command>::None` dispatches to `tui::launch(scope)`.

`launch` enters the terminal and paints the splash (`splash.rs`, `brand/README.md`) before it
reads anything, a few milliseconds after `af` starts. The scope resolves and every pane loads on
a thread of their own; the splash animates while it waits on that load a step at a time and ends
the moment the load arrives, so the browser is never later than it would be without one. That
load reads no Store: a pane that lists Stores (Tasks, Campaigns and, in a project scope, Workers)
starts their read on a thread of its own and the event loop collects each Store as it is read,
after the first frame, since the loop paints before it polls. The splash therefore waits for the
scope only, whatever Stores the machine holds: a registry of eighty Stores and four gigabytes of
event logs once kept it up for forty seconds
([ADR-0240](../adr/0240-open-the-browser-before-any-store-is-read.md)).
What it still waits for is small: the scope's files, the Store registry (one file), and the
Pipelines pane's read of `HEAD` through git, milliseconds on a local repository.
`q` or `<C-c>` on the splash quits at once; any other key typed meanwhile is the browser's first
input. A scope error (`--repo` outside a repository) leaves the terminal before it is printed.

`af review tui` no longer exists (PR #114). `af task tui` is not added: the Task pane covers it.

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
|       bugs               | note   weekly limit read from the local /usage screen     |
|   v pipelines/           |                                                           |
|       review             | <m> to show models                                        |
|   v tasks/               |                                                           |
|     v running (1)        |                                                           |
|         task-01J9…  62%  |                                                           |
|     > done (14)          |                                                           |
|                          |                                                           |
+--------------------------+-----------------------------------------------------------+
| NORMAL  providers/claude-main          j/k move  Enter open  / search  :cmd  ? help          |
+--------------------------+-----------------------------------------------------------+
```

Three regions, fixed for the whole session:

- **Left bar** (29 columns, `nvim-tree` style; hidden below 90 columns with `<C-b>` to toggle).
  A folding tree whose depth-1 folders are the five fixed tabs: `providers/`, `workers/`,
  `pipelines/`, `tasks/`, `campaigns/`. Selecting the root row opens the settings pane for the
  scope.
- **Main pane**: one renderer per node kind (§5). Scrollable; never wider than the terminal.
- **Status line**: vim mode word, breadcrumb of the selected node, the key legend for the
  focused region or the highlighted row's binding or the last message. At a narrow width the
  breadcrumb shrinks to the mode word, then goes, before the right-hand text is cut. `:` opens a one-line command prompt in the same row; `/` a search prompt.
  Once no key came for four seconds, the legend takes turns, four seconds each, with
  `? help - : run a command - q quit`.

Glyphs are printable ASCII only (`v`/`>` for folds, `+--`/`'--` for tree branches, `#`/`.` for
bars). Colour follows `brand/README.md`: the terminal's own foreground and ground everywhere,
reverse video for the cursor row, and colour only as a fill with ink on it, never as text. The
status line is blue, and pink while it carries an error. State chips put the brand's triad on the
words that carry a state: blue for running, green for passed, pink for failed or awaiting a
person, on the Task header's STATE, the stage marks, the progress at the end of a Task's bar row,
a Provider's STATUS, the Workers pane's Attempt counts and the `error` that starts an error row.
A Provider's limit bars are the one place the terminal gets a glyph that is not the frame's
byte: in colour each cell of a bar is U+2587 LOWER SEVEN EIGHTHS BLOCK in the bar's colour on
the terminal's own ground, so two bars one under the other keep a hairline between them; the
frame holds `#` and `.`, and without colour that is what is written.
`COLORTERM=truecolor` takes the brand's values, other terminals black on their own blue, green
and red, and `NO_COLOR` attributes only. This keeps the existing
`terminal_data_is_printable_ascii` test meaningful and matches the CLI's own tree output.

Minimum size 80x24; below it the screen shows one line naming the minimum.

## 4. Key model

The TUI is a modal, vim-shaped application. Keys are the same in every pane; a pane may add a
few pane-local verbs, listed in its section and in the status line.

### Motion (NORMAL, any region)

| key | effect |
|---|---|
| `j` `k` | row down / up |
| `h` `l` | in the bar: collapse / expand the folder (or move to parent / first child); in the main pane: scroll horizontally where a table overflows |
| `gg` `G` | first / last row |
| `<C-d>` `<C-u>` | half page |
| `<C-f>` `<C-b>` | full page (`<C-b>` on the bar toggles it instead) |
| `zo` `zc` `za` `zR` `zM` | fold open / close / toggle / all open / all closed (bar) |
| `/` `n` `N` | search forward within the focused region, next / previous match |
| `<Tab>` `<C-w>l` `<C-w>h` | move focus bar → main → bar |
| `]]` `[[` | next / previous sibling folder in the bar (providers → workers → …) |
| `Enter` `o` | open the selected node in the main pane |
| `y` | yank the selected id (Task id, plan id, provider id) via OSC 52 |
| `gf` | open the file behind the node in `$EDITOR` (worker prompt, pipeline TOML, config layer); the screen is released and re-entered, and the panes the edit could have changed read again |
| `R` | refresh the pane under the focus from disk: the bar's selected node's pane when the bar has focus, the opened pane otherwise (providers: discover and probe again) |
| `?`, `:help` | help overlay: the panes, `: <af command>`, then the keys; `?`, `q` or `Esc` closes it |
| `q` `:q` `ZZ` | quit; `<C-c>` cancels a prompt or a running probe first |

An upper-case letter with no binding of its own is its lower-case key, so Caps Lock changes
nothing: `Q` quits, `J` and `K` move. `G`, `N`, `R` and `Z` keep their own, and so does `D` in the
Tasks pane, where it prefills `task deliver`; to every other pane `D` is `d`, so `D` on a Provider
removes it as `d` does. The key completing a sequence folds the same way: `zO` opens a fold and
`gF` opens the file, while `ZZ`, `zR` and `zM` keep their case.

**Command line (`:`)** — the browser's own verbs, and any `af` command line:

```
:q                       :help [topic]            :e user|directory|project|local
:cd DIR                  :scope user|project      :state DIR
:task run ID --confirm-plan PLAN   :task show ID   :provider setup ID --kind claude|codex   ...
```

Commands are parsed with the same clap definition as the CLI (`Af::try_parse_from(["af", …])`),
so the TUI can never grow a second grammar. `:q`, `:cd`, `:scope`, `:state`, `:e` and `:help`
belong to the browser. `:state DIR` lists the Task Store `DIR` in `tasks/` for the rest of the
session (§5.5); it registers nothing. A line without a subcommand (`:--repo DIR`) would open a second browser and is refused
on the status line. Any other line runs as a child of the running `af` executable, with exactly
the parsed words as its arguments (ADR-0123). The child runs in the scope's root (the repository
toplevel, or `~`) and inherits the browser's environment. It carries `AF_DISPATCHED_FROM`, so
self-management never sends it to another release partway through a session.

The browser releases the terminal first. The child's process group then owns the terminal's
foreground, so `<C-c>` stops the child and never the browser. When the child ends, the released
screen shows one line, `af LINE: exit N -- Enter returns to the browser` (or the signal that
ended it), and waits for Enter. Then the browser re-enters and reads again the scope, the
settings and the Tasks, Campaigns, Workers and Providers panes (Providers with the usage probe, since
the command may have spent a window). It
keeps what is opened and the bar's selection. The status line says `af LINE: exit N`.

`<Tab>` completes subcommand names at every level from `Af::command()` itself, alongside the
browser's verbs. A Task ID argument completes from the Task IDs the Tasks pane lists for this
scope. Several matches are listed on the status line.

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

Mirrors `af provider status --usage`; the left bar lists every entry from `providers::discover`.
The folder's title names the scope as every pane's does, `PROVIDERS  project: hub  (af provider
status)`; an opened Provider's names the Provider, `PROVIDERS  claude-main`.
The main pane shows the same columns as the CLI table for the selected provider (for every
provider on the folder, a blank row between two of them), then one row per
`ProviderLimit` as a bar of twenty cells with `used_percent` and when the window resets (or who
does not report that), then `detail` as `note`. The model catalogs (`af provider models`, ADR-0148)
stay off screen until asked for: the pane's last row says `<m> to show models`, and `m` shows
every listed Provider's `models` section, one blank row under its rows, one row per exact ID of
that Provider's own catalog, until `m` hides them again.
A Claude login's windows are the ones its `/usage` screen shows: the five-hour session, then the
weekly ones. The window names are padded to the longest on the page, so every bar starts in one
column. The bar's used cells are drawn in the brand's colour for how much is used, green
under half, blue from half and pink from four fifths, the rest as a grey track, each cell the
block glyph of §3 so the bars keep a hairline between them; without colour they are `#` and
`.`. The quota windows are always on screen: the
pane discovers first (one bounded authentication check per context, and the catalogs), hands
the rows over, and then runs the bounded usage probe `--usage` runs (a Provider process or an
app-server round trip per login, no tokens, a couple of seconds) on its own; an authenticated
login's row says `reading the quota windows` meanwhile, and one that exposes no window says so.
Ambient candidates render greyed. Under one, the pane says how it got there and how to
make it go: `ambient (CODEX_HOME): found through CODEX_HOME, not in the registry` when the
browser's environment sets the variable, else `ambient (~/.codex): the codex CLI's default
directory, not in the registry`; then `register it: af provider add codex-main --kind codex`
with the first free id, `unsetting CODEX_HOME lists ~/.codex instead` when the variable is set
(the row is listed while the CLI is on `PATH` and its directory is not registered, so registering
it is what makes it go), and the `af provider setup` hint the CLI prints (ADR-0141). A default
context whose login a registered Provider of its kind already holds is not listed at all
(ADR-0260), as a logged-out one is not: the login is what the status answer names, for Claude
the account and the organization its `auth status` reports, and both must match, since one
account in two organizations is two logins; the Codex answer names no account, so a Codex default
context is compared as a path, as before. The login never leaves discovery: the row and the login
it answered with travel together only inside `discover_with_cancel`, and `ProviderStatus`, the
JSON status document and the browser carry no account label. A context left out is not a
sibling: the logged-in-sibling notes of a logged-out Provider are written after the drop, so no
note points at a row that is not listed. The names-only scan of ADR-0206 still names the
directory (`found but not registered: ~/.claude (not probed) → af provider detect`), and `af
provider detect` still lists it. Everything ADR-0141 keeps, stays: a default context with a
login of its own, one whose kind has no registered Provider, and one in any other state are
listed as before.

`d` on a registered Provider, on the bar or in its opened pane, fills the `:` line with
`provider remove ID` and does not submit it; Enter runs it, handing the terminal over as
described in §4 (ADR-0136). `d` on an ambient candidate says on the status line that there is no
registry entry to remove, and the `af provider add` command that adds it. After the command the pane discovers again, and an opened Provider it
no longer lists gives way to the folder.

`R` discovers and probes again, as a command's return does (a probe cancelled with `<C-c>` is
forgotten: the last complete inventory and its windows stay, and `R` reads again), with a
spinner in the bar row, polled from the event loop without blocking key handling.

### 5.3 Workers

The bar lists Worker packages found in the declared directories, in this order and with the
source folded in as a second-level group when more than one exists:
`.af/workers/` (reviewer Workers), `.af/task-packages/`, `.af/packages/`, `.af/vendor/`.
The user scope lists no Worker, and synced catalogs are not read.
A project scope's folder lists its Workers from the start: the load records the read and the
first poll after the first frame starts it, opened or not, as the other folders load
(ADR-0261); the user scope lists none. Every read (`HEAD`'s declarations through git, and every
Task Store's Attempts for STATE), the first and each one after, runs on a thread behind a
spinner on `workers/`, never on the key loop.

Main pane, three sections separated by rules:

1. **Identity** — `name`, `version`, `schema`, source path, lock pin from `af.lock` (or
   `unpinned`), runner (`program` + args as `af.lock` records them), attempt bounds
   (`signature.attempt.tokens`, `wall_ms`), effects and roles.
2. **State** — derived only from Store records for this scope, this checkout's own Store and
   the default Store of every other checkout of the Project the Tasks pane discovers (§5.5):
   how many Attempts of this Worker are `reserved`, `started`, `settled ok`, `settled failed`,
   `abandoned`, plus tokens charged and wall time across those Attempts. Empty when no Store
   has an Attempt naming the Worker; a discovery that fails is a warning, and the own Store
   still counts.
3. **Prompt** — the file the kernel sends: `reviewer.md` for a reviewer Worker,
   `instructions.md` for a Task Worker package (§8), rendered as plain text, scrollable, with
   `gf` to edit. If a package has no prompt file the section says so instead of guessing.

The folder's listing in the main pane is one row per Worker under its source; `Enter` on one
opens it, as the bar does, and the bar's cursor follows.

### 5.4 Pipelines

The folder's title names the scope as every pane's does (`PIPELINES  project: hub`), and its
listing in the main pane is one row per Pipeline; `Enter` on one opens it, as the bar does, and
the bar's cursor follows.

For now the pane is the text `af task explain --tree` prints, produced by
`task_execution::preview::render(…, tree = true)`, verbatim:

```
PIPE  review@1.0.0 (.af/pipelines/review.toml)  [configured]
+-- correctness: slot -> workers/correctness  attempts 1..1
+-- bugs: slot -> workers/bugs  attempts 1..1
'-- verify: call fixture/verify
    '-- gates: checks/test.sh
```

The bar lists what `HEAD` commits under `.af/pipelines/` and `.af/task-packages/`, because that
is the authority a plan compiles; every declaration is read from the one commit resolved at
load, a preview compiles that exact commit (never a `HEAD` that moved meanwhile) and reads the
entries again first when `HEAD` has moved, and a result for a commit the pane no longer shows
is dropped. A working-tree file that differs from `HEAD` in bytes, mode or existence, as `git
diff` judges it, carries `*` in its bar label (selected or not) and its pane says so, while `gf` still opens the working-tree
file, and the pane reads again after an editor hand-off. Git runs with a cleared environment,
so an inherited `GIT_DIR` cannot point it at another repository. A read of `HEAD` that fails is
shown as an error above the last good entries of the same repository, on the folder and on an
opened entry, and nothing is compiled from them; a scope change drops them; only an unborn
`HEAD` lists nothing. A package file is previewed only when the committed catalog pins its name
at that file; a file the catalog pins elsewhere, or not at all, says so and shows its contract
instead of another file's plan. Selecting a pipeline package with
no captured Task compiles a plan the token-free way `af task plan` does and renders that text; the two renders agree line for line except the identity lines
(`PLAN`, `TIME`, `--confirm-plan`), which carry a fresh plan identity and deadline on every
compilation. While that plan compiles, and when the kernel refuses it (required facts, public inputs
or a Provider the preview Task cannot supply), the pane draws the committed declaration as the
same kind of tree: `attempts N at most, M in parallel`, then one row per node in declared order
(`implement  worker implementer -> default/implementer  attempts 1..1`, `check  checks: kernel`,
`evaluate  verify evaluator -> ...  when check=passed`), a `call` expanded into the package the
catalog pins by that name, as committed, with the call's slot bindings applied the way the
kernel applies them (a bound slot is the caller's Worker, role and bounds); the contract follows
the tree and the refusal, when there is one, comes last. A review Pipeline under
`.af/pipelines/` is planned against a diff selector the browser does not hold, so it is always
drawn from its declaration: the Subject, the convergence policy, the budgets and the checks,
then one row per node (`correctness  reviewer -> correctness  after gate  demands required`,
`gather  <- correctness, bugs`), and the `af review plan` command that plans it against a diff.
No pane prints a declaration's TOML; `gf` opens the file. Selecting a Task's pipeline from the
Tasks pane shows the captured `ExecutionPlan`. `j`/`k` highlight a row; the status line shows
its slot binding and provider on a kernel plan, and the slot's declared Worker, role and bounds
(or a reviewer's package or runner) on a declared tree. The ASCII DAG from the review TUI is
not carried over; it can return as a `:set dag` view later.

### 5.5 Tasks

The bar groups by state: `running/`, `awaiting approval/`, `done/`, `failed/`; user scope adds
one level for the repository. Row text is `task-id  outcome  progress%` where progress is
`settled stages / stages in graph.order`.

The folder's main pane opens on the attention view, `NEEDS YOU  (af task list --attention)`,
above the Stores: the groups `af task list --attention` prints, read from each listed Task's
`state` alone (`docs/tasks.md`, "Which Tasks need you"). Each group that waits for a person and
holds a Task is a heading with its count, then per Task its bar id and state and, on the line
below, `next:` and its next command, broken between its arguments to fit the pane. A Task of a
Store reached only by naming it gets `--state DIR` in an `af task` command, as `r` and `D` add
it; a Task of a user-scope repository keeps its command, and one muted line says to run it in
that repository. Then `Running (N): IDS` names the running Tasks and `Nothing to do (N)` counts
the rest; with nothing waiting it reads `No Task needs you now.` A scope whose Stores list no
Task shows no attention view. A collected Task is not listed by the pane, so it is not counted.

`f` filters the pane to the running Tasks. The bar lists under `tasks/` the running Tasks of
every Store in one list, the one started last first, no group between; the folder lists one
row per Task, `task-id  progress%  elapsed  tokens  where`, where `where` is the bar group the
Task's Store stands in (a checkout's label, an explicit Store's path) or `this checkout`, and
the status line gives the Store's heading for the highlighted row. A running Task has no
outcome yet, so none is printed; a duration of a day or more prints as `6d 02h`, here as in
every duration the pane prints. The Stores that cannot be read follow the list in the folder,
each with its refusal, and are one muted `! N Stores unreadable` row in the bar, so an empty
list never stands for a Store that was not read; with none running the folder says `No Task is
running.` The filter is the pane's, kept across reads and `R`, and taken from the bar under
`tasks/` whatever the main pane shows; `f` again lists every Task grouped by state as above. An
opened Task the filtered bar no longer lists falls back to the folder, as any unlisted one
does; the attention view is left out while the filter is on. `Enter` on a Task row of the
folder, in the attention view or under a Store, filtered or not, opens the Task, as the bar
does: the same opening, one inspection of that Task on the key loop (one `af task show`), as
ADR-0240 has it.

The project scope lists the Project's Tasks, not this directory's alone (ADR-0158: a Project
is one repository across its worktrees and clones, and every checkout af worked in is a
Workspace of it). Beside this checkout's own Store, headed `this checkout (Workspace LABEL)`
when it is registered, the pane lists the default Store of every other checkout of the Project
that holds Tasks (a Store af wrote, its `events.sqlite` there; a directory nothing wrote is not
listed, and a Store whose database cannot be told is, with its refusal): the registered
Workspaces, from the Project Store read under its shared lock, by label, then the worktrees
`git worktree list` names that no Workspace covers, by directory name. Each is a bar level named by that label (`hub-wt-branch/ (3)`), and its heading is
`Workspace LABEL  PATH` or `worktree NAME (not a Workspace)  PATH`, the opaque Store path last.
A Workspace's Tasks are its checkout's default Store's, listed under its label whether or not
the Workspace also names a `--state` Store (a clone no `git worktree list` names is found this
way alone); a Store it names under another path, wherever it is kept, is the registry's, listed
below with the Workspaces noted beside it. This checkout's Workspace is the one registered at
its path (ADR-0158), whatever Store it names; a Workspace elsewhere naming this checkout's
default Store is noted on it, `this checkout (Workspace hub; the Store of Workspace x)`.
With no other checkout the own Store stands ungrouped as before. The read discovers them, never
the load (ADR-0240), on every read, so a worktree added or a Task run meanwhile shows at the
next `R`; a Project Store that cannot be read is a warning above the Stores, and the worktrees
are still listed. A checkout that is no repository, has no commit or is a shallow clone has no
Project to list.

Then come every Task Store the Store registry (`$XDG_STATE_HOME/af/stores.toml`) records, the
Stores `af task … --state DIR` made, and the ones `:state DIR` added for the session, each
once, never one a checkout above already lists. Each is one more bar level named by the end of
its path, and its main-pane heading is its path with `(--state, in stores.toml)` or `(:state,
this session)`, and the Workspaces that name it when the Project has such: `(--state, in
stores.toml; Workspaces wt-346, wt-363)`. `r` and `D` on the Tasks of any Store but this
checkout's own add `--state DIR` to the line. A registry this release cannot read is a warning
above the Stores; the scope's own Stores are still listed.

Every read of the Stores runs on a thread, the one a load, `R` or `:state DIR` records as much
as the live read of an opened running Task, each started at the first poll after it, which the
event loop reaches after the frame that follows, and in the user scope the walk of the state root for
every repository's Store is that thread's first step, never the load's: the bar shows `tasks/`
with a spinner while it runs, and
each Store is listed the moment it is read, in the order above, a refused one with its refusal,
so the folder fills while the rest are still read and no read of a Store ever holds the key loop
or the splash (opening one Task still inspects that Task on the key loop, as before). Nothing
interrupts the read, since it holds no key; `q` quits without waiting for it. The opened Task's
detail arrives with its own Store, so its bar row and its pane never disagree; a Task whose
Store refuses when opened, or whose HISTORY artifact cannot be given back, falls back to its
folder on that key and restarts a read in flight, so nothing read before the refusal lists it
again: the status line carries the HISTORY refusal's cause, and for the opening the message of
an entry found gone, its cause on the Store's row in the folder. An opened Task or Worker that a read no
longer lists falls back to its folder with the same message as when opening finds it gone;
never one whose Store is merely unread: a target list that changed within the scope (a Store
registered meanwhile, `:state DIR`) keeps every Store listed until the read answers for it, and
a Store of a target no longer named until the read is done, so the opened Task stays opened
while its Store is read again; the Stores stand in the order of the targets as they are now.
In the user scope the read discovers the repositories' Stores, and a walk that fails lists the
registered and session Stores alone once the read is done, with the cause in the folder. A Task
opened while an explicit read is in flight has its detail read again when its Store answers,
since a read started before carries none for it, or one from an earlier opening of the same
Task. Another scope lists nothing of the old.

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

Pane-local verbs: `f` toggles the running filter above; `Enter` on a HISTORY row opens that
artifact as pretty JSON; `p` jumps to the Task's pipeline in the Pipelines pane. `r` on a Task, on the bar or in its opened pane, fills the
`:` line with `task run ID --confirm-plan PLAN`, where PLAN is the Task's current recorded plan.
`D` on a verified Task fills it with `task deliver ID --branch af/ID --worktree ../ID --confirm`
and a space, and leaves the Task ID for the user to type, which keeps delivery's explicit confirmation. `D` on
a Task that is not verified says why on the status line. Neither verb submits the line. Enter
runs it, handing the terminal over as described in §4.

### 5.6 Campaigns

Mirrors `af review campaigns --format json` and `af review report --format json`, read by the
functions behind them (`enumerate_campaigns`, `read_campaign_view`, the report view), silently:
a warning the CLI prints on stderr would land on the screen. The sources are the default Campaign root, each
Store a plain `af review run` keeps under `review/local/`, and every `review` Store and
`review_root` directory the Store registry records. A Store reached twice is listed once.

```
CAMPAIGNS  project: hub

~/.local/state/af/review/campaigns
  pr-42                 round 1.1  open  -
      spend 40000 tok  wall 4.2 s  last write 2026-10-23 06:12Z
      nodes  correctness  bugs
```

The bar lists each Campaign by its label, with its state as a chip: `open` (blue) while its newest
Round has no Run Report, else its verdict: `pass` (green), `fail` or `incomplete` (pink); `new`
before its first Round. The folder shows each Store's path, then per Campaign its newest Round,
state and verdict, its spend (the Tasks' cumulative charge and the Rounds' wall-clock, as `af
review report` counts them) and its Store's last write in UTC, and the reviewer nodes of the newest
Round as chips of their latest Attempt's outcome (`running`, `selected`, `failed`): only that
Round's Attempts, so a Round that has none yet shows no chips, never the previous Round's, and
the latest Attempt is the one that started last, not the last one listed. Every read runs on a
thread, started at the first poll after the load or `R` that records it, each source listed as
it is read behind a spinner on `campaigns/`;
a Store is listed once, where the source that answers it first in source order puts it, and
that answer takes the place of what the last read listed for the Store wherever that was (a
Campaign a root listed may be a registered Store's answer this time, and a Campaign becomes the
refusal a source reports for its Store the moment that arrives); a Store no source answered yet
stays listed from the last read until one does, or until the read is done, so a source list
that changed within the scope never unlists an opened Campaign before a source answers for its
Store. The groups stand in source order.
While the pane shows, that thread reads every source again: once a second while any listed
Round is open, the Tasks pane's cadence, and every five seconds otherwise, so a Campaign started
meanwhile appears without a key.

One Campaign's pane shows STATE, SPEND and STORE, NODES (each reviewer's latest outcome,
charge and wall), LEDGER (the report's verdict and findings summary, then its findings grouped by
the reviewer that reported each) and COMMANDS: `review report`, `review ledger --long`, `review
show … KEY` and, while a Round is open, `review run … --restart-round`, each with `--campaign
LABEL` and, unless the default root resolves it by its label, `--state DIR`. Enter on a command
or a finding puts the line on the `:` line, unsubmitted. `y` copies the label. Stores another
release wrote are one muted row, as in the Tasks pane, with one command per release that wrote
them; another unreadable Store, a log that cannot even be looked at included, is an error row
naming its path and the error, and a `! … unreadable` bar entry. Nothing is written.

`af review campaigns` without `--format json` prints the same Campaigns as a 100-column table:
`CAMPAIGN  ROUND  STATE  VERDICT  TOKENS  WALL  UPDATED`, a cut cell ending in `~`, then its
problems.

## 6. Architecture

The TUI is a new module tree under `crates/af/src/tui/`:

```
crates/af/src/tui/
  mod.rs        launch(scope), event loop
  splash.rs     the splash painted while the scope and the panes load; the pixel worker
  term.rs       the /dev/tty raw-mode session; its panic hook restores the terminal only when
                the thread that entered raw mode panics, never for a background thread
  keymap.rs     mode + key sequence parser (gg, zo, ]], <C-w>l), one table, unit-tested
  tree.rs       the left bar: Node { kind, label, children, folded }, fold/search/motion
  scope.rs      user vs project resolution, path roots
  panes/
    settings.rs providers.rs workers.rs pipelines.rs tasks.rs campaigns.rs
  paint.rs      paint / paint_spans / Paint palette
```

Rendering needs no new crate: the terminal session is `rustix` termios (already a dependency
of the `af` crate) on `/dev/tty`, with the escape sequences written by hand (ADR-0119); a widget library is not
worth a new dependency for six list-and-detail panes. Each pane implements one trait:

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
   `q`/`:q`. **Delivered** (package M1, ADR-0119).
2. Providers and Pipelines panes (both reuse existing loaders and renderers directly).
   **Delivered** (package M1, ADR-0119). The terminal runs on `rustix::termios` rather than
   `crossterm`, whose lock entries the package could not add under `--locked`; see the ADR.
3. Tasks pane: list, detail, progress, tokens, time, history; running-Task poll.
   **Delivered** (package M3, ADR-0121). Deviations from §5.5:
   - The pane reads the `af task explain --json` document, which is `af task show --json` with
     the plan and its graph, for the graph order, the Attempt allowances and the Pipeline name.
     It resolves the artifacts that document names for the goal and for the node each execution
     record ran.
   - No `TaskCompleted@1` event exists. TOKENS takes `chargeable` from the document, and it
     shows a component only when `attempt_walls` covers every settled Attempt; otherwise the
     component is `-`.
   - TIME shows the wall between the first and last events, plus the check and
     dependency-preparation spans. No record carries a verification time.
   - SNAP names the source Snapshot, the derived Snapshot and the policy, not `HEAD@commit`,
     which no document carries.
   - Artifact IDs print as eight hex digits, times of day in UTC (`12:04:31Z`), and numbers
     without separators, so each equals its field.
   - An empty state group is not listed. A bar row that does not fit cuts the outcome first,
     down to nothing, and then the Task id.
   - The user scope's repository level is named by the opaque Task-state directory, because the
     Store records no repository path.
   - `p` opens the Pipelines pane's entry of the Task's Pipeline, which is the committed
     package's plan. The captured `ExecutionPlan` stays with `af task explain`.
4. Workers pane: identity, state and prompt. **Delivered** (package M4, ADR-0122). Deviations
   from §5.3:
   - The folder reads at load in a project scope, on its thread, as the other folders do: the
     load records the read and the first poll after the first frame starts it (ADR-0240), with
     a spinner on `workers/` meanwhile, so the bar lists the Workers without an open. STATE
     counts the Attempts of every checkout's Store, this one's and each discovered one's (§8),
     on that thread.
   - The user scope lists no Worker, and synced catalogs are not read. The pane says that
     Workers belong to a project.
   - The bar and every pane read what `HEAD` commits, like the Pipelines pane. A drifted
     declaration or prompt is marked `*`.
   - STATE counts reserved (open), settled ok, settled failed and released. No record kind is
     `abandoned`, and a started Attempt is still reserved until it settles. Released
     reservations are not Attempts, so they are neither charged nor timed. An Attempt's charge
     is the highest its settlement or a usage observation records. The wall is the sum of the
     recorded `attempt_walls`, with how many Attempts recorded one.
   - An Attempt reaches a Worker through its invocation's node, the node's single slot in the
     compiled graph of the plan the invocation ran under, and that slot's Worker name. The
     name then reaches the one package the committed pin places at it. An operator that serves
     several slots (a Provider admission, an optimization experiment) is credited to no Worker.
   - The Store keeps no index by Worker. The scan runs on each read of the opened pane.
   - IDENTITY shows the pin with eight hex digits of its digest. A reviewer's model and effort
     are read from its args, the way the kernel reads them.
   - `gf` on a bar entry opens the prompt when `HEAD` commits one, and the declaration
     otherwise.
5. Command line and the run/deliver hand-off; `gf`; yank. **Delivered** (package M5,
   ADR-0123). `gf` and `y` had already shipped with packages M1-M4. Deviations from §4 and §5.5:
   - Every handed-off command releases the terminal and waits for Enter, not only one that
     spawns Workers. `:e` and a typed `config edit` still edit in-process, and `:help` shows
     help in the main pane.
   - A Task ID completes after every `task` verb whose first positional argument clap names
     `task_id`, not only after `run`, `show`, `explain` and `deliver`.
   - The bar's legend is unchanged, since every golden paints it. `r` and `D` are listed in the
     opened Task's legend and in `:help`.
   - A child stopped by `<C-z>` is continued, because the browser has no job control.
   - A reload of the scope keeps the home directory, the Provider registry and the Task state
     root. They come from the process environment, which a child cannot change.
6. Campaigns, every Task Store, help and discoverability. **Delivered** (package P12). §3, §4,
   §5.2, §5.5 and §5.6 describe it. Deviations:
   - `?` opens the help overlay, so vim's backward search moved to `/` then `N`.
   - The Campaigns pane lists every Campaign of the machine in both scopes, as `af review
     campaigns` does: a Campaign records no repository path to filter by.
   - The commands a Campaign's pane offers are the CLI's own selectors (`--campaign LABEL
     [--state DIR]`), not a positional id.
   - A node chip's state is its latest Attempt's outcome in the Task accounting; the report
     records no per-node state of its own.

## 7a. Review

Every package is reviewed by `kernel/review-light`: `correctness` and `bugs` read the source, and
`kernel/uix` (Codex, gpt-6-sol, high; a reviewer cannot share the implementer's Claude principal) builds `af`, writes its own pseudo-terminal harness under
`target/uix-harness/`, drives the shipped panes key by key at 100x30 and 80x24, and compares each
captured screen with §3-§5 and with the CLI output the pane mirrors. Its findings quote the key
sequence, the captured screen and the expected one; a refused build or shell is a `block`, never a
source-only review.

## 8. Open questions

- Worker "state" needs the Store to answer "which Attempts named this Worker"; today that is a
  scan of every Task's execution records. Cheap for one repository, slow for the user scope. An
  index by `worker` in `tasks.sqlite` is the clean fix. Step 4 kept the scan; since ADR-0261 it
  runs on the Workers read's thread after the first frame in a project scope, opened or not, and
  the user scope lists no Worker.
- Which file is a package's prompt. **Answered** by package M4 (ADR-0122): the file the kernel
  itself sends, by the kernel's convention. That is `reviewer.md` for a reviewer Worker and
  `instructions.md` for a Task Worker package. A package without that file says so, and no
  other Markdown file is guessed. A declared `prompt = "…"` field in `af.worker/1` would still
  make the convention explicit.
