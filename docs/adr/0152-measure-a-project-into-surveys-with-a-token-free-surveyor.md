# ADR-0152: Measure a Project into Surveys with a token-free Surveyor

**Status:** accepted (2026-10-20). Package PJ2 of the
[Project design note](../design/project.md). Builds on
[ADR-0151](0151-give-a-repository-one-project-identity-and-register-its-checkouts-as-workspaces.md)
(the Project, its Project Store and every git process of `projects/git.rs`),
[ADR-0132](0132-measure-and-compare-source-candidates-in-the-kernel.md) (the kernel measures; a
model never writes a number), [ADR-0001](0001-tree-diff-behind-a-typed-method.md) (typed git
plumbing), [ADR-0032](0032-generate-review-authority-with-af-onboard.md),
[ADR-0115](0115-declare-the-af-layout-and-keep-task-files-out-of-git.md) and
[ADR-0116](0116-report-undeclared-af-paths-and-let-a-project-refuse-them.md) (nothing the kernel
records goes in the checkout), [ADR-0144](0144-hold-afs-disk-use-to-a-machine-budget.md) (the
Store registry and the Storage Budget) and
[ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md) (version
every changed shape).

## Context

A Project (ADR-0151) has an identity and Workspaces, and nothing else: af still cannot say how
big the repository is, how fast it changes, who works on it or what has been decided. Every
Worker rediscovers it with tool calls in each Attempt. The answer has to be a kernel record:
counted the same way every time, bounded, and free of anything a later Brief must not carry, an
email first of all.

## Considered options

- **A model-written description of the project.** Rejected: the kernel measures and a model never
  writes a number the kernel did not record (ADR-0132). A model-written description is a Report
  Task's Document, bound to one Snapshot and verified (ADR-0133); a later package may let a Brief
  bind one by artifact ID, beside the Survey and never inside it.
- **Read the working tree.** Rejected: the working tree is not the head, it may hold untracked
  or ignored files, and reading it through the file system would make a Survey depend on the
  checkout's state. The Surveyor reads the head's objects through git plumbing only.
- **Reuse by Tree Digest, or by the head commit alone.** Rejected: a new commit with the same
  tree (a revert of a revert, an empty commit, a merge that changes nothing) still changes
  `history` and `contributors`, and a new tag or branch at the same head changes `history`. A
  Survey is reused only under its whole survey key (clause 3).
- **A float for each share.** Rejected: every kernel record carries numbers as decimal text, and
  a float has no canonical spelling. Shares are basis points, rounded down.
- **Store the author email, or a hash of the whole identity line.** Rejected: an email is
  personal data a Brief would one day carry into a prompt. Under `identity = "name"` the email is
  never asked of git; under `"email_hash"` it is hashed as git prints it and dropped.
- **Scrub every `@` from the record.** Rejected: the no-email rule protects authors, not the
  repository's own words. A decision title, a path, a term or a check name is recorded as
  committed, bounded by the fixed maxima; rewriting it would make the record disagree with the
  tree.
- **A second pattern grammar with character classes.** Rejected: `.af/af.toml` already has one
  matcher, and two grammars for the same file would read the same pattern two ways.
- **Run the Survey in the background now.** Deferred: the detached after-run refresh, its
  single-flight lock and `[survey] refresh` are package PJ4. PJ2 surveys synchronously.

## Decision

