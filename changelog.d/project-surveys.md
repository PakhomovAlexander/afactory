- `af project survey|refresh|history`: the token-free Surveyor measures a Project's head into a
  Survey, `af/ProjectSurvey@2` in the Project Store, with the sections identity, tree, history,
  contributors, decisions, vocabulary and checks, every number decimal text, shares basis points
  rounded down (`3333` for one third of three), and each section complete, partial or unavailable
  with the bound it reached. A complete Survey is reused only while its survey key is unchanged (the
  head commit, the digest of one bounded `for-each-ref` observation of `refs/heads` and `refs/tags`,
  which also yields `history`'s tags and branches, the policy digest and Surveyor version 8), taken
  from the record; replay binds every `SurveyRecorded@1` to its record, or a collected Survey's to
  its tombstone, which copies every identity field of the record (head, refs, cutoff, completeness,
  policy, Surveyor version and previous Survey) once the event was verified against it, and `af
  project history`, `af project survey` with and without `--section` and `af project refresh` refuse
  a mismatched event, an older one included, naming its sequence and field, before any of its fields
  is used. The fold refuses before it reads: a record of an uncollected Survey that af cannot read
  back, gone or corrupt, refuses replay naming the event's sequence, the Survey and the read error
  and none of the event's other fields, and collection refuses and reports a Survey whose event
  disagrees with its record. `unchanged:` names the key in one line; a new tag or branch, a policy
  change or a partial newest Survey records a new one, and `af project refresh --force` records one
  regardless. It reads the head only through supervised git plumbing, the history from objects alone
  in a private bare repository attached to the object database (so untracked or `.git/info`
  attributes, local configuration and diff drivers change no count), every stream as it arrives and
  no record past 64 KiB, under one absolute `[survey] max_wall_ms` deadline that starts before
  `rev-parse`, covers the survey key (a key not computed in time records `SurveyFailed@1`, and so
  does every other way a refresh of a registered Project stops without a Survey: a newest Survey
  that does not replay, a record the Project Store cannot take, a deadline before `rev-parse`
  answered, whose head one more one-second `rev-parse` names), bounds every git process at the
  lesser of its own limit and the time left (its process group ended at the deadline, a silent git
  included), and is checked at every item of every stream, every declaration and every match of
  every pattern list, `[survey.languages]` included, and after every section's finishing step, so a
  slow section stops partial, never late and complete. The Tree Digest comes from a digest-only path
  equal to the committed capture's digest: no blob written to any CAS, at most 16 MiB of manifest
  buffering and 256 MiB of temporary manifest data, and absent with `deadline`, `max_record_bytes`,
  `manifest_bytes` or `temp_bytes`, never the digest of a part; its spilled runs live in an
  `af-tree-digest-<pid>-*` directory that the crash sweep of sandboxes and check runtimes reaps
  under the same liveness and preservation rules, and every refresh sweeps a killed one's first,
  before it decides to reuse a Survey, and again after its Tree Digest on every way out, within its
  deadline and never touching a live process's directory. A Survey is published through a
  pending-publication entry beside the Project Store's lock (`af/survey-pending@1`, at most 4 KiB:
  the Survey ID and its length, written and synced before any byte enters the CAS, then the device
  and inode of the file at the Survey's CAS name, recorded and synced once the bytes are durable
  there and before the event), cleared only once `SurveyRecorded@1` committed, so a crash between
  the record and its event leaves the entry, which collection counts and recovers, and the next
  refresh of the same bytes finishes it; debug builds stop at both crash points
  (`AF_TEST_SURVEY_STOP_BEFORE_IDENTITY`, `AF_TEST_SURVEY_STOP_BEFORE_EVENT`). A complete Survey is
  byte-identical for the same head, refs, policy, Surveyor and git versions, cutoff and previous
  Survey, and one that is not complete records where it stopped (`stopped`: the section, the item
  index, the commit or path, the reason), which `af project survey` prints. The history walk
  includes merges with `--diff-merges=off`, so a merge counts as a commit and a merge with no churn,
  and reads `git log -z` in NUL-separated fields with the commit ID repeated, so an author name or
  subject holding 0x1f is read whole and a record that does not parse, or a log that ends before its
  last record's NUL, stops the walk with `malformed_record` naming its commit, never a guessed
  identity, never complete history and never `SurveyFailed@1`; a malformed first record leaves
  `history` and `contributors` partial with zero counts, naming the head, and both sections name the
  record that stopped the walk by its ordinal (`stopped_record`, the one after the commits counted),
  a record git is still writing being awaited until its own terminators and only the stream's end
  cutting one. A gitlink is one file of zero bytes and lines, language `other`, never fetched or
  opened, and left out of the Tree Digest as the committed Manifest leaves it. `max_paths`,
  `max_entries` and `max_survey_bytes` are ceilings a layer may only lower, every pattern list holds
  at most 64 patterns, the history walk tracks at most `max_paths` paths, and every list is ranked
  before `max_entries` cuts it with `seen` and a reason, while display strings are cut to 256 bytes
  and marked `truncated` and identity strings stay exact. Every `[survey]` pattern uses the one
  grammar of `.af/af.toml` (brackets literal, `?` one byte), a decision's status and date come from
  its `**Status:**` or `Status:` line alone (the first status named, `amended` a flag, the first
  date that is a day of the calendar), terms are listed by name, `[survey.languages]` patterns give
  each file one language, the bytewise-first ahead of the built-in map, checks come from
  `.af/code-policy.toml` and every `.af/pipelines/*.toml` as `<source>:<name>` with the eight listed
  toolchain files, and the measurement policy is stated. Contributors carry a `key` they are grouped
  and ranked by beside their bounded display `name`, and are split into human and agent-assisted
  commits, each counted once; an author email is never stored, and an author name carrying an email
  address, blank or holding a control character anywhere in the whole name is `h:` and the hash of
  the name under both identity policies. `af project survey --section NAME` picks sections of the
  text; with `--json`, which prints the complete record, it is usage error 2. `af onboard --apply`
  records the first Survey without changing its own exit code, and `af project show --json` stays
  `af/project@1`
  ([ADR-0159](docs/adr/0159-measure-a-project-into-surveys-with-a-token-free-surveyor.md)).
