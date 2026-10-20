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
   number a decimal string of an integer (counts, bytes, lines, Unix seconds, basis points), a
   share being its population's basis points rounded down (`3333` for one third of three, `0`
   for an empty population); the Surveyor's serializer refuses any JSON number, float or
   integer. Its sections are `identity`
   (Project ID, `repository_id`, `[project] name` and `min_af` and the af pin of `.af/af.lock` as
   the head declares them, head commit, `refs_digest`, the Tree Digest (clause 9) or
   `tree_digest_reason`, one of `deadline`, `max_record_bytes`, `manifest_bytes` and
   `temp_bytes`, `cutoff_unix_s`, `policy_digest`, `surveyor_version` and `git_version`),
   `tree`, `history`, `contributors`, `decisions`, `vocabulary` and `checks`; `activity` is
   PJ5's. Each section carries `completeness` (`complete`, `partial` or `unavailable`) and, when
   not complete, the bound it reached as `reason` (`max_wall_ms`, `max_record_bytes`,
   `malformed_record`, `manifest_bytes`, `temp_bytes`, `max_paths`, `max_commits`, `max_entries`
   or `max_file_bytes`, the first that holds in that order; `manifest_bytes` and `temp_bytes` are
   `identity`'s alone, the ceilings that stopped its Tree Digest, and `malformed_record` is
   `history`'s and `contributors`', a log record that does not parse, clause 7); an unavailable
   section holds nothing else, and a section that cut a list, or left a value unmeasured, is never complete. A list cut at its maximum, or stopped
   by the deadline, records how many items it saw under `seen`, by list name. The
   record's `completeness` folds them: `unavailable` only when every section is, otherwise
   `partial` when any section is not complete. `previous_survey_id` names the Survey before it
   in the same Project Store. A Survey that is not complete records its exact stopping point,
   `stopped`, so a replay of its record reproduces it: the `section` the deadline cut first, or,
   when no deadline cut one, the first section that is not complete; that section's `reason`;
   `item`, how many items of that section the Survey's clock charged (clause 9's items, the one
   that found the deadline and the section's last check included, `0` for a section the deadline
   left unstarted); under `history` the `commit` its walk stopped in (`history.stopped_in`, which
   names the commit a record over 64 KiB, a commit over `max_paths` or a malformed record stopped
   the walk in), else the oldest commit it reached; and under `max_file_bytes` the `path` of the
   first declaration of the section, in its order, that was not read. A complete Survey has no
   `stopped`, and the validator and the schema refuse a record whose `stopped` is absent, present
   when complete, or names another section, reason, commit or path than its sections do. `af
   project survey` prints it on its own line, `stopped <section> at item <n>[, commit <12
   hex>][, <path>] (<reason>)`. The record holds no host path, Provider ID or author email.
2. **Storage.** The record goes into the Project Store's CAS; its ID is the CAS digest of its
   canonical bytes. `SurveyRecorded@1` {`survey_id`, `head`, `cutoff_unix_s`, `completeness`,
   `policy_digest`, `surveyor_version`, `refs_digest`, `previous_survey_id` when any,
   `recorded_unix_ms`} and
   `SurveyFailed@1` {`head`, `reason`, `recorded_unix_ms`} go into its log, each with a schema and
   in `run-event-v1.json`. A failure's reason is af's own sentence of at most 512 bytes, never a
   path or an `@`. No Survey event's fields reach an output or a decision unverified. Replay
   binds every `SurveyRecorded@1` it reads a record by to that record: `ProjectStore::survey`,
   through which `af project survey`, `af project show`, `af project refresh`, the reuse of
   clause 3 and the chain's newest Survey read a record, refuses an event whose `head`,
   `refs_digest`, `cutoff_unix_s`, `completeness`, `policy_digest`, `surveyor_version` or
   `previous_survey_id` differs from the record's `identity` and chain, and the survey key is
   taken from the verified record, never from the event alone. `af project history` verifies
   every event it prints before it prints any, newest first: a collected Survey's against the
   tombstone that bound every identity field of its record at collection (`head`,
   `refs_digest`, `cutoff_unix_s`, `completeness`, `policy_digest`, `surveyor_version` and
   `previous_survey_id`, clause 13), every other one against its record, so `--json` never
   emits a mismatched event, an older one included. Each refusal is the typed replay error
   `SurveyRecorded@1 at sequence <n> names a <field> its Survey <id> (or its tombstone
   SurveyCollected@1 at sequence <m>) does not record`, on the first mismatch. The fold refuses
   before it reads: a record of an uncollected Survey that the CAS cannot hand over (gone,
   unreadable, corrupt, or no valid Survey) is a verification failure, the typed replay error
   `SurveyRecorded@1 at sequence <n> names Survey <id>, whose record cannot be read: <error>`,
   in af's own words and naming no other field of the event; `af project history` (text and
   `--json`) refuses it wherever it stands in the chain, and every reader of the newest record
   refuses it for the newest. No command uses a collected event before checking it against its
   tombstone: `newest`, through which `af project survey` with and without `--section`, `af
   project show` and a refresh's reuse check read the newest Survey, and the collected answer
   of `af project survey`, bind the newest event to its tombstone first, the head an error
   message names included. A corrupt record is still the Storage Budget's to collect (clause
   13), and `af project refresh --force` records a new newest Survey without reading the old
   one. The reads happen where a field is used: `af project history` reads every uncollected
   record, the other commands the newest. The post-run preflight (PJ4) and the Brief (PJ3) read
   the newest Survey through the same `newest` and `ProjectStore::survey` and refuse the same
   way.
