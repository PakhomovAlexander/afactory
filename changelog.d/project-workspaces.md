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
  `git worktree remove`, so no clean filter runs and a staged or unmerged entry is refused. It
  goes by identity, never by name: before it moves anything it writes a machine-local removal
  record in the Project Store (`removals/<workspace id>.toml`) holding the checkout path, the
  device and inode of the checkout and of its administrative entry, the container it makes for
  each and the step each reached; it claims each directory into a container it creates
  exclusively beside it (mode 0700, through that container's own descriptor, never over a taken
  name), judges the checkout there through descriptors without following a link, deletes only
  what it claimed and then the container with `rmdir`. A replacement at the checkout's path is
  kept and reported, a container holding anything else keeps it and is reported with the first
  such path, and a directory merely named like a container is reported and never touched. An
  interrupted or failed removal says where what is left remains, keeps the Workspace registered
  and is finished by the same command from its record, which unregisters the Workspace exactly
  once. The rerun never takes a recorded step as proof: whatever the step, it first checks the
  administrative entry now, opened without following a link and read only while its device and
  inode are the ones the record holds, so another directory put at its path is a replacement,
  reported and never read; while the entry is replaced or locked or its `gitdir` names another
  checkout it touches nothing, a checkout already in its container staying there, reported by
  its claim name, and a failed check never deletes the removal record, so the next run goes by
  the recorded identities rather than inspecting afresh; then it checks that step's
  precondition on the file system, so a checkout moved back out of its container is claimed and
  judged again and a replacement at its path is kept, and an entry is claimed or deleted only
  while its `gitdir` names the checkout and it holds no `locked`, whatever the step; an entry that
  is locked, names another checkout or lacks its `gitdir` stays where it is, reported, and so does
  the Workspace's registration, as while an entry's `gitdir` cannot be read; a plain `workspace
  remove` is refused meanwhile. Automatic Workspace labels follow one stated rule within 128 bytes. `af
  onboard --apply`, `af task plan|start`, `af review run` and `af project` register the Project
  and the checkout lazily (`af task plan|start` before resolving `--workspace`), refuse a shallow
  clone with `git fetch --unshallow`, and never change a command's result; every git process they
  start runs supervised under a deadline. `af task deliver` registers the worktree it makes.
  `--workspace LABEL` on every `af task` command that takes `--state` selects a Workspace's Store
  and bindings. `af storage --json` is now `af/storage@2`, with the entry kind `project_store`,
  and `$XDG_STATE_HOME/af/stores.toml` is version 2: a version-1 file is refused until it is
  deleted, and Stores register again as commands use them
  ([ADR-0151](docs/adr/0151-give-a-repository-one-project-identity-and-register-its-checkouts-as-workspaces.md)).
- PJ1b close-out: this branch did not have it yet, so it is added here. Every `*-moved` step of
  `af project workspace remove --delete-worktree` checks its original path: stopped after
  `entry-moved` (or `checkout-moved`), a directory found at the entry's (or checkout's) original
  path is a replacement that stops the removal, the recorded directory staying in its container,
  nothing deleted and the Workspace registered, reported as `a new administrative entry appeared
  at <path> since this removal moved the recorded one; clear it or move the recorded entry back,
  then run the command again`; cleared or moved back, the same command finishes
  (`a_directory_at_the_original_path_after_any_moved_step_stops_the_removal`,
  [ADR-0151](docs/adr/0151-give-a-repository-one-project-identity-and-register-its-checkouts-as-workspaces.md)
  clause 5).
