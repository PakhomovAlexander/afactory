- The browser shows review Campaigns and every Task Store. Bare `af` gains a `campaigns/` folder
  that lists every review Campaign on the machine: the default Campaign root, the Store a plain
  `af review run` keeps for each repository under `review/local/`, and every `--state` Store
  and `--state-root` directory `stores.toml` records. Each row shows the Campaign's newest
  Round, its state, its verdict, what it spent, when its Store was last written, and its
  reviewer nodes as chips. The rows update once a second while a Round is open. Opening a
  Campaign shows its Ledger by reviewer. Enter on a row puts the matching `af review report`,
  `ledger`, `show` or `run --restart-round` command on the `:` line. The `tasks/` folder also
  lists every `--state` Task Store the registry records, each under its path, and `:state DIR`
  adds one for the session; `r` and `D` name such a Store with `--state`. `?` opens a help
  overlay naming the panes, the keys and `: <af command>`, so backward search is now `/` then
  `N`. While no key comes, the status line takes turns with `? help - : run a command - q
  quit`. An ambient Provider says how it got there and how to make it go: through
  `CLAUDE_CONFIG_DIR` or `CODEX_HOME`, or the CLI's default directory, with the
  `af provider add` command that registers it. Stores other releases wrote are one row in both
  panes. `af review campaigns` text is now a table in 100 columns (Campaign, Round, state,
  verdict, tokens, wall, last update); `--format json` is unchanged.
