# Require Provider bindings and isolate Claude reviewers

**Status:** accepted (2026-09-01); superseded in part by
[ADR-0113](0113-ga-reads-only-what-ga-writes.md): the durable, fenced, charged Provider Operation
that doctor and run shared, with its Campaign/Round admission evidence and smoke budget. Both
commands now admit Providers through the Review Task's captured Provider admission Attempts.

Every packaged model Worker must be bound explicitly with `--provider NODE=ID`. Missing bindings,
unknown node names, unavailable contexts, failed structural authentication, failed inference
smoke, and insufficient remaining budget all fail before a Gate or Worker dispatch. `af provider
doctor` exercises the exact durable, fenced, charged admission path that `af review run` uses and
stops before review execution; a later run for the same Campaign reuses its admitted result.

Runner adapters, not project configuration, own the execution security boundary. In particular,
the Claude adapter disables project/local customization, Hooks, plugins, MCP configuration,
skills, and unsafe tools, then grants only the read-only inspection tools required by the review
contract. The Codex adapter likewise rejects package-owned sandbox, working-directory, network,
and configuration overrides. Package manifests may choose a model and reasoning effort but cannot
weaken either adapter's security flags.

## Considered options

- **Fall through to ambient authentication when a binding is omitted.** Rejected because the
  executed Provider then differs from the captured Campaign authority and cannot be audited or
  budgeted before dispatch.
- **Trust repository `.claude/settings*.json` and Hooks.** Rejected because candidate-controlled
  configuration would acquire command execution and data-access authority outside the package
  and pipeline contracts.
- **Duplicate a cheap readiness probe in a separate doctor command.** Rejected because a probe
  that differs from run-time admission can report ready while the real fenced operation fails.
- **Require exact bindings, reuse one durable admission path, and make adapters own their
  security flags (chosen).** This keeps authorization explicit while preserving resumable paid
  preflight evidence.

## Consequences

- Configuring a Worker still authorizes delivery of its declared inputs without per-call prompts;
  it does not authorize an unspecified Provider or project-controlled reviewer hooks.
- Command and inline deterministic nodes need no Provider binding.
- Doctor may create Campaign/Round admission evidence and spend the configured smoke budget, but
  it never runs a Gate or Worker.
- Claude review packages are read-only by construction. A future capability profile requires a
  separate decision rather than a project setting.
