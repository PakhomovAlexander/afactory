- Worker packages can declare bounded source knowledge and same-slot private Notes in a pinned
  `knowledge.toml` companion. Typed queries retrieve exact source-addressed excerpts on later
  invocations, with freshness checks, explicit omissions and replayable context evidence on
  command and model transports. Private hypotheses never become independent reviewer context;
  cross-Task source facts require an explicit capability and content revalidation. See
  [the decision](docs/adr/2026-11-04-select-bounded-worker-knowledge-through-typed-ports.md).
