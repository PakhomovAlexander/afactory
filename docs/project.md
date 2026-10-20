# Projects and Workspaces

What `af project` knows about the repository it runs in, where it keeps that, how a checkout
for one line of work becomes a **Workspace** with its own Task Store, Task files and bindings,
and how the Surveyor measures the Project into **Surveys**. The terms are defined in
[`../CONTEXT.md`](../CONTEXT.md); the decisions are
[ADR-0151](adr/0151-give-a-repository-one-project-identity-and-register-its-checkouts-as-workspaces.md)
and [ADR-0152](adr/0152-measure-a-project-into-surveys-with-a-token-free-surveyor.md).

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
these events, each canonical JSON with numbers as decimal strings:

| Event | Holds |
|---|---|
| `ProjectRegistered@1` | the Project ID, `repository_id`, and the `[project] name` and `min_af` when `.af/af.toml` parsed ([schema](../schemas/project-registered-v1.json)) |
| `WorkspaceRegistered@1` | the Workspace ID, its label and its origin: `adopted`, `created` or `delivered` ([schema](../schemas/project-workspace-registered-v1.json)) |
| `WorkspaceRemoved@1` | the Workspace ID ([schema](../schemas/project-workspace-removed-v1.json)) |
| `SurveyRecorded@1` | the Survey's ID in the CAS, its head, cutoff, completeness, policy digest and Surveyor version, and the Survey before it ([schema](../schemas/project-survey-recorded-v1.json)) |
| `SurveyFailed@1` | the head and af's one-line reason why no Survey was recorded ([schema](../schemas/project-survey-failed-v1.json)) |

No event ever holds a path. Beside the log, outside every record, `workspaces.toml` holds this
machine's paths of each Workspace: its checkout, its Task Store, and, when it has them, its
bindings file and its Task-file directory. An unfinished `--delete-worktree` of a Workspace is
held by its removal record beside it, `removals/<workspace id>.toml`. af writes each file whole
through a temporary file and a rename, as it writes `stores.toml`.

The Store registry (`$XDG_STATE_HOME/af/stores.toml`) records every Project Store with the kind
`project`, so `af storage` counts it as a `project_store` entry
([schema](../schemas/storage-v3.json)). A registry that can hold that kind is `version = 2`; a
version-1 file an earlier release wrote is refused with the way out: delete
`$XDG_STATE_HOME/af/stores.toml`. Stores register again as commands use them, and the Stores
under af's default roots were never in it. A Project Store is never evicted: it indexes the
Project's Workspaces. The Task Stores of its Workspaces are counted, and their finished Tasks
collected, like every other Task Store.

## Workspaces

A Workspace is one registered checkout of the Project for one line of work. Its ID is opaque:

```text
workspace_id = "w-" + hex(SHA-256("af/workspace-id@1\0" + project_id + "\0" + canonical checkout path))[..16]
```

Its label is unique within the Project and follows the safe-name rule of Task IDs: ASCII letters,
digits, `-` and `_`, starting with a letter or digit, at most 128 bytes. A label you give
(`--label`, `workspace new LABEL`) must already follow it and is never rewritten.

An automatic label is made from the checkout's directory name by one rule:

- every byte outside ASCII letters, digits, `-` and `_` becomes `-` (a two-byte `é` becomes
  `--`);
- leading `-` and `_` are stripped;
- the result is cut to 119 bytes, leaving room for `-` and eight hex digits of the collision
  suffix within the 128-byte limit;
- an empty result is `checkout`.

So `repo.v2` is `repo-v2`, `my repo` is `my-repo`, `café` is `caf--` and `.hidden` is `hidden`.
When the label is already registered, explicitly or automatically, it gains `-` and the first
eight hex digits of the Workspace ID (`fixture-fedcba98`); when that is taken too, the command
fails and asks for `--label`.

**Registration is lazy and token-free.** `af onboard --apply`, `af task plan`, `af task start`,
`af review run` and every `af project` command run inside a checkout register the Project if it is
new and that checkout as a Workspace if it is new. `af task plan` and `af task start` register
before they resolve `--workspace`, so the first `af task plan --workspace <checkout directory
name>` in a checkout without a Project Store finds that checkout's Workspace. Such a Workspace has origin `adopted`, the
automatic label of the checkout's directory name, and the default Task Store for that checkout
as its Store, or the `--state` the command was given. Every default Task Store af created before is adopted this way,
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
undone newest first (the registration, the worktree, the directories made above it, the branch,
the Workspace directory), and the message says what was undone or, when an undo fails, exactly
what remains.

The undo deletes only what it can prove this command made
([ADR-0031](adr/0031-deliver-verified-tasks-to-new-local-worktrees.md)):

