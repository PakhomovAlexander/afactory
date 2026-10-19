# Project: the entity af works on, its Workspaces and its Surveys

**Status:** proposed 2026-10-19. **Baseline:** kernel `main` bf027de6 (after v0.12.0-rc.4).
**Execution model:** every package in §5 is one Task file kept outside the repository, compiled
and run through `kernel/implementation-reviewed` from `.af/task-catalog.toml`, with its Task
files, bindings and Store under `$XDG_STATE_HOME/af/workstreams/project/`. Package IDs are local
planning IDs, not issue or PR numbers. This note is direction, not shipped behaviour; where it
and an ADR disagree, the ADR wins.

The owner's requirement, in their words: af should have analytics about the projects it works
on and be the source of truth about a project. That needs an `af project` command prefix, a new
entity **Project**, **Workspaces** as forks of the Project per workstream, and analytics (lines
of code, history, decisions made, contributors) collected automatically when a project is
onboarded, available during a Task to the Workers that need it and to no other, under the
minimum-context value, and refreshed asynchronously by a dedicated entity.

## 1. Problem

Four gaps. The figures are from this repository and this machine, measured 2026-10-19.

**af has no entity for the thing it works on.** "Project" names three unrelated things today.
`.af/af.toml` carries `[project] name` and `min_af`, a declaration the kernel validates and
prints. Every Snapshot origin carries a `repository_id`, the sorted root commits of the
repository, which delivery and Campaign authority compare and nothing else reads. The
self-optimizer's `.af/optimization-sources.toml` requires a `project_id` digest the operator
types in by hand. The warm build cache is keyed by "the repository" through a fourth derivation.
Nothing joins them, and no record says "this Task and that Campaign were work on the same
project".

**State is keyed by checkout path, not by project.** The default Task Store is
`$XDG_STATE_HOME/af/task/local/<sha256(checkout path)[..16]>`, review state is
`af/review/local/<sha256(path)>` and Campaigns sit under `af/review/campaigns/<c-id>` by label.
Each git worktree therefore gets its own Store, which is right for isolation and wrong for
answering any question about the whole. On this machine afactory's own work is spread over 8
default Task Stores, 3 Campaign Stores, 18 hand-made directories under
`$XDG_STATE_HOME/af/workstreams/` (each with its own `tasks/`, `bindings.toml` and `state/`,
the convention `docs/design/research-pipelines.md` §3 describes) and 39 entries in the Store
registry. `af task list` shows one Store. "What has af done on afactory, and what did it cost"
is a shell loop over directories whose names are hashes.

**Workers start blind, and pay to orient on every Attempt.** The kernel's implementer receives
`requirements` and `source` and nothing else (`.af/task-packages/kernel/implementer/`), and its
instructions tell it to "read `AGENTS.md`, `CONTEXT.md` and the ADRs under `docs/adr/` before
judging or changing anything". That is 370,730 lines in 1,384 files, 139 ADRs and 69 defined
terms, rediscovered with tool calls in every Attempt, because every Attempt is a new
conversation (Warm Set). The spend is real but unmeasured; package PJ3 measures it before any
context is added, as the values require.

**Nobody can ask af about the project.** How big it is, how fast it changes, who works on it,
what has been decided, what af has spent on it: none has a kernel answer. History Capture
(`af self optimize`) is the nearest thing and is deliberately narrow: economics observations,
operator-configured sources, report-bound.

## 2. Terms

Proposed `CONTEXT.md` entries. Existing terms keep their meanings; the new ones are chosen so
that each either separates cleanly from a neighbour or merges a habit that already exists.

**Project**:
The durable identity of one repository across every clone, worktree and fork af has seen:
identified by its `repository_id` (the sorted root commits every Snapshot origin already
carries), labelled by `[project] name` in `.af/af.toml`, and owning one Project Store under
`$XDG_STATE_HOME/af/project/<id>/` that indexes its Workspaces and holds its Surveys.
_Avoid_: the `.af/` directory, which is a declaration the Project's tree carries, not the
entity; a checkout path, which is a Workspace; a Snapshot, which is one tree the Project had.

**Workspace**:
One registered checkout of a Project for one line of work: a worktree or clone, its Task Store,
its Task files and its bindings file, under one label. Every default Task Store af has ever
created is the Store of an unlabelled Workspace, adopted as such.
_Avoid_: **Warm Workspace**, a per-node template root inside one Campaign that is never a
checkout; and "workstream" as a second entity — the Workspace's label *is* the workstream's
name, and the owner's `workstreams/<name>/` directories are Workspaces made by hand.

**Survey**:
One immutable, kernel-measured description of a Project at one exact head: tree, languages,
history, contributors, decisions, vocabulary, checks and af activity, each section bounded by
declared policy and marked complete, partial or unavailable with its reason. A model never
writes one, and a complete Survey is reused while its survey key (§3.3) is unchanged and no
`--force` asks for another.
_Avoid_: **Measurement**, one declared command run against one Snapshot; **History Capture**,
the optimizer's economics observations; **Report**, one reviewer's claim; and the README.

**Surveyor**:
The kernel-installed, token-free operator that records Surveys: synchronously at onboarding and
on `af project refresh`, as one detached bounded process after a Task or Campaign run,
single-flight per Project, and never while the newest Survey is complete and the survey key
(§3.3) is unchanged, unless `--force` asks.
_Avoid_: a daemon — it is one process that reads, writes one record and exits; and a Worker — it
has no Provider, no Attempt and no budget.

**Project Brief**:
The bounded, named selection of Survey sections that a pipeline's `project` input port carries
into a Worker: declared as a variant in project policy, rendered deterministically from one
exact Survey at plan time, pinned by artifact ID in the Execution Plan and listed in the
Attempt's context manifest.
_Avoid_: the Survey itself, which never enters a prompt; ambient context; a prose summary a
model wrote.

