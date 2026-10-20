- Bare `af` no longer runs `git diff` against the working tree or `git hash-object` to mark
  drifted Pipelines and Workers. Both could run an fsmonitor or a clean filter that a checkout's
  own `.git/config` names. The browser now reads the committed blobs with `ls-tree` and
  `cat-file --batch`, reads working-tree files directly, and compares the two in-process. Every
  git call the browser makes, and the Task file advice's `git check-ignore`, now runs with
  capture's cleared environment and full `-c` override set
  (`review_source_git::git::isolated_command`). That advice still reads the global excludes file
  at git's default place
  ([ADR-0122](docs/adr/0122-show-committed-workers-and-their-recorded-attempts.md),
  [ADR-0115](docs/adr/0115-declare-the-af-layout-and-keep-task-files-out-of-git.md), amended).
- A tree that names a path git itself refuses is refused, with an error naming the path, at
  capture, materialization and Task binding. One shared predicate (`git_refuses_path`) covers
  `.git` in any ASCII case, its NTFS and HFS+ spellings (`.git.`, `.git` with a trailing space,
  `git~1`, HFS-ignorable code points) and a symlinked `.gitmodules`. On a sandbox volume that
  folds case or Unicode normalisation (probed once per root), materialization refuses two paths
  that would name one file, naming both. Committed capture streams `cat-file --batch` bodies
  through one 64 KiB buffer and no longer allocates a blob whole. Stored bytes and digests are
  unchanged
  ([ADR-0020](docs/adr/0020-stream-cas-materialization-and-clone-duplicates.md), amended).
  `review-source-git` gains the `unicode-normalization` dependency, already in `Cargo.lock`.
- A tree entry with a legacy regular-file mode such as `100664` is captured and reviewed as a
  file, executable when its owner bit is set, instead of failing the review.
- Text output of `af review plan`, `ledger`, `show`, `report` (text and Markdown), `render` and
  `run` shows every control character in a changed path, finding title or other repository- or
  model-authored string as a visible `\u{…}` escape. ESC, BEL and newlines no longer reach the
  terminal raw. `--json` output is unchanged.
