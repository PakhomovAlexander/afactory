AF universal resource-management design
=======================================
Accepted by the owner: 2026-10-19.
Implementation status: DEFERRED. This archive preserves the design for future work.
Creating the tracking issue does not start implementation, assign an engineer,
reserve model/testing capacity, or schedule delivery. Resume only when the owner
is ready and authorizes implementation.

Start here
----------
DESIGN-OVERVIEW.txt        Complete readable overview of the accepted architecture.
proposal/design.txt      Full normative architecture and behavior.
proposal/configuration.txt  Configuration semantics, defaults and policy changes.
proposal/decisions.txt   Architecture choices and trade-offs.
proposal/testing.txt     Measurement, failover, starvation and qualification strategy.
proposal/examples/      Illustrative machine/project configuration and diagnostics.
PERFORMANCE-NOTES.txt    Performance assessment and gates for future implementation.
DESIGN-ACCEPTANCE.json   Owner acceptance and its scope.
PROPOSAL-SHA256.json     SHA-256 checksums for all 17 normative source files.

Complete normative package
--------------------------
17 files: architecture, configuration, decisions, capability registry, revision
lineage, 75 acceptance requirements, 73 qualification cases, 84 clause bindings,
20 measurement oracles, qualification campaign and five configuration/JSON examples.
The files are byte-identical to authoring commit
ced8a3ce112ec43184da73a8eca1e62de1e2b721.
That ID names the original local design repository, not this publication commit.

Read historical status correctly
-------------------------------
The normative source is preserved without editing historical draft/review wording.
The owner's acceptance on 2026-10-19 supersedes its human draft status. It does not
convert a failed native Task into an accepted Task or claim implemented behavior.
Opus 5.5/max performed multiple architecture/correctness review rounds. The last
complete round reviewed d7fca16 and returned zero major and three minor findings;
its outer Task finalization failed after an Attempt exceeded its reservation.
Those three findings have authored corrections in this final source, with static
checks and six targeted negative controls. Native fix verification is pending.
Requested Fable 5.1/xhigh admission failed on mixed Fable/Opus identity before either
design reviewer ran. No completed Fable review is claimed. Performance and platform
qualification remain future implementation work. No product code is included here.

Resume order when authorized
---------------------------
R1 Contracts, configuration, read-only diagnostics and independent harness.
R2 Persistent domain registration, atomic reservations, launch fencing and recovery.
R3 Fairness, bounded queues, group/scoped-stale corrections and nested-work rules.
R4 Linux strict/native cooperative backends and all spawning paths.
R5 Docker inherited allocations, authorized execution, rootless/Desktop support.
R6 Kubernetes pod/Job authority, duplicate fencing and partition/restart recovery.
R7 Complete resource UI/history/explain/config diagnostics and bounded telemetry.
R8 Full platform qualification, performance/neighbor gates and staged release.

All commands and configuration in proposal/ describe the accepted future feature;
they are not an assertion of support in today's AF CLI. Existing Task execution,
verification, budget and publication authority must be preserved during delivery.
