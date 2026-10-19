- `af project survey|refresh|history`: the token-free Surveyor measures a Project's head into a
  Survey, `af/ProjectSurvey@1` in the Project Store, with the sections identity, tree, history,
  contributors, decisions, vocabulary and checks, every number decimal text and each section
  complete, partial or unavailable with the bound it reached. It reads the head only through
  supervised git plumbing under one `[survey] max_wall_ms` deadline, also bounded by
  `max_commits` and `max_file_bytes`, runs no hook or project code, calls no Provider and writes
  nothing inside the checkout. A head already surveyed under the same policy and Surveyor version
  prints `unchanged` and records nothing. `af onboard --apply` records the first Survey without
  changing its own exit code, and `af project show` prints the newest Survey's summary or the
  reason of the last `SurveyFailed@1`; `af/project@1` is unchanged. The new `[survey]` table of
  `.af/af.toml` sets the bounds and globs; contributors are named by author name or, under
  `identity = "email_hash"`, by a 16-hex hash of the email, which is never stored
  ([ADR-0152](docs/adr/0152-measure-a-project-into-surveys-with-a-token-free-surveyor.md)).
