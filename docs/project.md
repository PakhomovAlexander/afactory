# Projects and Workspaces

What `af project` knows about the repository it runs in, where it keeps that, and how a checkout
for one line of work becomes a **Workspace** with its own Task Store, Task files and bindings.
The terms are defined in [`../CONTEXT.md`](../CONTEXT.md); the decision is
[ADR-0151](adr/0151-give-a-repository-one-project-identity-and-register-its-checkouts-as-workspaces.md).

## Identity

A **Project** is one repository across every clone, worktree and fork af has seen. Its ID is
derived from the `repository_id` every Snapshot origin already carries, the repository's sorted
root commits joined by `,`:

```text
project_id = "sha256:" + hex(SHA-256("af/project-id@1\0" + repository_id))
```

It is never derived from a checkout path or a remote URL, so two worktrees of one repository, a
clone and a fork are the same Project. Two consequences follow from root commits:

- A **shallow clone**'s boundary commit is not a root commit, so af cannot tell which Project it
  belongs to. It registers nothing, and `af project` refuses with a message that names
  `git fetch --unshallow`.
- A **repository without commits** has no root commit and so no Project. Nothing is registered,
  and every other command behaves as before.

The `[project] name` and `min_af` of the registering checkout's `.af/af.toml` are recorded when
that file parses; they label the Project and are not part of its identity.

## The Project Store

Each Project owns one Project Store at `$XDG_STATE_HOME/af/project/<hex>/`, where `<hex>` is the
digest of the Project ID. It is a Store like the others: a `cas/` and an event log. The log holds
three events, each canonical JSON with numbers as decimal strings:

| Event | Holds |
|---|---|
| `ProjectRegistered@1` | the Project ID, `repository_id`, and the `[project] name` and `min_af` when `.af/af.toml` parsed ([schema](../schemas/project-registered-v1.json)) |
| `WorkspaceRegistered@1` | the Workspace ID, its label and its origin: `adopted`, `created` or `delivered` ([schema](../schemas/project-workspace-registered-v1.json)) |
| `WorkspaceRemoved@1` | the Workspace ID ([schema](../schemas/project-workspace-removed-v1.json)) |

No event ever holds a path. Beside the log, outside every record, `workspaces.toml` holds this
machine's paths of each Workspace: its checkout, its Task Store, and, when it has them, its
bindings file and its Task-file directory. af writes that file whole through a temporary file
and a rename, as it writes `stores.toml`.

The Store registry (`$XDG_STATE_HOME/af/stores.toml`) records every Project Store with the kind
`project`, so `af storage` counts it as a `project_store` entry
([schema](../schemas/storage-v2.json)). A Project Store is never evicted: it indexes the
Project's Workspaces. The Task Stores of its Workspaces are counted, and their finished Tasks
collected, like every other Task Store.

## Workspaces

A Workspace is one registered checkout of the Project for one line of work. Its ID is opaque:

```text
workspace_id = "w-" + hex(SHA-256("af/workspace-id@1\0" + project_id + "\0" + canonical checkout path))[..16]
```

Its label is unique within the Project and follows the safe-name rule of Task IDs: ASCII letters,
digits, `-` and `_`, starting with a letter or digit.

**Registration is lazy and token-free.** `af onboard --apply`, `af task plan`, `af task start`,
`af review run` and every `af project` command run inside a checkout register the Project if it is
new and that checkout as a Workspace if it is new. `af task plan` and `af task start` register
before they resolve `--workspace`, so the first `af task plan --workspace <checkout directory
name>` in a checkout without a Project Store finds that checkout's Workspace. Such a Workspace has origin `adopted`, is
labelled by the checkout's directory name (or that name, `-` and the first eight hex digits of
its ID when the name is taken), and its Store is the default Task Store for that checkout, or the
`--state` the command was given. Every default Task Store af created before is adopted this way,
and not a byte of it changes. Outside `af project`, registration is best effort: a failure prints
one warning on stderr and never changes the command's output or exit code.

`af task deliver` registers the worktree it created, after delivery succeeded, as a Workspace of
origin `delivered` labelled by the Task ID. Its Store is the default Task Store for that worktree.
The delivery receipt is unchanged.

## Commands

Every command answers on stdout; `--json` prints one versioned document. Exit codes are 0 for
success, 1 for a failure whose message names the next action (`af/error@1` under `--json`) and 2
for a usage error.

### `af project show`

The Project, its Store and every Workspace. `--json` prints `af/project@1`
([schema](../schemas/project-v1.json)).

```sh
$ af project show
Project sha256:348a84cceb7ed2686722948e6fe890b9b561e598afd49447f3ae5d0fb4c3f294
  name           afactory
  min_af         0.12
  repository_id  983cb9328f75130f02486adc0b67e0b4d4e8e75b
  store          /home/me/.local/state/af/project/348a84cc…
  registered     2026-10-19T12:20:02Z

2 Workspaces
afactory (adopted, registered 2026-10-19T12:20:02Z, this checkout)
  id        w-69257f546ed2a4ad
  path      /home/me/src/afactory
  store     /home/me/.local/state/af/task/local/7ce9bfd5fa7c983a
research (created, registered 2026-10-19T12:21:40Z)
  id        w-7960bd822c7c40ba
  path      /home/me/src/afactory-wt-research
  store     /home/me/.local/state/af/project/348a84cc…/workspaces/research/state
  bindings  /home/me/.local/state/af/project/348a84cc…/workspaces/research/bindings.toml
  tasks     /home/me/.local/state/af/project/348a84cc…/workspaces/research/tasks
```

### `af project workspaces`

The same Workspaces, in label order. `--json` prints `af/project-workspaces@1`
([schema](../schemas/project-workspaces-v1.json)). A path this machine's `workspaces.toml` does
not know is shown as unknown and left out of the document.