## 3. Design

### 3.1 Identity and the Project Store

The Project ID is `sha256:` + hex(SHA-256(`af/project-id@1\0` ‖ `repository_id`)), the
domain-separated digest form the Campaign ID already uses. It is a digest on purpose: the
self-optimizer's `project_id` fields (`af.optimization-sources/1`, `af.optimization-history/1`)
already require exactly this shape, so the Project ID is pasted into them unchanged and the three
identities of §1 become one. Those schemas keep `project_id` required and nothing here changes
them (ADR-0145); letting the optimizer resolve the Project ID itself would be a new
`optimization-sources` version in a later package.

Root commits as identity have consequences the design accepts and names:

- A fork shares the Project of its origin. That is the intent: a fork is a Workspace.
- A shallow clone's boundary commit is not a root. Registration is refused with a message naming
  `git fetch --unshallow`; nothing is recorded.
- A repository with no commits has no Project yet. `af onboard --apply` still writes the authority
  bundle it writes today, which never needed a commit; registration and the first Survey wait for
  the first commit, reported as `no commits yet` on stderr, and the next lazy registration picks
  the Project up.
- Merging an unrelated history later changes `repository_id`, and so the Project ID. af reports
  the former Project beside the new one; joining them is not in v1.

The Project Store reuses the `review-store` layers every other Store has (`cas/` and the
SQLite event log; architecture.md, "The store"). It lives at
`$XDG_STATE_HOME/af/project/<hex>/`, and the Store registry records it so the Storage Budget
sweep reaches it (ADR-0144). Beside the Store, outside every durable record, sits the
machine-local `workspaces.toml`: the checkout paths, Store paths and bindings paths of this
machine's Workspaces, recorded best effort exactly as `stores.toml` records Stores. A durable
record names a Workspace by ID and label only; no host path ever enters one.

Registration is lazy and token-free. The first `af onboard --apply`, `af task plan`,
`af task start`, `af review run` or `af project` command run inside a checkout registers the
Project (if new) and that checkout as a Workspace (if new). Nothing is written inside the
checkout: `.af/` stays declarations only (ADR-0032, ADR-0115).

### 3.2 Workspaces

A Workspace record holds its ID (`w-` + hex(SHA-256(`af/workspace-id@1\0` ‖ Project ID ‖ `\0`
‖ canonical checkout path))[..16], the same derivation the default Task Store already uses for
its directory name), its label, its origin (`adopted`, `created` or `delivered`) and when it
was registered. The paths live in `workspaces.toml`.

```sh
af project workspace new research --branch agent/research      # a fork for one workstream
af project workspace add ../afactory-wt-research --label research --state "$W/state"
af project workspace remove research                            # unregister; keeps the worktree
af project workspace remove research --delete-worktree          # also removes a clean worktree
af project workspaces --json
```

`workspace new LABEL` runs `git worktree add` at `<toplevel>-wt-<label>` beside the checkout
(the owner's existing naming) on branch `af/<label>` from the invoking checkout's HEAD, unless
`--path`, `--branch` or `--from` say otherwise, and creates
`$XDG_STATE_HOME/af/project/<hex>/workspaces/<label>/` holding an empty `state/` Store, an empty
`tasks/` directory and a `bindings.toml` template that names no account. It makes no commit and
no push: the same local-only boundary delivery keeps (ADR-0031). `workspace add PATH` adopts an
existing checkout with its default Store, or the `--state`, `--bindings` and `--tasks` paths
given, which is how each of the owner's `workstreams/<name>/` directories becomes a Workspace
without moving a byte. `workspace remove` never deletes a Store that still holds an uncollected
Task; it names `af task gc`. `--delete-worktree` requires the worktree's index and its files both to equal its HEAD
commit, through delivery's supervised, filter-free cached-index comparison and the kernel's
committed capture, refusing a staged, unmerged or differing entry without moving a byte; it never calls `git worktree remove`,
whose cleanliness check runs the repository's clean filters (ADR-0001), and it deletes the
directory and the worktree's own administrative entry itself.

`--workspace LABEL` on every `af task` subcommand that takes `--state` resolves `--state` (and,
for `plan` and `start`, an absent `--bindings`) against the Workspace; `--workspace` together with
`--state` is a usage error, exit 2, as ADR-0151 clause 6 decides. `af review run --workspace` and
a relative `--file` resolved against the Workspace are a separate later decision. The worktree `af task deliver`
creates is registered as a Workspace of origin `delivered`, labelled by the Task ID, after
delivery succeeds and best effort, so `af project workspaces` lists it and
`af project workspace remove --delete-worktree` can retire it. Delivery's own contract is
unchanged.

### 3.3 Surveys

A Survey is `af/ProjectSurvey@1`: one canonical JSON record, numbers as decimal strings like
every other kernel record, a share being its population's basis points rounded down (`3333`
for one third, `0` for an empty population), with these sections. Every section carries `completeness`
(`complete`, `partial`, `unavailable`) and a reason when it is not complete; the Survey's own
`completeness` folds them.