1. **The record.** A Survey is `af/ProjectSurvey@1` ([schema](../../schemas/project-survey-v1.json)):
   RFC 8785 canonical JSON, every map sorted by key, every list in a stated order, and every
   number a decimal string of an integer (counts, bytes, lines, Unix seconds, basis points); the
   Surveyor's serializer refuses any JSON number, float or integer. Its sections are `identity`
   (Project ID, `repository_id`, `[project] name` and `min_af` and the af pin of `.af/af.lock` as
   the head declares them, head commit, `refs_digest`, the Tree Digest of the head's committed
   capture or `tree_digest_reason`, `cutoff_unix_s`, `policy_digest`, `surveyor_version` and
   `git_version`), `tree`, `history`, `contributors`, `decisions`, `vocabulary` and `checks`;
   `activity` is PJ5's. Each section carries `completeness` (`complete`, `partial` or
   `unavailable`) and, when not complete, the bound it reached as `reason` (`max_wall_ms`,
   `max_commits`, `max_paths`, `max_entries` or `max_file_bytes`); an unavailable section holds
   nothing else, and a section that cut a list, or left a value unmeasured, is never complete. A
   list cut at its maximum records how many items it saw under `seen`, by list name. The
   record's `completeness` folds them: `unavailable` only when every section is, otherwise
   `partial` when any section is not complete. `previous_survey_id` names the Survey before it
   in the same Project Store. The record holds no host path, Provider ID or author email.
2. **Storage.** The record goes into the Project Store's CAS; its ID is the CAS digest of its
   canonical bytes. `SurveyRecorded@1` {`survey_id`, `head`, `cutoff_unix_s`, `completeness`,
   `policy_digest`, `surveyor_version`, `refs_digest`, `previous_survey_id` when any,
   `recorded_unix_ms`} and
   `SurveyFailed@1` {`head`, `reason`, `recorded_unix_ms`} go into its log, each with a schema and
   in `run-event-v1.json`. A failure's reason is af's own sentence of at most 512 bytes, never a
   path or an `@`.
3. **Reuse on an unchanged survey key.** The survey key is the head commit, `refs_digest` =
   `sha256:` + hex(SHA-256 of the exact stdout of `git for-each-ref
   --format=%(refname)%09%(objectname) refs/heads refs/tags`), the `policy_digest` and the
   `surveyor_version` (now 2); `activity`'s watermark joins it in PJ5. Before any work beyond
   `rev-parse HEAD`, that `for-each-ref`, reading the policy and reading the log, a newest
   `SurveyRecorded@1` that is `complete` and carries an unchanged survey key is reused: the
   command prints one line, `unchanged: head <12 hex>, refs <16 hex>, policy <16 hex>, surveyor
   2`, with that Survey's summary and records nothing. A new commit, tag or branch, a policy
   change or a newer Surveyor records a new Survey; a newest Survey that is `partial` or
   `unavailable` is measured again; `af project refresh --force` records one regardless of the
   key.
4. **Time.** `cutoff_unix_s` is the clock when the Survey starts, and no other field depends on
   when it was taken; a debug build reads `AF_TEST_SURVEY_CUTOFF_UNIX_S` to fix it, as
   `AF_TEST_PROJECT_GIT_DEADLINE_MS` lowers the git deadline and `AF_TEST_SURVEY_SLOW_ITEM_US`
   makes every streamed item cost that many microseconds, and a release build reads none of
   them. A window holds a commit when `cutoff - window < committer time <= cutoff`, for windows
   of 30, 90 and 365 days. So a Survey is byte-identical for the same head, refs, policy,
   cutoff, Surveyor version and git version.