- **The worktree's directory.** The command makes it itself, empty, and `git worktree add` adds
  into it; it counts as made only when that `create_dir` succeeded on a path that did not exist,
  and the command records its identity (device and inode). A path that appears between the
  checks and the add is refused, and nothing of it is deleted. Directory identity and path
  membership never prove ownership of bytes, and an entry of any kind can be another writer's,
  so the undo deletes the directory only when its identity is unchanged, every entry in it,
  directories as well as files and links, is its `.git` (absent or a file naming an entry this
  add owns), a file or link of the captured commit equal in path, kind, mode and content (the
  comparison delivery's rollback makes), or a directory above such a path, or a submodule's
  empty directory, that holds nothing else, and the index this add wrote is unchanged (absent,
  empty, or the commit's tree). Any other entry, an empty directory included, keeps the
  directory. A `.git` naming another entry keeps the directory alone:
  `the directory <path>, kept because its .git names <entry>, not an entry of this add; delete it once no add uses it`.
  Anything else keeps the worktree whole, with this add's entry and the branch it is on, and
  names the first differing path, such as
  ``the directory <path>, kept because it holds `sentinel.txt`, which this add did not write; delete it once no add uses it``
  (or `sentinel`, an empty directory),
  ``the directory <path>, kept because `README.md` differs from what this add wrote; delete it once no add uses it``,
  `the directory <path>, kept because it was replaced since this add created it; delete it once no add uses it`
  or
  ``the worktree <path>, kept with its administrative entry worktrees/<name> because its index differs from <commit> at `staged.txt`; remove it with `git worktree remove --force <path>` once nothing in it is needed``
  and `the branch refs/heads/<name>, kept because the worktree <path> kept above is on it`.
- **The directories above it.** The missing parents of a `--path` are created one at a time
  from the deepest that exists, and only those the command's own `create_dir` made are
  recorded, each with its identity (device and inode); one another process created meanwhile is
  never removed. The undo removes the recorded ones deepest first, each only while it is still
  that directory and empty. A directory removed and recreated at the same path by anyone is
  another directory: it is left and reported as
  `the directory <path>, kept because it was replaced since this add created it`.
- **The branch.** It is deleted with `git update-ref -d <ref> <oid>`, giving the commit it was
  created at, so a branch that moved meanwhile is kept and reported as
  `the branch refs/heads/<name> moved since this add created it and is kept`.
- **Administrative entries.** The command notes the entries under the repository's `worktrees/`
  before the add, so an add that fails or reaches its deadline after creating the worktree's
  `.git` file or its entry is undone too. The undo deletes, directly, each new entry whose
  `gitdir` names the worktree's `.git`, and reports it as
  `removed the administrative entry worktrees/<name>`. Only such an entry is provably this add's.

Git creates an entry, locked, before it writes its `gitdir`, and a concurrent `git worktree add`
of another path with the same directory name creates one too, so any other new entry named after
the worktree's directory is left where it is and reported as left behind with exactly one of
these lines:

```text
<common dir>/worktrees/<name>: a locked entry without a gitdir, created during this add and possibly another add's; delete that directory once no add is running (git worktree prune keeps a locked entry)
<common dir>/worktrees/<name>: an entry without a gitdir, created during this add and possibly another add's; run git worktree prune once no add is running
<common dir>/worktrees/<name>: an entry created during this add whose gitdir names <that path>, another add's
```

These lines are part of the report even when the undo's final `git worktree list --porcelain`
check fails or git still lists the path, and the report never says nothing was left behind while
such an entry exists. The undo never runs `git worktree prune`, which would also remove stale
entries of other worktrees, beyond what this command made. Entries of other worktrees are left
alone.

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
ones. `--delete-worktree` also removes the checkout, but only a linked worktree of the
repository that is not locked, and only when it is clean: its index and its files must both
equal its `HEAD` commit. The index is compared the way delivery compares one, with `git
diff-index --cached` and external diff and textconv disabled, so a staged addition,
modification or deletion, or an unmerged entry, makes the worktree dirty even when its files
equal `HEAD`. The files must equal the commit's tree in content and mode, and the worktree may
hold no other file, ignored or not, besides git's own `.git`; they are compared through the same
committed capture `workspace new` fills a worktree from, against the files on disk. No filter,
hook, diff driver or other program the repository configures runs; it never calls `git worktree
remove`, whose cleanliness check runs the repository's clean filters. A dirty worktree is
refused with the first differing path and nothing changes; so are an unknown checkout and the
checkout the command runs in. A clean worktree is removed by deleting its directory and then its
own entry under the repository's `worktrees/`, nothing else. The branch stays either way.

The removal goes by what it inspected, never by a name. Before it moves or deletes anything it
writes a removal record in the Project Store, `removals/<workspace id>.toml`, never in the
repository: the checkout path, the identity (device and inode) of the checkout and of its entry,
the container it makes for each with that container's identity, and the step each reached. It
rewrites the record at every step and deletes it when it is done, just before the Workspace is
unregistered. Each directory is first claimed: moved into a container the command creates
beside it, `<name>.af-removing-<pid>-<16 random hex digits>` with mode 0700, through that
container's own descriptor once its identity is checked, never over a name that is taken, and
found there by its identity. The checkout is judged there, its files read through descriptors
without following a link, and put back when it is dirty. Only what was claimed is deleted, then
the container with `rmdir`. A container that holds anything else keeps it, the message names the
first such path, and the Workspace stays registered until you clear it. A directory another
process put at the checkout's path before the claim is kept and the command refused with nothing
changed; one put there after the claim is kept and reported. When the entry cannot be moved
once the directory is gone, or cannot be deleted once moved, the command fails with

```text
removed the worktree <path>; its administrative entry <common dir>/worktrees/<name> remains: <os error>; fix it and run the command again
removed the worktree <path>; its administrative entry remains at <container>/<name>: <os error>; fix it and run the command again
```

and the Workspace stays registered. Run the same command again once the cause is fixed: it
finishes from the record, step by step, acting only on directories whose identity the record
holds, then unregisters the Workspace and reports what it removed; its messages begin `the
worktree <path> is already gone;`. An entry whose `gitdir` cannot be read may still name the
checkout, so the command stops with that file and the error and keeps the Workspace registered.
A directory that only looks like a container, one nobody recorded or a worktree or entry whose
own name has that form, is never touched and is reported as `left <directory>: it is named like
a removal's container, but no removal record names it, so it is not this removal's`. Without a
record, a checkout that no longer exists has each entry whose `gitdir` still names it removed
the same way, and is otherwise unregistered and reported as already gone. While a removal record
exists, `workspace remove` without `--delete-worktree` is refused and names the command that
finishes it.

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

## Surveys

A **Survey** is one kernel-measured description of the Project at its head, the record
`af/ProjectSurvey@1` ([schema](../schemas/project-survey-v1.json)) in the Project Store's CAS. The
**Surveyor** records one at `af onboard --apply` and on `af project refresh`, synchronously, and
spends no token: the kernel counts, and a model never writes a number
([ADR-0132](adr/0132-measure-and-compare-source-candidates-in-the-kernel.md)). It reads the head
only through supervised git plumbing (`ls-tree`, `cat-file --batch`, `rev-list`, `log`,
`for-each-ref`), every stream read as it arrives and never buffered whole, runs no checkout,
hook, filter, textconv or project code, calls no Provider, takes no writer lease on a Task Store
and writes nothing inside the checkout. The history is measured from objects alone: `rev-list`,
`log` and the hot paths' `ls-tree` run in a private bare repository attached only to the
checkout's object database, the administrative directory af's tree diffs use
([ADR-0001](adr/0001-tree-diff-behind-a-typed-method.md)), so the checkout, its index, its local
configuration, an untracked or `.git/info` attributes file, a diff driver and a replacement ref
change no count.

The record is canonical JSON: every map sorted by key, every list in the order stated below, and
every number a decimal string of an integer (counts, bytes, lines, Unix seconds, and shares in
basis points of their population rounded down: `3333` for one third of three, `0` for an empty
population). It is byte-identical for the same head, refs, policy, cutoff,
Surveyor version and git version. `cutoff_unix_s`, the clock when the Survey started, is the only
field that depends on when it was taken.

| Section | Holds |
|---|---|
| `identity` | the Project ID and `repository_id`; the head's `[project] name`, `min_af` and `.af/af.lock` af pin; the head commit, `refs_digest` and the Tree Digest, or `tree_digest_reason` when it is absent (`deadline`, `max_record_bytes`, `manifest_bytes` or `temp_bytes`); `cutoff_unix_s`, `policy_digest`, `surveyor_version` and `git_version` |
| `tree` | files, bytes and lines in total; files, lines and bytes per top-level directory (root-level entries under `.`); files and lines per language; the `tests` split; `files_without_lines` |
| `history` | reachable commits; the commits reached and their merges, in total and in the last 30, 90 and 365 days; first and last committer time; churn (commits, lines added and removed) per top-level directory over 90 days; the `hot_paths` paths with the most commits in 90 days, most first and ties by path, each `removed` when the head no longer holds it; the tag count and the newest five tags matching `release_tags` and the local branch count, from the survey key's one ref observation |
| `contributors` | per identity, ranked by commits, then lines added, then `key`: its `key`, its display `name` under `identity = "name"`, commits, lines added and removed, first and last committer time, `active_90d`; the identities active in 90 days; human and agent-assisted commits and their shares; the top identity's share |
| `decisions` | every decision record up to `max_entries`, by number then path: number (the file name's four leading digits), path, title (the first H1), and status and date from the first line that starts with `**Status:**` or `Status:`, never another line naming a status (prose saying `proposed` before it names nothing): the first of `proposed`, `accepted`, `superseded`, `deprecated` and `rejected` on it (so `accepted; superseded in part by` is `accepted`), `amended: true` when the line says `amended`, and the first `YYYY-MM-DD` on it when that is a day of the calendar (`2024-02-29` is, `2025-02-29` and `2026-02-31` are not); `count` and counts by status, `unknown` for a missing or malformed status, `unreadable` for a file not read |
| `vocabulary` | the term names of the `vocabulary` file's `**Term**:` headings, sorted by name whatever order the file writes them in, each once, and their count; never a definition |
| `checks` | the names and programs, never an argument, of the checks of exactly `.af/code-policy.toml` (source `code-policy`) and every `.af/pipelines/*.toml` in the head tree, routed or not (source the file's basename), each `[[checks]]`, a check's identity being `<source>:<name>` so equal names in two pipelines are two checks; the toolchain files at the root among exactly these eight, `rust-toolchain.toml`, `rust-toolchain`, `.tool-versions`, `.nvmrc`, `.node-version`, `.python-version`, `.ruby-version` and `.java-version`, and no other (`.go-version` and `Cargo.toml` are none); the count of `.github/workflows/*.yml` and `*.yaml`; the declarations it could not read, under `unreadable` |

A line is a newline of a text blob, one with no NUL in its first 8 KiB, at or under
`max_file_bytes`. A larger or binary blob, a symlink and a submodule entry count as a file and
its bytes only; symlinks and submodules are of language `other`. Each section carries
`completeness`: `complete`, `partial` with what it reached, or `unavailable`, and the bound it
reached as `reason` when it is not complete: `max_wall_ms`, `max_record_bytes`,
`manifest_bytes`, `temp_bytes` (these two `identity`'s alone), `max_paths`, `max_commits`,
`max_entries` or `max_file_bytes`, the first that holds in that order. A section
is never `complete` when it cut a list or left a value unmeasured. The record's own `completeness` is `unavailable` only when every section is,
otherwise `partial` when any section is not complete.

The measurement policy, so the same inputs give the same bytes: commits are walked from the head
in `git rev-list --date-order` order, newest first, and cut at `max_commits`, which makes
`history` and `contributors` partial and records the oldest commit reached; a window holds a
commit when `cutoff - window < committer time <= cutoff`, so a commit authored long before it
was committed counts by its committer date; a merge counts as a commit and a merge and adds no
churn; renames are not detected, so a rename is a removal and an addition; a binary change is
one file touched and zero lines; ties break by path, then by commit ID. `identity.git_version`
is the exact `git --version` stdout, trimmed, because git decides these numbers.

`max_wall_ms` is one absolute deadline for the whole Survey, its start plus `max_wall_ms`, from
before `rev-parse HEAD` to the last section. Every git process the Surveyor starts runs under the
lesser of its own limit and what remains of that deadline, in a process group that is ended when
it passes, also while af waits for output that never comes. The deadline is checked at every item
of every stream (tree entries, blobs, commits, numstat lines, decision files, tags and refs, the
survey key's refs among them), after every stream, at every item of the work after one (the
tree's aggregation, the ranking of identities, and each decision, line of the vocabulary file,
declaration, an oversized one included, and declared check as it is parsed), before every match
of every pattern list, `[survey.languages]` included, and once more after each section's
finishing work (the tree's directory and language cuts, the history's materialization, the
contributors' sort, the decisions' aggregation, the checks' ranking). A deadline that passes
before the survey key exists records `SurveyFailed@1`, `the survey key could not be computed
within max_wall_ms`. The section the deadline stops, in its work or its finishing, is `partial`
with what it reached and finished, or `unavailable` when it reached nothing, never `complete`;
every section after it is `unavailable`, and the Survey is still recorded within that one
deadline, past it by at most one item or one pattern match and the end of the git command then
running.

The Tree Digest is the content digest the head's committed capture would have, of the whole tree,
independent of `exclude` and every maximum, computed by a digest-only path: no blob is written to
any CAS and no listing is buffered whole; each blob streams through the hash in one fixed buffer,
the listing is held in at most 16 MiB of manifest buffering and spilled in sorted runs to at most
256 MiB of temporary manifest data, fixed ceilings no project may raise, and the runs are merged
in the canonical path order the capture's digest hashes, so the two digests are equal. When it
does not finish it is absent, never the digest of a part, and its temporary storage is removed:
`tree_digest_reason` is `deadline` when the deadline passed first (every later section is then
`unavailable`), `max_record_bytes` for a listing record over 64 KiB, and `manifest_bytes` or
`temp_bytes` when a ceiling stopped it, which makes `identity` partial with that reason and stops
nothing else.

Fixed maxima bound the rest. `max_paths` tree entries are considered after `exclude`; beyond it
`tree` is `partial` with reason `max_paths` and `seen.paths`, while the other sections still find
their files in the rest of the listing. A list of a section (directories, languages, churn, hot
paths, tags, decisions, terms, contributors, and the declared checks, toolchain files and
unreadable declarations of `checks`) holds at most `max_entries` items. Every list is ranked
before it is cut, and its totals cover the whole population: contributors by commits, then lines
added, both descending, then `key`; hot paths by commits in 90 days, then path; release tags by
tagger time (annotated) or the target's committer time (lightweight), newest first, undated
last, then name; directories, languages, churn, terms and checks by name; decisions by number,
then path. A cut list records how many it saw under `seen`, by list name, and its section's
reason is `max_entries`. `max_paths`, `max_entries` and `max_survey_bytes` are ceilings: a
layer may lower one to a positive integer at most 200000, 1000 and 1048576, and zero, a value
above the ceiling or one that is no integer, from `.af/af.toml` and `AF_SURVEY__MAX_PATHS` alike,
is refused naming the key and the ceiling. The intermediate state is bounded too: the history
walk tracks at most `max_paths` distinct paths, and no streamed record (a tree entry, a numstat
line, a ref) is read past 64 KiB; beyond either the measurement stops, the section is `partial`
with reason `max_paths` or `max_record_bytes`, and its totals and rankings cover what it
observed. A display string (a contributor name, a decision title, a term, a tag, a check
name or program) is cut at a character boundary to 256 bytes and marked `truncated: true`
beside it; identity strings (every field of `identity`, commit IDs, digests, ref names and
paths) are recorded exactly. A record over `max_survey_bytes` records `SurveyFailed@1` naming
the bytes it needed, and nothing else. A `vocabulary` file, a decision file or a declaration
(`.af/code-policy.toml`, a pipeline file) over `max_file_bytes`, not text or not valid UTF-8 is
never decoded lossily and makes its section `partial` with reason `max_file_bytes`: the decision is listed with status `unreadable` and no
title, the vocabulary is `present` with `terms` and `count` absent, and the declaration is named
under `checks.unreadable`.

### The `[survey]` policy

The `[survey]` table of `.af/af.toml` deep-merges like every other table, so the directory layer
can set it once for every checkout beneath it, and `AF_SURVEY__<KEY>` overrides one key
(`af help environment`). Every key is typed and an unknown key is refused.

```toml
[survey]
max_wall_ms = 60000        # one deadline for the whole Survey
max_commits = 100000       # history walked newest first, at most this many commits
max_file_bytes = 1048576   # larger blobs count files and bytes, not lines
max_paths = 200000         # tree entries after exclude, and history paths: a ceiling
max_entries = 1000         # items in any list of a section: a ceiling
max_survey_bytes = 1048576 # bytes of the record: a ceiling
exclude = []               # patterns never counted, e.g. ["vendor/**"]
tests = ["**/tests/**", "**/*_test.*", "**/test_*", "**/fixtures/**"]
decisions = "docs/adr/*.md" # a decision when the name starts with four digits and a dash
vocabulary = "CONTEXT.md"
release_tags = "v*"
identity = "name"          # or "email_hash"
agent_authors = ["*[bot]"]
agent_trailers = ["*Claude*", "*Codex*", "*GPT*"]
hot_paths = 20

[survey.languages]         # ahead of the built-in map below; bytewise-first pattern wins
"*.pest" = "pest"
```

Every pattern (`exclude`, `tests`, `decisions`, `release_tags`, `agent_authors`,
`agent_trailers` and the keys of `[survey.languages]`) matches with the one grammar of
`.af/af.toml`'s routes: anchored at the start, matched by `/`-separated segment, `**` spans any
number of segments, `*` any run within one, `?` exactly one byte of the path's encoded spelling
(so `caf?.rs` does not match `café.rs`, whose `é` is two bytes, and `caf??.rs` does), and every
other byte is literal, brackets included. So `*[bot]` matches `dependabot[bot]` and no name that
merely ends in `t`, and `[0-9]` matches only those three characters. A pattern is at most 256
bytes and every pattern list, `[survey.languages]` included, at most 64 patterns: a longer one
is refused when the policy loads, naming the key, and records `SurveyFailed@1`. There are no character ranges: a file `decisions` matches is a decision only when
its name starts with four ASCII digits and a dash, so `docs/adr/0152-x.md` is one and
`docs/adr/README.md` is not. `exclude` removes entries before anything is counted, in every
section and in the numstat of `history` and `contributors`. `policy_digest` is `sha256:` +
hex(SHA-256 of the canonical JSON of the effective policy); a changed policy surveys the same
head again.

Each file counts for exactly one language: the bytewise-lexicographically first
`[survey.languages]` pattern that matches it (a pattern without `/` matches the file name, one
with `/` the whole path, so `*.rs` and `src/**` overlap and `src/core.rs` is the first one's,
since `*` sorts before `s`), over the built-in map, an exact lookup that names a file by its
exact name, else by its longest extension; a file nothing matches is `other`, like a symlink or
a submodule. The built-in map:

| Language | Files |
|---|---|
| `c` | `*.c`, `*.h` |
| `cpp` | `*.cc`, `*.cpp`, `*.cxx`, `*.hpp` |
| `csharp` | `*.cs` |
| `css` | `*.css`, `*.scss` |
| `docker` | `Dockerfile` |
| `go` | `*.go` |
| `html` | `*.htm`, `*.html` |
| `java` | `*.java` |
| `javascript` | `*.cjs`, `*.js`, `*.jsx`, `*.mjs` |
| `json` | `*.json` |
| `kotlin` | `*.kt` |
| `lua` | `*.lua` |
| `make` | `Makefile`, `*.mk` |
| `markdown` | `*.md` |
| `nix` | `*.nix` |
| `php` | `*.php` |
| `protobuf` | `*.proto` |
| `python` | `*.py` |
| `ruby` | `*.rb` |
| `rust` | `*.rs` |
| `shell` | `*.bash`, `*.sh`, `*.zsh` |
| `sql` | `*.sql` |
| `svg` | `*.svg` |
| `swift` | `*.swift` |
| `text` | `*.txt` |
| `toml` | `*.toml` |
| `typescript` | `*.ts`, `*.tsx` |
| `yaml` | `*.yaml`, `*.yml` |

### Contributors and email

Every contributor carries a `key`, by which it is grouped and ranked, never by its display text.
Under `identity = "name"`, the default, the key is the hash of the exact author-name bytes and
`name` is the author name, cut to 256 bytes and marked `truncated` when longer, so two names
sharing their first 256 bytes are two identities with equal `name`; the Surveyor never asks git
for an email. Under `identity = "email_hash"` the key is

```text
key = hex(SHA-256(lowercase(trim(author email))))[..16]
```

with no `name`, and the email is dropped as soon as it is hashed. An author name any
whitespace-separated token of which is an email address (an `@` between non-empty parts), such
as `carol@fixture.invalid` or `Alice alice@example.org`, or that is blank or holds a control
character anywhere in the whole name, past the 256-byte cut included, is replaced whole,
before any cut and under both policies, by `h:` and the same 16-hex rule applied to the name, as its key and, under `name`, its name, and is grouped by that
key under `email_hash` too; its email is not used. The
no-email rule covers author identities only: under either policy no author email appears in a
record, an event, a document, a text output or an error message, while repository text
(decision titles, paths, terms, check names, hot paths) is recorded as committed, `@` included,
bounded by the fixed maxima.

A commit is agent-assisted when its author name matches `agent_authors` or the name of any
`Co-Authored-By:` trailer (the text before its `<address>`) matches `agent_trailers`, otherwise
human; the trailer is matched and never kept. Each commit counts once in that partition, so a
commit with two agent trailers is one agent-assisted commit, and once for its author, who keeps
the per-identity totals: a human's commit with an agent trailer raises both that human's count
and `agent_commits`. Shares are of the commits reached, and an empty window makes every share
`0`.

### `af project refresh`

Records a Survey of the head now and prints `recorded` and its summary; `--json` prints the
record. A complete newest Survey with an unchanged survey key is reused: the survey key is the
head commit, `refs_digest`, the policy digest and the Surveyor version. `refs_digest` is SHA-256
of the exact stdout of one bounded ref observation, `git for-each-ref
--format=%(refname)%09%(objectname)%09%(objecttype)%09%(creatordate:unix) refs/heads refs/tags`,
whose lines also give `history` its tags (an annotated tag dated by its tagger time, a
lightweight one by its commit's) and its branch count, so the key and the statistics come from
one read and a ref created after it is in neither; an observation the deadline cuts, or a ref
over 64 KiB, records `SurveyFailed@1`. Then nothing beyond `rev-parse HEAD`, that
`for-each-ref`, the policy and the log is read, the command prints one line naming the key and
the newest Survey's summary, and nothing is recorded:

```text
unchanged: head 219207013596, refs df39bc03fecd7f04, policy f9da720b696edb5a, surveyor 5
```

A new commit, even one with the same tree, a new tag or branch, a policy change or a newer
Surveyor records a new Survey, and so does a newest Survey that is `partial` or `unavailable`.
`af project refresh --force` records one regardless of the key. A Survey that cannot be taken,
for a `[survey]` key that does not load, a git that fails or a record over `max_survey_bytes`,
records `SurveyFailed@1` and exits 1.

An example, from the integration test's fixture repository
(`crates/af/tests/it/project_survey.rs`), not from afactory:

```sh
$ af project refresh
recorded
Survey sha256:5dc2bbc98c964255b4c4ac78f7ef47b1e5b57f9025479a72468251e65b6df756
  head          219207013596994d436542a8fb656f7e3cdb2d17
  cutoff        2027-01-15T08:00:00Z
  completeness  complete
  identity      survey-fixture, tree sha256:91eca2613149502df38f713937b8bd8fa7fb15ce4ece7ff53738d842ff51fa12
  tree          18 files, 78 lines, 1621 bytes, 1 test files
  history       8 commits (1 merges), 4 in 30 days, 6 in 90 days
  contributors  4 identities, 4 active in 90 days, agent commits 3750 bp
  decisions     3 (accepted 2, proposed 1)
  vocabulary    2 terms in CONTEXT.md
  checks        3 declared, 2 toolchain files, 1 CI workflows
```

### `af project survey` and `af project history`

`af project survey` prints the newest Survey's summary, and `--json` the complete record's
canonical bytes, unchanged; `--section NAME` (repeated, or names separated by commas) is an
option of the text alone, printing the summary's first lines and the named sections' lines in
record order, and `--section` with `--json` is usage error 2. It exits 1, naming `af project
refresh`, when the Project has none. `af project history` lists the Survey chain newest first,
each Survey's ID, recording time, head, cutoff and completeness; `--json` prints
`af/project-history@1` ([schema](../schemas/project-history-v1.json)). Each Survey names the one
before it as `previous_survey_id`.

No Survey event's fields are printed or used unverified. Before it prints anything, `af project
history` binds every `SurveyRecorded@1` of the chain to the record it names, or a collected
Survey's to the tombstone that copied every identity field of its record (head, refs, cutoff,
completeness, policy, Surveyor version and previous Survey), and `af project survey` with and
without `--section`, `af project show` and `af project refresh` bind the newest one the same
way before any of its fields is used, the head an error names included; an event that
disagrees, an older one included, is refused with `SurveyRecorded@1 at sequence <n> names a
<field> its Survey <id> does not record` (or `its tombstone SurveyCollected@1 at sequence <m>`)
and exit 1. The fold refuses before it reads: a record of an uncollected Survey that af cannot
read back (gone, unreadable, corrupt) refuses replay with `SurveyRecorded@1 at sequence <n>
names Survey <id>, whose record cannot be read: <error>`, naming no other field of the event,
in `af project history` wherever it stands and in every reader of the newest Survey for the
newest. A corrupt record is still the Storage Budget's to collect, and `af project refresh
--force` records a new newest Survey without reading the old one.

```sh
af project survey --section tree --section history
af project survey --json | jq '.history.hot_paths[] | select(.removed) | .path'
af project history --json | jq -r '.surveys[].survey_id'
```

`af project show` prints the newest Survey's summary below the Workspaces, or the reason of the
newest `SurveyFailed@1` when no Survey was recorded after it. Its `--json` document,
`af/project@1`, is unchanged.

### At onboarding

`af onboard --apply`, once it registered the Project and the Workspace, records the first Survey
and prints one line, `af onboard: recorded Survey … of … (complete)`, on stdout, or on stderr
under `--json`. A Survey that fails prints one line on stderr; onboarding's output and exit code
are its own. `af onboard` without `--apply` records nothing, and a second `af onboard --apply`
with an unchanged survey key records nothing and prints its `unchanged:` line.

PJ2 ships Survey collection with this synchronous writer: `SurveyCollected@1`, `[storage]
keep_surveys` retention, the allocated-byte inventory and budget-driven deletion; PJ4 adds only
the trigger after a run. The tombstone a collected Survey leaves is described under
[Collection](#collection).

### Collection

Every Survey is an entry of kind `survey` of the machine budget: `af storage` lists it with its
record's CAS bytes, its path the CAS object, and its cutoff as its last use, and `--json` prints
`af/storage@3` ([schema](../schemas/storage-v3.json)). `af project refresh` collects nothing,
however often it runs. Surveys go only in the Storage Budget's own passes
([ADR-0144](adr/0144-hold-afs-disk-use-to-a-machine-budget.md)):

- `af storage prune --apply`, and the sweep after a run when `[storage] auto_gc = true`, collect
  the Surveys of each Project beyond the newest `[storage] keep_surveys` (50; machine layers only,
  `AF_STORAGE__KEEP_SURVEYS`), oldest first, each idle at least `keep_days`;
- every sweep's budget step may take any Survey, least recently used first among every entry,
  while af holds more than `max_bytes`, even below `keep_surveys`.

Neither takes the newest complete Survey, a Survey of a Project Store whose `project.lock` cannot
be taken now (a refresh or a plan holds it, or af cannot open it), or a Survey used within the
hour, so the protected Surveys may exceed `keep_surveys`. The Project's log, `workspaces.toml` and operator files are never reclaimed.

A collected Survey leaves a `SurveyCollected@1` tombstone in the Project Store's log, the run
`survey-collection` ([schema](../schemas/project-survey-collected-v1.json)): its ID, head, cutoff,
completeness and the totals of `tree` (files, bytes, lines) and `history` (commits, merges). The
tombstone is appended before the record is deleted, and the next sweep deletes any record a
tombstone names that an interruption left, with the rule `recovery`. `af project history` marks
a collected Survey `collected` from its tombstone; its `--json` document lists the chain as
before. `af project survey` exits 1 when the newest Survey was collected, saying so and naming
`af project refresh`, and `af project show` then prints no Survey. A Survey's
`previous_survey_id` is a name: a collected predecessor keeps nothing alive.

A sweep plans once, over one inventory snapshot, and `af storage prune` without `--apply` prints
that plan in `af/storage-prune@2` ([schema](../schemas/storage-prune-v2.json)): every entry it
removes, in order, with the bytes it frees, its rule (`recovery`, then `collection` and
`keep_surveys`, then `budget`) and af's total after it, and under `left` every file or Survey it
leaves with its reason. `--apply` runs that same plan entry by entry and never chooses again: each
entry says what it freed and the total after it, and a Survey whose job found what the plan did
not (the lock lost, its record replaced) keeps its place with nothing freed and a `deviation`, for
the next sweep to plan again. So the bytes of a deletion an interruption left count once: a
preview that frees them first and then fits the budget stops exactly where the applied sweep
stops.

A record is reached, read and deleted only through directory descriptors, the removal boundary of
[ADR-0144](adr/0144-hold-afs-disk-use-to-a-machine-budget.md): the Project Store from `/`, then
`cas`, `objects` and the record's two-digit prefix directory, each opened without following a
link, and the record stated and opened relative to its prefix directory. Its deletion names the
inode that was checked: each job creates its own container `cas/removing/<pass-id>/` (mode 0700,
a fresh random ID, never one that exists), renames the record into it and, just before the
unlink, states it again relative to that container; only the device and inode stated before its
tombstone is unlinked, and the bytes reported are that inode's. Only a process of the same user
can replace a file inside a container af just created, the trust boundary af's delivery already
accepts, and a replacement the check sees is refused: it is moved back to its name (or kept in the
container as `<survey-id>.replaced`, a name no sweep deletes, when the name was taken again),
nothing is deleted, and the sweep reports the Survey `refused (replaced during collection)`.

The next sweep settles what a dead pass left the same way. A file in its container named by a
collected Survey is that Survey's record, moved: renamed into the new pass's container, checked
and deleted under `recovery`, and the empty container goes. A file at that Survey's CAS name is
then somebody else's: listed under `left` as `foreign` with its bytes, counted in the Project
Store's bytes and never the Survey's, and never deleted by any sweep. Without a moved record, the
file at a collected Survey's name is deleted only when its bytes are the record's (they hash to
its ID), and is `foreign` otherwise. Anything else under `cas/removing` is listed and left; names
are read raw, so one that is not UTF-8 is `unreadable_name`, shown lossily in text and in its
percent-encoded spelling with `path_encoded` in `--json`.

A symbolic link on the way, or a directory on another device than the Store's, refuses the Survey:
inventory measures nothing through it and lists it in use, and a sweep reports it as a failure
with the reason, adds no tombstone and touches nothing outside the Store. A record the file system
will not let af read (permission denied, an I/O error) is refused the same way and keeps its place
in the log. A record over 1 MiB, the most the Surveyor ever writes and af ever reads as a Survey,
is left as `oversize`, `refused (oversize, <bytes> bytes)`. A record within that ceiling whose
bytes were read and are no Survey (they do not match its digest, or do not parse) is collected all
the same, since reclaiming corrupt bytes is the budget's job: its tombstone carries `unreadable`,
af's reason in at most 256 bytes, in place of the totals, its bytes are freed, and the sweep and
`af project history` show it as `collected (unreadable)`.

The preview and the applied sweep decide `held` by one predicate: `project.lock` opened as the
applied sweep opens it and released at once. The Surveys of a Project Store a refresh or a plan
holds, or whose lock af cannot open or lock (`lock unavailable: <error>`, say a lock file it may
not write), and the deletions an interruption left there, are left as `held` with that reason
(`held survey …` in text), never removed. A Task's `af/TaskStorageSweep@1` counts a Survey's
removal without listing it; its kinds are unchanged.

```sh
af storage --json | jq '[.entries[] | select(.kind == "survey")] | length'
AF_STORAGE__KEEP_SURVEYS=10 af storage prune --apply
af project history | grep collected
```

Patterns match in bounded work: the matcher of `.af/af.toml`, the routes' and the Surveyor's alike,
costs at most (pattern length + 1) × (path length + 1) steps and never backtracks, a `[survey]`
pattern is at most 256 bytes and a pattern list at most 64 patterns, and a policy beyond either is
refused naming the key. The Surveyor checks its deadline before each pattern it matches, so a
pattern list overruns `max_wall_ms` by at most one match, and the section it cut is partial with
reason `max_wall_ms`. `?` is one byte of a path's canonical encoded spelling: `caf?.rs` does not
match `café.rs`, whose `é` is two bytes, and `caf??.rs` does; a byte that is not UTF-8 is spelled
`%E9`, three bytes
([ADR-0152](adr/0152-measure-a-project-into-surveys-with-a-token-free-surveyor.md), clauses 13
and 14).

## What is written where

| Written | Where | By |
|---|---|---|
| The Project Store: `cas/`, `events.sqlite`, `project.lock` | `$XDG_STATE_HOME/af/project/<hex>/` | any registration |
| `workspaces.toml` | beside the Project Store's log | any registration |
| A `project` entry | `$XDG_STATE_HOME/af/stores.toml` | any registration |
| `workspaces/LABEL/` with `state/`, `tasks/`, `bindings.toml` | in the Project Store | `workspace new` |
| A worktree and a branch | beside the checkout, never inside it; in the repository's git directory | `workspace new` |
| A Survey and `SurveyRecorded@1`, or `SurveyFailed@1` | in the Project Store's `cas/` and log | `af project refresh`, `af onboard --apply` |
| The Tree Digest's spilled manifest runs (no blob, at most 256 MiB, only when the manifest exceeds 16 MiB) and the history's private bare repository (no object, attached to the checkout's object database) | `af-tree-digest-*` and `review-kernel-tree-diff-*` under `$TMPDIR`, each removed when its step ends, a stopped one included | `af project refresh`, `af onboard --apply` |

Nothing is ever written inside a checkout: `.af/` holds declarations only
([ADR-0032](adr/0032-generate-review-authority-with-af-onboard.md),
[ADR-0115](adr/0115-declare-the-af-layout-and-keep-task-files-out-of-git.md)). No record holds a
host path. No command commits, pushes, contacts a remote or removes a Store. `workspace remove
--delete-worktree` removes only a worktree whose files equal its commit. The integration tests assert
that `git status --porcelain --ignored` stays empty and the checkout's files stay as they were
after every `af project` command and `af task plan --workspace`.