| Section | Holds | Source | Bound or policy |
|---|---|---|---|
| `identity` | Project ID, `repository_id`, `[project] name` and `min_af`, the af release `.af/af.lock` pins, head commit, Tree Digest, cutoff, policy digest, Surveyor version, git version | `.af/af.toml`, `.af/af.lock`, git | the Tree Digest is the kernel's capture digest of the whole head tree, independent of `exclude` and of every maximum: absent with reason `deadline` when the capture did not finish, never the digest of a part |
| `tree` | files and bytes in total; per top-level directory files, lines and bytes; per language files and lines; the `tests` split | `git ls-tree -l`, blob reads | lines only for text blobs at or under `max_file_bytes` (1 MiB); `[survey] exclude` globs; `[survey] tests` globs; `[survey.languages]` extends the built-in extension map |
| `history` | commits and merges in total and in the last 30, 90 and 365 days; first and last commit time; churn (commits, lines added, removed) per top-level directory over 90 days; hot paths; tags in total and the newest five matching `release_tags`; local branch count | `git log --numstat`, `rev-list`, `tag` | `max_commits` (100,000; beyond it `partial`, oldest commit reached recorded); `hot_paths` (20) |
| `contributors` | per identity: commits, lines added and removed, first and last time, active in 90 days; the top identity's share; human and agent-assisted commit counts and shares | `git log` | `[survey] identity = "name"` (default) or `"email_hash"`: an email is hashed or absent, never stored, and an author name any whitespace-separated token of which is an email address (an `@` between non-empty parts) is replaced whole, before any truncation and under both policies, by `h:` and the first 16 hex digits of its hash; `agent_authors` (`*[bot]`) and `agent_trailers` (`Co-Authored-By` matching `*Claude*`, `*Codex*`, `*GPT*`): a commit is agent-assisted when its author or any `Co-Authored-By` name matches, otherwise human; each commit counts once in that partition and its author keeps the per-identity totals; an empty window makes every share `0` |
| `decisions` | per decision record: number, title, status, date; counts by status | the `decisions` pattern, default `docs/adr/*.md`, then the name rule (four ASCII digits and a dash): title from the first H1; status and date from the first line that starts with `**Status:**` or `Status:`: the status is the first of `proposed`, `accepted`, `superseded`, `deprecated`, `rejected` on that line (`accepted; superseded in part by` is `accepted`; `amended` becomes a flag when the line says so), the date the first `YYYY-MM-DD` on it, each `unknown` when absent or malformed | `max_entries`; a Brief variant picks the newest N (§3.5) |
| `vocabulary` | the defined term names and their count | the `vocabulary` file, default `CONTEXT.md`: bold-colon headings | names only; definitions stay in the tree |
| `checks` | declared check names and programs; toolchain declarations present; CI workflow file count | `.af/code-policy.toml`, review pipelines' `[[checks]]`, a fixed file list, `.github/workflows/` | names and programs only, no arguments |
| `activity` (PJ5) | per Workspace: Tasks by outcome, Attempts, chargeable tokens, active time; Campaigns, open and fixed Findings by severity; delivered Tasks; Developer Plan Decisions, Resolutions and Demand waivers; the window `cutoff - 30 days < t <= cutoff`, a Task counted by its completion event, an Attempt and its charge by its settlement event, a delivery and a decision by its recording event, active time as the run spans cut to the window, Finding counts as the current state beside the changes within the window, and `unknown` where a Store cannot say | every Store of the discovery set, read-only: the Workspaces' Task Stores, every Store under the default Task root (`$XDG_STATE_HOME/af/task/local`) and the default review roots (`review/local` and `review/campaigns`), none of which the registry lists, enumerated under the Survey's bounds, and every explicit Store and review-root entry of the registry, deduplicated by canonical path; work whose Store belongs to no Workspace forms the `unassigned` group, and a Store two or more Workspaces name forms a `shared` group keyed by its instance ID that lists their Workspace IDs, counted once in the Project's totals and referenced, never repeated, by each Workspace's line; through one aggregation projection PJ5 adds that joins each Store's validated Task accounting with its Campaign Ledger's current Resolutions and Demand waivers (`af task show --json` alone carries neither); Stores are deduplicated by their recorded identity, a Task counts for this Project only when its recorded source `repository_id` matches, Task IDs are qualified by Store, and a Task without retained source evidence counts as `unknown` | a Store this release cannot read makes its Workspace `unavailable` with the replay error (ADR-0145); Provider IDs and host paths never enter the section |

Lines are newline counts: deterministic, no tokenizer. A hot path names the path the commits
touched; one absent from the head tree is marked `removed` (on afactory today three of the ten
hottest paths of the last 90 days no longer exist). The Survey records `previous_survey_id`, so
`af project history` walks the chain. Identity is the record's artifact ID.

The newest Survey is reused, and nothing recorded, when its *survey key* equals the current one:
the head commit, the digest of `git for-each-ref refs/heads refs/tags` (names and targets), the
policy digest, the Surveyor version and, for `activity`, the activity watermark PJ5 defines: for every
Store of the discovery set its instance ID and the digest of what one read transaction captured
from it, every run prefix's events and its `attempt_wall` rows, so the aggregate and the
watermark come from the same captured bytes (a Store replaced at its path with different events
under equal counts changes the key; a read the Survey deadline cuts marks the Store `partial` and
never enters the key as complete), plus the Workspace membership list. The instance ID is opaque, assigned by the Project Store in its machine-local
`stores` table the first time it meets a Store's canonical path (symbolic links resolved), and
recorded in the Survey in place of the path; the Task Store itself is never written. An alias of
a Store resolves to the same canonical path and counts once; a copy or a move is a new path and
so a new instance, whose Tasks count again until the operator removes it from the set. A new commit with the same tree, a new tag or branch, a policy change, a newer af or
newly recorded work therefore records a new Survey; an unchanged repository does not, however
often runs end. A newest Survey that is `partial` is measured again on the next refresh. The 30,
90 and 365-day windows are relative to the Survey's own `cutoff`, so a reader knows the as-of
time; `af project show` prints the Survey's age, and `af project refresh --force` records
regardless of the key.

