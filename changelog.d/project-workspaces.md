- `af project`: a repository now has one Project identity, derived from its root commits and never
  from a path or a remote URL, with a Project Store under `$XDG_STATE_HOME/af/project/`. Its
  checkouts are Workspaces, each with its own Task Store, Task files and bindings file. `af
  project show` and `af project workspaces` print them (`--json`: `af/project@1`,
  `af/project-workspaces@1`). `af project workspace new LABEL` makes a worktree, a branch and a
  Workspace directory without a commit, filling the worktree the way `af task deliver` does, so no
  checkout, filter or hook of the repository runs; it refuses a `--path` inside the checkout,
  another Workspace or af's state (exit 2) and a `--from` commit of another history before it
  makes anything, and undoes what it made when a later step fails, deleting only administrative
  entries whose `gitdir` names its worktree, reporting a locked entry without one as left behind,
  and never running `git worktree prune`. `workspace add PATH` adopts an existing checkout, and
  `workspace remove LABEL` unregisters one and never removes a Store; `--delete-worktree` removes
  a worktree only when its files equal its commit, compared through the committed capture rather
  than `git worktree remove`, so no clean filter runs. Automatic Workspace labels follow one
  stated rule within 128 bytes. `af
  onboard --apply`, `af task plan|start`, `af review run` and `af project` register the Project
  and the checkout lazily (`af task plan|start` before resolving `--workspace`), refuse a shallow
  clone with `git fetch --unshallow`, and never change a command's result; every git process they
  start runs supervised under a deadline. `af task deliver` registers the worktree it makes.
  `--workspace LABEL` on every `af task` command that takes `--state` selects a Workspace's Store
  and bindings. `af storage --json` is now `af/storage@2`, with the entry kind `project_store`,
  and `$XDG_STATE_HOME/af/stores.toml` is version 2: a version-1 file is refused until it is
  deleted, and Stores register again as commands use them
  ([ADR-0151](docs/adr/0151-give-a-repository-one-project-identity-and-register-its-checkouts-as-workspaces.md)).
