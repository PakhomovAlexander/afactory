- A diagnostic child's page now links and embeds only sanitized flamegraphs. inferno's original
  SVG, which may hold a script, stays archived byte for byte in the child bundle, named by path
  and hash but never linked. `report --diagnostic` now refuses a child whose selected cases are
  not the parent's workloads, whatever IDs they share. Each case must keep the parent case's
  parameters, seed, timing, cache, oracle and AF environment; only its pairs, rank, alpha,
  orders and shard may differ. The report also refuses a child whose profiles claim another
  recipe kind than its frozen plan states, or whose evidence is not of that kind. Children are
  classified by the plan's kind, never by a recipe ID. Diagnostic plans now freeze the recipe's
  kind, base recipe and tools (`af.perf-run/4`), and earlier plans are refused. The comparison
  fixtures commit only their golden verdicts and generate their run bundles at test time
  ([ADR-0179](docs/adr/0179-link-only-sanitized-views-and-bind-a-diagnostic-child-to-its-parent-workloads-and-frozen-recipe-kind.md)).
