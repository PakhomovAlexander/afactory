- The repository's catalog now also pins a light default Task pipeline for small, local changes,
  `default-light/implementation` and `default-light/verification`: one implementing iteration
  (`claude-opus-5-5`, medium; 1,000,000 tokens, 2 h), the correctness reviewer (`gpt-6-sol`,
  high; 300,000 tokens, 30 min) and the evaluator (`gpt-6-astra`, high; 200,000 tokens, 20 min),
  with no bug-bounty or gate-time reviewer: the correctness reviewer also owns concrete
  functional defects on the changed paths. The catalog's `[review.reviewers]` now marks `bugs`
  advisory so that pipeline may leave it unbound; a pipeline that binds it counts its findings
  and its missing result as before, but its benchmark Demands no longer block convergence.
  `docs/tasks.md` ("The light default pipeline") gives its limits and Provider bindings
  ([ADR-0262](docs/adr/0262-offer-a-light-default-pipeline-for-small-changes.md)).
