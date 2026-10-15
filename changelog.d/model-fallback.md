- Claude Task Attempts now run with `CLAUDE_CODE_DISABLE_REFUSAL_FALLBACK=1`, so Claude Code no
  longer answers with another model when the bound model's safety classifier flags a request. A
  refusal is classified `model_refused`, charged what it reports and not retried; a reply from a
  model the Worker did not bind is still refused, now naming both models, their output tokens,
  `safety_stops` and `num_turns`; a model ID Claude Code does not know is `model_unavailable`.
  `af task run` and `af review run` print the reason and the next step on stderr for each such
  failed Attempt, the switch first even when the result's usage is also malformed or lists more
  than 32 models, with those accounting reasons after it in the diagnostic; an entry for another
  model that reports no activity, or only malformed counters, is never a switch. An error result whose `result` carries Claude
  Code's safeguards text is `model_refused` whatever its `errors` holds. A model or effort binding
  outside the one spelling af prints (an ASCII letter or digit, then at most 127 letters, digits,
  `-`, `.`, `_` or `:`) is refused before any process starts, naming only its byte length, ahead
  of the versioned-model rule, and every Worker's binding is checked before the first Provider is
  probed; a reported model ID outside it reads `[not printable]`
  ([ADR-0147](docs/adr/0147-stop-the-claude-model-fallback-and-name-the-model-that-answered.md)).
