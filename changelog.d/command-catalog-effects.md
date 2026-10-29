- `af help --json` declares the effects four commands left out: `af provider models` may write
  machine state for a Codex Provider (`codex app-server` caches its model list in
  `$CODEX_HOME/models_cache.json`), `af review plan --file` writes the Task Store,
  `af review run --file` (and `af review --file`) may write remote state with a pipeline that
  declares remote checks, and `af task deliver` writes the Task Store as well as files
  ([ADR-0247](docs/adr/0247-generate-a-machine-readable-command-catalog.md)).