- PJ2 round 9 re-verified round 8, which had no code review, item by item: (i) a log that ends
  inside a record is `malformed_record`, partial with the counts before it and zero counts for the
  first record, `unavailable` kept for a walk that never ran: fixed, adding the record's ordinal and
  the stated, tested difference between a record still being written and a cut one
  (`a_record_git_is_still_writing_is_awaited_and_only_the_streams_end_cuts_it`,
  `a_log_stream_cut_before_its_final_nul_is_partial_history_naming_the_cut_commit`); (ii) the spill
  sweep on every refresh, before and after the Tree Digest, sparing a live Surveyor's directory:
  fixed, the sweep after the digest was missing
  (`the_spill_sweep_runs_after_the_tree_digest_on_every_way_out`); (iii) a gitlink as one file of
  zero bytes and lines in `files_without_lines`, language `other`, never fetched or opened:
  complete, its fixture now also proving no `cat-file` asks for the target
  (`a_gitlink_is_one_file_of_no_bytes_or_lines_never_fetched_and_digests_as_the_manifest`); (iv)
  `previous_survey_id` and the partial stopping point as reproducibility inputs: complete
  (`forced_refreshes_at_one_cutoff_differ_only_in_the_chain_and_a_partial_one_in_where_it_stopped`);
  (v) the ceilings: `max_paths` complete
  (`the_fixed_maxima_cut_with_their_reasons_and_identity_strings_stay_exact`,
  `a_historical_commit_over_max_paths_leaves_history_partial_and_the_survey_recorded`), the 64 KiB
  `max_record_bytes` fixed with its missing end-to-end fixture
  (`every_streamed_record_over_64_kib_stops_at_max_record_bytes`), while the discovery ceilings and
  the activity CAS-read budget belong to the `activity` section, which is PJ5's and not on this
  branch, so neither was found complete nor fixed here and ADR-0159 clause 12 states them for PJ5;
  (vi) every persisted shape at its version with its schema and parity fixture: complete, every
  Survey shape still unreleased at `@1` (this round's `stopped_record` changes
  `af/ProjectSurvey@1`'s schema and parity in the same change) and Surveyor version 8 so no earlier
  build's record is reused. The writer's publication order was fixed too: the CAS made durable
  before the identity, every entry write's directory synced, the identity only of a regular file of
  the record's length, the 4 KiB bound and the second stop
  (`the_writers_pending_entry_is_what_collection_reads_at_either_crash_point`), and every early stop
  now records `SurveyFailed@1` (`every_refresh_that_stops_without_a_survey_records_its_failure`).
- PJ2 close-out, the writer: the Survey record is `af/ProjectSurvey@2`, since round 9 added
  `stopped_record` to `@1` without a new version; `ProjectSurveyV2`, its schema
  (`schemas/project-survey-v2.json`) and parity fixtures replace `@1`, whose schema is gone, and
  the label `af/ProjectSurvey@1` is refused by type with ADR-0145's message
  (`the_superseded_survey_label_is_refused_by_type_and_schema`). A record of it already in a
  Project Store is never deleted for its version: `af project history` lists it by ID and time as
  `unreadable: superseded version` (`--json` is now `af/project-history@2` for that entry),
  `af project show` says so, `af project survey` refuses it naming the version, and a refresh
  never reuses it (`a_superseded_survey_record_is_listed_unreadable_kept_and_never_reused`). A
  refresh tries `project.lock` only until its `max_wall_ms` deadline and stops with the bounded
  failure `store_locked`, recorded while the holder still holds the lock
  (`a_held_project_store_lock_stops_a_refresh_with_store_locked_within_its_deadline`). A retry of
  a Survey whose pending entry already names an object keeps that identity and goes on only while
  its CAS name holds that object, so a retry whose CAS write fails leaves the entry naming the
  first object for collection to reclaim
  (`a_retry_whose_put_fails_keeps_the_first_objects_identity_for_collection`), and the writer
  refuses a `pending-surveys` that is a symbolic link or no directory of the Store, naming it and
  writing nothing through it
  (`a_pending_directory_replaced_by_a_link_refuses_the_refresh_and_nothing_is_written_outside`)
  ([ADR-0159](docs/adr/0159-measure-a-project-into-surveys-with-a-token-free-surveyor.md) clause
  15).
- Second close-out, the writer: a Survey's bytes are staged in the writer's own
  `pending-surveys/<64 hex>.object`, its identity recorded in the pending entry, and only then
  renamed to its CAS name, never over a taken name, so nothing is ever at a CAS name no entry
  describes and every state between has its recovery: a refresh stopped before the identity
  (`AF_TEST_SURVEY_STOP_BEFORE_IDENTITY`) leaves staged bytes and an empty CAS name, one stopped
  after it (the new `AF_TEST_SURVEY_STOP_BEFORE_LINK`) an entry naming the staged object, and the
  next refresh of the same bytes finishes either
  (`the_writers_pending_entry_is_what_collection_reads_at_either_crash_point`). The entry is now
  `af/survey-pending@2` (`schemas/project-survey-pending-v2.json`), whose `object` carries the
  birth time `born_unix_ns` beside device and inode, since ext4 hands a freed inode number to the
  next file at once; `@1` is refused naming its recovery, and a file system that reports no birth
  time refuses the publication. The Surveys ADR is renumbered ADR-0159, after the numbers main
  took
  ([ADR-0159](docs/adr/0159-measure-a-project-into-surveys-with-a-token-free-surveyor.md) clause
  2).
