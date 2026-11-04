- Keep dry-run triage proposals unresolved through temporary private holds and changed inputs.
  Freshness invalidation queues new work without releasing the proposal's sweep fence. Only a
  fresh checked replacement may publish past a proposal with no unresolved effects; retire it
  atomically with successful publication, after the final freshness check and observed writes.
  Pointer clears, unchecked claims, rejected freshness and failed publication leave the original
  proposal fenced. Ordinary tick recovery can publish a later fresh replacement and resume sweep.
  Completed and explicitly aborted effects permit replacement. Label intents survive input
  changes after the write returns and partial or unknown writes; only observed intended
  runner-owned labels and hint removals complete them before replacement, retirement and sweep.
  A refusal before any mutation may abort a new intent, but cannot erase an earlier unresolved
  attempt.
  Clarify that owner labels set effective values without resolving triager disagreement.