Measurement policy, so the same inputs give the same bytes (values §3): commits are walked from
HEAD in `git rev-list --date-order` order, newest first, and cut at `max_commits`; a window
holds a commit when `cutoff - window < committer time <= cutoff`; a merge counts as a commit and
a merge but contributes no churn (`--numstat` runs with `--no-merges`); renames are not detected
(`--no-renames`), so a rename is a removal and an addition; a binary `numstat` entry counts one
file touched and zero lines; ties break by path, then by commit ID. `identity` records the git
version because git decides these numbers.

Cost, measured on afactory (993 commits, 1,384 files): `git log --numstat` over the whole
history takes 0.71 s and `git ls-tree -r -l` 0.02 s; line counting reads about 15 MB of blobs.
The target is under 3 s cold on this repository, and the PJ2 pull request records the measured
number and the record's size (values §3). A Survey is tens of kilobytes.

### 3.4 The Surveyor

The Surveyor runs in three ways, and in none of them does it call a Provider:

1. **At onboarding.** `af onboard --apply` registers the Project and the Workspace and records
   the first Survey synchronously, printing its summary. This is the "collected on project
   onboard automatically" requirement; a second `af onboard --apply` with an unchanged survey key
   records nothing.
2. **On demand.** `af project refresh` records a Survey now and waits; `--json` prints it.
3. **After a run.** When `af task run`, `af task start --execute`, `af task deliver` or
   `af review run` ends, after the Storage Budget sweep, af computes the survey key (the parent's bounded reads: `rev-parse`,
   `for-each-ref`, the policy and the log) and spawns `af project refresh --detached` only when
   the key changed or the newest Survey is not complete, when `[survey] refresh = "after-run"`
   (the default; `"manual"` turns it off).
   That is one bounded process that reads, writes one record and exits, the precedent being
   `af self refresh-check`; the kernel still runs no daemon and polls nothing (values §3, §8).
   It takes the single-flight lock `$XDG_RUNTIME_DIR/af/project-<hex>.lock`, waiting for it up
   to `max_wall_ms` (a refresher that never gets it records that as its failure), and compares
   the survey key again under the lock: it measures only when no complete Survey matches the
   current key, so equal keys coalesce into one Survey and a key that changed while the holder
   measured (a Task settling after the holder captured its watermark, PJ5) is measured by the
   waiter in turn.

Bounds come from `[survey]`: `max_wall_ms` (60,000), `max_commits`, `max_file_bytes`. A bound
hit makes the sections after it `partial` or `unavailable`; the Survey is still recorded. Fixed
maxima bound everything else: `max_paths` (200,000) tree entries, `max_entries` (1,000) items in
any list of a section (directories, languages, hot paths, tags, decisions, terms, contributors),
256 bytes for any display string (a contributor name, a title, a term, a language or a tag
label; a longer one is cut at a character boundary and marked), while identity strings (every
field of `identity`, commit IDs, digests, ref names and paths) are recorded exactly, and
`max_survey_bytes` (1 MiB) for the record. These three are ceilings: a `[survey]` key may lower
one to any positive integer and never raise it, and zero, a value above the ceiling or one that
does not fit are refused when the policy loads, from the project layer and the environment form
alike. Every git stream is read incrementally under the one
deadline, never buffered whole; a list cut at its maximum records the count it saw and the
reason `max_entries`, after ranking, so the entries kept are the same for the same input and
the totals cover the whole population: contributors by commits, then lines added, then identity;
hot paths by touching commits in 90 days, then path; tags by tagger time (annotated) or target
committer time (lightweight), undated last, then name; directories, languages, terms and checks
by name; decisions by number, then path; a Survey that cannot fit even so, or whose exact identity strings alone cannot fit, records
`SurveyFailed@1` with the bytes it needed. A
failure records `SurveyFailed@1` with its reason, `af project show` prints it, and the command
that spawned the refresh keeps its own exit code: a Survey never fails a Task.

The Surveyor reads the checkout with git plumbing (`ls-tree`, `cat-file --batch`, `log`,
`rev-list`, `tag`) under the typed-method rule ADR-0001 set for tree diffs, runs no hook and no
project code, and opens Stores read-only without taking a writer lease. It therefore needs no
sandbox and no Check runtime. A running Task is never affected: its Brief was pinned at plan
time (§3.5), and a refresh writes only the Project Store.

Why no model: the kernel measures, and a model never writes a number the kernel did not record
(ADR-0132). A model-written description of the project is a Report Task's Document, bound to
one exact Snapshot and accepted by an independent verifier (ADR-0133); §6 says how one could
later feed a Brief section without changing this rule.

### 3.5 Project Briefs

`af/ProjectBrief@1` holds the Survey ID and Project ID it was rendered from, the variant name,
the Survey head (commit, and Tree Digest when the Survey has one; preview and explain say
`tree digest unavailable` otherwise), the rendered
sections, the sections dropped with their reasons, and `rendered_bytes`.

Variants are project policy in `.af/af.toml`. Two are built in: `orient`, shown here as a
project would write it, and `none`, which suppresses construction: no Brief artifact, no
manifest entry, nothing on the port (so a port naming it must be optional), and a preview line
`project: none (variant none)`, with or without a Survey; an explicit Task-file binding still
takes precedence over it.

```toml
[brief.orient]
sections = ["tree", "vocabulary", "decisions", "checks", "hot_paths"]
max_bytes = 8192
decisions = 12
hot_paths = 20
```

A project adds variants or overrides these. A Brief's `decisions` holds the variant's newest N
records: by date descending, undated last, then number descending, then path. `contributors` and `activity` are never part of
`orient`: an implementer does not need names or bills.

A pipeline that wants a Brief declares the root port and wires it only where it is wanted:

```toml
[contract.inputs.project]
artifact_type = "af/ProjectBrief@1"
cardinality = "one"
optional = true
covers = []
root_default = "project_brief"
brief = "orient"
[contract.inputs.project.affinity]
kind = "unbound"

[[nodes]]
id = "implement"
# …
[nodes.inputs.project]
kind = "input"
port = "project"
```

The Worker package admits the port in `[signature.contract.inputs.project]` and its
`input.schema.json`, unbound and covering nothing like the root port: a Brief describes the
Survey's head, which may be older than or divergent from the Task's source Snapshot, so its
envelope and the Worker's `ArtifactInput` carry no subject Snapshot ID and the Survey head stays
provenance in the payload; the Snapshot checks of every other port are untouched. A node that does not wire the port sees nothing, so the pipeline, reviewed
and pinned in the lock like every other Worker input, decides which Workers see the project.
That is the "not for all of them, configurable" requirement, and it uses no new mechanism.

The Task's initial capture pins the newest complete Survey of the Project whose `repository_id`
equals the captured source's, copying the record into the Task Store's CAS (or recording that
there is none), with the effective `[brief]` policy as part of the captured policy, before any
pipeline is selected. Resolution is then a pure function of that pinned data wherever
`empty_review_history` is filled today (`normalize_root_inputs`: the Task-file adapter, proposal
validation and a generated pipeline's handoff): render the variant; drop whole sections, last-declared first, until the
Brief's exact serialized bytes, metadata and drop reasons included, fit `max_bytes`, recording
each drop (a section is never cut mid-way, as Worker Notes are dropped whole with a reason); a
variant whose `max_bytes` is below 1,024 is refused when the policy loads, and a Brief whose
mandatory fields alone exceed `max_bytes` is a plan-time error naming the variant and the bytes,
never an oversized Brief and never a silently empty port; publish the Brief into the Task Store's CAS; pin its
artifact ID in the compiled plan. `af task run`, resume, retry and replay never resolve again
(ADR-0117). Without a Survey the optional port is empty and the preview says so:
`project: none (no Survey recorded; run af project refresh)`. A Task file binds the port like any
other bindable root input: ADR-0134 admits a recorded Task result output of type
`af/ProjectBrief@1` (a pipeline that exposes its constructed `project` input as a public output
records one) and refuses the exact-artifact form; an explicit binding suppresses the
`project_brief` construction for that port.

Staleness is reported, not refused, in v1, and it is the consuming Task's, not the Brief's: the
compiled plan records a staleness receipt keyed by the Brief's artifact ID and the Task's source
commit, with `behind_commits` the number of commits from the Survey head to the source commit
when the former is an ancestor of the latter, and `unknown` otherwise or without a committed
revision, computed alike for a constructed and an explicitly bound Brief without rewriting the
bound artifact; `af task plan` and `af task explain` print it beside the port.

What the Worker sees is one more entry in its context manifest — name `project`, required by
`port:project`, the artifact ID and type, bytes and estimated tokens — and `af project brief
--variant orient` prints the exact bytes before any token is spent, the way `af review render`
does for a reviewer. On afactory `orient` renders to roughly 6 KiB, about 1,500 tokens, once per
Attempt. For comparison, `CONTEXT.md` alone is 529 lines, about 8,000 tokens, and the implementer
is told to read it in full today; PJ3 measures the Attempt spend with and without the Brief on
the kernel's own Tasks before `orient` is wired into `kernel/implementer`.

A rendered `orient` for afactory at bf027de6, abridged:

```json
{
  "variant": "orient",
  "head": {"commit": "bf027de6…", "tree_digest": "sha256:…"},
  "tree": {
    "files": "1384", "lines": "370730",
    "by_directory": [
      {"path": "crates",   "files": "582", "lines": "295595", "tests_lines": "114758"},
      {"path": "docs",     "files": "182", "lines": "21256"},
      {"path": "schemas",  "files": "184", "lines": "19500"},
      {"path": "fixtures", "files": "256", "lines": "13309"},
      {"path": ".af",      "files": "87",  "lines": "8041"},
      {"path": "(root)",   "files": "16",  "lines": "5859"},
      {"path": "scripts",  "files": "21",  "lines": "4811"},
      {"path": "brand",    "files": "38",  "lines": "1563"},
      {"path": ".github",  "files": "11",  "lines": "696"}
    ],
    "by_language": [{"language": "rust", "lines": "293270"}, {"language": "markdown", "lines": "25690"}]
  },
  "hot_paths": [
    {"path": "CHANGELOG.md", "commits": "152"},
    {"path": "docs/adr/README.md", "commits": "149"},
    {"path": "crates/review-core/tests/schema_parity.rs", "commits": "124"},
    {"path": "docs/workstream.md", "commits": "115", "removed": true}
  ],
  "vocabulary": {"file": "CONTEXT.md", "count": "69", "terms": ["Task", "Observation", "History Capture", "…"]},
  "decisions": {"count": "139", "newest": [{"number": "0150", "title": "Name the path a status-line error carries", "status": "accepted", "date": "2026-10-18"}]},
  "checks": {"declared": [{"name": "check", "program": "make"}, {"name": "markdownlint", "program": "npx"}], "toolchain": ["rust-toolchain.toml"], "ci_workflows": "4"}
}
```

### 3.6 `af project`

| Command | Does |
|---|---|
| `af project show [--json]` | Identity, name, the Workspaces, the newest Survey's summary and age, the last refresh outcome. `--json` is `af/project@1`. |
| `af project survey [--id ID] [--section NAME…] [--json]` | The newest Survey, or the one named; text renders one table per section. `--json` is the `af/ProjectSurvey@1` record. |
| `af project refresh [--json]` | Run the Surveyor now and wait. `--detached` is the internal form the post-run spawn uses. |
| `af project history [--json]` | The Survey chain: ID, head, cutoff, completeness, bytes. |
| `af project brief [--variant NAME] [--survey ID] [--json]` | The exact bytes the `project` port would carry, token-free; `--json` adds the manifest entry. |
| `af project workspaces [--json]` | Every Workspace: label, origin, path, Store, Tasks and spend (once PJ5 lands). |
| `af project workspace new\|add\|remove` | §3.2. |
| `af project tasks [--workspace LABEL] [--json]` (PJ5) | Every Task across the Project's Workspaces, with outcome and spend: `af task list` over the whole Project. |

Exit codes: 0, 1 for a failure that names its next action, 2 for usage, and the existing
`af/error@1` document under `--json`. `af onboard --apply` grows the registration and the first
Survey (§3.4); no `af project init` exists, because onboarding is already the one way a project
starts.

### 3.7 Records and events

All new shapes carry `kind@version`, are canonical JSON with decimal-string numbers, get a
schema under `schemas/project-*.json` and the parity tests the repository expects, and follow
ADR-0145: a later shape change is a new version.

| Record or event | Where | Holds |
|---|---|---|
| `ProjectRegistered@1` | Project Store | Project ID, `repository_id`, name, `min_af` |
| `WorkspaceRegistered@1`, `WorkspaceRemoved@1` | Project Store | Workspace ID, label, origin; never a path |
| `SurveyRecorded@1`, `SurveyFailed@1`, `SurveyCollected@1` | Project Store | Survey artifact ID, head, cutoff, completeness; or the failure reason; or the tombstone summary |
| `af/ProjectSurvey@1` | Project Store CAS | §3.3 |
| `af/ProjectBrief@1` | Task Store CAS | §3.5 |
| `af/project@1`, `af/project-workspaces@1`, `af/project-tasks@1` | `--json` documents | the projections `af project` prints |
| `workspaces.toml` | beside the Project Store | machine-local paths, best effort, like `stores.toml` |

### 3.8 Configuration

`.af/af.toml` gains two tables in the project layer. They deep-merge like every other table, so
the owner's `.af/af.toml` in the directory above every afactory checkout can set them once for
all of them (`af help layers`).

```toml
[survey]
refresh = "after-run"          # or "manual"
max_wall_ms = 60000
max_commits = 100000
max_file_bytes = 1048576
max_paths = 200000             # these three are ceilings: a project may only lower them
max_entries = 1000
max_survey_bytes = 1048576
exclude = []                   # globs never counted, e.g. ["vendor/**"]
tests = ["**/tests/**", "**/*_test.*", "**/test_*", "**/fixtures/**"]
decisions = "docs/adr/*.md"    # counted when the name starts with four digits and a dash
vocabulary = "CONTEXT.md"
release_tags = "v*"
identity = "name"              # or "email_hash"
agent_authors = ["*[bot]"]
agent_trailers = ["*Claude*", "*Codex*", "*GPT*"]
hot_paths = 20

[survey.languages]             # extends the built-in extension map
"*.pest" = "pest"

[brief.orient]                 # overrides the built-in variant
sections = ["tree", "vocabulary", "decisions", "checks", "hot_paths"]
max_bytes = 8192
```

Patterns use the semantics of the matcher the `.af/af.toml` routes already use (`glob_matches`
in `crates/af/src/project.rs`), which PJ2 re-implements for both by dynamic programming, so a
match costs pattern length times text length and never backtracks (the recursive form takes
billions of steps on `*a` thirty times followed by `b` against thirty `a`s, a failing match
inside the string limits and outside
every deadline); a pattern is at most 256 bytes and a list at most 64 patterns, refused at policy
load beyond that, and the Surveyor checks its deadline between matches: `**` spans segments, `*` a run inside one, `?` one character, and
every other character is literal, brackets included, so `*[bot]` matches `dependabot[bot]` and no
name that merely ends in `t`. There are no character ranges; the decision files are selected by
`decisions` and then by the four-digits-and-a-dash name rule.

`[storage] keep_surveys = 50` is machine-only like the rest of `[storage]`. `AF_SURVEY__REFRESH`
and the other environment forms follow `af help environment`.

### 3.9 Storage Budget and collection

The Project Store is never evicted whole: it indexes the Project's Workspaces, and the Workspace
directories beneath it hold Task Stores and operator files (`tasks/`, `bindings.toml`) that the
Storage Budget inventories one by one under ADR-0144's own rules, as PJ1 implements in
`crates/af/src/storage.rs`. What the budget collects from a Project is Surveys, and PJ2 defines how: Surveys beyond
`keep_surveys` are collected oldest first, only in the Storage Budget's own collection passes (an
explicit `af storage` collection with `--apply`, or the automatic pass when `auto_gc = true`),
under ADR-0144's retention age and recent-use protections, so `keep_surveys` is a target that
protected Surveys may exceed; `af project refresh` collects nothing itself. Collection appends `SurveyCollected@1` (the collected
Survey's ID, head, cutoff, completeness and the totals of `tree` and `history`) and then deletes
the record from the CAS; replay treats a `SurveyRecorded@1` whose Survey a tombstone names as
collected, so `af project history` answers from the tombstone and `af project survey` reports the
newest Survey only while it is uncollected; `previous_survey_id` is a name, never a reachability
edge, so a collected predecessor keeps nothing alive and a chain that reaches a collected Survey
ends at its tombstone. The tombstone is appended before the bytes go, so an interruption between
the two leaves a record the next sweep finishes. Both collection paths, the count and the budget, protect the newest complete Survey and any
held one. Under the machine budget (ADR-0144 clause 4)
every Survey is besides an inventoried entry of kind `survey`, sized by its CAS bytes and last
used at its cutoff, that the budget pass may reclaim oldest first through the same tombstone and
deletion even below `keep_surveys`; protected are the newest complete Survey, any Survey a running
refresh or Brief build holds under the Project Store lock, and the recent-use window ADR-0144
grants every entry; the Project's log, `workspaces.toml` and operator files are never reclaimed.
PJ2 ships the inventory, the byte accounting and the budget-driven tombstone and deletion with
the first Survey writer; PJ4 adds only the detached refresh. Collection never breaks a Task: a plan pins a
Brief in the Task Store, not the Survey in the Project Store.

## 4. Values check

- **Wise token consumption.** Nothing here spends a token: the Surveyor is an operator, the
  Brief is deterministic rendering. The one place tokens change is a Brief entering a prompt,
  and that is opt-in per pipeline node, bounded in bytes, visible in the manifest, and measured
  before the kernel's own packages adopt it (PJ3).
- **As little information in Worker context as possible.** A Brief is a declared, typed,
  bounded port, named in the manifest with its bytes; the Survey never enters a prompt;
  `contributors` and `activity` are excluded from `orient`; a node that does not wire the port
  sees nothing.
- **Performance.** Target under 3 s for a full Survey of afactory, measured and recorded in PJ2;
  an unchanged survey key costs the key's own reads (`rev-parse`, `for-each-ref`, the policy
  and the log) and no measurement; `af project show` reads one projection.
