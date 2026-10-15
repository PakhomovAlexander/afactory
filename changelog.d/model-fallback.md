- Claude Task Attempts now run with `CLAUDE_CODE_DISABLE_REFUSAL_FALLBACK=1`, so Claude Code no
  longer answers with another model when the bound model's safety classifier flags a request. A
  refusal is classified `model_refused`, charged what it reports and not retried; a reply from a
  model the Worker did not bind is still refused, now naming both models, their output tokens,
  `safety_stops` and `num_turns`; a model ID Claude Code does not know is `model_unavailable`.
  `af task run` and `af review run` print the reason and the next step on stderr for each such
  failed Attempt
  ([ADR-0147](docs/adr/0147-stop-the-claude-model-fallback-and-name-the-model-that-answered.md)).
