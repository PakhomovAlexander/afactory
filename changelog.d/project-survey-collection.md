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
  `foreign`, `oversize`, `unreadable_name`, `too_deep`, `unmeasurable`); `--apply` runs the same plan entry by
  entry, keeps an entry of any kind whose removal failed (the lock lost, what it names replaced
  or gone since the inventory, an I/O error) in its place as `outcome` `failed: <reason>` with
  nothing freed and the running total unchanged instead of choosing again, while a deletion that
  stopped part way, a failed final `rmdir` included, reports the bytes that actually left the disk,
  accounted from the removal's own start (what the directory held immediately before anything was
  unlinked less what its claim still holds), and so stops where its preview stops, a pending recovery's bytes
  counted once and its totals what af then holds. A directory nested deeper than the removal
  walk's 128 levels is left `too_deep` by preview and apply alike and listed in use by `af
  storage`. The claims dead processes left (`.af-removing-*`) and the containers of dead passes
  are in that one inventory, `claim` entries under `recovery` that the preview and the applied
  sweep list alike with the same bytes, a container that held a moved record planned after it,
  and nothing is removed beside the plan; every one is an `af storage` entry of kind `claim`,
  `state` `abandoned`, with its measured bytes, so the listing's claims are the preview's claim
  recoveries, and its bytes are counted once: in its kinds and total as the preview counts it,
  or, for a claim inside a Project Store, in the Store's bytes, the claim saying so with
  `counted_in`, while one the inventory
  cannot measure is left `too_deep` or `unmeasurable` with no bytes and never touched. A registration whose path holds no Store is dropped by the plan
  (`registry_dropped`, the same in preview and apply), after the inventory reached its parent's
  claims, so a registered Store a dead process renamed into its claim is recovered alike by
  both. Each uncollected Survey's `SurveyRecorded@1` is verified against its record once, in the
  Project Store's one read, and the plan and the inventory read only what that verified: an event
  whose cutoff its record disproves is never the Survey's last use, and the Survey is refused and
  kept. A Survey's record is measured once by device, inode and bytes, and one replaced between
  inventory and the plan is `refused (replaced since inventory)` and never adopted. A record is reached and read only through directory descriptors that follow no link, and
  deleted inside a container each job creates for itself, `cas/removing/<pass-id>/` (mode 0700, a
  fresh random ID), where it is stated again relative to the container just before the unlink, so
  only the inode found before the tombstone is deleted and a replacement is refused, put back,
  and the Survey reported `refused (replaced during collection)`; the next sweep collects it. The
  next sweep also settles a dead pass's container the same way: a file that took a collected
  Survey's CAS name after its record was moved, or that holds bytes other than the record's, is
  `foreign`, listed with its bytes, never counted as the Survey's and never deleted; once a
  Survey's record has left the CAS the Project Store's `settled.json`
  (`af/ProjectSurveysSettled@1`) says so, so a byte-identical file at its name stays `foreign`
  sweep after sweep, and a settlement whose directory sync fails puts the earlier record back and
  deletes nothing, for the next sweep to retry. A crash between a Survey's bytes and its
  `SurveyRecorded@1` leaves the writer's pending-publication entry
  (`pending-surveys/<64 hex>.json`, `af/survey-pending@1`) that `af storage` counts as a Survey
  `pending` and recovery settles: cleared when the event committed, otherwise only the object it
  describes (device, inode and length, checked again under the lock, `skipped: object changed`
  otherwise) is reclaimed, the entry cleared only once that object is gone, so a reclamation
  interrupted inside its pass's container is finished by the next sweep, never left `foreign`;
  any object nothing names is `foreign`; and a
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
  matches ([ADR-0158](docs/adr/0158-measure-a-project-into-surveys-with-a-token-free-surveyor.md)).
- PJ2c close-out, collection: a pending entry whose event never committed is cleared only once no
  file with its inode remains at its Survey's CAS name or in a pass's container, whatever its
  length; an object truncated or grown in place, or moved so, keeps the entry (`failed: … kept:
  object changed, still present`) and is listed `foreign` until it is gone
  (`a_pending_entry_stays_while_its_changed_object_is_present_and_goes_once_it_is_deleted`).
  When the directory sync after `settled.json` is installed fails, the put-back is synced too
  before the pass goes on, and when that fails as well both failures are reported and the
  Survey is left `unverified settlement`, its record not deleted until a later sweep reads a
  consistent `settled.json` (`a_settlement_whose_rollback_sync_fails_too_is_left_unverified_and_deletes_nothing`).
  Where the inventory walks Stores it holds to the Store discovery ceilings: the Surveys of at most
  1,000 Project Stores, and each Store's log read through one cursor of at most 100,000 rows, 1 MiB
  per row and 16 MiB over all rows; a Store past one is reported naming the ceiling and its
  Surveys are kept, one fixture per ceiling
  (`project_stores_beyond_the_store_discovery_ceiling_are_reported_and_their_surveys_kept`,
  `a_project_store_log_beyond_the_row_ceiling_keeps_its_surveys`,
  `a_project_store_row_beyond_the_row_bytes_ceiling_keeps_its_surveys`,
  `a_project_store_log_beyond_the_buffered_bytes_ceiling_keeps_its_surveys`). The 64 MiB
  per-Store activity CAS-read budget is out of scope here: it bounds the `activity` section, which
  does not exist yet and belongs to PJ5
  ([ADR-0158](docs/adr/0158-measure-a-project-into-surveys-with-a-token-free-surveyor.md) clause
  15).
- Second close-out, collection: before its tombstone a collection records the object it selects,
  its device, inode, birth time and length, in `settled.json`, now `af/ProjectSurveysSettled@2`
  (`selected`, `schemas/project-surveys-settled-v2.json`; `@1` is refused naming its recovery),
  and recovery follows that identity, never a hash: a byte-identical copy put at the name after a
  crash that followed the tombstone, another inode or the same one born later, is `foreign` sweep
  after sweep (`a_byte_identical_replacement_after_a_tombstone_survives_two_sweeps_as_foreign`,
  `a_record_replaced_before_its_rename_is_refused_and_its_copy_stays_foreign`). Objects the writer
  staged are reclaimed under `recovery` before their entries
  (`a_survey_stopped_before_its_cas_name_leaves_staged_bytes_a_later_prune_reclaims`), and a file
  system that reports no birth time refuses collection and deletes nothing
  (`a_file_system_without_birth_time_refuses_collection_and_deletes_nothing`). Every byte count
  of `af storage` and `af storage prune` is the allocated bytes of regular files, directories and
  links counting nothing, so the numbers agree on ext4 and APFS. The prune's running total
  reconciles a claim that grew since inventory, what its removal measured at its start taking the
  place of the inventoried bytes before what left is subtracted (inventoried at 10 MiB, grown to
  20 MiB, 8 MiB freed: 12 MiB retained, `a_grown_claims_partial_deletion_reports_the_bytes_it_retains`),
  and reports the total and `stop` `unknown` when what a partial deletion left cannot be measured
  (`a_partial_deletion_whose_remainder_cannot_be_walked_reports_the_total_unknown`), so its
  document is now `af/storage-prune@3` (`schemas/storage-prune-v3.json`) and `@2` is refused
  ([ADR-0158](docs/adr/0158-measure-a-project-into-surveys-with-a-token-free-surveyor.md) clause
  13).
