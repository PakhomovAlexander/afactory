- Surveys join the Storage Budget: every Survey is an `af storage` entry of kind `survey` with its
  CAS bytes, last used at its cutoff (`af/storage@3`, `af/storage-prune@2`). An explicit
  `af storage prune --apply`, or the sweep after a run when `[storage] auto_gc = true`, collects
  the Surveys of each Project beyond the newest `[storage] keep_surveys` (50,
  `AF_STORAGE__KEEP_SURVEYS`), oldest first and idle at least `keep_days`, and the budget may take
  any Survey, oldest first, while af is over `max_bytes`; neither takes the newest complete
  Survey, one the Project Store's lock holds or one used within the hour, and
  `af project refresh` collects nothing. Collection appends a `SurveyCollected@1` tombstone with
  the Survey's head, cutoff, completeness and tree and history totals before it deletes the
  record, and the next sweep finishes a deletion an interruption left. `af project history` marks
  a collected Survey `collected` from its tombstone, and `af project survey` says that a collected
  newest Survey was collected and names `af project refresh`. `.af/af.toml` patterns, the routes'
  and the Surveyor's alike, now match by dynamic programming with the same grammar, so a match
  costs at most (pattern length + 1) × (path length + 1) cells and `*a` thirty times then `b` no
  longer hangs; a `[survey]` pattern is at most 256 bytes and a list at most 64 patterns
  ([ADR-0152](docs/adr/0152-measure-a-project-into-surveys-with-a-token-free-surveyor.md)).