- **Simple and clear.** Three new records with one home each; Workspace merges the
  `workstreams/` habit rather than adding beside it; the Project ID reuses the kernel's existing
  repository identity and the optimizer's existing digest shape; no `af project init`.
- **Deterministic.** A Survey is a function of the head tree, the reachable history, the policy
  and the registered Stores at a recorded cutoff; a Brief is a function of one Survey and one
  variant; plans pin artifact IDs.
- **Unix-native.** Every command has `--json`; state under `$XDG_STATE_HOME/af`, the lock under
  `$XDG_RUNTIME_DIR/af`; no service, no browser; the detached refresh is a child that exits.

## 5. Packages

Each package is one Task, delivered, verified and merged before the next starts, with the
`af task report` block in its pull request (ADR-0142). Every package adds its records' schemas,
parity tests, a changelog note under `changelog.d/` and the ADR named.

### PJ1 — Project and Workspace

Deliverables: the Project ID derivation; the Project Store and its registry entry;
`workspaces.toml`; lazy registration from `onboard`, `task plan|start` and `review run`
(registration only); `af project show|workspaces|workspace new|add|remove`; `--workspace` on
every `af task` subcommand that takes `--state` and on no review command; delivered worktrees
registered; the `ELSEWHERE` paragraph of `review_config::layout`
and `docs/task-execution/task-file.md` pointing Task files at the Workspace directory.
ADR: *Give a repository one Project identity and register its checkouts as Workspaces.*

