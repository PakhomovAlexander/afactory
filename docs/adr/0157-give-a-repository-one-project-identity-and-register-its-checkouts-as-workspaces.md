# ADR-0157: Give a repository one Project identity and register its checkouts as Workspaces

**Status:** accepted (2026-10-19); clauses 4, 5 and 8 amended (2026-10-20) by PJ1b, clause 5
again (2026-10-20) by its second round, clauses 3 and 5 by its third, clause 5 by its
fifth, clauses 3 and 5 by its sixth, clause 5 by its seventh, eighth and ninth, clause 5
by the PJ2 close-out (2026-10-20: every `*-moved` step checks its original path), and clause 5
by the second close-out (2026-10-20: the entry check follows the recorded step, and every
identity carries its birth time, the removal record `version = 2`), and clause 5 by the
third close-out (2026-10-21: a completed removal record outlives the unregistration). Package PJ1
of the
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
   replaced whole through a temporary file and a rename under the Project Store's lock; an
   unfinished `--delete-worktree` of it is held by its removal record beside that file (clause
   5). The projection `af/project@1` is a function of the log and `workspaces.toml`.
4. **Lazy, token-free registration.** `af onboard --apply`, `af task plan`, `af task start`,
   `af review run` and every `af project` command register the Project when it is new and the
   invoking checkout as a Workspace when it is new: origin `adopted`, its Store the default Task
   Store for that checkout or the explicit `--state`, and an automatic label made from the
   checkout's directory name by one deterministic rule: every byte outside ASCII letters, digits,
   `-` and `_` becomes `-`; leading `-` and `_` are stripped; the result is cut to 119 bytes,
   leaving room for `-` and eight hex digits of the collision suffix within the 128-byte
   safe-name limit; an empty result is `checkout`. So `repo.v2` is `repo-v2`, `café` is `caf--`
   and `.hidden` is `hidden`. An automatic label that collides with any registered label,
   explicit or automatic, gains `-` and the first eight hex digits of the Workspace ID (a longer
   one, such as a delivered Task ID, is first cut to 119 bytes); when that is taken too,
   registration fails with a message naming `--label`. A label the operator gives (`--label`,
   `workspace new LABEL`) keeps strict validation and is never rewritten.
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
   succeeded undoes the earlier ones newest first (registration, worktree, the directories made
   above it, branch ref, Workspace directory) and reports what it undid, or exactly what remains
   when an undo fails; so every refusal leaves nothing of its own behind and the same command
   succeeds once its cause is gone. Every deletion of an undo rests on ownership evidence
   (ADR-0031), by four rules. First, the destination directory is recorded as made only when
   this command's own `create_dir` succeeded on a path that did not exist, together with its
   identity (device, inode and birth time), and `git worktree add` then adds into that empty
   directory; a path that appears between the checks and the add is a refusal with nothing of it
   deleted. The
   missing directories above it are created one at a time from the deepest existing ancestor,
   and only those this command's own `create_dir` made are recorded, each with its identity
   (device, inode and birth time, as every identity below) at creation, never one another
   process made meanwhile; the undo removes
   only those, deepest first, each only while it is still that directory, its identity
   unchanged, and empty. A directory at the same path that is not that one, removed and
   recreated by anyone, is left and reported as
   `the directory <path>, kept because it was replaced since this add created it`, and a recorded
   one that is not empty as `the directory <path>, kept because it is not empty`.
   Second, directory identity and path membership never prove ownership of bytes, and an entry
   of any kind can be another writer's: the undo deletes the destination only when its identity
   is unchanged, every entry inside it, directories as well as files and links, is `<path>/.git`,
   which is absent or a file naming an entry this add owns (one whose `gitdir` names
   `<path>/.git`), a path of the captured manifest equal in kind, mode and content (the
   complete-entry comparison of delivery's rollback, `manifest_is_subset`), or a directory above
   such a path, or at or above a submodule path the add leaves as an empty directory, that holds
   nothing else; any other entry, an empty directory included, keeps the destination and is the
   path the report names. The index this add wrote must also be unchanged: absent or empty, as
   `git worktree add --no-checkout` leaves it, or the commit's tree through delivery's
   cached-index comparison. When `.git` names another entry the
   directory is kept, this add's entries still go, and the report says
   `the directory <path>, kept because its .git names <entry>, not an entry of this add; delete it once no add uses it`.
   When any other comparison fails the worktree is kept whole, its directory, this add's entries
   and the branch it is on, and reported as left behind with the first differing path, in one of
   these lines:

   - ``the directory <path>, kept because it holds `<file>`, which this add did not write; delete it once no add uses it``
   - ``the directory <path>, kept because `<file>` differs from what this add wrote; delete it once no add uses it``
   - `the directory <path>, kept because it was replaced since this add created it; delete it once no add uses it`
   - ``the worktree <path>, kept with its administrative entry worktrees/<name> because its index differs from <commit> at `<file>`; remove it with `git worktree remove --force <path>` once nothing in it is needed``,
     followed by `the branch refs/heads/<name>, kept because the worktree <path> kept above is on it`

   Third, the branch this add created is deleted with
   `git update-ref -d <ref> <oid>`, giving the OID it was created at, so a branch that moved
   meanwhile is kept and reported as
   `the branch refs/heads/<name> moved since this add created it and is kept`. Fourth, the
   worktree is recorded as made before `git worktree add` runs, with the names under `<common
   dir>/worktrees/` noted then, so an add that fails or reaches its deadline after creating the
   worktree's `.git` file or its administrative entry is undone too. The undo deletes only the
   new administrative entries positively attributable to this add: an entry is this add's only
   when its `gitdir` names `<path>/.git`. Each is deleted directly and reported as `removed the
   administrative entry worktrees/<name>`, or as remaining while git still lists the path or such
   an entry survives. Git creates an entry, locked, before it writes the entry's `gitdir`, and a
   concurrent `git worktree add` of another path with the same basename creates one too, so a new
   entry named after the path (its basename, alone or followed by digits) that is not this add's
   is never deleted. The undo reports each such entry as left behind with exactly one of these
   recovery lines:

   - `<common dir>/worktrees/<name>: a locked entry without a gitdir, created during this add and possibly another add's; delete that directory once no add is running (git worktree prune keeps a locked entry)`
   - `<common dir>/worktrees/<name>: an entry without a gitdir, created during this add and possibly another add's; run git worktree prune once no add is running`
   - `<common dir>/worktrees/<name>: an entry created during this add whose gitdir names <that path>, another add's`

   These reports belong to the undo's report whether the worktree's undo succeeds or fails, also
   when the final `git worktree list --porcelain` check fails or git still lists the path, so the
   operator always sees each surviving entry's path and its recovery line; the report never says
   nothing was left behind while such an entry exists. `git worktree prune` never runs during an
   undo: it would also remove stale entries of other worktrees, beyond this add's ownership
   (ADR-0031). The final `git worktree list --porcelain` check stays: the path must no longer be
   listed. A step of which nothing exists is not reported. It makes no commit and no push.

   `workspace add PATH` adopts an existing checkout of the same Project, refusing one of another
   Project with both IDs. `workspace remove LABEL` unregisters; it never removes a Store, and
   when the Store holds a Task that is not collected it says so and names `af task gc`.
   `--delete-worktree` never runs `git worktree remove`, whose cleanliness check hashes the
   worktree's files through the repository's clean filters (ADR-0001). It removes only a linked
   worktree that is not locked and whose `.git` file names an entry under `<common
   dir>/worktrees/` whose `gitdir` names the worktree. The worktree is clean when its index and
   its files both equal its `HEAD` commit. The index is compared through delivery's supervised,
   filter-free cached-index comparison (`git diff-index --cached` with external diff and textconv
   disabled, as `index_matches_head` makes it), so a staged addition, modification or deletion
   and an unmerged entry each make it dirty. The files are compared with the commit's tree in
   content and mode through the committed capture `workspace new` materializes from
   (`review_source_git`) against the files on disk, any file the tree lacks, ignored or not,
   other than git's own `.git` making it dirty; no filter, hook, diff driver or other program the
   repository configures runs, and every git call runs under the supervision below. A dirty
   worktree is refused with the first such path, and nothing is changed. A clean worktree is
   removed by deleting its directory and then that worktree's own administrative entry, nothing
   else; its branch is kept.

   Names prove nothing; a record does. Before `--delete-worktree` moves or deletes anything it
   writes the Workspace's removal record in the Project Store, beside `workspaces.toml` and never
   in a repository: `removals/<workspace id>.toml`, `version = 2`, machine-written and replaced
   whole through a temporary file and a rename under the Project Store's lock. It holds the
   registered checkout path; the identity of the checkout and of its administrative entry as
   inspected; for each, the container this removal named and that container's identity once
   made; and the step each reached (`inspected`, `container`, `contained`, `deleting`, `deleted`,
   `done`). It is rewritten before each next step. A completed record, every directory `done`,
   outlives the unregistration: the Workspace is unregistered (`WorkspaceRemoved@1` appended,
   then its paths dropped from `workspaces.toml`) while the record stays, and only then is the
   record deleted, so no crash leaves a registered Workspace whose removal holds no identities,
   and the Workspace is unregistered exactly once. Recovery reconciles the three idempotently
   before it deletes the record, never inspecting the path again: a rerun that finds a completed
   record of a registered Workspace touches nothing on disk, unregisters it and deletes the
   record; a rerun of `workspace remove LABEL --delete-worktree` whose label names only a removed
   Workspace with a completed record drops that Workspace's paths when a crash left them, deletes
   the record and reports `removed Workspace <label> (<id>)` with `it was already unregistered`,
   appending no second `WorkspaceRemoved@1`; and a registration of the same Workspace ID again
   (`workspace add`, `workspace new`) settles such a record before its own event, so the new
   Workspace's removal inspects afresh. Only a completed record of a Workspace the log no longer
   registers is deleted that way; any other record stays for its removal. A worktree created at
   the registered path after such a crash is therefore never inspected, claimed or deleted by
   the retry. `workspaces.toml` holds no removal state and stays version 1 (ADR-0145), and the
   record keeps version 2: its shape is unchanged, only when it is deleted.

   An identity is a directory's device, its inode and its birth time in nanoseconds since the
   Unix epoch, spelled `<device>:<inode>:<birth time>`: a file system may hand a freed inode
   number to the next directory it creates (ext4 does, at once), so device and inode name a slot,
   and the birth time, which `rename(2)` keeps and a later directory does not share, names the
   directory. It is read with the inode it belongs to (`statx` on Linux, `st_birthtime` on
   macOS), never replaced by the change time (a rename changes it) or the modification time
   (anyone may set it). A file system that reports no birth time leaves the identity incomplete,
   and the removal is refused at inspection with nothing changed:
   `<path> is on a file system (<type>) that reports no birth time, so af cannot tell it from a file created later at the same inode and leaves it`.
   Version 2 is the first record whose identities carry the birth time; a version-1 record is
   refused as a superseded version (ADR-0145), and so is an identity without a birth time. The
   undo of `workspace new` above compares the same identities.

   Each directory is claimed before it is judged or deleted. The removal names a container
   beside it, `<name>.af-removing-<pid>-<16 random hex digits>`, records that name, creates it
   with `mkdir` mode 0700 (a name that exists is drawn again), records its identity, opens it
   without following a link and, only while `fstat` of that descriptor equals the recorded
   identity, renames the directory into it with `renameat` relative to that descriptor, never
   over a name that is taken. It then finds the directory there by its recorded identity. A
   container replaced between its creation and the rename is never used and is reported; a
   directory that took the path between the identity check and the rename is put back, never
   over a name taken again. The checkout's commit is read through its administrative entry as
   git's directory, never through the checkout, before the claim; the claimed checkout is judged
   in its container, its index through the cached-index comparison above and its files by a
   descriptor-based, no-follow traversal against that committed capture. A dirty one is put
   back at its path and refused with nothing changed; when another directory took that path
   meanwhile it stays in its container, and the message says where. Only the claimed directory
   is deleted, through descriptors, a link unlinked and never followed and no permission
   changed; the container then goes with `rmdir`. A container holding anything else keeps it and
   is reported with the first such path, and the Workspace stays registered until an operator
   clears it. A replacement found at the registered path before the claim is kept and the
   removal refused with nothing changed; one found there after the claim is kept and reported.
   The administrative entry goes the same way once the checkout is gone, claimed only while its
   identity is the recorded one. Whatever step the record holds, an entry is read before it is
   moved into its container and again when it is found there, and the checkout is claimed and
   judged only while its entry passes the same reading: the entry's `gitdir` must name
   `<path>/.git` as git resolves it from the inspected path, and it may hold no `locked`. One
   that fails either is left where it is, reported as
   `its administrative entry <entry> is kept: its gitdir names <other> now, not this worktree`
   or `its administrative entry <entry> is locked, so it is kept; …`, and the Workspace stays
   registered. A missing `gitdir` is no proof: such an entry is left and reported as
   `its administrative entry <entry> is kept: its gitdir is missing, so it is not known to name this checkout`,
   and the Workspace stays registered until an operator clears it. Only an entry found in its
   container once its deletion was recorded (`deleting`) may lack it, because that deletion
   removes the entry's files in name order, `gitdir` among the first; a `gitdir` still there
   must name `<path>/.git` all the same. A `gitdir` that cannot be read (permission, or bytes
   that name no path) is never "no entry": it may name the checkout, so the removal stops with
   that file and the read error and does not unregister. When the entry's container cannot be made or
   the move fails once the directory is gone, the command fails with

   `removed the worktree <path>; its administrative entry <common dir>/worktrees/<name> remains: <os error>; fix it and run the command again`

   (with `; so does the empty directory <container> made to receive it` before `; fix it` when
   that empty container could not be removed either), and when the deletion after the move
   fails, never naming the original path, with

   `removed the worktree <path>; its administrative entry remains at <container>/<name>: <os error>; fix it and run the command again`

   and the Workspace stays registered. A rerun acts through the record and the file system
   together; the recorded step alone proves nothing. Before the rerun claims, judges, deletes or
   moves anything of a live directory, or reads the checkout's commit, it makes one entry check,
   bound to the entry the record holds and never to whatever sits at its path; which check the
   directory needs follows its recorded step and where the entry is now. The checkout is live
   until its deletion is recorded `deleted`; the entry until its own deletion is recorded
   `deleting`. For the live checkout, and for the live entry, identity, `gitdir` and lock are
   checked: the administrative entry is opened without following a link, at its own path for
   the checkout's rerun and wherever its recorded identity is found for its own, and `fstat` of
   that descriptor must equal the identity the record holds for it. A directory, link or file of
   another identity at the entry's path, its recorded identity in neither its path nor its
   container, is a replacement: it is never read, and the check fails as
   `its administrative entry was replaced since inspection: <entry> is not the directory this removal inspected, so it is left alone`.
   Only an identity match goes on to read its `gitdir` and `locked`, through that same
   descriptor: its `gitdir` must name `<path>/.git` and it may hold no `locked`. An entry whose
   deletion is recorded (`deleting`) and that is found by its identity in its container resumes
   its deletion without its `gitdir`, which that deletion may have removed (identity and lock
   still checked, and a `gitdir` still there must name `<path>/.git`); one found in neither its
   container nor its path has been deleted, and whatever its path holds now is not it: no check
   reads that, and the rerun goes on to the recorded locations. An entry recorded `deleted`, and
   a checkout recorded `deleted`, has nothing left to check: its rerun verifies the recorded
   container (its identity, and that it is empty) and advances to `done`. A failed
   check stops the removal with that condition and leaves everything as found: the checkout
   at its path, or in its container, reported there by its claim name as

   `the worktree <path> is kept at <container>/<name>, where this removal moved it, because <condition>; the Workspace stays registered; move it back to <path> to keep it`

   so an operator can restore it or finish with the same command, the entry, the record and the
   registration untouched. A failed precondition, the entry check's or a step's own below,
   never deletes the removal record, whatever the step and also when nothing has moved yet: a
   failed entry check leaves it byte for byte as it was, and a step's own leaves it holding at
   most that step taken back to `inspected` where what it named was never used, so the next run
   finishes from the identities it holds instead of inspecting afresh. Apart from a completed
   removal, the record is deleted only while nothing has moved and the worktree itself refuses
   the removal (its changes, a commit that cannot be read, a move the operating system
   refuses), or when the run that wrote it, inspecting afresh, finds a failed check of what it
   has just inspected: then nothing of that run is left behind. Only after the entry check does
   the rerun re-establish the precondition of the step recorded for the directory, and a
   precondition that fails never advances the step:

   | Step | What the rerun verifies on the file system before acting, after the entry check |
   |---|---|
   | a live directory (the checkout before `deleted`, an entry before `deleting`) | The entry check: the administrative entry, opened at its path without following a link (or found by identity in its container, for its own rerun), has the identity the record holds, device, inode and birth time; another identity there is a replacement, never read, and the removal stops with nothing deleted, moved or unregistered. Only then are its `gitdir` and `locked` read. |
   | an entry at `deleting` | Found by identity in its container, its deletion resumes without its `gitdir`, its lock still refused; found in neither its container nor its path, its deletion finished, and what holds its path now is not it, kept and reported; back at its path by identity, it is live again and checked as above. |
   | a directory at `deleted` | No entry check: nothing of it is left to read. Its recorded container is verified (its identity, and empty) and removed, and it advances to `done`. |
   | every step | A failed precondition, the entry check's or the step's own, leaves the removal record: never deleted, byte for byte as it was after a failed entry check, so the next run acts on the recorded identities. |
   | `inspected` | The registered path holds the recorded identity: nothing there is a directory gone without this removal, another identity a replacement, kept. The entry's `gitdir` names `<path>/.git` and it holds no `locked`, for the checkout's claim as for the entry's own. Only then is a container named. |
   | `container` | Nothing is proved: the name was recorded before the container was made, so a directory found there is reported and left, and the directory is `inspected` again. |
   | `contained` | The container at the recorded name has the recorded identity, or it is reported and kept. The directory is found in it by identity, or at its path by identity, and then moved in only while the container is empty and its entry passes the reading above. In the container the checkout is judged (its entry's `gitdir` and lock, its index, its files) and an entry read again before either is deleted. With the container gone, the registered path decides as for `deleting`. |
   | `contained`, moved (`checkout-moved`, `entry-moved`) | The directory found in its container by identity, moved there by an earlier run or by this one, leaves its original path free. A directory at that path now is a replacement that stops the removal before anything is judged or deleted: the recorded directory stays in its container, nothing is deleted, the record stays as it is and the Workspace stays registered, reported for an entry as `a new administrative entry appeared at <path> since this removal moved the recorded one; clear it or move the recorded entry back, then run the command again` (prefixed with where the recorded entry is kept), and for the checkout as `the worktree <path> is kept at <container>/<name>, where this removal moved it, and the Workspace stays registered: a new directory appeared at <path> since this removal moved the recorded worktree; clear it or move the recorded worktree back, then run the command again`. Cleared, or with the recorded directory moved back to its path, the same command finishes the removal. |
   | `deleting` | The directory is found in its container by identity, an entry there holding no `locked` and naming `<path>/.git` by any `gitdir` still there, and its deletion finishes. Otherwise the registered path decides: the recorded identity there is the directory moved back, claimed again from `inspected` and judged again; only when neither the container nor the path holds the recorded identity is it `deleted`, and another identity at the path then is not it, kept and reported, the Workspace unregistered once the rest is done. |
   | `deleted` | The directory went by its identity. Its container goes only while it has its recorded identity and is empty; a directory at the checkout's path is kept and reported. A crash after the container's removal and before `done` is recorded finds no container, and the directory is `done`. |
   | `done` | Nothing; the next directory. |

   Once every directory is `done`, the rerun unregisters and then deletes the record, as above. A recorded container that is not the one made is
   reported and left, and a name recorded for a container that was never identified proves
   nothing about a directory found there. A directory that merely looks like a container,
   beside the checkout or among the entries, whether nobody recorded it or it is a worktree or
   an entry whose own name has that form, is never touched and is reported as
   `left <directory>: it is named like a removal's container, but no removal record names it, so it is not this removal's`.
   A rerun's messages begin `the worktree <path> is already gone;` once the checkout's deletion
   is recorded. Without a record a removal is fresh (a completed record is never deleted before
   the unregistration, so a registered Workspace without one has no removal begun): a registered checkout path that no longer
   exists has each entry at its own path whose `gitdir` names `<path>/.git` recorded and removed
   as above, and is otherwise unregistered and reported as already gone. `workspace remove
   LABEL` without `--delete-worktree` is refused while the Workspace has a removal record,
   naming the command that finishes it. Only a refusal that changed nothing says "nothing was
   changed": a dirty worktree, refused with the first differing path, a replaced checkout, an
   unknown checkout, the running checkout, a rerun that meets an unreadable `gitdir`, and a rerun
   stopped by a failed precondition before anything moved, which adds where its record stays:
   `<condition>; nothing was changed: the removal stays recorded in <record>, and the same command finishes it from there once that is resolved`. Exit
   codes are 0, 1 (with `af/error@1` under `--json` and the next action in the message) and 2 for usage. Every
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
   neither written nor read (ADR-0145). For the same reason the Store registry `stores.toml`,
   whose kind set now includes `project`, is `version = 2`: the reader accepts exactly 2 and the
   writer writes 2. A version-1 file is refused with one message naming the recovery: delete
   `$XDG_STATE_HOME/af/stores.toml`; Stores register again as commands use them, and Stores
   under the default roots were never in it. Until then, recording a Store warns once on stderr
   and `af storage` reports the registry unreadable.
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
  worktree's directory, `.git` file and administrative entry, or only an entry with its `gitdir`
  and `HEAD`, after which the same label succeeds; reports, and leaves, a locked entry without a
  `gitdir`, and both entries of a concurrent add of the same basename (one whose `gitdir` names
  another path, one without a `gitdir`), after which the operator's `git worktree prune` and the
  same label succeed; removes a clean worktree and its administrative entry with
  `--delete-worktree` and refuses a changed, re-moded or untracked file untouched, leaving no
  container and no record, while a clean filter the repository configures never runs; with a
  read-only `worktrees/`, reports a
  `--delete-worktree` that removed the directory but not the entry with the exact message, keeps
  the Workspace, and finishes it on the rerun once the permission is back, and unregisters a
  checkout already gone; reports the locked entry with its recovery line when the undo's final
  `git worktree list` fails; refuses a destination that appears after the checks with nothing of
  it deleted; keeps and reports a directory whose `.git` file names another add's entry, and a
  branch that moved during the add, undoing the rest; fails within the deadline, leaving no child,
  when a fake `git` sleeps; succeeds on the first `af task plan --workspace <checkout directory
  name>`; and keeps `git status --porcelain --ignored` empty and the checkout's files unchanged
  after every `af project` command and `af task plan --workspace`. It refuses
  `--delete-worktree` of a worktree whose files equal `HEAD` while its index holds a staged
  modification, or an unmerged entry, with nothing deleted; keeps and reports the destination of
  a failed add that a fake `git` wrote a sentinel file or an empty `sentinel/` directory into,
  replaced, or changed a tracked path of, and the whole worktree, entry and branch with it, of
  one that staged a change into the new index; keeps a missing parent another process created
  before the add while removing the one the command created, and leaves and reports a created
  parent a fake `git` removed and recreated; names the entry inside its container
  `<name>.af-removing-<pid>-<hex>` when it cannot be emptied and finishes it on the rerun by
  its recorded identity, also once its `gitdir` is gone; stopped as a crash would right after
  the entry's unlink (`entry-unlinked`), part way through its deletion once its `gitdir` is gone
  (`entry-partial`), after the container's removal (`entry-rmdir`, `checkout-rmdir`) and after
  the checkout's unlink (`checkout-unlinked`), finishes on the rerun and unregisters once, and
  after `entry-unlinked` or `entry-partial` also while a new entry another add made holds the
  entry's path, which it keeps
  (`a_rerun_after_the_entrys_unlink_or_part_of_its_deletion_finishes_beside_a_new_entry`);
  refuses `--delete-worktree` on a file system without birth times, nothing changed and no
  record written (`a_file_system_without_birth_time_refuses_a_removal_and_changes_nothing`, the
  debug build's `AF_TEST_NO_BIRTH_TIME`); leaves untouched an empty directory at
  the predictable `<name>.af-removing-<pid>` of the removing process; refuses, still
  registered, a rerun that meets an unreadable `gitdir`, finishing once it is readable; stopped
  as a crash would (a debug build's `AF_TEST_PROJECT_REMOVAL_STOP`) after every step from the
  naming of the checkout's container to the entry's `done`, among them after the checkout's
  deletion and before the entry's move, holds both identities and the steps in the record,
  refuses a plain `workspace remove`, and finishes on an unchanged file system with nothing left
  and exactly one `WorkspaceRemoved@1`; stopped after `checkout-moved` or `checkout-deleting`
  with the checkout moved back to its path and its container removed, claims it again and
  removes it when it is clean, puts it back and refuses it when it is not, and keeps a
  replacement at the path and the moved checkout, never leaving a checkout whose `.git` names no
  entry; stopped after every step at which the entry exists, with the entry moved back to its
  path and then locked, pointed at another checkout or stripped of its `gitdir`, leaves it so,
  names the condition, keeps the Workspace registered, and finishes once the operator clears it;
  stopped after `checkout-moved` or `entry-moved` with a new directory at the original path,
  keeps the recorded directory in its container, deletes nothing, keeps the record and the
  Workspace and names the path, and finishes once the new directory is cleared or the recorded
  one moved back
  (`a_directory_at_the_original_path_after_any_moved_step_stops_the_removal`);
  keeps and reports a checkout another process replaced after the
  inspection, changing nothing, and a directory another process made at the path after the
  claim; keeps and reports a foreign file put into a leftover container until the operator
  removes it; leaves and reports unrecorded directories named like containers, sentinels
  intact; and removes a worktree whose own name looks like a container, the removal of its
  sibling leaving it alone.
  Unit tests undo every kind of
  step in reverse order, including a partial worktree whose entry a stopped add left locked,
  delete only the entry whose `gitdir` names the path, report a locked and an unlocked entry
  without one with their exact recovery lines, keep and report a branch that moved, and never
  prune; keep a destination holding an empty directory, a directory inside a submodule's or one
  where the manifest has a file, while the directories above the manifest's paths pass; remove
  an ancestor only while its identity is unchanged; read back a removal record and refuse
  another version (the superseded version 1 among them), Workspace, identity form (one without
  its birth time, or with a zero one), relative path or step without its container; refuse a
  claimed tree whose identity has the same device and inode and a later birth time;
  draw a fresh container name each time; delete a claimed tree through descriptors without
  following a link or changing a permission, refusing another identity; compare a claimed
  worktree with its manifest through descriptors, naming the first re-moded, untracked or
  changed path and skipping only git's own `.git` file; resolve an entry's `gitdir` as git
  reads it; read an entry's `gitdir` and lock wherever it is, keeping it when its `gitdir` is
  missing except during its own deletion, names another checkout, cannot be read or it is locked; test which administrative entries belong
  to an add; give the stated automatic
  label for `repo.v2`, a name with spaces, a Unicode name, a leading-punctuation name, a 200-byte
  name and a collision with an explicit label, none over 128 bytes; refuse a version-1
  `stores.toml` with its recovery and write version 2, keeping every entry and kind; and find no
  unsupervised process start under `crates/af/src/projects`.
  `crates/review-core/tests/schema_parity/project.rs` pins the three events and two documents,
  and `crates/af/tests/it/cli_surface.rs` the help surface.