3. **Reuse on an unchanged survey key.** The survey key is the head commit, `refs_digest` =
   `sha256:` + hex(SHA-256 of the exact stdout of the one ref observation, `git for-each-ref
   --format=%(refname)%09%(objectname)%09%(objecttype)%09%(creatordate:unix) refs/heads
   refs/tags`), the `policy_digest` and the `surveyor_version` (now 6); `activity`'s watermark
   joins it in PJ5. That observation is the Survey's only read of ref names: its lines, each a
   name, a target, the target's object type and the creator date git resolves through that
   target (an annotated tag's tagger time, a commit's committer time, none for another object),
   are hashed into the key and folded, as they stream, into the tag and branch statistics of
   `history` (clause 7), so a ref created after it is in neither. The key is computed under the
   Survey's one `max_wall_ms` Clock, which starts before `rev-parse HEAD`: `rev-parse` runs under
   what remains of it, and `for-each-ref` streams under it, one ref per item, each ref at most
   64 KiB; a deadline reached before the key exists, or a ref over its bound, records
   `SurveyFailed@1` with that reason and no Survey, since the key cannot be complete (`the survey
   key could not be computed within max_wall_ms`; only an exit 1 when `rev-parse` itself did not
   answer, since the event names a head). Before any work beyond `rev-parse HEAD`, that `for-each-ref`,
   reading the policy and reading the log, a newest uncollected Survey whose record is
   `complete` and carries an unchanged survey key is reused: the command prints one line,
   `unchanged: head <12 hex>, refs <16 hex>, policy <16 hex>, surveyor 6`, with that Survey's
   summary and records nothing. A new commit, tag or branch, a policy
   change or a newer Surveyor records a new Survey; a newest Survey that is `partial` or
   `unavailable` is measured again; `af project refresh --force` records one regardless of the
   key.
