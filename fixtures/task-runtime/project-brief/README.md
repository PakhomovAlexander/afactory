# project-brief

Project Briefs on a declared port, for
[ADR-0153](../../../docs/adr/0153-carry-a-bounded-project-brief-on-a-declared-port.md) and
`crates/af/tests/it/project_brief.rs`:

- `fixture/oriented` declares the root port `project` (`root_default = "project_brief"`,
  `brief = "orient"`) and wires it to its implementer only; the evaluator does not see it.
- `fixture/exposing` is `fixture/oriented` with the port also exposed as the public output
  `brief`, forwarded unchanged, so a later Task can bind `project` to it. A public output read
  from an optional root input that is absent is unavailable to the compiler, so it is planned
  only where a Survey was recorded.
- `fixture/forged` is the same graph with `fixture/forger` as its implementer: a Worker that
  returns the Brief it was handed as an `af/ProjectBrief@1` output of its own, which output
  admission refuses.
- `oriented.json` is the Task file; the test writes its variants (task IDs, `inputs`, the
  pipeline) and every `.af/af.toml` `[brief]` policy itself.

The package digests are placeholders the test pins from the copied tree before it commits.
