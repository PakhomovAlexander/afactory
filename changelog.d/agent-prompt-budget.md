- A finding's `agent_prompt.text` in `af task findings --json`, `af review report --json` and
  their `--format sarif` keeps its location, requested change and verification even when the
  evidence or the title is too long to fit: it cuts the evidence first and then the title, each
  marked `[… truncated to fit the prompt]`. Only a requested change too long to fit even then is
  cut, with the same marker, while the location and verification stay whole; the structured
  `requested_change` field always holds the complete bounded value
  ([ADR-0248](docs/adr/0248-hand-review-findings-to-the-agent-that-will-fix-them.md)).
  `docs/agents.md` now says that a `not_run` reviewer has no admitted review result for the
  round, rather than that it never looked.
