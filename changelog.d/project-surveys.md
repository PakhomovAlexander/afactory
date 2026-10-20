- `af project survey|refresh|history`: the token-free Surveyor measures a Project's head into a
  Survey, `af/ProjectSurvey@1` in the Project Store, with the sections identity, tree, history,
  contributors, decisions, vocabulary and checks, every number decimal text, shares basis points
  rounded down (`3333` for one third of three), and each section complete, partial or
  unavailable with the bound it reached. A complete Survey is reused only while its survey key is
  unchanged (the head commit, the digest of one bounded `for-each-ref` observation of
  `refs/heads` and `refs/tags`, which also yields `history`'s tags and branches, the policy
  digest and Surveyor version 5), taken from the record; replay binds every `SurveyRecorded@1`
  to its record, or a collected Survey's to its tombstone, which copies every identity field of
  the record (head, refs, cutoff, completeness, policy, Surveyor version and previous Survey)
  once the event was verified against it, and `af project history`, `af project survey` with
  and without `--section` and `af project refresh` refuse a mismatched event, an older one
  included, naming its sequence and field, before any of its fields is used. The fold refuses
  before it reads: a record of an uncollected Survey that af cannot read back, gone or corrupt,
  refuses replay naming the event's sequence, the Survey and the read error and none of the
  event's other fields, and collection refuses and reports a Survey whose event disagrees with
  its record. `unchanged:` names the key in one line; a new tag or branch, a policy
  change or a partial newest Survey records a new one, and `af project refresh --force` records
  one regardless. It reads the head only through supervised git plumbing, the history from
  objects alone in a private bare repository attached to the object database (so untracked or
  `.git/info` attributes, local configuration and diff drivers change no count), every stream as
  it arrives and no record past 64 KiB, under one absolute `[survey] max_wall_ms` deadline that
  starts before `rev-parse`, covers the survey key (a key not computed in time records
  `SurveyFailed@1`), bounds every git process at the lesser of its own limit and the time left
  (its process group ended at the deadline, a silent git included), and is checked at every item
  of every stream, every declaration and every match of every pattern list, `[survey.languages]`
  included, and after every section's finishing step, so a slow section stops partial, never
  late and complete. The Tree Digest comes from a digest-only path equal to the committed
  capture's digest: no blob written to any CAS, at most 16 MiB of manifest buffering and 256 MiB
  of temporary manifest data, and absent with `deadline`, `max_record_bytes`, `manifest_bytes` or
  `temp_bytes`, never the digest of a part. `max_paths`, `max_entries` and `max_survey_bytes`
  are ceilings a layer may only lower, every pattern list holds at most 64 patterns, the history
  walk tracks at most `max_paths` paths, and every list is ranked before `max_entries` cuts it
  with `seen` and a reason, while display strings are cut to 256 bytes and marked `truncated` and
  identity strings stay exact. Every `[survey]` pattern uses the one grammar of `.af/af.toml`
  (brackets literal, `?` one byte), a decision's status and date come from its `**Status:**` or
  `Status:` line alone (the first status named, `amended` a flag, the first date that is a day
  of the calendar), terms are listed by name, `[survey.languages]` patterns give each file one
  language, the bytewise-first ahead of the built-in map, checks come from
  `.af/code-policy.toml` and every `.af/pipelines/*.toml` as `<source>:<name>` with the eight
  listed toolchain files, and the measurement policy is stated. Contributors carry a `key` they
  are grouped and ranked by beside their bounded display `name`, and are split into human and
  agent-assisted commits, each counted once; an author email is never stored, and an author name
  carrying an email address, blank or holding a control character anywhere in the whole name is
  `h:` and the hash of the name under both identity policies. `af project survey --section NAME` picks sections of the
  text; with `--json`, which prints the complete record, it is usage error 2. `af onboard
  --apply` records the first Survey without changing its own exit code, and `af project show
  --json` stays `af/project@1`
  ([ADR-0152](docs/adr/0152-measure-a-project-into-surveys-with-a-token-free-surveyor.md)).
