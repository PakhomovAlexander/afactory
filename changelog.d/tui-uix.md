- The browser's Providers pane always shows every login's quota windows: discovery hands the rows
  over, then the usage probe `af provider status --usage` runs follows on its own, so `R` no longer
  stands for a separate probe. Each window is a bar of twenty cells drawn in the brand's colour
  for how much is used (green under half, blue from half, pink from four fifths) on a grey track
  with a hairline between two bars, with the percentage used and when the window resets; the
  `models` section stands one blank row under them
  ([ADR-0261](docs/adr/0261-shape-the-browser-around-what-a-person-looks-for.md)).
- The Claude usage probe reads the five-hour session window from the `/usage` screen as well as
  the weekly ones: `af provider status --usage` and the browser list `Claude session 5h` first
  ([ADR-0261](docs/adr/0261-shape-the-browser-around-what-a-person-looks-for.md)).
- The Providers pane keeps the model catalogs off screen until asked for: its last row says
  `<m> to show models`, and `m` shows every listed Provider's `models` section (one row per exact
  ID) until `m` hides them again
  ([ADR-0261](docs/adr/0261-shape-the-browser-around-what-a-person-looks-for.md)).
- With Caps Lock on the browser still answers: an upper-case letter with no binding of its own
  (`Q`, `J`, `K`, ...) is read as its lower-case key; `G`, `N`, `R` and `Z` keep their own, and so
  does `D` in the Tasks pane, where it prefills `task deliver`; to every other pane `D` is `d`, so
  `D` on a Provider removes it as `d` does. The key completing a sequence folds the same way:
  `zO` opens a fold and `gF` opens the file
  ([ADR-0261](docs/adr/0261-shape-the-browser-around-what-a-person-looks-for.md)).
- `af provider status` and the Providers pane leave out a CLI default context whose login a
  registered Provider of its kind already holds (the Claude CLI's `~/.claude` reading the same
  Keychain login as a registered directory), as they leave out a logged-out one; the row is not
  named as a logged-in sibling either, and the names-only scan still names the directory
  ([ADR-0260](docs/adr/0260-leave-out-a-default-context-that-holds-a-registered-login.md)).
- The Workers folder lists its Workers from the start in a project scope, read on its thread
  after the first frame like the other folders; it no longer waits for an open
  ([ADR-0261](docs/adr/0261-shape-the-browser-around-what-a-person-looks-for.md)).
- `Enter` on a Worker or a Pipeline listed in the main pane of its folder opens it, as the bar
  does; the folders' legends say so
  ([ADR-0261](docs/adr/0261-shape-the-browser-around-what-a-person-looks-for.md)).
- The Pipelines pane never prints a declaration's TOML: a review Pipeline under
  `.af/pipelines/` is drawn from its committed declaration (Subject, convergence, budgets,
  checks, one row per node with its package or runner, gate and Demands, and the `af review
  plan` command), and a package the kernel has not planned yet, or refuses to plan, is drawn the
  same way (one row per node with its slot's Worker and Attempt bounds, a `call` expanded into
  the pinned callee with the call's bindings applied, the contract, then the refusal). The
  status line names the highlighted row's Worker
  ([ADR-0261](docs/adr/0261-shape-the-browser-around-what-a-person-looks-for.md)).
- The project scope is the Project's view, not the directory's: the bar's root and the panes'
  titles carry the `[project] name` the repository's committed `.af/af.toml` declares (read
  again on `R` at the root), and `tasks/` lists, after this checkout's own Store, the default
  Store of every other worktree of the Project that holds
  Tasks (its registered Workspaces by label, then the worktrees git lists by name), each a bar
  group of its own; a registry Store a Workspace uses is listed once, with the Workspaces beside
  it. A Worker's STATE counts the Attempts of every checkout's Store. The checkouts are
  discovered on the read's thread, never at startup
  ([ADR-0261](docs/adr/0261-shape-the-browser-around-what-a-person-looks-for.md)).
- `f` in the Tasks pane lists the running Tasks only: those of every Store in one list, the one
  started last first, each with its progress, how long it has run, its charge so far and where
  it runs; the Stores that cannot be read follow, so an empty list never hides one. `f` again
  lists every Task grouped by state. `Enter` on a Task row of the Tasks folder opens it, as the
  bar does ([ADR-0261](docs/adr/0261-shape-the-browser-around-what-a-person-looks-for.md)).
- A duration of a day or more in the Tasks pane prints as `6d 02h`, where it printed hours
  alone (`142h 22m`)
  ([ADR-0261](docs/adr/0261-shape-the-browser-around-what-a-person-looks-for.md)).
