- `af project`: a repository now has one Project identity, derived from its root commits and
  never from a path or a remote URL, with a Project Store under `$XDG_STATE_HOME/af/project/`.
  Its checkouts are Workspaces, each with its own Task Store, Task files and bindings file.
  `af project show` and `af project workspaces` print them (`--json`: `af/project@1`,
  `af/project-workspaces@1`). `af project workspace new LABEL` makes a worktree, a branch
  and a Workspace directory without a commit, `workspace add PATH` adopts an existing checkout,
  and `workspace remove LABEL` unregisters one and never removes a Store. `af onboard --apply`,
  `af task plan|start`, `af review run` and `af project` register the Project and the checkout
  lazily, refuse a shallow clone with `git fetch --unshallow`, and never change a command's
  result. `af task deliver` registers the worktree it makes. `--workspace LABEL` on every
  `af task` command that takes `--state` selects a Workspace's Store and bindings. `af storage
  --json` is now `af/storage@2`, with the entry kind `project_store`
  ([ADR-0151](docs/adr/0151-give-a-repository-one-project-identity-and-register-its-checkouts-as-workspaces.md)).
