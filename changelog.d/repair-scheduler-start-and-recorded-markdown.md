- Serialize foreground scheduler lease acquisition and row initialization with stop requests, so
  first startup and restart cannot erase an acknowledged stop. Preserve later explicit restart.
- Add explicit `--raw-markdown --observed-output PORT --stale-after-ms N` result presentation for
  already-authored named text, using a separate recorded observation time without JSON wrapping.
  Keep structured rendering, freshness, terminal-control protection and read-only watch behavior.
  See [the proposed decision](docs/adr/2026-11-11-serialize-scheduler-start-and-select-recorded-markdown.md).
- Sustained Store retention remains unresolved; the unchanged 16 MiB journal can still exhaust.
  These repairs alone do not establish #796/#797 requirements acceptance.
