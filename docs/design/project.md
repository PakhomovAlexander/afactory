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
writes one, and a Survey of an unchanged head is never recorded twice.
_Avoid_: **Measurement**, one declared command run against one Snapshot; **History Capture**,
the optimizer's economics observations; **Report**, one reviewer's claim; and the README.

**Surveyor**:
The kernel-installed, token-free operator that records Surveys: synchronously at onboarding and
on `af project refresh`, as one detached bounded process after a Task or Campaign run,
single-flight per Project, and never when the head is unchanged.
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
Task; it names `af task gc`.

`--workspace LABEL` on `af task plan|start|run|show|list|explain|deliver|report` and
`af review run` resolves `--state`, `--bindings` and a relative `--file` against the Workspace;
an explicit `--state` still wins and is recorded as it is today. The worktree `af task deliver`
creates is registered as a Workspace of origin `delivered`, labelled by the Task ID, after
delivery succeeds and best effort, so `af project workspaces` lists it and
`af project workspace remove --delete-worktree` can retire it. Delivery's own contract is
unchanged.

### 3.3 Surveys

A Survey is `af/ProjectSurvey@1`: one canonical JSON record, numbers as decimal strings like
every other kernel record, with these sections. Every section carries `completeness`
(`complete`, `partial`, `unavailable`) and a reason when it is not complete; the Survey's own
`completeness` folds them.

| Section | Holds | Source | Bound or policy |
|---|---|---|---|
| `identity` | Project ID, `repository_id`, `[project] name` and `min_af`, the af release `.af/af.lock` pins, head commit, Tree Digest, cutoff, policy digest, Surveyor version | `.af/af.toml`, `.af/af.lock`, git | — |
| `tree` | files and bytes in total; per top-level directory files, lines and bytes; per language files and lines; the `tests` split | `git ls-tree -l`, blob reads | lines only for text blobs at or under `max_file_bytes` (1 MiB); `[survey] exclude` globs; `[survey] tests` globs; `[survey.languages]` extends the built-in extension map |
| `history` | commits and merges in total and in the last 30, 90 and 365 days; first and last commit time; churn (commits, lines added, removed) per top-level directory over 90 days; hot paths; tags in total and the newest five matching `release_tags`; local branch count | `git log --numstat`, `rev-list`, `tag` | `max_commits` (100,000; beyond it `partial`, oldest commit reached recorded); `hot_paths` (20) |
| `contributors` | per identity: commits, lines added and removed, first and last time, active in 90 days; the top identity's share; human and agent commit shares | `git log` | `[survey] identity = "name"` (default) or `"email_hash"`: an email is hashed or absent, never stored; `agent_authors` (`*[bot]`) and `agent_trailers` (`Co-Authored-By` matching `*Claude*`, `*Codex*`, `*GPT*`) |
| `decisions` | per decision record: number, title, status, date; counts by status; the newest N | the `decisions` glob, default `docs/adr/[0-9][0-9][0-9][0-9]-*.md`: title from the first H1, status and date from the first line that names one | `decisions` (12) |
| `vocabulary` | the defined term names and their count | the `vocabulary` file, default `CONTEXT.md`: bold-colon headings | names only; definitions stay in the tree |
| `checks` | declared check names and programs; toolchain declarations present; CI workflow file count | `.af/code-policy.toml`, review pipelines' `[[checks]]`, a fixed file list, `.github/workflows/` | names and programs only, no arguments |
| `activity` (PJ5) | per Workspace: Tasks by outcome, Attempts, chargeable tokens, active time; Campaigns, open and fixed Findings by severity; delivered Tasks; Developer Plan Decisions, Resolutions and Demand waivers; the last 30 days | every registered Store, read-only, through one aggregation projection PJ5 adds that joins each Store's validated Task accounting with its Campaign Ledger's current Resolutions and Demand waivers (`af task show --json` alone carries neither); Stores are deduplicated by their recorded identity, a Task counts for this Project only when its recorded source `repository_id` matches, Task IDs are qualified by Store, and a Task without retained source evidence counts as `unknown` | a Store this release cannot read makes its Workspace `unavailable` with the replay error (ADR-0145); Provider IDs and host paths never enter the section |

Lines are newline counts: deterministic, no tokenizer. A hot path names the path the commits
touched; one absent from the head tree is marked `removed` (on afactory today three of the ten
hottest paths of the last 90 days no longer exist). The Survey records `previous_survey_id`, so
`af project history` walks the chain. Identity is the record's artifact ID.

