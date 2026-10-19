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
- **Skip by Tree Digest, as the design note's §3.3 says.** Rejected: a new commit with the same
  tree (a revert of a revert, an empty commit, a merge that changes nothing) still changes
  `history` and `contributors`. The skip rule compares the head commit instead.
- **A float for each share.** Rejected: every kernel record carries numbers as decimal text, and
  a float has no canonical spelling. Shares are basis points, rounded down.
- **Store the author email, or a hash of the whole identity line.** Rejected: an email is
  personal data a Brief would one day carry into a prompt. Under `identity = "name"` the email is
  never asked of git; under `"email_hash"` it is hashed as git prints it and dropped.
- **Run the Survey in the background now.** Deferred: the detached after-run refresh, its
  single-flight lock and `[survey] refresh` are package PJ4. PJ2 surveys synchronously.

## Decision

1. **The record.** A Survey is `af/ProjectSurvey@1` ([schema](../../schemas/project-survey-v1.json)):
   RFC 8785 canonical JSON, every map sorted by key, every list in a stated order, and every
   number a decimal string of an integer (counts, bytes, lines, Unix seconds, basis points); the
   Surveyor's serializer refuses any JSON number, float or integer. Its sections are `identity`
   (Project ID, `repository_id`, `[project] name` and `min_af` and the af pin of `.af/af.lock` as
   the head declares them, head commit, the Tree Digest of the head's committed capture,
   `cutoff_unix_s`, `policy_digest`, `surveyor_version`), `tree`, `history`, `contributors`,
   `decisions`, `vocabulary` and `checks`; `activity` is PJ5's. Each section carries
   `completeness` (`complete`, `partial` or `unavailable`) and, when not complete, the bound it
   reached as `reason` (`max_wall_ms` or `max_commits`); an unavailable section holds nothing
   else. The record's `completeness` folds them: `unavailable` only when every section is,
   otherwise `partial` when any section is not complete. `previous_survey_id` names the Survey
   before it in the same Project Store. The record holds no host path, Provider ID or email.
2. **Storage.** The record goes into the Project Store's CAS; its ID is the CAS digest of its
   canonical bytes. `SurveyRecorded@1` {`survey_id`, `head`, `cutoff_unix_s`, `completeness`,
   `policy_digest`, `surveyor_version`, `previous_survey_id` when any, `recorded_unix_ms`} and
   `SurveyFailed@1` {`head`, `reason`, `recorded_unix_ms`} go into its log, each with a schema and
   in `run-event-v1.json`. A failure's reason is af's own sentence of at most 512 bytes, never a
   path or an `@`.
3. **Skip unchanged.** Before any work beyond `rev-parse HEAD`, reading the policy and reading the
   log, a head commit equal to the newest `SurveyRecorded@1`'s with the same `policy_digest` and
   `surveyor_version` is not surveyed again: the command prints `unchanged` with that Survey's
   summary and records nothing. This tightens §3.3's "tree digest" to the head commit, because a
   new commit with the same tree still changes `history`.
4. **Time.** `cutoff_unix_s` is the clock when the Survey starts, and no other field depends on
   when it was taken; a debug build reads `AF_TEST_SURVEY_CUTOFF_UNIX_S` to fix it, as
   `AF_TEST_PROJECT_GIT_DEADLINE_MS` lowers the git deadline, and a release build never reads it.
   The 30, 90 and 365-day windows count commits whose committer time is at or after the cutoff
   minus the window. So a Survey is byte-identical for the same head, policy, cutoff and Surveyor
   version.
