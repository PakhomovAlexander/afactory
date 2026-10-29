- `af self optimize` resolves a relative `[[sources]].path` in `.af/optimization-sources.toml`
  from the repository root, like every other `.af/` path, instead of from `.af/`: a source
  declared as `history.jsonl` that lived at `.af/history.jsonl` is now declared as
  `.af/history.jsonl`. A declared source that is missing or cannot be opened or read, a read
  failure after it opened included, still records the unchanged gap `unavailable_<adapter>_<hash>`,
  which does not name the source; the command now names it on stderr before the run, with its
  `source_id`, resolved path and whether it is missing or unreadable (#196).