Acceptance: two worktrees of one fixture repository register one Project with two Workspaces;
a shallow clone is refused with the `--unshallow` message; adopting an existing default Store
changes no byte of it; `--workspace` resolves the same paths as the explicit flags and the
recorded Task is identical either way; the repository's own `.af/` still loads unchanged and the
layout test still passes; `af --help` and `af project --help` stay under 50 ms.

### PJ2 — Survey and Surveyor

Deliverables: `af/ProjectSurvey@1` with `identity`, `tree`, `history`, `contributors`,
`decisions`, `vocabulary` and `checks`; the Surveyor operator with its bounds; `af onboard
--apply` recording the first Survey; `af project survey|refresh|history`; the survey key and
`--force`; `SurveyFailed@1` with its schema and replay and the outcome `af project refresh`
prints; Survey collection under `[storage] keep_surveys` with the replay rules of §3.9, and
Surveys as reclaimable entries of the machine-budget pass. ADR: *Measure a Project into Surveys with a token-free Surveyor.*

Acceptance: a fixture repository with known counts yields a byte-identical expected Survey at a
fixed cutoff; a second run with an unchanged survey key records nothing, while a new tag, a
policy change, a partial newest Survey or `--force` records one; `max_commits` and `max_wall_ms`
produce `partial` sections and a recorded Survey; an early deadline leaves the Tree Digest absent
with its reason, never a partial digest; a repository whose `repository_id` joins seven roots
(286 bytes) keeps it exact; `max_paths = 0`, a value above its ceiling and an overflowing one
are refused at policy load; one third of three commits is the share `3333`; the
failing adversarial pattern (`*a` thirty times then `b`, against thirty `a`s) returns false within
a stated work bound through routing and Survey policy alike, and its matching form is a separate
correctness case; the status
fixture (two dates on one line, a partial supersession, a missing date, no status line) yields the
stated statuses and dates; no author email appears in a record under either identity policy, an email-only author name
and a name carrying an email beside ordinary text included; the measurement-policy fixture (divergent author and committer dates, a merge, a
rename, a binary change, the `max_commits` boundary) yields the stated totals; an oversized commit
and a very large tree stay within the fixed maxima with recorded reasons; collection beyond
`keep_surveys` appends tombstones, frees the bytes, replays `af project history` and recovers
after an interruption between the tombstone and the deletion; budget pressure below
`keep_surveys` reclaims the oldest Survey and never the newest complete one, a held one or the
Project's files; a refresh and a collection at once leave one consistent log; the pull request
states the measured wall time and record size on afactory.

### PJ3 — Project Brief

