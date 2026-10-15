- `af provider models [ID …] [--json] [--resolve SPELLING[/EFFORT]]` lists each Provider's model
  catalog token-free, from the harness's own protocol (Claude Code's `list_models` on a
  stream-json session closed before any turn, Codex's `model/list` on a bare app-server), with
  its source, harness version and freshness, one row per exact ID with its display name, aliases
  and effort levels, and what the list proves; only the charged `af provider doctor` proves that
  an account can run a model. `--resolve 'Fable 5.1/xhigh'` names the exact ID, checks the effort
  level and prints the `reviewer.toml` `[runner]`, the `worker.toml` `[runner]` and the bindings
  `[providers]` blocks, each a TOML document that parses on its own; `--json` prints
  `af/provider-models@1`; exit 4 when a CLI cannot serve af, 7 when a catalog is unavailable. One validation module now checks every model and effort level before
  anything is charged: an alias is refused with the exact ID it resolves to, an effort level the
  Provider or model does not list with the levels it lists, a misspelling of a known ID with the
  nearest IDs, and a custom ID that passes the spelling rule is accepted as `unverified against
  the catalog`. `af task plan` and `af review plan` print a `model` line per Model Worker and
  refuse before any Provider process starts; both adapters refuse such packages too, so a
  reviewer package naming `--model opus` must name `claude-opus-5-5`. Every refusal names the
  Provider, the rule, the nearest accepted spellings and `af provider models <id>`. The TUI's
  Providers pane shows each Provider's own models and the Workers pane each runner's verdict
  once per Provider of its kind, named by its ID
  ([ADR-0148](docs/adr/0148-discover-provider-models-and-validate-every-binding-before-a-charge.md)).
