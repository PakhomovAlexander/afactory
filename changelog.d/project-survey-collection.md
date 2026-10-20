- Surveys join the Storage Budget: every Survey is an `af storage` entry of kind `survey` with its
  allocated CAS bytes, last used at its cutoff (`af/storage@3`, `af/storage-prune@2`; the
  superseded `af/storage@2` schema is gone and both earlier versions are refused, while
  `af/TaskStorageSweep@1` keeps its shape and kinds and counts a Survey's removal without listing
  it, so it needs no new version). An explicit `af storage prune --apply`, or the sweep after a
  run when `[storage] auto_gc = true`, collects the Surveys of each Project beyond the newest
  `[storage] keep_surveys` (50, `AF_STORAGE__KEEP_SURVEYS`), oldest first and idle at least
  `keep_days`, and the budget may take any Survey, oldest first, while af is over `max_bytes`;
  neither takes the newest complete Survey, one the Project Store's lock holds or one used within
  the hour, and `af project refresh` collects nothing. Collection appends a `SurveyCollected@1`
  tombstone with the Survey's head, cutoff, completeness and tree and history totals before it
  deletes the record, and the next sweep finishes a deletion an interruption left, which
  `af storage prune` without `--apply` lists under `recovery` with the bytes it frees, entry for
  entry as the applied sweep does. A record is reached and read only through directory
  descriptors that follow no link, and deleted by rename-then-verify: moved into
  `cas/removing/<survey-id>` of its Store and unlinked only when that is the inode found before
  the tombstone, so a record replaced in between is moved back, nothing is deleted, the Survey is
  reported `refused (replaced during collection)` and the next sweep collects it. A CAS prefix
  directory replaced by a symbolic link refuses its Survey, which is reported and keeps its record
  and its place in the log; the Surveyor, the reader of a Survey and collection share one 1 MiB
  ceiling, and a record over it is refused as `oversize` with its size and left, while a corrupt
  record within it is collected with an `unreadable` tombstone in place of its totals and its
  bytes freed, shown as `collected (unreadable)` by the sweep and `af project history`; one the
  file system will not let af read is refused and left as it was. The preview and the sweep decide
  `held` by one predicate, `project.lock` taken as the sweep takes it: the Surveys of a Project
  Store a refresh or a plan holds, or whose lock af cannot open (`lock unavailable: <error>`),
  are listed under `held` with the same reason by both, and nothing is collected. `af project history` marks a collected Survey `collected`
  from its tombstone, and `af project survey` says that a collected newest Survey was collected
  and names `af project refresh`. `.af/af.toml` patterns, the routes' and the Surveyor's alike,
  now match by dynamic programming with the same grammar, `?` one byte of the canonical encoded
  spelling, so a match costs at most (pattern length + 1) × (path length + 1) cells and `*a`
  thirty times then `b` no longer hangs; a `[survey]` pattern is at most 256 bytes and a list at
  most 64 patterns, and the Surveyor checks its deadline before each pattern it matches
  ([ADR-0152](docs/adr/0152-measure-a-project-into-surveys-with-a-token-free-surveyor.md)).
