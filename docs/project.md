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
bindings file and its Task-file directory. af writes that file whole through a temporary file
and a rename, as it writes `stores.toml`.

The Store registry (`$XDG_STATE_HOME/af/stores.toml`) records every Project Store with the kind
`project`, so `af storage` counts it as a `project_store` entry
([schema](../schemas/storage-v2.json)). A registry that can hold that kind is `version = 2`; a
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
  into it; it counts as made only when that `create_dir` succeeded on a path that did not exist.
  A path that appears between the checks and the add is refused, and nothing of it is deleted.
  The undo deletes the directory only while its `.git` is absent or is a file naming an entry
  this add owns; otherwise it keeps the directory and reports why, such as
  `the directory <path>, kept because its .git names <entry>, not an entry of this add; delete it once no add uses it`.
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
repository that is not locked, and only when it is clean: its files must equal its `HEAD`
commit's tree in content and mode, and it may hold no other file, ignored or not, besides git's
own `.git`. The comparison reads the commit through the same committed capture `workspace new`
fills a worktree from, against the files on disk, so no filter, hook, diff driver or other
program the repository configures runs; it never calls `git worktree remove`, whose cleanliness
check runs the repository's clean filters. A clean worktree is removed by deleting its directory
and then its own entry under the repository's `worktrees/`, nothing else; the entry goes whole or
stays whole. When the entry cannot be removed once the directory is gone, the command fails with

```text
removed the worktree <path>; its administrative entry <common dir>/worktrees/<name> remains: <os error>; fix it and run the command again
```

and the Workspace stays registered. Run the same command again once the cause is fixed: a
registered checkout that no longer exists has the entries whose `gitdir` still names it removed
(a locked one is refused), is unregistered, and both are reported; without such an entry it is
unregistered and reported as already gone. A dirty worktree is refused with the first differing
path and nothing changes; so are an unknown checkout and the checkout the command runs in. The
branch stays either way.

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
only through supervised git plumbing (`ls-tree`, `cat-file --batch`, `rev-list`, `log`, `tag`,
`for-each-ref`, and the committed capture for the Tree Digest), runs no checkout, hook, filter,
textconv or project code, calls no Provider, takes no writer lease on a Task Store and writes
nothing inside the checkout.

The record is canonical JSON: every map sorted by key, every list in the order stated below, and
every number a decimal string of an integer (counts, bytes, lines, Unix seconds, and shares in
basis points rounded down). It is byte-identical for the same head, policy, cutoff and Surveyor
version. `cutoff_unix_s`, the clock when the Survey started, is the only field that depends on
when it was taken.

| Section | Holds |
|---|---|
| `identity` | the Project ID and `repository_id`; the head's `[project] name`, `min_af` and `.af/af.lock` af pin; the head commit and its Tree Digest; `cutoff_unix_s`, `policy_digest` and `surveyor_version` |
| `tree` | files, bytes and lines in total; files, lines and bytes per top-level directory (root-level entries under `.`); files and lines per language; the `tests` split; `files_without_lines` |
| `history` | reachable commits; the commits reached and their merges, in total and in the last 30, 90 and 365 days; first and last committer time; churn (commits, lines added and removed) per top-level directory over 90 days; the `hot_paths` paths with the most commits in 90 days, most first and ties by path, each `removed` when the head no longer holds it; the tag count and the newest five tags matching `release_tags`; the local branch count |
| `contributors` | per identity: commits, lines added and removed, first and last committer time, `active_90d`; the identities active in 90 days; human and agent commits and their shares; the top identity's share |
| `decisions` | per record the `decisions` glob matches: number (the file name's leading digits), path, title (the first H1), status and date (from the first other line naming `accepted`, `proposed`, `superseded`, `deprecated` or `rejected`, and the first ISO date on it); counts by status, `unknown` for none; the newest 12 by number |
| `vocabulary` | the term names of the `vocabulary` file's `**Term**:` headings, in file order, and their count; never a definition |
| `checks` | the names and programs of `.af/code-policy.toml`'s checks and of the `[[checks]]` of the review pipelines in `.af/pipelines/`, never an argument; the toolchain declarations at the root (`rust-toolchain.toml`, `.nvmrc`, `.python-version`, `.tool-versions`, `go.mod`, `package.json`, `Cargo.toml`, `pyproject.toml`); the count of `.github/workflows/*.yml` and `*.yaml` |

A line is a newline of a text blob, one with no NUL in its first 8 KiB, at or under
`max_file_bytes`. A larger or binary blob, a symlink and a submodule entry count as a file and
its bytes only; symlinks and submodules are of language `other`. Each section carries
`completeness`: `complete`, `partial` with what it reached, or `unavailable`, and the bound it
reached as `reason` when it is not complete. `max_commits` makes `history` and `contributors`
partial and records the oldest commit reached. `max_wall_ms` is one deadline for the whole
Survey: the section it stops is partial or unavailable and every section after it unavailable,
and the Survey is still recorded. The record's own `completeness` is `unavailable` only when
every section is, otherwise `partial` when any section is not complete.

### The `[survey]` policy

The `[survey]` table of `.af/af.toml` deep-merges like every other table, so the directory layer
can set it once for every checkout beneath it, and `AF_SURVEY__<KEY>` overrides one key
(`af help environment`). Every key is typed and an unknown key is refused.

```toml
[survey]
max_wall_ms = 60000        # one deadline for the whole Survey
max_commits = 100000       # history walked newest first, at most this many commits
max_file_bytes = 1048576   # larger blobs count files and bytes, not lines
exclude = []               # path globs never counted, e.g. ["vendor/**"]
tests = ["**/tests/**", "**/*_test.*", "**/test_*", "**/fixtures/**"]
decisions = "docs/adr/[0-9][0-9][0-9][0-9]-*.md"
vocabulary = "CONTEXT.md"
release_tags = "v*"
identity = "name"          # or "email_hash"
agent_authors = ["*[bot]"]
agent_trailers = ["*Claude*", "*Codex*", "*GPT*"]
hot_paths = 20

[survey.languages]         # extends the built-in map below; never replaces it
"*.pest" = "pest"
```

`exclude`, `tests` and `decisions` are path globs, anchored at the repository root and matched
by segment: `**` spans any number of segments, `*` any run within one, `?` one character and
`[...]` one character of a class (`[0-9]`, `[!.]`). `exclude` removes entries before anything is
counted, in every section and in the numstat of `history` and `contributors`.
`agent_authors`, `agent_trailers` and `release_tags` are name globs over a whole text with `*`
and `?` only, so `*[bot]` matches a name ending in `[bot]`. `policy_digest` is `sha256:` +
hex(SHA-256 of the canonical JSON of the effective policy); a changed policy surveys the same
head again.

A file's language is its exact name's entry, else its longest extension's, in
`[survey.languages]` first and then in the built-in map, else `other`:

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

Under `identity = "name"`, the default, a contributor is the author name, and the Surveyor never
asks git for an email. A name that is blank, longer than 512 bytes or holds `@` is recorded by
the hash rule below applied to the name, so an address written as a name never enters a record.
Under `identity = "email_hash"` a contributor is

```text
identity = hex(SHA-256(lowercase(trim(author email))))[..16]
```

and the email is dropped as soon as it is hashed. Under either policy no email appears in a
record, an event, a document, a text output or an error message. A commit is an agent's when its
author name matches an `agent_authors` glob or a `Co-Authored-By:` trailer value matches an
`agent_trailers` glob; the trailer is matched and never kept.

### `af project refresh`

Records a Survey of the head now and prints `recorded` and its summary; `--json` prints the
record. A head whose commit, policy digest and Surveyor version equal the newest Survey's is not
surveyed again: nothing beyond `rev-parse HEAD`, the policy and the log is read, the command
prints `unchanged` with the newest Survey's summary, and nothing is recorded. A new commit with
the same tree is surveyed, because it changes `history`. A Survey that cannot be taken, for a
`[survey]` key that does not load or a git that fails, records `SurveyFailed@1` and exits 1.

On this repository's tree, committed once (the measurement of
[ADR-0152](adr/0152-measure-a-project-into-surveys-with-a-token-free-surveyor.md)):