4. **Time.** `cutoff_unix_s` is the clock when the Survey starts, and no other field depends on
   when it was taken; a debug build reads `AF_TEST_SURVEY_CUTOFF_UNIX_S` to fix it, as
   `AF_TEST_PROJECT_GIT_DEADLINE_MS` lowers the git deadline and `AF_TEST_SURVEY_SLOW_ITEM_US`
   makes every item cost that many microseconds, an item of a stream and of every loop after
   one alike (an aggregated tree entry, a ranked identity, a declaration, a line of terms, a
   declared check, a section's last check), `AF_TEST_SURVEY_SLOW_SECTION` naming the one section
   whose items alone cost it, `AF_TEST_SURVEY_SLOW_FINISH` the one section whose finishing step
   lasts past the deadline, and `AF_TEST_SURVEY_DIGEST_MANIFEST_BYTES` and
   `AF_TEST_SURVEY_DIGEST_TEMP_BYTES` lower the Tree Digest's ceilings (clause 9); a release
   build reads none of them. A window holds a commit when `cutoff - window < committer time <= cutoff`, for windows
   of 30, 90 and 365 days. So a complete Survey is byte-identical for the same head,
   `refs_digest`, `policy_digest`, Surveyor version, git version, cutoff and
   `previous_survey_id`, the last being the one field two forced refreshes of an unchanged
   checkout at one cutoff differ in; and a Survey that is not complete records where it stopped
   (clause 1), the one input the clock alone decides, so a replay of its record reproduces that
   stopping point.
5. **Policy.** The `[survey]` table of `.af/af.toml` sits in the project layer and deep-merges
   like every other table; every key is typed and an unknown one is refused, `refresh` included
   until PJ4. The keys and defaults are `max_wall_ms = 60000`, `max_commits = 100000`,
   `max_file_bytes = 1048576`, `max_paths = 200000`, `max_entries = 1000`,
   `max_survey_bytes = 1048576` (these three are ceilings: a layer may lower one to any
   positive integer at most 200000, 1000 and 1048576, and zero, a value above the ceiling or one
   that does not fit an integer is refused when the policy loads, from the project layer and the
   environment form alike, naming the key and the ceiling; an integer TOML itself cannot hold
   is refused with its file), `exclude = []`,
   `tests = ["**/tests/**", "**/*_test.*", "**/test_*", "**/fixtures/**"]`,
   `decisions = "docs/adr/*.md"`, `vocabulary = "CONTEXT.md"`,
   `release_tags = "v*"`, `identity = "name"` (or `"email_hash"`), `agent_authors = ["*[bot]"]`,
   `agent_trailers = ["*Claude*", "*Codex*", "*GPT*"]` and `hot_paths = 20`.
   `[survey.languages]` maps patterns to languages ahead of the built-in extension map that
   [docs/project.md](../project.md#surveys) lists, a pattern list like the others (clause 14):
   each file counts for exactly one language, the bytewise-lexicographically first configured
   pattern that matches it (one without `/` matches the file name, one with `/` the whole path),
   over the built-in map, which names a file by its exact name, else its longest extension; a
   file nothing matches is `other`, as symlinks and submodules are. Every pattern (`exclude`,
   `tests`, `decisions`, `release_tags`, `agent_authors`, `agent_trailers`, the keys of
   `[survey.languages]`) matches with `crate::project::glob_matches`, the one grammar of
   `.af/af.toml`: `**` spans segments, `*` a run inside one, `?` one byte of the encoded path
   (clause 14), and every other byte is literal, brackets included, so `*[bot]` matches `dependabot[bot]` and no name that merely ends in `t`,
   and `[0-9]` is three literal characters. A file `decisions` matches is a decision only when
   its name starts with four ASCII digits and a dash, so `docs/adr/README.md` is not one.
   `policy_digest` is `sha256:` + hex(SHA-256 of the canonical JSON of the effective policy).
   The generic environment form of the configuration ladder applies
   (`AF_SURVEY__MAX_COMMITS=1`); nothing bespoke is added.
6. **Contributors.** Each contributor carries a `key`, by which it is grouped and ranked, never
   by its display text. `identity = "name"` keys one by the first 16 hex digits of SHA-256 of
   the exact `%an` bytes, and never asks git for the email, and records `name`, the display
   text: the author name, cut at a character boundary to 256 bytes and marked `truncated` when
   longer, so two names sharing a 256-byte prefix are two identities with equal names. An author
   name any whitespace-separated token of which is an email address (an `@` between non-empty
   parts), or that is blank or holds a control character anywhere in the whole name (a control
   character past the 256-byte cut included), is replaced whole, before any cut and under both
   policies, by `h:` and the first 16 hex digits of SHA-256 of the lowercased,
   trimmed name, as its key and, under `name`, its name, and is grouped by that key under
   `email_hash` too, never by the email hash; the email is not used for it.
   `identity = "email_hash"` keys one by the first 16 hex digits of SHA-256 of the lowercased,
   trimmed `%ae` and records no name. The no-email
   rule covers author identities only: an author email never enters a record, an event, a
   document, a text output or an error, while repository text (decision titles, paths, terms,
   check names, hot paths) is recorded as committed, `@` included, bounded by clause 9. A commit
   is agent-assisted when its author name matches `agent_authors` or the name of any
   `Co-Authored-By:` trailer (the text before its `<address>`) matches `agent_trailers`,
   otherwise human; each commit counts once in that partition (`human_commits`,
   `agent_commits` and their shares in basis points) and once for its author, who keeps the
   per-identity totals; an empty window makes every share `0`. `contributors` carries, per
   identity, the commits, lines added and removed, first and last committer time and
   `active_90d`, the identities ranked by commits, then lines added, both descending, then key,
   up to `max_entries`, the count active in 90 days, and the top identity's share.
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
   newest first, and cut at `max_commits` (beyond it `partial`, with `oldest_reached`); the
   walk includes merges and runs `--numstat` with `--diff-merges=off`, so a merge counts as a
   commit and a merge and diffs nothing, contributing no churn; renames are
   not detected (`--no-renames`), so a rename is a removal and an addition; a binary numstat
   entry (`-` `-`) is one file touched and zero lines; ties break by path, then by commit ID.
   `history` comes from `git rev-list --count`, that `git log --numstat` and one `git ls-tree
   --name-only` of the hot paths (to mark the ones the head no longer holds `removed`), each run
   in a private bare repository attached only to the resolved object database, through the
   kernel's own mechanism, `review_source_git::Repo::object_repository`, the administrative
   directory that isolates ADR-0001's tree diffs (`--git-dir` that directory, its
   `GIT_OBJECT_DIRECTORY` the checkout's objects, `GIT_NO_REPLACE_OBJECTS=1`, from a directory
   outside the checkout), so the checkout, the index, local configuration, working-tree and
   `.git/info` attributes, diff drivers and replacement refs cannot change a count: history is
   measured from objects alone. Its local branches, tag count and newest five tags matching
   `release_tags` come from the survey key's one ref observation (clause 3), never from a second
   read of ref names. It holds commits and merges
   in total and per window, first and last committer time, churn per top-level directory over
   90 days, and the `hot_paths` paths with the most commits in 90 days. Every bounded list is
   ranked before `max_entries` cuts it and its totals cover the whole population: contributors
   by commits, then lines added, both descending, then key; hot paths by touching commits in 90
   days, descending, then path; release tags by tagger time (annotated) or the target's
   committer time (lightweight), newest first, undated last, then name; directories,
   languages, churn, terms and checks by name; decisions by number, then path. The tree is
   folded as its listing and its blobs stream, and a file's language is the one clause 5
   names. Intermediate state is bounded too: the history walk tracks at most `max_paths`
   distinct paths (the paths of the 90-day window, and the paths of any one commit), and no
   streamed record (a tree entry, a numstat line or log field, a ref, a `cat-file` header) is
   read past 64 KiB; beyond either the walk or the listing stops, the section is `partial`
   with reason `max_paths` or `max_record_bytes` (`contributors` with the walk), and its totals
   and rankings cover the population observed before it; a ref of the survey key past 64 KiB
   records `SurveyFailed@1`. The log also runs with `--no-mailmap --no-textconv --no-ext-diff
   --diff-algorithm=myers` and `log.showRoot=true`, so neither a
   checkout's configuration nor a program it names changes or computes a count, and
   `identity.git_version` records the exact `git --version` stdout, trimmed, because git
   decides these numbers. The log is `-z` and its format ends every field with `%x00`, a byte
   no author name, email, trailer or subject git writes can hold, never with a byte an author
   can write (0x1f split a name of 256 printable bytes and 0x1f, and the fields after it, into
   another identity): the commit ID, the parents, the committer time, the author name, the
   author email under `email_hash` alone, the commit ID again, the `Co-Authored-By` values and
   the commit ID a third time. A commit cannot hold its own ID, so the second proves the fields
   before it were read whole and the third ends the trailers, however many there are; a numstat
   line is told from a commit by its tabs. A record that still does not parse (a field that
   holds a NUL, so the ID is not where the format repeats it, a parent or a time that is not
   one, a numstat line or an item that is neither) stops the walk at that commit: `history` and
   `contributors` are `partial` with reason `malformed_record` (`unavailable` when it was the
   first commit), the commits before it counted, and `history.stopped_in` and the stopping
   point name it; no identity is ever taken from such a record.
8. **Decisions, vocabulary, checks.** `decisions` holds every decision record (clause 5) up to
   `max_entries`, sorted by number, then path, with `count` and `by_status`; a Brief variant
   picks the newest N in PJ3. Each has the number (the four leading digits), the path, the
   title from the first H1, and the status and date from the first line that starts with
   `**Status:**` or `Status:`: the status is the first of `proposed`, `accepted`, `superseded`,
   `deprecated` and `rejected` on that line (as a word, in any case; so `accepted; superseded in
   part by` is `accepted`), with `amended: true` when the line says `amended`, and the date is
   the first `YYYY-MM-DD` on it when that is a day of the calendar (a month `01` to `12` and a
   day that month has in that year: `2024-02-29` is one, `2025-02-29` and `2026-02-31` are not,
   and the record's validator and schema refuse them); each is absent, the status counted
   `unknown` in `by_status`, when absent or malformed. A line naming a status that does not
   start so, prose included, names nothing. `vocabulary` lists the names of
   the `**Term**:` headings of the `vocabulary` file, by name, each once, and never a
   definition. `checks` lists the names and programs (an absolute program reduced to its file
   name; never an argument) of exactly these sources: `.af/code-policy.toml`'s `[checks.*]`, and
   each `[[checks]]` of every `.af/pipelines/*.toml` in the head tree, routed or not. A check's
   source is `code-policy` or the pipeline file's basename and its identity `<source>:<name>`,
   so equal names in two pipelines are two checks and the first declared of an identity is
   kept. It lists the toolchain files present at the root among `rust-toolchain.toml`,
   `rust-toolchain`, `.tool-versions`, `.nvmrc`, `.node-version`, `.python-version`,
   `.ruby-version` and `.java-version`, and counts `.github/workflows/*.yml` and `*.yaml`.
   `declared`, `toolchain` and `unreadable` are each cut at `max_entries` after ranking, with
   `seen` and reason `max_entries`. A file these sections read that is over `max_file_bytes`,
   not text or not valid UTF-8 is never decoded lossily: it makes its section `partial` with
   reason `max_file_bytes`, never `complete` with nothing in it: a decision is listed with
   status `unreadable` and no title, the vocabulary is `present` with `terms` and `count`
   absent, and a declaration is named under `unreadable`.
9. **Bounds and processes.** The Survey has one absolute deadline, its start plus
   `max_wall_ms`. Every git process of the Surveyor runs through `projects/git.rs`, supervised
   in delivery's isolated command, with a deadline equal to the lesser of its own limit
   (`DEFAULT_GIT_TIMEOUT_SECONDS` of `crates/review-source-git/src/git.rs`) and what remains of
   the Survey's deadline (`Git::within`; `Git::run_within` for a one-line answer,
   `Git::stream_within` for every stream, `Git::repo_within` for the Tree Digest, every command
   of which ends by that one instant), in its own process group, which is terminated when that
   deadline passes, also while af waits for output that never comes, so a silent git never
   extends a refresh; a git stopped at its own limit before the Survey's deadline fails the
   Survey. Every stream (`cat-file --batch`, `ls-tree`, `log --numstat`, `for-each-ref`) is
   consumed as it arrives, never buffered whole, and every loop over tree entries, blobs,
   commits, numstat lines, decision files, tags and refs, and every loop over what a stream read,
   checks the clock once per item: the tree's aggregation, folded as its streams arrive; the
   ranking of identities; the reading of each decision, of each line of the vocabulary file and
   of each declaration, an oversized or unreadable one included, which is charged before it is
   handed on, and of each declared check, each parsed as `cat-file` yields it; and the clock is
   checked after every git stream, so a stream that ends after the deadline ended late. Every
   section's finishing work (the tree fold's finish with its directory and language cuts,
   history's materialization, the contributors' sort and materialization, the decisions'
   aggregation, the checks' finish) is followed by one last check before the section is
   recorded, and a section is complete only when that check found the deadline ahead. A section
   whose work, or finishing, passed the deadline stops there and is `partial` with reason
   `max_wall_ms` and what it reached and finished (`unavailable` when it reached nothing), never
   `complete`, every later section is `unavailable`, and the Survey is still recorded within the
   one deadline, past it by at most one item or one pattern match and the end of the process
   group of the git command then running. The Tree Digest is the committed capture's content
   digest of the whole head tree, independent of `exclude` and every maximum, computed by a
   digest-only path, `review_source_git::committed_digest`: no blob is written to any CAS and no
   listing is buffered whole; each blob streams through the hash in one fixed 64 KiB buffer; the
   listing is read once into sorted runs held in at most 16 MiB of manifest buffering, each run
   beyond it spilled to at most 256 MiB of temporary manifest data in the system's temporary
   directory, fixed ceilings no project may raise, and the runs are merged into one `cat-file
   --batch` in canonical order, under the Survey's deadline. The runs live beneath a
   process-owned temporary root the crash sweeper reaps: one directory
   `af-tree-digest-<pid>-<random>` (`review_source_git::digest::SPILL_PREFIX`) that
   `review_sandbox::stale` recognises beside `af-sandbox-<pid>-*` and `af-check-<pid>-*`, under
   the same rules: kept while a process with that pid exists, left for the operator when marked
   `preserved`, and otherwise removed through directory descriptors opened without following a
   link. The sweep of review and Task work takes such a directory, and the Surveyor sweeps the
   `af-tree-digest-` directories alone before each digest, so a refresh killed after it spilled
   leaves its runs only until the next refresh or run. Its digest equals
   `Manifest::content_digest` of the committed capture of the same tree: both hash the entry
   count first and then every entry in the byte order of its encoded path through one framing,
   `ManifestDigest`. When it does not finish it is absent with `tree_digest_reason`: `deadline`
   when the deadline passed (and every later section is `unavailable`), `max_record_bytes` for a
   listing record over 64 KiB, `manifest_bytes` when one manifest record does not fit the
   manifest buffer, `temp_bytes` when the runs would exceed the temporary ceiling, the last
   three making `identity` partial with that reason and stopping no other section; it is never
   the digest of a part, and its temporary storage is removed on every way out, a ceiling's
   included. Fixed maxima
   bound the rest: `max_entries` items in any list of a section (directories, languages, churn,
   hot paths, tags, decisions, terms, contributors, and the declared checks, toolchain files and
   unreadable declarations of `checks`), 256 bytes for a display string (a
   contributor name, a decision title, a term, a tag or a check name or program, cut at a
   character boundary and marked `truncated: true`), while identity strings (every field of
   `identity`, commit IDs, digests, ref names and paths) are recorded exactly, and
   `max_survey_bytes` for the record: one that needs more records `SurveyFailed@1` naming the
   bytes it needed, and nothing else. It runs plumbing only: no checkout, hook, filter, textconv
   or project code, and nothing written inside the checkout. Any other failure records
   `SurveyFailed@1`. A Survey never calls a Provider and never takes a writer lease on a Task
   Store; it holds the Project Store's lock only to read the log and to record.
10. **Commands.** `af project survey [--section NAME…] [--json]` prints the newest Survey's
    summary or its record, and exits 1 with one line naming `af project refresh` when there is
    none. `--section` is an option of the text alone: it picks the named sections' lines of the
    summary, in record order; `--json` prints the complete recorded `af/ProjectSurvey@1`
    unchanged, and `--section` with `--json` is usage error 2. `af project refresh
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
    builds a Brief) cannot be taken now, or one used within the hour, so `keep_surveys` is a target the
    protected Surveys may exceed; the Project's log, `workspaces.toml`, `project.lock` and
    operator files are never reclaimed, and the Project Store is still never evicted whole. A
    collection takes the lock without waiting, so a held Store is left for the next pass, reads
    the protections again under it, and for each Survey appends `SurveyCollected@1`
    {`survey_id`, `head`, `cutoff_unix_s`, `completeness`, `refs_digest`, `policy_digest`,
    `surveyor_version`, `previous_survey_id` when any, the `tree` totals `files`, `bytes` and
    `lines` and the `history` totals `commits` and `merges` when the record holds them,
    `collected_unix_ms`} ([schema](../../schemas/project-survey-collected-v1.json), in
    `run-event-v1.json`) before it deletes the record from the CAS. A sweep plans once: one
    function takes one inventory snapshot and returns the plan, the ordered entries it removes,
    each with the bytes it frees, its rule (`recovery` for what an interruption left, then
    `collection` and, for Surveys beyond `keep_surveys`, `keep_surveys`, then `budget`) and the
    total after it, and the entries it leaves with their reasons (`held`, `foreign`, `oversize`,
    `unreadable_name`), computed over an in-memory model of the snapshot that its own removals
    update and never over a second inventory, so the bytes of a record an interruption moved
    under `cas/removing` are subtracted once, from the Project Store entry that holds them, and
    never restored (Task Stores are told apart into their finished Tasks, in place, only when
    the snapshot holds more than `max_bytes`). `af storage prune` prints that plan; `--apply`
    runs the same plan entry by entry and reports what each entry did, the bytes it freed and the
    total after it, and a Survey's job that finds what the plan did not (the lock lost, the
    Survey protected or collected since, its record replaced, gone or refused) marks it on that
    entry as `deviation` with nothing freed, and the sweep never plans again; the next one does.
    The tombstone binds the
    event: every field but `survey_id` and `collected_unix_ms` is copied from the record after
    the event was verified against it on every field clause 2 compares, never from the event,
    so replay refuses a collected event that differs from its tombstone on any of them, naming
    the field. A Survey whose event disagrees with its record is refused, preview and pass
    alike, and reported `refused: its SurveyRecorded@1 names a <field> its record does not
    record, so it is kept`: no tombstone is appended and its record stays.
    A record is reached, read and deleted only through directory descriptors, ADR-0144's removal
    boundary: the Project Store from `/`, then `cas`, `objects` and the digest-prefix directory,
    each opened `O_NOFOLLOW | O_DIRECTORY`, and the record stated without following it (its
    device and inode, the identity read before the tombstone) and opened `O_NOFOLLOW` relative
    to its prefix directory. The deletion names the inode it checks: each job creates its own
    container `cas/removing/<pass-id>/` exclusively (`mkdirat` with mode 0700 under 128 fresh
    random bits, failing when the name exists) on the Store's device, renames the record into
    it with `renameat`, never over a taken name, and immediately before `unlinkat` states it
    again with `fstatat`, without following it, relative to the container's own descriptor; only
    the device and inode stated before the tombstone is unlinked, and the bytes reported are the
    allocated bytes of that inode. This is the Store's trust boundary: inside a container this
    pass created, only a process of the same user can replace a file between that check and the
    unlink, the boundary delivery's rollback already accepts (ADR-0031), and a replacement the
    check observes is refused and reported: the file is moved back to its name, or, when the
    name is taken again, renamed to `<survey-id>.replaced` in the container, a name no pass
    deletes, nothing is deleted, and the Survey is reported `refused (replaced during
    collection)`; its tombstone, appended before the rename, stays. A symbolic
    link on that way or in place of the record, a directory on another device than the Store's,
    or a record the file system will not let af read (permission denied, an I/O error) refuses
    the Survey: inventory measures nothing through a link and lists it in use, and the pass
    reports it with its reason, appends no tombstone and deletes nothing. The Surveyor's writer,
    the reader of a Survey and collection share one ceiling, `MAX_SURVEY_BYTES` (1 MiB, the
    ceiling of `max_survey_bytes`): collection reads a record up to it and one byte more, and a
    record over it, by its stated length or its bytes, is never taken for corrupt: the plan
    leaves it as `oversize` with `refused (oversize, <bytes> bytes)`, inventory lists it in use
    for that reason, and it keeps its record and its place in the log. A record within the
    ceiling whose bytes were read but are no Survey (they do not match its digest or do not
    parse), or that is gone from the CAS, is collected all the same, since reclaiming corrupt
    bytes is the budget's job:
    its tombstone carries `unreadable`, af's own sentence of at most 256 bytes and never the
    record's content, in place of the totals, and, since no record vouches for them, the
    event's identity fields as collection read them, and the sweep and `af project history` show it as
    `collected (unreadable)`. Inventory, the plan and each applied job decide that a Project
    Store's Surveys are held by one predicate,
    `lock_available`: `project.lock` opened with the flags the applied pass holds it with and
    locked without waiting, then released at once. While a refresh or a plan holds it their
    reason is that; while af cannot open or lock the file (it is unreadable or unwritable) it is
    `lock unavailable: <error>`; either way the plan leaves every Survey it would take
    from that Store, and every deletion it would finish there, as `held` with that same reason,
    inventory lists them in use, and nothing is collected. The tombstones are the run
    `survey-collection` of the Project Store's log, beside the run `project` that holds the
    chain, so the chain's own sequence is only ever the Surveyor's. Every sweep's plan first
    finishes what an interruption left, read without the lock, under each Store's lock when it
    runs: `cas/removing` is walked with raw (`OsStr`) names end to end, `fstatat`, rename and
    unlink taking the raw name. A regular file in a container of a dead pass (every pass holds
    the Store's lock, so a container found under it is a dead one's) named by a tombstoned
    Survey is that Survey's record, moved: its bytes are the Survey's, and it is settled by the
    same sequence, renamed into the new pass's own container, checked there and unlinked, and
    the dead container goes once it is empty. A file at that Survey's CAS name is then a foreign
    object: it is left in place and listed as `foreign` with its bytes, counted in the Store's
    total and never as the Survey's, and never deleted by any path. Without a moved record, the
    file at a tombstoned Survey's name is its record, deleted as above, only when its bytes are
    the record's (they hash to its ID); any other is `foreign` in the same way. Everything else
    under `cas/removing` is left and listed: a name that is not UTF-8 as `unreadable_name`, its
    display lossy only in the text output and its `--json` path in `review_core::encode_path`'s
    spelling with `path_encoded`, and anything else (no pass container, not a regular file, no
    tombstone naming it, a `.replaced` file) as `foreign`; a container of a dead pass that holds
    nothing is removed. Each such deletion has the rule `recovery`, and the preview lists exactly
    those entries, so preview and pass agree entry for entry. A debug build reads
    `AF_TEST_SURVEY_STOP_AFTER_TOMBSTONES=1` to stop after the tombstones, and
    `AF_TEST_SURVEY_REPLACE_BEFORE_RENAME=<directory>` to replace a record by a byte-identical
    copy between its identity read and its rename. Replay
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
    (`exclude`, `tests`, `agent_authors`, `agent_trailers` and the keys of `[survey.languages]`)
    at most 64 patterns, beyond which the policy is refused naming the key and the refresh
    records `SurveyFailed@1`. Every pattern list the Surveyor applies checks the Survey's
    deadline before each match: `exclude`, `tests`, `agent_authors` and `agent_trailers` through
    one loop, `any_match_within`, and `[survey.languages]` in bytewise order through
    `Policy::language_within`, so one match is the most a deadline can be overrun by; a list cut
    by the deadline matches nothing more (a file whose language it cut is not folded), and the
    section it cut is partial with reason `max_wall_ms` and every later one unavailable. Only the
    built-in fallback of a language is an exact lookup of a name or an extension, not a pattern
    match, and `decisions` and `release_tags` are single patterns applied once per streamed
    item.

## Measured on this repository

A Task's sandbox holds the Snapshot's tree and no history, so the Surveyor cannot be measured
on afactory inside the Task that builds it: the operator measures `af project refresh` on the
real checkout, and the pull request states its wall time and the record's size (§5 PJ2 of the
design note). This ADR records only the bounds above and the fixture's measured size: the
record of `crates/af/tests/fixtures/project-survey/expected.json`, eight commits and 18 files,
is 4,572 bytes plus the length of `git --version` less 11 (4,595 bytes with
`git version 2.54.0 (Apple Git-157)`), against `max_survey_bytes = 1048576`.

## Consequences

- `af project refresh` answers how big the Project is, how it changes, who works on it, what it
  decided, what words it defines and what it checks, from one record whose every number the
  kernel counted. PJ3 renders Briefs from it; nothing here enters a prompt.
- A second refresh with an unchanged survey key costs `rev-parse`, the one `for-each-ref`, the
  policy and one read of the log, and a refresh whose key changed reads the refs no second
  time. A new tag or branch, a changed `[survey]` policy or a new Surveyor
  version surveys the same head again, and so does a partial newest Survey.
- A Survey cut by `max_wall_ms`, `max_record_bytes`, `max_paths`, `max_commits`, `max_entries`
  or `max_file_bytes` is recorded and says so, with `seen` for every cut list; a reader never
  mistakes it for a complete one. A Survey's wall time, size and intermediate state are bounded
  whatever the repository holds, the survey key's reads included.
- Contributor counts under `name` split one person who wrote under two names; `email_hash` joins
  them by address at the cost of opaque identities.
- Surveys accumulate in the Project Store until a later round collects them; each is a few
  kilobytes, at most `max_survey_bytes`.
- Tests: `crates/af/tests/it/project_survey.rs` builds a fixture repository with fixed dates,
  names, emails and messages (a merge, a rename, a binary change, a commit authored a year
  before it was committed, a human commit with an agent trailer, one with two, an author named
  by an email address and an ADR titled with one) and compares its Survey byte for byte with
  `crates/af/tests/fixtures/project-survey/expected.json`, with this machine's `git --version`
  in its one place, stating the totals and pinning the walkthrough's disputed cases (status
  words in prose before a status line, a vocabulary written out of order, `.tool-versions`
  beside `.go-version`, and `*.rs` and `src/**` both matching `src/core.rs`); proves the `max_commits` boundary in date order; proves
  an unchanged survey key records nothing and prints the key, while a new commit, tag or
  branch, a partial newest Survey and `--force` each record one chained to the one before;
  refuses `max_wall_ms = 1`, which computes no survey key, and records a partial Survey with the
  Tree Digest absent and its reason when the deadline falls inside `identity`; stops a
  2,000-file tree at the deadline inside `tree` with every later section unavailable and the
  refresh under `max_wall_ms` + 2 s; with `AF_TEST_SURVEY_SLOW_SECTION`, leaves `tree`,
  `contributors`, `decisions`, `vocabulary` and `checks` each partial with what it reached when
  the deadline falls inside its own work after its streams, every earlier section complete and
  every later one unavailable, each refresh under `max_wall_ms` + 2 s; fails a refresh whose
  fake git never answers the key's `for-each-ref` with `SurveyFailed@1` within `max_wall_ms` +
  2 s; refuses at replay a hand-written `SurveyRecorded@1` whose `refs_digest` differs from its
  record's, in `af project history` (text and `--json`), `survey` and `refresh`, naming the
  sequence and the field, and an older one in `history`; refuses replay in `history` (text and
  `--json`), `survey` (with and without `--section`) and `refresh` when the newest record of
  two uncollected Surveys is corrupt, naming its sequence and the read error and none of the
  event's fields, and in `history` when the older one is gone; binds a collected Survey's
  tombstone to every identity field of its record, refuses `survey` with and without
  `--section`, `refresh` and `history` once the collected newest event's head names another
  commit, and `history` once its `refs_digest` changed, naming the field; and refuses to
  collect, preview and pass alike, a Survey whose event disagrees with its record, leaving its
  record and appending no tombstone; leaves each section partial, every
  earlier one complete and every later one unavailable, when `AF_TEST_SURVEY_SLOW_FINISH` makes
  its finishing step outlast the deadline; stops forty oversized declarations ahead of a
  readable one within one item each; leaves `2026-02-31` and `2025-02-29` unknown and keeps
  `2024-02-29`; keys blank and control-character author names, 256 printable bytes followed by
  a control character included, by the hash of the name under both identity policies, two
  emails one identity; ends a silent fake git's process group at a
  2-second deadline with the Survey partial and the refresh within 2 s more; digests a blob
  over `max_file_bytes`, a tree beyond `max_paths` and spilled manifest runs equal to the
  committed capture's digest, and leaves the digest absent with `manifest_bytes` or
  `temp_bytes` and no `af-tree-digest-` directory behind; kills one refresh after its spill and
  sweeps its directory at the next refresh while a live refresh's is kept; reads a name of 256
  printable bytes and 0x1f as its `h:` key under both identity policies and a subject holding
  0x1f and a newline as one agent-assisted commit; leaves `history` and `contributors`
  partial with `malformed_record`, `stopped_in` and the stopping point naming the commit, when a
  git writes a commit's time as no number, under both identity policies; counts a four-commit history with one merge
  at `max_commits` 4, 3 and 1 (four commits, a merge and three authors; three, the merge and
  three; the merge alone and its author, no churn); finds two forced complete refreshes at one
  cutoff equal but for `previous_survey_id`, and a partial one at `max_commits` 7 recorded twice
  with the same stopping point, printed by `af project survey`, and a deadline in the decisions'
  finishing step stopping at its fourth item; refuses `--section` with `--json`
  with exit 2 while `--json` prints the complete record; refuses a 65-pattern language map;
  leaves the history counts unchanged under an untracked `.gitattributes`, a
  `.git/info/attributes` and a local diff driver that each turn the checkout's own `2 0` into
  `- -`; keeps a tag created between the key and the history's `log` out of both; refuses zero, above-ceiling and overflowing `max_paths`, `max_entries` and
  `max_survey_bytes` from the project layer and the environment; states the status lines (two
  dates, a partial supersession with `amended`, a missing date, no status line), the one-third
  share `3333`, the ranking by commits before lines added and a cut at `max_entries = 1`, the
  checks of an unused pipeline, of equal names in two pipelines and of two toolchain files, and
  the languages of overlapping configured patterns, of a pattern over a built-in extension and
  of an unmatched file; leaves `history` partial with `max_paths` for a small head whose one
  historical commit touched more paths than `max_paths`; lists one of two oversized pipelines
  under `unreadable` with `seen` 2 at `max_entries = 1`; leaves a non-UTF-8 decision,
  vocabulary and declaration unreadable; hashes the names `carol@fixture.invalid` and
  `Alice alice@example.org` under both identity policies; cuts at `max_paths` and
  `max_entries` with `seen`,
  records `SurveyFailed@1` naming the bytes under `max_survey_bytes = 1024`, cuts and marks a
  300-byte author name, keeps a second name with the same first 256 bytes a second identity
  with equal display text, and keeps a 286-byte seven-root `repository_id` exact; leaves an
  oversized vocabulary, decision and declaration partial with `max_file_bytes`; finds no author
  email in any byte of the state home or any output under either identity policy, an
  email-shaped author name included, while the ADR titled with an address is recorded as
  committed; records the first Survey at `af onboard --apply` once; and records and shows a
  `SurveyFailed@1` for an unknown `[survey]` key. PJ1's test that no `af project` command
  writes inside the checkout covers `refresh`, `survey` and `history`. Unit tests beside the
  Surveyor refuse a JSON number in a record and sort every map, match every pattern with the
  one grammar (`dependabot[bot]` matches `*[bot]`, `[0-9]` is literal, `docs/adr/README.md` is
  no decision), refuse unknown and invalid policy keys, read git's listing, batch and log
  streams item by item and stop at a passed deadline, at a record over 64 KiB and at a commit
  over `max_paths`, stop the ranking, the terms and the checks at a passed deadline, and fold
  windows, the partition, shares, rankings, status lines, languages, cut lists, decisions,
  terms and checks, stop language matching within one pattern of a passed deadline, refuse a
  65-pattern language map, hash blank and control-character names under both policies, a
  control character past the 256-byte cut included, refuse an unreadable record naming the
  sequence and the error, bind a tombstone to every identity field of its record and refuse an
  event that differs, fold the ref observation into tags and branches, check the clock
  after a finishing step, read a 0x1f name whole under both policies and a trailer holding 0x1f
  and a newline as one, and stop at a NUL in a name (under both policies), a time, a parent, a
  numstat count or an item that is no record with `malformed_record` naming the commit and
  passing on the commits before it. `review_sandbox::stale` sweeps a dead `af-tree-digest-`
  directory and keeps a live and a preserved one.
  `crates/review-source-git/tests/it/tree_digest.rs` proves the digest-only path equal to the
  committed capture held and spilled, its typed stops with nothing left in temporary storage,
  and the private object repository reading `2 0` where the checkout reads `- -`.
  `crates/review-core/tests/schema_parity/project.rs` pins
  the record, its stopping point (required exactly when not complete, the deadline's section
  over an earlier bound, `history`'s commit, the first declaration not read, `malformed_record`
  in the walk's sections alone), both events and the history document, and `crates/af/tests/it/cli_surface.rs` the
  help.
