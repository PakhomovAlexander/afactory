AF resource management design
=============================

Status: final design draft, revised 2026-10-17. All three Review L findings have authored
corrections and updated qualification contracts. Native fix verification and formal Task
acceptance remain pending; the prior passing round covers commit d7fca16, not these new bytes.
This is a design artifact, not a claim of shipped behavior or a product-source edit.

Read design.txt for the complete product and architecture specification.
Read configuration.txt for configuration semantics and defaults.
Read decisions.txt for architectural trade-offs.
Read capability-registry.csv for the closed version 1 capability predicates.
Read revision-notes.txt for prior review dispositions and the continuation boundary.
Read examples/ for illustrative TOML configuration and JSON diagnostics.
Read acceptance.csv for the implementation acceptance matrix.
Read testing.txt for the measurement, crash/failover, fairness and release qualification strategy.
Read measurement-oracles.csv for 20 independent measurement contracts.
Read qualification-cases.csv for 73 mandatory test scenarios linked to 75 feature acceptance rows.
Read qualification-coverage.csv for exact injection/outcome clauses and required executable hooks.
Read qualification-plan.toml for proposed campaign tiers, budgets and evidence requirements.

User decision: cooperative scheduling is the default when hard enforcement is unavailable;
tasks requiring strict limits must use a capable executor or refuse execution.

Scope: AF sharing resources with other applications, agents and simultaneous AF Tasks on
developer machines, Linux VMs, Docker and Kubernetes. The OpenClaw VM is one test case.

Existing source baseline: afactory-wt-small-vm-integration at ccc32cce0520745df14013e6fba0ea1ae471f2f8.
Existing VM candidate: source manifest 67c5702a9baec3992e76c36f5b5c40296567af659d447c3f518f7b2e04bd13ef.
Existing candidate code: ../openclaw-2vcpu-2026-10-07/execution/main-integration-repair-codex/candidate-inspection/.

Proposed CLI names and configuration fields are illustrative contracts for implementation;
they must not be presented as commands accepted by the current AF release.

Product documentation and implementation must be introduced through native AF Tasks under
the product repository's AGENTS.md. No code, provider installation, service configuration,
GitHub issue or pull request was changed by producing this design.