```sh
af project workspaces --json | jq -r '.workspaces[] | "\(.label) \(.path)"'
```

### `af project workspace new LABEL`

Makes a Workspace for one line of work, the way the owner made them by hand:

- a worktree at `<checkout>-wt-LABEL` beside the invoking checkout, on a new branch `af/LABEL`
  at the invoking checkout's `HEAD`; `--path DIR`, `--branch NAME` and `--from REV` say
  otherwise;
- the directory `$XDG_STATE_HOME/af/project/<hex>/workspaces/LABEL/` with an empty `state/` for
  the Workspace's Task Store, an empty `tasks/` for its Task files, and a `bindings.toml`
  template that holds only `schema = "af.task-bindings/1"`, an empty `[providers]` table and a
  comment;
- the registration, with origin `created`.

The worktree is filled the way `af task deliver` fills one
([ADR-0031](adr/0031-deliver-verified-tasks-to-new-local-worktrees.md)), never by a git
checkout: the branch is created with `git update-ref`, the worktree with `git worktree add
--no-checkout`, its index with `git read-tree HEAD`, and its files are written from a committed
capture of the commit's tree. No checkout, filter, hook or other program the repository
configures runs on the machine, so a `.gitattributes` filter whose smudge command the local
configuration names never runs, and the worktree holds exactly the commit's tree. Every git
process runs supervised, with a deadline that stops its whole process group, in delivery's
isolated environment.

Every check runs before anything is made, in this order: the label is free; the path is
admissible and absent; the branch is valid and absent; `--from` names a commit whose root
commits are the Project's own (`git rev-list --max-parents=0`), so a commit of an orphan branch
or an unrelated history is refused; the Workspace directory is absent. A `--path` at or inside
the invoking checkout (such as `.af/research`), inside another registered Workspace or worktree
of the repository, or inside `$XDG_STATE_HOME/af` is a usage error, exit 2; the other refusals
exit 1. A refusal leaves no branch, worktree or directory behind, and the same command succeeds
once its cause is gone. When a step fails after an earlier one succeeded, what was made is
undone newest first (the registration, the worktree, the directories git created above it, the
branch, the Workspace directory), and the message says what was undone or, when an undo fails,
exactly what remains.

The command makes no commit and no push, the same local-only boundary delivery keeps.

```sh
af project workspace new research
af project workspace new research --branch agent/research --from main --path ../research
```

### `af project workspace add PATH`

Registers an existing checkout of this Project, a worktree or a clone, with origin `adopted`. Its
Store is that checkout's default Task Store unless `--state` names another; `--bindings` and
`--tasks` name its bindings file and Task-file directory, and `--label` its label. This is how a
hand-made `workstreams/<name>/` directory of `tasks/`, `bindings.toml` and `state/` becomes a
Workspace without moving a byte:

```sh
W="$XDG_STATE_HOME/af/workstreams/research"
af project workspace add ../afactory-wt-research --label research \
  --state "$W/state" --bindings "$W/bindings.toml" --tasks "$W/tasks"
```

A checkout of another Project is refused with both Project IDs; a checkout that is already a
Workspace is refused with its label.

### `af project workspace remove LABEL`

Unregisters the Workspace. Its Task Store is never removed. When the Store still holds a Task that
is not collected, the command lists those Tasks and names `af task gc`, which collects finished
ones. `--delete-worktree` also removes the checkout, but only when `git worktree remove` accepts
it as a clean linked worktree; otherwise nothing changes. The branch stays either way.

```sh
af project workspace remove research
af project workspace remove research --delete-worktree
```

## `--workspace LABEL`

Every `af task` command that takes `--state` also takes `--workspace LABEL`: `plan`, `start`,
`run`, `show`, `list`, `explain`, `export`, `output`, `refresh`, `approve`, `reject`,
`decision-payload`, `observe-adoption`, `gc`, `report` and `deliver`. It resolves `--state` to the
Workspace's Store and, on `plan` and `start`, `--bindings` to its bindings file when that file
exists and no `--bindings` was given. Giving both `--workspace` and `--state` is a usage error.
Only the paths change: a Task planned with `--workspace` records the same inputs, bindings and
plan as one planned with the explicit `--state` and `--bindings`.

```sh
cd ../afactory-wt-research
af task plan --workspace research --file "$TASKS/ticket.json"
af task run TASK_ID --workspace research --confirm-plan PLAN_ID
af task deliver TASK_ID --workspace research --branch af/TASK_ID --worktree ../TASK_ID --confirm TASK_ID
```

## What is written where

| Written | Where | By |
|---|---|---|
| The Project Store: `cas/`, `events.sqlite`, `project.lock` | `$XDG_STATE_HOME/af/project/<hex>/` | any registration |
| `workspaces.toml` | beside the Project Store's log | any registration |
| A `project` entry | `$XDG_STATE_HOME/af/stores.toml` | any registration |
| `workspaces/LABEL/` with `state/`, `tasks/`, `bindings.toml` | in the Project Store | `workspace new` |
| A worktree and a branch | beside the checkout, never inside it; in the repository's git directory | `workspace new` |

Nothing is ever written inside a checkout: `.af/` holds declarations only
([ADR-0032](adr/0032-generate-review-authority-with-af-onboard.md),
[ADR-0115](adr/0115-declare-the-af-layout-and-keep-task-files-out-of-git.md)). No record holds a
host path. No command commits, pushes, contacts a remote or removes a Store. `workspace remove
--delete-worktree` removes only a worktree git accepts as clean. The integration tests assert
that `git status --porcelain --ignored` stays empty and the checkout's files stay as they were
after every `af project` command and `af task plan --workspace`.
