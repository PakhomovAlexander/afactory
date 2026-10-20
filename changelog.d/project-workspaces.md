- `af project`: a repository now has one Project identity, derived from its root commits and never
  from a path or a remote URL, with a Project Store under `$XDG_STATE_HOME/af/project/`. Its
  checkouts are Workspaces, each with its own Task Store, Task files and bindings file. `af
  project show` and `af project workspaces` print them (`--json`: `af/project@1`,
  `af/project-workspaces@1`). `af project workspace new LABEL` makes a worktree, a branch and a
  Workspace directory without a commit, filling the worktree the way `af task deliver` does, so no
  checkout, filter or hook of the repository runs; it refuses a `--path` inside the checkout,
  another Workspace or af's state (exit 2) and a `--from` commit of another history before it
  makes anything, and undoes what it made when a later step fails, deleting only what it can
  prove it made: the worktree directory its own `create_dir` made, only while it keeps that
  identity, every entry in it, an empty directory as much as a file, is a path of the captured
  commit equal in kind, mode and content or a directory above one holding nothing else, its
  `.git` is absent or names this add's entry and the index the add wrote is unchanged, otherwise
  keeping the worktree and naming the first differing path; only the parent directories it
  created itself, each only while it is still that directory by device and inode, a replacement
  at its path being left and reported; administrative entries whose `gitdir` names its
  worktree; the branch only while
  it still holds the commit it was created at and no kept worktree is on it. Every other new
  entry is reported as left behind with one fixed recovery line, also when the undo's final check
  fails, and `git worktree prune` never runs. `workspace add PATH` adopts an existing checkout,
  and `workspace remove LABEL` unregisters one and never removes a Store; `--delete-worktree`
  removes a worktree only when its index and its files both equal its commit, the index through
  delivery's cached-index comparison and the files through the committed capture rather than
  `git worktree remove`, so no clean filter runs and a staged or unmerged entry is refused; it
  moves the worktree's administrative entry into a container directory it creates exclusively,
  `<name>.af-removing-<pid>-<random hex>`, before deleting it, so the move never replaces another
  directory; when its directory went but its entry could not, the message names where the entry
  remains, in its container or not, the Workspace stays registered, and the same command
  recognises the container and finishes it, refusing while an
  entry's `gitdir` cannot be read. Automatic Workspace labels follow one stated rule within 128
  bytes. `af
  onboard --apply`, `af task plan|start`, `af review run` and `af project` register the Project
  and the checkout lazily (`af task plan|start` before resolving `--workspace`), refuse a shallow
  clone with `git fetch --unshallow`, and never change a command's result; every git process they
  start runs supervised under a deadline. `af task deliver` registers the worktree it makes.
  `--workspace LABEL` on every `af task` command that takes `--state` selects a Workspace's Store
  and bindings. `af storage --json` is now `af/storage@2`, with the entry kind `project_store`,
  and `$XDG_STATE_HOME/af/stores.toml` is version 2: a version-1 file is refused until it is
  deleted, and Stores register again as commands use them
  ([ADR-0151](docs/adr/0151-give-a-repository-one-project-identity-and-register-its-checkouts-as-workspaces.md)).
