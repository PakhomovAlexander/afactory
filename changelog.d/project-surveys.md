- `af project survey|refresh|history`: the token-free Surveyor measures a Project's head into a
  Survey, `af/ProjectSurvey@1` in the Project Store, with the sections identity, tree, history,
  contributors, decisions, vocabulary and checks, every number decimal text and each section
  complete, partial or unavailable with the bound it reached. A complete Survey is reused only
  while its survey key is unchanged (the head commit, a digest of `refs/heads` and `refs/tags`,
  the policy digest and Surveyor version 2), and `unchanged:` names that key in one line; a new
  tag or branch, a policy change or a partial newest Survey records a new one, and
  `af project refresh --force` records one regardless. It reads the head only through supervised
  git plumbing, every stream as it arrives, under one `[survey] max_wall_ms` deadline checked at
  every item, so a slow section stops partial and the refresh stays bounded; new fixed maxima
  `max_paths`, `max_entries` and `max_survey_bytes` cut lists with `seen` and a reason, display
  strings are cut to 256 bytes and marked `truncated`, and identity strings stay exact. Every
  `[survey]` pattern uses the one grammar of `.af/af.toml` (brackets literal; `decisions` is
  `docs/adr/*.md` and a decision's name starts with four digits and a dash), and the measurement
  policy is stated: `--date-order` from the head, windows by committer time, merges without
  churn, no rename detection, binary changes as one file and zero lines, and the git version
  recorded. Contributors are split into human and agent-assisted commits, each counted once; an
  author email is never stored under either identity policy, an email-shaped author name is
  hashed, and repository text is recorded as committed. `af onboard --apply` records the first
  Survey without changing its own exit code
  ([ADR-0152](docs/adr/0152-measure-a-project-into-surveys-with-a-token-free-surveyor.md)).