5. **Policy.** The `[survey]` table of `.af/af.toml` sits in the project layer and deep-merges
   like every other table; every key is typed and an unknown one is refused, `refresh` included
   until PJ4. The keys and defaults are `max_wall_ms = 60000`, `max_commits = 100000`,
   `max_file_bytes = 1048576`, `max_paths = 200000`, `max_entries = 1000`,
   `max_survey_bytes = 1048576`, `exclude = []`,
   `tests = ["**/tests/**", "**/*_test.*", "**/test_*", "**/fixtures/**"]`,
   `decisions = "docs/adr/*.md"`, `vocabulary = "CONTEXT.md"`,
   `release_tags = "v*"`, `identity = "name"` (or `"email_hash"`), `agent_authors = ["*[bot]"]`,
   `agent_trailers = ["*Claude*", "*Codex*", "*GPT*"]` and `hot_paths = 20`.
   `[survey.languages]` maps a file name or `*.ext` to a language and extends the
   built-in map that [docs/project.md](../project.md#surveys) lists; it never replaces it. Every
   pattern (`exclude`, `tests`, `decisions`, `release_tags`, `agent_authors`, `agent_trailers`)
   matches with `crate::project::glob_matches`, the one grammar of `.af/af.toml`: `**` spans
   segments, `*` a run inside one, `?` one character, and every other character is literal,
   brackets included, so `*[bot]` matches `dependabot[bot]` and no name that merely ends in `t`,
   and `[0-9]` is three literal characters. A file `decisions` matches is a decision only when
   its name starts with four ASCII digits and a dash, so `docs/adr/README.md` is not one.
   `policy_digest` is `sha256:` + hex(SHA-256 of the canonical JSON of the effective policy).
   The generic environment form of the configuration ladder applies
   (`AF_SURVEY__MAX_COMMITS=1`); nothing bespoke is added.
6. **Contributors.** `identity = "name"` names a contributor by `%an`, and never asks git for the
   email; a name longer than 256 bytes is cut at a character boundary and marked `truncated`. An
   author name that is itself an email address (an `@` between non-empty parts, no whitespace),
   or that is blank or holds a control character, is replaced under both policies by `h:` and
   the first 16 hex digits of SHA-256 of the lowercased, trimmed name. `identity = "email_hash"`
   names one by the first 16 hex digits of SHA-256 of the lowercased, trimmed `%ae`. The no-email
   rule covers author identities only: an author email never enters a record, an event, a
   document, a text output or an error, while repository text (decision titles, paths, terms,
   check names, hot paths) is recorded as committed, `@` included, bounded by clause 9. A commit
   is agent-assisted when its author name matches `agent_authors` or the name of any
   `Co-Authored-By:` trailer (the text before its `<address>`) matches `agent_trailers`,
   otherwise human; each commit counts once in that partition (`human_commits`,
   `agent_commits` and their shares in basis points) and once for its author, who keeps the
   per-identity totals; an empty window makes every share `0`. `contributors` carries, per
   identity, the commits, lines added and removed, first and last committer time and
   `active_90d`, the identities with the most commits up to `max_entries`, the count active in
   90 days, and the top identity's share.
7. **Tree and history.** `tree` comes from `git ls-tree -r -l` of the head and one
   `git cat-file --batch`, both read as they stream. `exclude` removes entries before anything
   is counted, in `tree`, `decisions`, `vocabulary`, `checks` and the numstat of `history` and
   `contributors`; at most `max_paths` entries are considered, and beyond it `tree` is `partial`
   with reason `max_paths` and `seen.paths`, while the later sections still find their files in
   the rest of the listing. A line is a newline of a text blob (no NUL in its first 8 KiB) at or
   under `max_file_bytes`; a larger or binary blob, a symlink and a submodule entry count as
   files and bytes only, and `files_without_lines` says how many there were. Symlinks and
   submodules are language `other`; root-level entries group under the directory `.`. The
   measurement policy: commits are walked from the head in `git rev-list --date-order` order,
   newest first, and cut at `max_commits` (beyond it `partial`, with `oldest_reached`); a merge
   counts as a commit and a merge and contributes no churn (the walk prints no diff for a
   merge, `--diff-merges=off`, which is what `--numstat --no-merges` would count); renames are
   not detected (`--no-renames`), so a rename is a removal and an addition; a binary numstat
   entry (`-` `-`) is one file touched and zero lines; ties break by path, then by commit ID.
   `history` comes from `git rev-list --count`, that `git log --numstat`, one `git ls-tree
   --name-only` of the hot paths (to mark the ones the head no longer holds `removed`) and one
   `git for-each-ref --sort=-creatordate refs/heads refs/tags` (local branches, the tag count
   and the newest five tags matching `release_tags`): commits and merges in total and per
   window, first and last committer time, churn per top-level directory over 90 days, and the
   `hot_paths` paths with the most commits in 90 days. The log also runs with `--no-mailmap
   --no-textconv --no-ext-diff --diff-algorithm=myers` and `log.showRoot=true`, so neither a
   checkout's configuration nor a program it names changes or computes a count, and
   `identity.git_version` records the exact `git --version` stdout, trimmed, because git
   decides these numbers.
8. **Decisions, vocabulary, checks.** `decisions` holds every decision record (clause 5) up to
   `max_entries`, sorted by number, then path, with `count` and `by_status`; a Brief variant
   picks the newest N in PJ3. Each has the number (the four leading digits), the path, the
   title from the first H1, and the status and date from the first other line naming
   `accepted`, `proposed`, `superseded`, `deprecated` or `rejected` (as a word, in any case) and
   the first ISO date on it; `by_status` counts `unknown` for none. `vocabulary` lists the names
   of the `**Term**:` headings of the `vocabulary` file, in file order and never a definition.
   `checks` lists the names and programs (an absolute program reduced to its file name; never
   an argument) of `.af/code-policy.toml`'s `[checks.*]` and of the `[[checks]]` of the review
   pipelines, which the `.af/` layout keeps under `.af/pipelines/`; the toolchain declarations
   present at the root among `rust-toolchain.toml`, `.nvmrc`, `.python-version`,
   `.tool-versions`, `go.mod`, `package.json`, `Cargo.toml` and `pyproject.toml`; and the count
   of `.github/workflows/*.yml|yaml`. A file these sections read that is over `max_file_bytes`
   or not text makes its section `partial` with reason `max_file_bytes`, never `complete` with
   nothing in it: a decision is listed with status `unreadable` and no title, the vocabulary is
   `present` with `terms` and `count` absent, and a declaration is named under `unreadable`.
9. **Bounds and processes.** Every git process of the Surveyor runs through `projects/git.rs`,
   supervised in delivery's isolated command, with a deadline of what remains of the one
   `max_wall_ms` (`Git::run_within` for a one-line answer, `Git::stream_within` for every stream,
   and `Git::repo_within`, whose every git command ends by that one deadline, for the committed
   capture of the Tree Digest, into a scratch CAS outside the checkout). Every stream (`cat-file --batch`, `ls-tree`, `log --numstat`,
   `for-each-ref`) is consumed as it arrives, never buffered whole, and every loop over tree
   entries, blobs, commits, numstat lines, decision files, tags and refs checks the clock once
   per item: a section that reaches the deadline stops at that item and is `partial` with reason
   `max_wall_ms` and what it reached, every later section is `unavailable`, and the Survey is
   still recorded, so its wall time is at most `max_wall_ms` plus one item plus what remained of
   one git command's deadline. The Tree Digest is the committed capture's digest of the whole
   head tree, independent of `exclude` and every maximum; a capture that does not finish leaves
   it absent with `tree_digest_reason = "max_wall_ms"`, never a digest of a part. Fixed maxima
   bound the rest: `max_entries` items in any list of a section (directories, languages, churn,
   hot paths, tags, decisions, terms, contributors, checks), 256 bytes for a display string (a
   contributor name, a decision title, a term, a tag or a check name or program, cut at a
   character boundary and marked `truncated: true`), while identity strings (every field of
   `identity`, commit IDs, digests, ref names and paths) are recorded exactly, and
   `max_survey_bytes` for the record: one that needs more records `SurveyFailed@1` naming the
   bytes it needed, and nothing else. It runs plumbing only: no checkout, hook, filter, textconv
   or project code, and nothing written inside the checkout. Any other failure records
   `SurveyFailed@1`. A Survey never calls a Provider and never takes a writer lease on a Task
   Store; it holds the Project Store's lock only to read the log and to record.
10. **Commands.** `af project survey [--json]` prints the newest Survey's summary or its record,
    and exits 1 with one line naming `af project refresh` when there is none. `af project refresh
    [--json] [--force]` records a Survey now and prints `recorded`, or the `unchanged:` line of
    clause 3, with the summary (`--json`: the record, and the `unchanged:` line on stderr). `af project history [--json]` lists the chain newest first (`af/project-history@1`,
    [schema](../../schemas/project-history-v1.json)). `af project show` gains, in text only, the
    newest Survey's summary, or the newest `SurveyFailed@1`'s reason when no Survey was recorded
    after it; `af/project@1` is unchanged, since a changed shape would be `@2`. Exit codes are
    PJ1's. Nothing of the Surveyor runs on the help path.
11. **Onboarding.** `af onboard --apply`, after it registered the Project and the Workspace,
    records the first Survey synchronously and prints one summary line (on stderr under
    `--json`). A Survey that fails prints one line on stderr and changes neither the exit code
    nor anything else onboarding does; `af onboard` without `--apply` records nothing, and a
    second `--apply` with an unchanged survey key records nothing.
12. **Not here.** Project Briefs (PJ3); the detached after-run refresh, the single-flight lock and
    `[survey] refresh` (PJ4); and the `activity` section (PJ5). PJ4 also adds collection: until
    then every Survey stays in the Project Store, which ADR-0151 already never evicts, and
    `[storage] keep_surveys` and `SurveyCollected@1` do not exist.
13. **Collection and the budget.** This clause replaces the collection sentence of clause 12:
    PJ2 ships Survey collection with the synchronous Surveyor, `SurveyCollected@1`, `[storage]
    keep_surveys` retention, the allocated-byte inventory and budget-driven deletion, and PJ4
    adds only the trigger after a run.
    Every Survey is an entry of kind `survey` of the machine budget (ADR-0144 clause 4): its
    record's allocated CAS bytes, taken out of the Project Store entry's, last used at its
    cutoff, its `path` the CAS object. `af storage` lists it, and its documents become
    `af/storage@3` ([schema](../../schemas/storage-v3.json)) and `af/storage-prune@2`
    ([schema](../../schemas/storage-prune-v2.json)), each gaining the kind `survey`, and their
    earlier versions are refused; a sweep's `af/TaskStorageSweep@1` counts a Survey's removal in
    `omitted_removals` without listing it, so its shape and its kinds are unchanged and it keeps
    its version. Surveys are collected
    only in the Storage Budget's own passes, under ADR-0144's protections, and `af project
    refresh` collects nothing. The collection pass (`af storage prune --apply`, or the sweep after
    a run when `auto_gc = true`) takes, per Project, the Surveys beyond the newest `[storage]
    keep_surveys = 50` (machine layers only, `AF_STORAGE__KEEP_SURVEYS`), oldest first, each idle
    at least `keep_days`; the budget step of every sweep ADR-0144 runs (after a run, before a new
    warm key, at the free-disk floor, on request) may take any Survey, least recently used first
    among every entry, while af holds more than `max_bytes`, even below `keep_surveys`. Neither takes the newest complete Survey, a Survey of a Project Store whose
    lock (`project.lock`, which a refresh holds while it reads and records and a plan while it
    builds a Brief) is held, or one used within the hour, so `keep_surveys` is a target the
    protected Surveys may exceed; the Project's log, `workspaces.toml`, `project.lock` and
    operator files are never reclaimed, and the Project Store is still never evicted whole. A
    collection takes the lock without waiting, so a held Store is left for the next pass, reads
    the protections again under it, and for each Survey appends `SurveyCollected@1`
    {`survey_id`, `head`, `cutoff_unix_s`, `completeness`, the `tree` totals `files`, `bytes` and
    `lines` and the `history` totals `commits` and `merges` when the record holds them,
    `collected_unix_ms`} ([schema](../../schemas/project-survey-collected-v1.json), in
    `run-event-v1.json`) before it deletes the record from the CAS. A record is reached, read
    and deleted only through directory descriptors, ADR-0144's removal boundary: the Project
    Store from `/`, then `cas`, `objects` and the digest-prefix directory, each opened
    `O_NOFOLLOW | O_DIRECTORY`, and the record opened `O_NOFOLLOW` and removed by `unlinkat`
    relative to its prefix directory, only while that name still holds the file that was found.
    A symbolic link on that way or in place of the record, a directory on another device than
    the Store's, or a record the file system will not let af read (permission denied, an I/O
    error) refuses the Survey: inventory measures nothing through a link and lists it in use,
    and the pass reports it with its reason, appends no tombstone and deletes nothing. A record
    whose bytes were read but are no Survey (they do not match its digest, exceed the largest
    record or do not parse), or that is gone from the CAS, is collected all the same, since
    reclaiming corrupt bytes is the budget's job: its tombstone carries `unreadable`, af's own
    sentence of at most 256 bytes and never the record's content, in place of the totals, and
    the sweep and `af project history` show it as `collected (unreadable)`. A preview, `af
    storage prune` without `--apply`, applies the same `project.lock` check as the pass: the
    Surveys it would take from a held Project Store are listed under `held`, never as removals,
    exactly as the applied pass lists them. The tombstones are the run
    `survey-collection` of the Project Store's log, beside the run `project` that holds the
    chain, so the chain's own sequence is only ever the Surveyor's. Every applied sweep first
    deletes the record of any tombstoned Survey still in a CAS, reported with the rule
    `recovery`, so an interruption between a tombstone and its deletion is finished by the next
    pass; a debug build reads `AF_TEST_SURVEY_STOP_AFTER_TOMBSTONES=1` to stop there. Replay
    treats a `SurveyRecorded@1` whose Survey a tombstone names as collected: `af project history`
    answers for it from the tombstone and marks it `collected` in text (`af/project-history@1`
    is unchanged, since a marked entry would be `@2`), and `af project survey` shows the newest
    Survey only while it is uncollected and otherwise exits 1 saying it was collected and naming
    `af project refresh`; `af project show` then prints no Survey. `previous_survey_id` is a
    name, never a reachability edge: a collected predecessor keeps nothing alive, and a chain that
    reaches one ends at its tombstone.