The newest Survey is reused, and nothing recorded, when its *survey key* equals the current one:
the head commit, the digest of `git for-each-ref refs/heads refs/tags` (names and targets), the
policy digest, the Surveyor version and, for `activity`, the sequence number of every registered
Store (PJ5). A new commit with the same tree, a new tag or branch, a policy change, a newer af or
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
   onboard automatically" requirement; a second `af onboard` on an unchanged head records
   nothing.
2. **On demand.** `af project refresh` records a Survey now and waits; `--json` prints it.
3. **After a run.** When `af task run`, `af task start --execute`, `af task deliver` or
   `af review run` ends, after the Storage Budget sweep, af spawns `af project refresh
   --detached` when `[survey] refresh = "after-run"` (the default; `"manual"` turns it off).
   That is one bounded process that reads, writes one record and exits, the precedent being
   `af self refresh-check`; the kernel still runs no daemon and polls nothing (values §3, §8).
   It takes the single-flight lock `$XDG_RUNTIME_DIR/af/project-<hex>.lock` and leaves silently
   if another refresh holds it, and it checks the head before any work: an unchanged survey key
   means no process at all beyond `rev-parse` and `for-each-ref`.

Bounds come from `[survey]`: `max_wall_ms` (60,000), `max_commits`, `max_file_bytes`. A bound
hit makes the sections after it `partial` or `unavailable`; the Survey is still recorded. Fixed
maxima bound everything else: `max_paths` (200,000) tree entries, `max_entries` (1,000) items in
any list of a section (directories, languages, hot paths, tags, decisions, terms, contributors),
256 bytes for any string (a longer one is cut at a character boundary and marked), and
`max_survey_bytes` (1 MiB) for the record. Every git stream is read incrementally under the one
deadline, never buffered whole; a list cut at its maximum records the count it saw and the
reason `max_entries`; a Survey that cannot fit even so records `SurveyFailed@1`. A
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
the Survey head (commit and Tree Digest), `behind_commits` when it can be computed, the rendered
sections, the sections dropped with their reasons, and `rendered_bytes`.

Variants are project policy in `.af/af.toml`. Two are built in: `orient`, shown here as a
project would write it, and `none`.

```toml
[brief.orient]
sections = ["tree", "vocabulary", "decisions", "checks", "hot_paths"]
max_bytes = 8192
decisions = 12
hot_paths = 20
```

A project adds variants or overrides these. `contributors` and `activity` are never part of
`orient`: an implementer does not need names or bills.

A pipeline that wants a Brief declares the root port and wires it only where it is wanted:

```toml
[contract.inputs.project]
artifact_type = "af/ProjectBrief@1"
cardinality = "one"
optional = true
root_default = "project_brief"
brief = "orient"

[[nodes]]
id = "implement"
# …
[nodes.inputs.project]
kind = "input"
port = "project"
```

The Worker package admits the port in `[signature.contract.inputs.project]` and its
`input.schema.json`. A node that does not wire the port sees nothing, so the pipeline, reviewed
and pinned in the lock like every other Worker input, decides which Workers see the project.
That is the "not for all of them, configurable" requirement, and it uses no new mechanism.

Resolution happens where `empty_review_history` is filled today, in the Task-file adapter at
plan time: take the newest recorded Survey of the Project whose `repository_id` equals the
captured source's; render the variant; drop whole sections, last-declared first, until the
Brief's exact serialized bytes, metadata and drop reasons included, fit `max_bytes`, recording
each drop (a section is never cut mid-way, as Worker Notes are dropped whole with a reason); a
variant whose `max_bytes` is below 1,024 is refused when the policy loads, and a Brief whose
mandatory fields alone exceed `max_bytes` is a plan-time error naming the variant and the bytes,
never an oversized Brief and never a silently empty port; publish the Brief into the Task Store's CAS; pin its
artifact ID in the compiled plan. `af task run`, resume, retry and replay never resolve again
(ADR-0117). Without a Survey the optional port is empty and the preview says so:
`project: none (no Survey recorded; run af project refresh)`. A Task file does not bind the port
in v1: ADR-0134 admits only a recorded Task result output on a bindable port and refuses the
exact-artifact form, and no Task produces a Brief yet. A later amendment may let a Report Task's
recorded Brief bind it under exactly that rule.

Staleness is reported, not refused, in v1: `behind_commits` is the number of commits from the
Survey head to the source commit when the former is an ancestor of the latter, and `unknown`
otherwise; `af task plan` and `af task explain` print it beside the port.

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
max_paths = 200000
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