```sh
$ af project refresh
recorded
Survey sha256:9cc87b20813a8b277017bb8baf5294c4b7be57dc77c6044d90818e12bccf92ce
  head          ac9e57b8bd9fb51e3743dfc7f5879643038888cb
  cutoff        2026-10-19T20:59:06Z
  completeness  complete
  identity      afactory, tree sha256:9a7f085e974ffcc336e1f6281527cca122f8798e3bfd5da50ffb142d3c01e750
  tree          1414 files, 383808 lines, 15224411 bytes, 502 test files
  history       1 commits (0 merges), 1 in 30 days, 1 in 90 days
  contributors  1 identities, 1 active in 90 days, agent commits 0 bp
  decisions     141 (accepted 140, superseded 1)
  vocabulary    73 terms in CONTEXT.md
  checks        9 declared, 2 toolchain files, 4 CI workflows
```

### `af project survey` and `af project history`

`af project survey` prints the newest Survey's summary, and `--json` its record's canonical bytes;
it exits 1, naming `af project refresh`, when the Project has none. `af project history` lists the
Survey chain newest first, each Survey's ID, recording time, head, cutoff and completeness;
`--json` prints `af/project-history@1` ([schema](../schemas/project-history-v1.json)). Each
Survey names the one before it as `previous_survey_id`.

```sh
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
are its own. `af onboard` without `--apply` records nothing, and a second `af onboard --apply` on
the same head records nothing.

Surveys stay in the Project Store; collecting old ones behind a tombstone comes with the detached
after-run refresh in a later package.

## What is written where

| Written | Where | By |
|---|---|---|
| The Project Store: `cas/`, `events.sqlite`, `project.lock` | `$XDG_STATE_HOME/af/project/<hex>/` | any registration |
| `workspaces.toml` | beside the Project Store's log | any registration |
| A `project` entry | `$XDG_STATE_HOME/af/stores.toml` | any registration |
| `workspaces/LABEL/` with `state/`, `tasks/`, `bindings.toml` | in the Project Store | `workspace new` |
| A worktree and a branch | beside the checkout, never inside it; in the repository's git directory | `workspace new` |
| A Survey and `SurveyRecorded@1`, or `SurveyFailed@1` | in the Project Store's `cas/` and log | `af project refresh`, `af onboard --apply` |
| The committed capture of the head, for its Tree Digest | a scratch directory under `$TMPDIR`, removed when the Survey ends | `af project refresh`, `af onboard --apply` |

Nothing is ever written inside a checkout: `.af/` holds declarations only
([ADR-0032](adr/0032-generate-review-authority-with-af-onboard.md),
[ADR-0115](adr/0115-declare-the-af-layout-and-keep-task-files-out-of-git.md)). No record holds a
host path. No command commits, pushes, contacts a remote or removes a Store. `workspace remove
--delete-worktree` removes only a worktree whose files equal its commit. The integration tests assert
that `git status --porcelain --ignored` stays empty and the checkout's files stay as they were
after every `af project` command and `af task plan --workspace`.
