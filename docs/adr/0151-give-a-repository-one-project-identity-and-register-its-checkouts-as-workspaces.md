# ADR-0151: Give a repository one Project identity and register its checkouts as Workspaces

**Status:** accepted (2026-10-19). Package PJ1 of the
[Project design note](../design/project.md). Builds on
[ADR-0031](0031-deliver-verified-tasks-to-new-local-worktrees.md) (delivery makes only a local
branch and worktree), [ADR-0032](0032-generate-review-authority-with-af-onboard.md),
[ADR-0115](0115-declare-the-af-layout-and-keep-task-files-out-of-git.md) and
[ADR-0116](0116-report-undeclared-af-paths-and-let-a-project-refuse-them.md) (`.af/` holds
declarations only; nothing the kernel records goes in the checkout),
[ADR-0144](0144-hold-afs-disk-use-to-a-machine-budget.md) (the Storage Budget and the Store
registry) and [ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md)
(version every changed shape).

## Context

af has no entity for the thing it works on. "Project" names three unrelated things: the
`[project] name` and `min_af` that `.af/af.toml` declares; the `repository_id` every Snapshot
origin carries (the sorted root commits of `HEAD`), which only delivery and Campaign authority
compare; and the `project_id` digest the self-optimizer's sources make the operator type in.
Nothing joins them.

State is keyed by checkout path. The default Task Store is
`$XDG_STATE_HOME/af/task/local/<sha256(checkout path)[..16]>`, so each worktree gets its own
Store, which is right for isolation and wrong for any question about the whole repository. The
owner's practice fills the gap by hand: one worktree per line of work named `<repo>-wt-<label>`
beside the checkout, and a directory `$XDG_STATE_HOME/af/workstreams/<label>/` holding `tasks/`,
`bindings.toml` and `state/`, passed to every `af task` command as `--state` and `--bindings`.
On the owner's machine afactory's work is spread over eight default Task Stores and eighteen such
directories.

## Considered options

- **Identity by canonical checkout path.** Rejected: that is exactly what the default Task Store
  does, and it is why two worktrees of one repository are two unrelated things. A moved or
  re-cloned checkout would become a new Project.
- **Identity by remote URL.** Rejected for the reason `Repo::repository_id` already gives: the
  same content served from two remotes is one repository, a repository may have no remote, and a
  URL is attacker-controlled configuration.
- **One Project Store replacing the per-checkout Task Stores.** Rejected: one Store per checkout
  is the isolation each line of work needs (its own writer lease, its own collection, its own
  delivery lock), every existing Store would have to be migrated, and ADR-0145 forbids a
  migration path. The Project indexes Workspaces and their Stores instead of absorbing them.
- **A committed `.af/project.toml` naming the Project and its Workspaces.** Rejected: Workspaces
  are machine-local paths, and `.af/` holds declarations a reviewer approves, never state the
  kernel records (ADR-0115, ADR-0116). The `[project] name` that `.af/af.toml` already declares
  labels the Project; nothing new is declared.
- **Registration by an explicit `af project init`.** Rejected: onboarding is already the one way
  a project starts, and a command that must run first would leave every existing Store
  unadopted. Registration is lazy instead.

## Decision

1. **Identity.** The Project ID is `sha256:` + hex(SHA-256(`af/project-id@1\0` ‖
   `repository_id`)), the domain-separated digest form Campaign IDs use and the shape
   `af.optimization-sources/1` already requires of `project_id`. A shallow clone, whose boundary
   commit is not a root, is refused with a message naming `git fetch --unshallow`; a repository
   without commits has no Project and registers nothing.
