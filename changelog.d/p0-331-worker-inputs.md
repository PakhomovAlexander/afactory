- A Worker is told to read only rules and context its own tree holds. The "Read first" list names
  the Snapshot the Worker reads, and lists that tree's `.af/rules.md` and context files by digest,
  recorded as `af/TaskReadFirst@2` (`@1` is neither written nor read). A continued Task (`af task
  run NEW --from TASK`) lists its candidate's rules. Its plan prints `read first: from Snapshot
  S, …` on stderr, and a `rules:` line with both digests when they differ from what TASK read.
  Continuing a Task that an earlier release planned is refused, with the command that plans it
  again
  ([ADR-0254](../docs/adr/0254-name-the-tree-of-a-workers-rules-and-deliver-the-diff-subject-as-a-typed-input.md)).
- The Diff Subject is a typed input. A judging Worker declares the optional `diff_subject` port,
  and the kernel fills it with an `af/DiffSubject@1` whose payload is the `af/diff-subject@1`
  document. Its absence where a base exists, or any other value, fails before dispatch. No
  Pipeline may bind it. `../diff-subject.json` is no longer written; `../base` stays.
  `kernel/bugs` 1.6.0, `kernel/correctness` 1.4.0, `kernel/evaluator` 1.3.0, `kernel/ux` 1.2.0
  and `kernel/uix` 1.6.0 read the port and are re-pinned.
- Judging Attempts still materialize their tree and base. Cloning a warm template with the fresh
  stamps (#266) and integrity check a safe clone needs measured slower than materializing on
  this repository. `attach_base_template` now stamps the cloned base with the current time, and
  `cargo run -p review-pipeline --example judge-materialization` repeats the measurement.
- A Worker port named `diff_subject` of any type but `af/DiffSubject@1` is refused when the plan
  is compiled, bound or not, instead of planning and then failing before its Worker starts
  ([ADR-0254](../docs/adr/0254-name-the-tree-of-a-workers-rules-and-deliver-the-diff-subject-as-a-typed-input.md)).
