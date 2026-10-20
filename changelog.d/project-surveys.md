- `af project survey|refresh|history`: the token-free Surveyor measures a Project's head into a
  Survey, `af/ProjectSurvey@1` in the Project Store, with the sections identity, tree, history,
  contributors, decisions, vocabulary and checks, every number decimal text, shares basis points
  rounded down (`3333` for one third of three), and each section complete, partial or
  unavailable with the bound it reached. A complete Survey is reused only while its survey key is
  unchanged (the head commit, a digest of `refs/heads` and `refs/tags`, the policy digest and
  Surveyor version 3), taken from the record, which replay now binds to its `SurveyRecorded@1`
  and refuses naming the event's sequence and field when they differ; `unchanged:` names that key
  in one line; a new tag or branch, a policy change or a partial newest Survey records a new one,
  and `af project refresh --force` records one regardless. It reads the head only through
  supervised git plumbing, every stream as it arrives and no record past 64 KiB, under one
  `[survey] max_wall_ms` deadline that starts before `rev-parse`, covers the survey key (a key
  not computed in time records `SurveyFailed@1`) and is checked at every item of every stream and
  of every loop after one (the tree's aggregation, the ranking of identities, each decision, term
  line and declaration), so a slow section stops partial, never late and complete, and the
  refresh stays bounded; `max_paths`, `max_entries` and `max_survey_bytes` are ceilings a layer
  may only lower, zero, above-ceiling and non-integer values refused naming the key, the history
  walk tracks at most `max_paths` paths, and every list, the checks' `declared`, `toolchain` and
  `unreadable` included, is ranked before `max_entries` cuts it with `seen` and a reason, while
  display strings are cut to 256 bytes and marked `truncated` and identity strings stay exact.
  Every `[survey]` pattern uses the one grammar of `.af/af.toml` (brackets literal; `decisions` is
  `docs/adr/*.md` and a decision's name starts with four digits and a dash), a decision's status
  and date come from its `**Status:**` or `Status:` line (the first status named, `amended` a
  flag, the first date), `[survey.languages]` patterns give each file one language ahead of the
  built-in map, checks come from `.af/code-policy.toml` and every `.af/pipelines/*.toml` as
  `<source>:<name>` with the listed toolchain files, a declaration that is not valid UTF-8 is
  unreadable, and the measurement policy is stated: `--date-order` from the head, windows by
  committer time, merges without churn, no rename detection, binary changes as one file and zero
  lines, and the git version recorded. Contributors carry a `key` they are grouped and ranked by
  (commits, then lines added) beside their bounded display `name`, and are split into human and
  agent-assisted commits, each counted once; an author email is never stored under either
  identity policy, an author name carrying an email address is `h:` and the hash of the name
  under both, and repository text is recorded as committed. `af onboard --apply` records the
  first Survey without changing its own exit code. `af project show --json` stays
  `af/project@1`: the Survey reaches `af project show` as text only
  ([ADR-0152](docs/adr/0152-measure-a-project-into-surveys-with-a-token-free-surveyor.md)).
