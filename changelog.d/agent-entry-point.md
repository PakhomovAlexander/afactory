- The agent pointer `af setup agents` installs in `AGENTS.md` and both skills is now four rules:
  run `af help agents` first, add `--json`, run `state.next_action`, and never add `--login`
  in an agent session. `af help agents` starts the loop at step 1 with no readiness check, and
  a command it describes needs no `effects` lookup first
  ([ADR-0258](docs/adr/0258-point-agents-at-the-loop-in-four-rules-and-own-skills-by-their-bytes.md)).
- The generated `SKILL.md` files open with YAML frontmatter, so Codex and Claude Code load
  them. `af setup agents --write` replaces only a skill whose bytes are one af wrote, the
  previous version included; any other file, one under a recomputed digest line included, is
  refused as foreign.
- With no Provider of a reviewer's kind registered, the `review_binding_missing` refusal of
  `af review plan` and `af review run` names `af provider detect` as the next command and says
  which kind each reviewer needs; with a choice, it lists `NODE: ID, ID` in words, never as a
  debug map. `af review plan` also prints the plan it computed: its reviewers and their models,
  gates, convergence and budgets before the refusal, and the whole document in
  `details.plan` under `--json`.
- `af provider detect` no longer says every authenticated login is registered when the only auth
  directory it found is logged out: it says no authenticated login was found and points to the
  private login step.
- `af help agents --json`, and `af help TOPIC --json` for every page `af help` prints, now prints
  the page as one `af/help-topic@1` document (`schemas/help-topic-v1.json`): its name, description
  and text; `af help errors CAUSE --json` too. Words that are a command still print the command
  catalog, and words that name neither still exit 2.
- On the default branch with an uncommitted change and reviewers that still need a Provider,
  `af review plan --json` no longer says the default branch cannot be determined: its refusal's
  `next_action` is the command with the selectors af would choose, and the message names each
  reviewer that still needs a Provider. Running that command reaches the binding refusal, which
  names `af provider detect` or the choice.
- `af review plan --uncommitted --policy-rev REV` without `--base` is refused like `af review run`:
  `review_selectors_missing`, with the command that adds `--base REV`, instead of an internal
  capture error.
