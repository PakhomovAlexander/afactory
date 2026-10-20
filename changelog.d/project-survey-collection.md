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
  deletes the record. A sweep plans once over one inventory snapshot: `af storage prune` prints
  that plan, every removal in order with the bytes it frees, its rule (`recovery`, `collection`,
  `keep_surveys`, `budget`) and the total after it, and under `left` what it leaves (`held`,
  `foreign`, `oversize`, `unreadable_name`); `--apply` runs the same plan entry by entry, keeps an
  entry of any kind whose removal failed (the lock lost, what it names replaced, an I/O error) in
  its place as `outcome` `failed: <reason>` with nothing freed and the running total unchanged
  instead of choosing again, and so stops where its preview stops, a pending recovery's bytes
  counted once and its totals what af then holds. The claims dead processes left
  (`.af-removing-*`) and the containers dead passes left empty are in that one inventory, `claim`
  entries under `recovery` that the preview and the applied sweep list alike with the same bytes,
  and nothing is removed beside the plan; a Survey's record is measured once by device, inode and
  bytes, and one replaced between inventory and the plan is `refused (replaced since inventory)`
  and never adopted. A record is reached and read only through directory descriptors that follow no link, and
  deleted inside a container each job creates for itself, `cas/removing/<pass-id>/` (mode 0700, a
  fresh random ID), where it is stated again relative to the container just before the unlink, so
  only the inode found before the tombstone is deleted and a replacement is refused, put back,
  and the Survey reported `refused (replaced during collection)`; the next sweep collects it. The
  next sweep also settles a dead pass's container the same way: a file that took a collected
  Survey's CAS name after its record was moved, or that holds bytes other than the record's, is
  `foreign`, listed with its bytes, never counted as the Survey's and never deleted, and a
  leftover whose name, or whose container's name, is not UTF-8 is walked with its raw name, listed
  `unreadable_name` (its `--json` path percent-encoded) and left with its container. A CAS prefix directory replaced by a symbolic link
  refuses its Survey, which is reported and keeps its record and its place in the log; the
  Surveyor, the reader of a Survey and collection share one 1 MiB ceiling, and a record over it is
  left as `oversize` with its size, while a corrupt record within it is collected with an
  `unreadable` tombstone in place of its totals and its bytes freed, shown as `collected
  (unreadable)` by the sweep and `af project history`; one the file system will not let af read
  is refused and left as it was. The preview and the sweep decide `held` by one predicate,
  `project.lock` taken as the sweep takes it: the Surveys of a Project Store a refresh or a plan
  holds, or whose lock af cannot open (`lock unavailable: <error>`), are left `held` with the
  same reason by both, and nothing is collected. `af project history` marks a collected Survey
  `collected` from its tombstone, and `af project survey` says that a collected newest Survey was
  collected and names `af project refresh`. `.af/af.toml` patterns, the routes' and the
  Surveyor's alike, now match by dynamic programming with the same grammar, `?` one byte of the
  canonical encoded spelling, so a match costs at most (pattern length + 1) × (path length + 1)
  cells and `*a` thirty times then `b` no longer hangs; a `[survey]` pattern is at most 256 bytes
  and a list at most 64 patterns, and the Surveyor checks its deadline before each pattern it
  matches ([ADR-0152](docs/adr/0152-measure-a-project-into-surveys-with-a-token-free-surveyor.md)).