5. **Policy.** The `[survey]` table of `.af/af.toml` sits in the project layer and deep-merges
   like every other table; every key is typed and an unknown one is refused, `refresh` included
   until PJ4. The keys and defaults are `max_wall_ms = 60000`, `max_commits = 100000`,
   `max_file_bytes = 1048576`, `exclude = []`,
   `tests = ["**/tests/**", "**/*_test.*", "**/test_*", "**/fixtures/**"]`,
   `decisions = "docs/adr/[0-9][0-9][0-9][0-9]-*.md"`, `vocabulary = "CONTEXT.md"`,
   `release_tags = "v*"`, `identity = "name"` (or `"email_hash"`), `agent_authors = ["*[bot]"]`,
   `agent_trailers = ["*Claude*", "*Codex*", "*GPT*"]` and `hot_paths = 20`.
   `[survey.languages]` maps a file name or `*.ext` to a language and extends the
   built-in map that [docs/project.md](../project.md#surveys) lists; it never replaces it. Path
   globs (`exclude`, `tests`, `decisions`) match by segment with `**`, `*`, `?` and `[...]`
   classes; name globs (`agent_authors`, `agent_trailers`, `release_tags`) know only `*` and `?`,
   so `*[bot]` matches a name that ends in `[bot]`. `policy_digest` is `sha256:` + hex(SHA-256 of
   the canonical JSON of the effective policy). The generic environment form of the
   configuration ladder applies (`AF_SURVEY__MAX_COMMITS=1`); nothing bespoke is added.
6. **Contributors.** `identity = "name"` names a contributor by `%an`, and never asks git for the
   email; a name that is blank, longer than 512 bytes or holds `@` is recorded by the hash rule
   applied to the name. `identity = "email_hash"` names one by the first 16 hex digits of SHA-256
   of the lowercased, trimmed `%ae`. The raw email never enters a record, an event, a document, a
   text output or an error. A commit is an agent's when its author name matches an
   `agent_authors` glob or a `Co-Authored-By:` value matches an `agent_trailers` glob.
   `contributors` carries human and agent commit counts and their shares, per identity the
   commits, lines added and removed, first and last committer time and `active_90d`, the count
   active in 90 days, and the top identity's share.
7. **Tree and history.** `tree` comes from `git ls-tree -r -l` of the head and one
   `git cat-file --batch`. `exclude` removes entries before anything is counted, in `tree`,
   `decisions`, `vocabulary`, `checks` and the numstat of `history` and `contributors`. A line is
   a newline of a text blob (no NUL in its first 8 KiB) at or under `max_file_bytes`; a larger or
   binary blob, a symlink and a submodule entry count as files and bytes only, and
   `files_without_lines` says how many there were. Symlinks and submodules are language `other`;
   root-level entries group under the directory `.`. `history` comes from `git rev-list --count`,
   `git log --numstat` newest first up to `max_commits` (beyond it `partial`, with
   `oldest_reached`), `git tag --list --sort=-creatordate` (the count, and the newest five
   matching `release_tags`) and `git for-each-ref refs/heads` (local branches): commits and merges
   in total and per window, first and last committer time, churn per top-level directory over 90
   days, and the `hot_paths` paths with the most commits in 90 days (ties by path), each marked
   `removed` when the head no longer holds it. The log runs with `--no-mailmap --no-renames
   --no-textconv --no-ext-diff --diff-algorithm=myers` and `log.showRoot=true`, so neither a
   checkout's configuration nor a program it names changes or computes a count.
8. **Decisions, vocabulary, checks.** `decisions` reads the files the `decisions` glob matches:
   the number from the file name's leading digits, the title from the first H1, the status and
   date from the first other line naming `accepted`, `proposed`, `superseded`, `deprecated` or
   `rejected` (as a word, in any case) and the first ISO date on it; counts by status (`unknown`
   for none) and the newest 12 by number. `vocabulary` lists the names of the `**Term**:`
   headings of the `vocabulary` file, in file order and never a definition. `checks` lists the
   names and programs (an absolute program reduced to its file name; never an argument) of
   `.af/code-policy.toml`'s `[checks.*]` and of the `[[checks]]` of the review pipelines, which
   the `.af/` layout keeps under `.af/pipelines/`; the toolchain declarations present at the root
   among `rust-toolchain.toml`, `.nvmrc`, `.python-version`, `.tool-versions`, `go.mod`,
   `package.json`, `Cargo.toml` and `pyproject.toml`; and the count of
   `.github/workflows/*.yml|yaml`.
9. **Bounds and processes.** Every git process of the Surveyor runs through `projects/git.rs`,
   supervised in delivery's isolated command, with a deadline of what remains of the one
   `max_wall_ms` (`Git::run_within`, and `Git::repo_within` for the committed capture of the Tree
   Digest, into a scratch CAS outside the checkout). It runs plumbing only: no checkout, hook,
   filter, textconv or project code, and nothing written inside the checkout. A section that
   cannot finish before the deadline is `partial` with what it reached or `unavailable` with
   reason `max_wall_ms`, every section after it is `unavailable`, and the Survey is still
   recorded. Any other failure records `SurveyFailed@1`. A Survey never calls a Provider and never
   takes a writer lease on a Task Store; it holds the Project Store's lock only to read the log
   and to record.
10. **Commands.** `af project survey [--json]` prints the newest Survey's summary or its record,
    and exits 1 with one line naming `af project refresh` when there is none. `af project refresh
    [--json]` records a Survey now and prints `recorded` or `unchanged` with the summary (`--json`:
    the record). `af project history [--json]` lists the chain newest first (`af/project-history@1`,
    [schema](../../schemas/project-history-v1.json)). `af project show` gains, in text only, the
    newest Survey's summary, or the newest `SurveyFailed@1`'s reason when no Survey was recorded
    after it; `af/project@1` is unchanged, since a changed shape would be `@2`. Exit codes are
    PJ1's. Nothing of the Surveyor runs on the help path.
11. **Onboarding.** `af onboard --apply`, after it registered the Project and the Workspace,
    records the first Survey synchronously and prints one summary line (on stderr under
    `--json`). A Survey that fails prints one line on stderr and changes neither the exit code
    nor anything else onboarding does; `af onboard` without `--apply` records nothing, and a
    second `--apply` on an unchanged head records nothing.
12. **Not here.** Project Briefs (PJ3); the detached after-run refresh, the single-flight lock and
    `[survey] refresh` (PJ4); and the `activity` section (PJ5). PJ4 also adds collection: until
    then every Survey stays in the Project Store, which ADR-0151 already never evicts, and
    `[storage] keep_surveys` and `SurveyCollected@1` do not exist.

## Measured on this repository

`af project refresh` took 0.82 s, 0.70 s and 0.71 s of wall time in three runs, each into an
emptied Project Store, on this repository's tree at commit
`ac9e57b8bd9fb51e3743dfc7f5879643038888cb`, and the record it wrote is 9,635 bytes (1,414
files, 383,808 lines, 141 decisions, 73 terms, 9 declared checks). A fourth run on the same head
printed `unchanged` in 0.06 s. The binary was a release build of this change on an Apple M3 Max
with 14 cores. The commit is this change's tree on Snapshot
`sha256:63e40149d0fd055d3b132719059f70bd10f33b24be109028434f0b16db43dbba`, committed once into a
fresh repository, because the Task sandbox that built it holds no `.git`; it differs from the
delivered tree only in this paragraph's figures and the example output in
[docs/project.md](../project.md#af-project-refresh). Its history is that one commit, so the
figures do not include a walk of afactory's own history, which the design note measured with
`git log --numstat` at 0.71 s; `max_wall_ms` bounds that walk.

## Consequences

- `af project refresh` answers how big the Project is, how it changes, who works on it, what it
  decided, what words it defines and what it checks, from one record whose every number the
  kernel counted. PJ3 renders Briefs from it; nothing here enters a prompt.
- A second refresh of an unchanged head costs `rev-parse`, the policy and one read of the log.
  A changed `[survey]` policy or a new Surveyor version surveys the same head again.
- A Survey cut by `max_wall_ms` or `max_commits` is recorded and says so; a reader never mistakes
  it for a complete one.
- Contributor counts under `name` split one person who wrote under two names; `email_hash` joins
  them by address at the cost of opaque identities.
- Surveys accumulate in the Project Store until PJ4 collects them; each is a few kilobytes.
- Tests: `crates/af/tests/it/project_survey.rs` builds a fixture repository with fixed dates,
  names, emails and messages and compares its Survey byte for byte with
  `crates/af/tests/fixtures/project-survey/expected.json`; proves a second refresh records
  nothing and prints `unchanged` and a new commit chains a Survey; records partial Surveys under
  `max_commits = 1` and `max_wall_ms = 1` with exit 0; finds none of the fixture's emails in any
  byte of the state home or any output under either identity policy and checks the 16-hex rule;
  records the first Survey at `af onboard --apply` once; and records and shows a `SurveyFailed@1`
  for an unknown `[survey]` key. PJ1's test that no `af project` command writes inside the
  checkout covers `refresh`, `survey` and `history`. Unit tests beside the Surveyor refuse a JSON
  number in a record and sort every map, match both glob forms, refuse unknown and invalid
  policy keys, parse git's listing, batch and log bytes, and fold windows, shares, decisions,
  terms and checks. `crates/review-core/tests/schema_parity/project.rs` pins the record, both
  events and the history document, and `crates/af/tests/it/cli_surface.rs` the help.