14. **Patterns.** `crate::project::glob_matches`, the matcher of the routes of `.af/af.toml` and
    of every `[survey]` pattern, matches by dynamic programming with the semantics of clause 5
    unchanged (segment-wise, `**` spans segments, `*` a run inside one, `?` one byte, every other
    byte literal, brackets included). The "one character" of clause 5 is one byte of the path's
    canonical encoded spelling (`review_core::encode_path`), as the byte matcher before it read
    it: `caf?.rs` does not match `café.rs`, whose `é` is two bytes, and `caf??.rs` does, and a
    byte that is not UTF-8 is spelled `%E9`, three bytes, the same through routing and the
    Surveyor's `exclude`. The matcher runs over pattern segments and path segments, then over pattern
    bytes and text bytes inside one pair, visiting only what a prefix reached. A match evaluates
    at most (pattern length + 1) × (path length + 1) cells and never backtracks. The recursive
    form took billions of steps to refuse `*a` thirty times followed by `b` against thirty `a`s,
    a pattern inside every string limit whose failure no deadline interrupted; the matcher
    refuses it in 961 cells, under its 61 × 30 = 1,830, through routing and the Surveyor's
    `exclude` alike, and its matching form, `*a` thirty times against thirty `a`s, matches. Where
    the Surveyor loads its policy, a `[survey]` pattern is at most 256 bytes and each pattern list
    (`exclude`, `tests`, `agent_authors`, `agent_trailers`) at most 64 patterns, beyond which the
    policy is refused naming the key and the refresh records `SurveyFailed@1`. Every pattern
    list the Surveyor applies (`exclude`, `tests`, `agent_authors`, `agent_trailers`) goes
    through one loop, `any_match_within`, that checks the Survey's deadline before each match,
    so one match is the most a deadline can be overrun by; a list cut by the deadline matches
    nothing more, and the section it cut is partial with reason `max_wall_ms` and every later one
    unavailable. A language is an exact lookup of a name or an extension, not a pattern match,
    and `decisions` and `release_tags` are single patterns applied once per streamed item.