2. **The Project Store.** Each Project owns `$XDG_STATE_HOME/af/project/<hex>/`, a Store with
   `cas/` and an event log holding `ProjectRegistered@1` {`project_id`, `repository_id`, `name`
   and `min_af` when the registering checkout's `.af/af.toml` parses, `registered_unix_ms`},
   `WorkspaceRegistered@1` {`workspace_id`, `label`, `origin`, `registered_unix_ms`} and
   `WorkspaceRemoved@1` {`workspace_id`, `removed_unix_ms`}: canonical JSON, numbers as decimal
   strings, each with a schema under `schemas/project-*.json` and in `run-event-v1.json`. No
   record holds a host path.
3. **Workspaces.** A Workspace is one registered checkout. Its ID is `w-` + the first 16 hex
   digits of SHA-256(`af/workspace-id@1\0` ‖ Project ID ‖ `\0` ‖ canonical checkout path); its
   label is unique within the Project and follows the safe-name rule of Task IDs; its origin is
   `adopted`, `created` or `delivered`. Its paths (checkout, Task Store, and optionally bindings
   file and Task-file directory) live only in `workspaces.toml` beside the log, machine-written,
   replaced whole through a temporary file and a rename under the Project Store's lock. The
   projection `af/project@1` is a function of the log and that file.
4. **Lazy, token-free registration.** `af onboard --apply`, `af task plan`, `af task start`,
   `af review run` and every `af project` command register the Project when it is new and the
   invoking checkout as a Workspace when it is new: origin `adopted`, labelled by the checkout's
   directory name (or that name, `-` and the first eight hex digits of the Workspace ID when it
   is taken), its Store the default Task Store for that checkout or the explicit `--state`.
   Every existing default Task Store is adopted this way, unchanged. `af task plan` and `af task
   start` register before they resolve `--workspace`, so the first `--workspace <checkout
   directory name>` in a checkout without a Project Store succeeds; the others register once the
   command ran. Outside `af project`, registration is best effort: a failure prints one warning
   on stderr and never changes the command's output or exit code. Nothing is written inside a
   checkout.
5. **`af project`.** `show` and `workspaces` print the projection (`--json`: `af/project@1`,
   `af/project-workspaces@1`). `workspace new LABEL [--branch] [--path] [--from]` makes branch
   `af/LABEL` at the invoking checkout's `HEAD` and a worktree of it at `<checkout>-wt-LABEL`,
   creates `workspaces/LABEL/` in the Project Store with an empty `state/`, an empty `tasks/` and
   a `bindings.toml` template naming no account, and registers origin `created`. It fills the
   worktree the way delivery does (ADR-0031) and never by a git checkout: the branch by `git
   update-ref --stdin`, the worktree by `git worktree add --no-checkout`, its index by `git
   read-tree HEAD`, its files by `review_source_git::materialize` of the `--from` commit's
   committed capture, so no checkout, filter, hook or other program the repository configures
   runs on the host. Every check that can refuse runs before the first mutation: the label is
   free; the path is admissible and absent; the branch is valid and absent; `--from` is a commit
   whose root commits (`rev-list --max-parents=0`) equal the Project's `repository_id`; the
   Workspace directory is absent. A `--path` at or beneath the invoking checkout's top level,
   beneath a registered Workspace or worktree of the repository, or beneath `$XDG_STATE_HOME/af`
   is a usage error (exit 2) with nothing created. A step that fails after an earlier one
   succeeded undoes the earlier ones newest first (registration, worktree, the directories git
   created above it, branch ref, Workspace directory) and reports what it undid, or exactly what
   remains when an undo fails; so every refusal leaves nothing behind and the same command
   succeeds once its cause is gone. The worktree is recorded as made before `git worktree add`
   runs, so an add that fails or reaches its deadline after creating the worktree's directory,
   its `.git` file or its entry under `<gitdir>/worktrees/` is undone too: by `git worktree
   remove --force --force` when git can remove it, otherwise by removing the directory if it
   exists, unlocking the entry a stopped add leaves locked, and `git worktree prune`. Git
   creates the entry, locked, before the path and its `gitdir` file, and neither lists nor
   prunes such an entry, so the intent also records the names under `<common
   dir>/worktrees/` before the add; the undo deletes every entry that is new since then and
   belongs to this add (named by the path's basename, alone or followed by digits, with no
   `gitdir` or one naming `<path>/.git`), prunes, and reports each as `removed the
   administrative entry worktrees/<name>`, or as remaining while git still lists the path or
   such an entry survives. An entry of another add is left alone. A step of which nothing
   exists is not reported. It makes no commit and no push. `workspace add
   PATH` adopts an existing checkout of the same Project, refusing one of another Project with
   both IDs. `workspace remove LABEL` unregisters; it never removes a Store, and when the Store
   holds a Task that is not collected it says so and names `af task gc`; `--delete-worktree`
   removes the worktree only when `git worktree remove` accepts it clean. Exit codes are 0, 1
   (with `af/error@1` under `--json` and the next action in the message) and 2 for usage. Every
   git process of the Project modules runs through `review_process` with a deadline (30 s for
   plumbing, capture's own for a committed capture) that stops its whole process group, in
   delivery's isolated command: a cleared environment, a private `HOME`, no user or system
   configuration, no hooks, transport or prompts. A git that outlives its deadline fails the
   command with a message naming the deadline.
6. **`--workspace LABEL`** on every `af task` subcommand that takes `--state` resolves `--state`
   to the Workspace's Store and, on `plan` and `start`, `--bindings` to its bindings file when
   that file exists and none was given. Beside `--state` it is a usage error. It changes paths
   only: the recorded Task equals one planned with the explicit flags.
7. **Delivery.** After `af task deliver` succeeded, the worktree it created is registered, best
   effort, as a Workspace of origin `delivered` labelled by the Task ID, its Store the default
   Task Store for that worktree. Delivery's contract and receipt are unchanged.
8. **Storage.** The Store registry records each Project Store under the new kind `project`, so
   the Storage Budget sweep reaches it. `af storage` counts it as the new entry kind
   `project_store`, never evicted, since it indexes the Project's Workspaces; the Task Stores of
   its Workspaces are counted and collected as Task Stores. A Project Store another release wrote
   is an unreadable Store, kept while it holds Workspace directories. Because the `af/storage`
   document gains a kind, it becomes `af/storage@2` (`schemas/storage-v2.json`), and version 1 is
   neither written nor read (ADR-0145). `stores.toml` keeps its version: its entries are machine
   paths with a kind, every file an earlier release wrote still reads, and an earlier release
   fails closed on the new kind, reporting the registry unreadable, as ADR-0145 permits.
9. **Words.** `CONTEXT.md` defines **Project** and **Workspace** as the design note words them.
   The `.af/` layout's "elsewhere" paragraph and the Task-file walkthrough point Task files at a
   Workspace's `tasks/` directory; the plan-time advisory of ADR-0115 keeps its example.

## Consequences

- Two worktrees, a clone and a fork of one repository are one Project, and `af project show`
  answers the same from each. "What has af done on this repository" has one place to start, which
  later packages of the design note build on.
- The owner's `workstreams/<label>/` directories become Workspaces with `af project workspace
  add`, without moving a byte, and new lines of work start with `af project workspace new`.
  `--workspace research` replaces the `--state` and `--bindings` pair on every `af task` command.
- Merging an unrelated history into a repository changes its root commits and so its Project ID;
  the former Project's Store stays, unjoined. A shallow clone has no Project until it is
  unshallowed.
- Every `af task plan`, `af task start`, `af review run` and `af onboard --apply` inside a
  checkout costs a few read-only git invocations and one small write to the Project Store the
  first time; a failure costs one stderr line and nothing else.
- Tests: `crates/af/tests/it/project.rs` registers one Project with two Workspaces from two
  worktrees and validates both documents, refuses a shallow clone, registers nothing for a
  repository without commits, adopts a default Store without changing a byte, compares a Task
  planned with `--workspace` with one planned with the explicit flags, creates and removes
  Workspaces (keeping a Store that holds an uncollected Task), registers a delivered worktree
  with an unchanged receipt, and times `af --help` and `af project --help` under 50 ms. It also
  proves that `workspace new` never runs a configured smudge filter and writes exactly the
  commit's tree with a clean `git status`; refuses `--path .af/research`, the checkout itself,
  a path inside another Workspace and one inside af's state with exit 2 and nothing created;
  leaves worktrees, refs and Workspace directories unchanged when it refuses `--from` an orphan
  branch or undoes a `worktree add` that failed or reached its deadline after creating the
  worktree's directory, `.git` file and administrative entry, or only a locked entry, with or
  without its `gitdir` and `HEAD`, after which the same label succeeds; fails within the deadline, leaving no child, when a fake `git` sleeps; succeeds on
  the first `af task plan --workspace <checkout directory name>`; and keeps `git status
  --porcelain --ignored` empty and the checkout's files unchanged after every `af project`
  command and `af task plan --workspace`. Unit tests undo every kind of step in reverse order,
  including a partial worktree whose entry a stopped add left locked and a locked entry git
  does not list, test which administrative entries belong to an add, and find no unsupervised
  process start under `crates/af/src/projects`.
  `crates/review-core/tests/schema_parity/project.rs` pins the three events and two documents,
  and `crates/af/tests/it/cli_surface.rs` the help surface.