Deliverables: `af/ProjectBrief@1`; the `project_brief` root default and the port's `brief` key;
built-in `orient` and `none`; the `[brief.*]` policy; plan-time resolution, pinning and the
drop rule; `behind_commits`; the manifest entry; preview and `af task explain` lines;
`af project brief`. ADR: *Carry a bounded Project Brief on a
declared port.*

Acceptance: a Task file binding the port to a recorded Brief output suppresses construction and
the exact-artifact form is refused; one Brief bound into two Tasks at different source commits
gets two staleness receipts; a generated pipeline that introduces the port after capture builds
its Brief from the pinned Survey, a refresh or a removed Project Store in between changing
nothing; a pipeline wiring the port to one of two Workers delivers the Brief to that Worker's
manifest and not the other's; replay after a refresh renders the pinned Brief, not the newer
Survey; a variant over `max_bytes` drops the last section and records the reason; without a
Survey the plan previews the empty port and runs; a `max_bytes` below 1,024 is refused at policy
load, and the all-sections-dropped and exact-boundary cases behave as §3.5 says. The pull request reports Attempt tokens for
three kernel Tasks run twice, with the implementer wired to `orient` and without, before the
kernel's own `kernel/implementer` package adopts the port in a separate pull request.

### PJ4 — Detached refresh and staleness

Deliverables: the post-run spawn behind `[survey] refresh`; the runtime-directory lock; the
`af project show` refresh line; `behind_commits` in preview and explain. ADR: *Refresh Surveys
after runs*, naming the PJ2 clauses it supersedes; PJ2's accepted ADR gains only
a status-line link (ADR-0113 clause 6).

Acceptance: a Task run on a changed head is followed by exactly one new Survey; two concurrent
runs produce one; a run on an unchanged survey key spawns no process (the parent compares the
key; process creation is tested apart from recording); a Surveyor failure leaves the
Task's exit code unchanged and is visible in `af project show`; a Task settling after a running
refresh captured its watermark is measured by the next refresher, which waited for the lock; a
refresher that never gets the lock within `max_wall_ms` records a failure.

### PJ5 — Activity across Workspaces

Deliverables: the `activity` section from every registered Store through the aggregation
projection of §3.3 (validated Task accounting joined with Campaign Ledger views), the discovery
set with the default review roots and the `unassigned` group, Store instance IDs, the activity
watermark, Store deduplication and Project attribution; `af project tasks`; Tasks and spend per Workspace in `af project workspaces`;
Developer Plan Decisions, Resolutions and waivers counted as decisions; the Provider-ID and path
exclusion. ADR: *Fold every Workspace's recorded work into the Project's Survey.*

Acceptance: a fixture with two Workspaces and a Store this release cannot read yields exact
totals for the two and `unavailable` with the replay error for the third; no Provider ID, host
path or email appears in the section; a shared Store, a mixed-Project Store and a duplicate Task ID across
two Stores count each Task once and only for its own Project; a default named and an unnamed
review Campaign and an explicit Store are all discovered, an alias once and a copy as a second
instance; a default Task Store whose checkout is gone and which no Workspace names is discovered
and counted under `unassigned`; a Store two Workspaces name forms one `shared` group counted once; a Task completed 31
days before the cutoff contributes nothing and an unfinished one only its settled Attempts; a
Store replaced at its path with different events under equal counts changes the key; a Resolution and a waiver recorded
after a Task finished change the next `activity` without changing Task spend.

## 6. Not in v1

- **A model-written project summary.** The one acceptable path is a Report Task whose Document
  is bound to an exact Snapshot and verified (ADR-0133); a later package could let a variant
  name a `summary` section that binds such a Document by artifact ID, keeping the Survey free of
  model output.
- **Remote facts.** Pull requests, issues and CI history from GitHub are not read; merge commits
  stand in for integration cadence. Reading a remote needs an authorization like the Remote
  Check mapping and is a separate decision.
- **Cross-machine aggregation.** The Project Store is machine-local. A Survey is one JSON
  record and travels as a file; a shared Project Store is the shared-coordinator question
  architecture.md leaves open.
- **Semantic indexes.** No embeddings, symbol graphs or ownership inference. `CODEOWNERS` as a
  section is a cheap later addition.
- **A TUI screen.** `af project` is the surface; the TUI reads its `--json` documents when it
  wants to.
- **Replacing History Capture.** The optimizer keeps its observations and receipts; `activity`
  is an aggregate for people and Briefs, and the Project ID only removes a hand-typed field.

## 7. Open decisions for the owner

1. **The name Workspace.** It is the owner's word and matches "a fork per workstream", but
   CONTEXT.md already has *Warm Workspace*. The note keeps Workspace and relies on the two-word
   term, as CONTEXT.md does today. The alternative is *Workstream*, which ADR-0116, ADR-0117 and
   the research design already use for the same thing.
2. **Where the policy lives.** `[survey]` and `[brief.*]` in `.af/af.toml`, as here, or a
   separate declared `.af/project.toml` beside `code-policy.toml`. The note prefers `af.toml`:
   it is already "project policy", and the layer merge gives the directory layer for free.
3. **Default refresh.** `after-run` (one detached process per run that changed the head) or
   `manual`. The note defaults to `after-run`.
4. **Contributor identity.** `name` by default, with emails never stored; or `email_hash` for
   deduplication across name spellings (afactory has `apakhomov` and `Aleksandr Pakhomov`).
5. **Whether the kernel's own implementer gets `orient`.** Decided by PJ3's numbers, in its own
   pull request.
6. **Staleness as a refusal.** v1 only reports `behind_commits`; a `[brief] max_behind_commits`
   that runs the Surveyor before planning, or refuses, is a knob the note leaves out until a
   stale Brief is seen to mislead a Worker.

## 8. Review

To be filled from the design-review Campaign on this note.
