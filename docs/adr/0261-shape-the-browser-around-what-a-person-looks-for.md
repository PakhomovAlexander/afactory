# ADR-0261: Shape the browser around what a person looks for

**Status:** proposed (2026-10-29). Amends
[ADR-0112](0112-refuse-agent-mediated-provider-logins.md) for the
browser's Providers pane, [ADR-0121](0121-show-recorded-tasks-in-the-browser.md) for what the
Tasks pane lists, and the pane rules of
[ADR-0119](0119-open-a-read-first-browser-on-bare-af.md); keeps
[ADR-0240](0240-open-the-browser-before-any-store-is-read.md) in force for every read it adds.
[ADR-0260](0260-leave-out-a-default-context-that-holds-a-registered-login.md) records the one
decision of the same round that reaches `af provider status`.

## Context

The browser (`docs/design/tui.md`) was built pane by pane from the CLI's documents. Its owner then
used a development build of it for a day against a real machine: four Provider logins, one
Project with seventy-two Workspaces and sixty-five explicit Task Stores, and a dozen Tasks running
at any time. Fourteen rounds of "this is what I look for, and this is what I see" followed, each
fixed and tried again within minutes. The rounds found the same thing from different sides: the
panes showed what the CLI prints, in the CLI's order, where a person at the screen looks for one
answer first.

- The Providers pane showed quota windows only after an explicit probe (`R`), as ADR-0112 keeps
  `af provider status` fast by default; the person opening the pane wants to know how much of each
  login is left, and that answer is the probe.
- The models of every Provider took most of the pane; nobody reads them daily.
- With Caps Lock on, the browser answered no key: every letter was an unbound upper-case verb.
- The Workers folder listed nothing until opened; the Pipelines pane printed a declaration's TOML;
  `Enter` on a row of a folder listing did nothing, where the bar's `Enter` on the same entry
  opened it.
- In a worktree, the Tasks pane listed that directory's Store alone, though the Project
  ([ADR-0158](0158-give-a-repository-one-project-identity-and-register-its-checkouts-as-workspaces.md)) is one across its
  worktrees, and the Tasks the person asked about ran in the others.
- With a dozen Stores read, the question "what runs now?" had no answer short of reading every
  group.

## Decision

The browser's panes are shaped around the question a person opens them with, and keep the CLI's
documents as their only source and ADR-0240's rule that no Store is read before the first frame.

- **Limits always, as bars.** The Providers pane discovers the logins, hands the rows over, then
  runs the `--usage` probe on its own thread for every listed Provider, without being asked: each
  window is one row, `limit  NAME  [bar]  NN% used  resets in ...`, the bar twenty cells in the
  brand's colour for the share used (green under half, blue from half, pink from four fifths) on
  a grey track, the window names padded to one column across every listed Provider so the bars
  align, one blank row between two Providers' blocks in the folder. `R` reads again; there is no
  probe-only verb. The Claude probe reads the five-hour session window too (`Claude session 5h`).
  ADR-0112's default stands for the CLI: `af provider status` probes usage only with `--usage`.
- **Models on demand.** The catalogs are off screen until `m` shows every listed Provider's
  section; one muted row says `<m> to show models` or `<m> to hide models`.
- **Caps Lock answers.** An upper-case letter with no binding of its own (`Q`, `J`, `K`, ...) is
  read as its lower-case key; `G`, `N`, `R`, `Z` and the Tasks pane's `D` keep their own.
- **Folders list from the start, and their rows open.** The Workers folder reads its Workers on
  its thread after the first frame in a project scope, as the other folders do; `Enter` on a
  Worker or a Pipeline listed in the main pane of its folder opens it, as the bar does, and so
  does `Enter` on a Task row of the Tasks folder.
- **Declarations are drawn, never dumped.** A review Pipeline under `.af/pipelines/` is drawn from
  its committed declaration (Subject, convergence, budgets, checks, one row per node with its
  package or runner, gate and Demands, the `af review plan` command); a package the kernel has not
  planned, or refuses to plan, the same way (its nodes with their slot's Worker and Attempt
  bounds, a `call` expanded into its pinned callee with the call's bindings applied, the contract,
  then the refusal). The kernel's plan, when it has one, is shown as before.
- **The project scope is the Project's view.** The bar's root and the panes' titles carry the
  `[project] name` the repository declares. Beside this checkout's own Store, the Tasks pane lists
  the default Store of every other checkout of the Project that holds Tasks: the Workspaces the
  Project Store registers, read under its shared lock, by label, then the worktrees `git worktree
  list` names that no Workspace covers, by directory name, each a bar group of its own, headed
  `Workspace LABEL  PATH` or `worktree NAME (not a Workspace)  PATH`. A registry Store a Workspace
  uses is listed once, with the Workspaces noted beside it. The checkouts are discovered on the
  read's thread, on every read; a Project Store that cannot be read is a warning above the
  Stores. A Worker's STATE counts the Attempts of every checkout's Store.
- **`f` shows what runs now.** In the Tasks pane, `f` filters the bar and the folder to the
  running Tasks of every Store in one list, the one started last first: the bar flattens to the
  Tasks and one muted row for the Stores that cannot be read; the folder lists one row per Task
  with its progress, how long it has run, its charge so far and where it runs (a checkout's label,
  the end of an explicit Store's path), the status line naming the Store of the highlighted row,
  and the Stores that cannot be read follow with their refusals, so an empty list never stands
  for a Store that was not read. `f` again lists every Task grouped by state. A duration of a day
  or more prints as `6d 02h`.

## Considered options

- **Keep the usage probe behind `R`** (ADR-0112's default, in the browser too). Rejected: the
  pane's one question is the quota, and a browser that waits for a key to answer it is slower
  than the CLI with `--usage`. The CLI keeps its default, since a script wants the status alone.
- **List the Project's Workspaces by directory, from git alone.** Rejected: a Workspace that
  names a `--state` Store is not found through its directory's default Store, and its label is the
  name the person gave it; git lists only the worktrees of this clone. Both sources are read, the
  Project Store first.
- **A `running/` group under every Store** (the grouping as it stands). Rejected as the only
  answer: with a dozen Stores the question is still answered Store by Store. The groups stay; the
  filter is the answer across them.
- **The filter as a `:` command** (`:filter running`). Rejected: one key toggles it and the
  legend names it; the `:` line is for `af` commands and the browser's few verbs.
- **Print a declaration's TOML, since it is what the file holds.** Rejected: the pane's job is
  the plan the declaration describes; the file is one `gf` away.

## Consequences

- The Providers pane spends a usage probe on every open and `R` (a few seconds for four logins,
  no tokens); `af provider status` does not.
- The Tasks pane of a project scope opens the Project Store read-only under its shared lock on
  every read, and runs `git worktree list` there; neither is on the key loop. A worktree added or
  a Task run meanwhile shows at the next `R`.
- The folder's `Enter` and the bar's `Enter` open the same entry; the legends say so.
- The design doc (`docs/design/tui.md` §2, §3, §4, §5.2–§5.5) carries each rule; the golden
  screens under `crates/af/tests/fixtures/tui/` were re-rendered for the Providers bars, the
  Workers rows and the Tasks headings.