## Measured on this repository

A Task's sandbox holds the Snapshot's tree and no history, so the Surveyor cannot be measured
on afactory inside the Task that builds it: the operator measures `af project refresh` on the
real checkout, and the pull request states its wall time and the record's size (§5 PJ2 of the
design note). This ADR records only the bounds above and the fixture's measured size: the
record of `crates/af/tests/fixtures/project-survey/expected.json`, eight commits and 16 files,
is 4,392 bytes plus the length of `git --version` less 11 (4,415 bytes with
`git version 2.54.0 (Apple Git-157)`), against `max_survey_bytes = 1048576`.

## Consequences

- `af project refresh` answers how big the Project is, how it changes, who works on it, what it
  decided, what words it defines and what it checks, from one record whose every number the
  kernel counted. PJ3 renders Briefs from it; nothing here enters a prompt.
- A second refresh with an unchanged survey key costs `rev-parse`, `for-each-ref`, the policy
  and one read of the log. A new tag or branch, a changed `[survey]` policy or a new Surveyor
  version surveys the same head again, and so does a partial newest Survey.
- A Survey cut by `max_wall_ms`, `max_commits`, `max_paths`, `max_entries` or `max_file_bytes`
  is recorded and says so, with `seen` for every cut list; a reader never mistakes it for a
  complete one. A Survey's wall time and size are bounded whatever the repository holds.