Patterns use the matcher the `.af/af.toml` routes already use (`glob_matches` in
`crates/af/src/project.rs`): `**` spans segments, `*` a run inside one, `?` one character, and
every other character is literal, brackets included, so `*[bot]` matches `dependabot[bot]` and no
name that merely ends in `t`. There are no character ranges; the decision files are selected by
`decisions` and then by the four-digits-and-a-dash name rule.

`[storage] keep_surveys = 50` is machine-only like the rest of `[storage]`. `AF_SURVEY__REFRESH`
and the other environment forms follow `af help environment`.

### 3.9 Storage Budget and collection

The Project Store is never evicted whole: it indexes the Project's Workspaces, and the Workspace
directories beneath it hold Task Stores and operator files (`tasks/`, `bindings.toml`) that the
Storage Budget inventories one by one under ADR-0144's own rules, as PJ1 implements in
`crates/af/src/storage.rs`. What the budget collects from a Project is Surveys: Surveys beyond `keep_surveys` are collected oldest first behind a
`SurveyCollected@1` tombstone that keeps the head, cutoff and totals, so `af project history`
still answers. Collection never breaks a Task: a plan pins a Brief in the Task Store, not the
Survey in the Project Store.

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
  an unchanged head costs one `rev-parse`; `af project show` reads one projection.
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
`workspaces.toml`; lazy registration from `onboard`, `task plan|start`, `review run`;
`af project show|workspaces|workspace new|add|remove`; `--workspace` on the Task and review
commands; delivered worktrees registered; the `ELSEWHERE` paragraph of `review_config::layout`
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
--apply` recording the first Survey; `af project survey|refresh|history`; the skip-unchanged
rule. ADR: *Measure a Project into Surveys with a token-free Surveyor.*

Acceptance: a fixture repository with known counts yields a byte-identical expected Survey at a
fixed cutoff; a second run on the same head records nothing; `max_commits` and `max_wall_ms`
produce `partial` sections and a recorded Survey; emails never appear in a record under either
identity policy; the measurement-policy fixture (divergent author and committer dates, a merge, a
rename, a binary change, the `max_commits` boundary) yields the stated totals; an oversized commit
and a very large tree stay within the fixed maxima with recorded reasons; the pull request states
the measured wall time and record size on afactory.

### PJ3 — Project Brief

Deliverables: `af/ProjectBrief@1`; the `project_brief` root default and the port's `brief` key;
built-in `orient` and `none`; the `[brief.*]` policy; plan-time resolution, pinning and the
drop rule; `behind_commits`; the manifest entry; preview and `af task explain` lines;
`af project brief`. ADR: *Carry a bounded Project Brief on a
declared port.*

Acceptance: a pipeline wiring the port to one of two Workers delivers the Brief to that Worker's
manifest and not the other's; replay after a refresh renders the pinned Brief, not the newer
Survey; a variant over `max_bytes` drops the last section and records the reason; without a
Survey the plan previews the empty port and runs; a `max_bytes` below 1,024 is refused at policy
load, and the all-sections-dropped and exact-boundary cases behave as §3.5 says. The pull request reports Attempt tokens for
three kernel Tasks run twice, with the implementer wired to `orient` and without, before the
kernel's own `kernel/implementer` package adopts the port in a separate pull request.

### PJ4 — Detached refresh and staleness

Deliverables: the post-run spawn behind `[survey] refresh`; the runtime-directory lock;
`SurveyFailed@1`; the `af project show` refresh line; `behind_commits` in preview and explain.
No new ADR: this amends PJ2's.

Acceptance: a Task run on a changed head is followed by exactly one new Survey; two concurrent
runs produce one; a run on an unchanged survey key spawns nothing; a Surveyor failure leaves the Task's
exit code unchanged and is visible in `af project show`.

### PJ5 — Activity across Workspaces

Deliverables: the `activity` section from every registered Store through the aggregation
projection of §3.3 (validated Task accounting joined with Campaign Ledger views), Store
deduplication and Project attribution; `af project tasks`; Tasks and spend per Workspace in `af project workspaces`;
Developer Plan Decisions, Resolutions and waivers counted as decisions; the Provider-ID and path
exclusion. ADR: *Fold every Workspace's recorded work into the Project's Survey.*

Acceptance: a fixture with two Workspaces and a Store this release cannot read yields exact
totals for the two and `unavailable` with the replay error for the third; no Provider ID, host
path or email appears in the section; a shared Store, a mixed-Project Store and a duplicate Task ID across
two Stores count each Task once and only for its own Project; a Resolution and a waiver recorded
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
