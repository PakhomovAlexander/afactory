- `af self optimize` now reads current Codex sessions: `session_meta` declares the one session a
  source is attributed to, a `response_item` ID no longer resets it, `token_usage_record`'s
  cumulative `thread_token_usage` and `token_count` are kept as session maxima, and
  `task_started`, `task_complete` and `turn_aborted` give task spans. A foreign `session_meta`,
  `token_usage_record` or project-leaving `turn_context` changes nothing declared and
  quarantines the session-less records after it until a valid declared `token_usage_record`;
  the gaps `foreign_session_record` and `quarantined_unscoped_record` keep coverage partial.
  A repeated declared `session_meta` with a foreign `cwd` or `project_id` quarantines as well, a
  `token_usage_record` before the declaring `session_meta` is refused rather than adopted, a
  `token_count` or task event before it is not attributed, a `token_usage_record` with a foreign
  `cwd` or `project_id` is refused and quarantines even when it names the declared session (a
  present `cwd` or `project_id` that is not a string counts as foreign), and an
  incremental capture replays the retained prefix while rechecking it, so the declared session
  and a quarantine carry over the capture cursor.
  `docs/task-execution/self-optimizer.md` ("Native Codex sessions") lists the records
  ([ADR-0264](docs/adr/0264-own-codex-session-records-and-quarantine-after-a-foreign-one.md)).
  The malformed-project-evidence regressions are classified `functional` and run in the fast
  gate. Their matrix does not yet prove declared identity, membership and spans through a
  cursor-continued capture; that coverage is tracked in
  [issue #465](https://github.com/PakhomovAlexander/afactory/issues/465).