- Contributor counts under `name` split one person who wrote under two names; `email_hash` joins
  them by address at the cost of opaque identities.
- Surveys accumulate in the Project Store until a later round collects them; each is a few
  kilobytes, at most `max_survey_bytes`.
- Tests: `crates/af/tests/it/project_survey.rs` builds a fixture repository with fixed dates,
  names, emails and messages (a merge, a rename, a binary change, a commit authored a year
  before it was committed, a human commit with an agent trailer, one with two, an author named
  by an email address and an ADR titled with one) and compares its Survey byte for byte with
  `crates/af/tests/fixtures/project-survey/expected.json`, with this machine's `git --version`
  in its one place, stating the totals; proves the `max_commits` boundary in date order; proves
  an unchanged survey key records nothing and prints the key, while a new commit, tag or
  branch, a partial newest Survey and `--force` each record one chained to the one before;
  records a partial Survey under `max_wall_ms = 1` with the Tree Digest absent and its reason;
  stops a 2,000-file tree at the deadline inside `tree` with every later section unavailable and
  the refresh under `max_wall_ms` + 2 s; cuts at `max_paths` and `max_entries` with `seen`,
  records `SurveyFailed@1` naming the bytes under `max_survey_bytes = 1024`, cuts and marks a
  300-byte author name and keeps a 286-byte seven-root `repository_id` exact; leaves an
  oversized vocabulary, decision and declaration partial with `max_file_bytes`; finds no author
  email in any byte of the state home or any output under either identity policy, an
  email-shaped author name included, while the ADR titled with an address is recorded as
  committed; records the first Survey at `af onboard --apply` once; and records and shows a
  `SurveyFailed@1` for an unknown `[survey]` key. PJ1's test that no `af project` command
  writes inside the checkout covers `refresh`, `survey` and `history`. Unit tests beside the
  Surveyor refuse a JSON number in a record and sort every map, match every pattern with the
  one grammar (`dependabot[bot]` matches `*[bot]`, `[0-9]` is literal, `docs/adr/README.md` is
  no decision), refuse unknown and invalid policy keys, read git's listing, batch and log
  streams item by item and stop at a passed deadline, and fold windows, the partition, cut
  lists, decisions, terms and checks. `crates/review-core/tests/schema_parity/project.rs` pins
  the record, both events and the history document, and `crates/af/tests/it/cli_surface.rs` the
  help.
