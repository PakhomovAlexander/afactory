# ADR-0050: Reduce Review Tasks with the canonical domain Ledger

Status: accepted for the authorized Task increment, 2026-09-11.

The common Task runtime needs standalone and embedded Review without a nested Campaign
executor, another budget, or a second database. Historical Finding and Demand semantics must
remain stable, including scope, canonical identity, disputes and convergence.

Extract canonical reduction into a pure Store-domain function returning the reduced Ledger,
canonical artifact identities and prepared domain events. Historical Campaign ingestion adds
its existing Round causation and appends those events as before. Task Review uses the same
reducer and records its result through the common Task invocation/output path. A parity fixture
compares exact artifact identities and Finding/Demand projections, including a required Demand.

Add installed `review-bind` and `review-reduce` Task operators. Binding derives the Subject
from immutable declared source inputs and explicit prior Review history. Worker context retains
the actual Subject and Change Set. Reduction is an atomic gather barrier over the policy's
required reviewers and checks. Definitions must bind each required input; missing runtime
results remain inspectable but cannot produce authoritative partial sets or close a Round.

Each required reviewer uses a reserved verification role. The compiler protects verification
roles even when their output is evidence consumed by a later reducer rather than a direct
public acceptance receipt. This preserves verifier capacity in composed implementation graphs.

`af/TaskReviewRound@1` retains the exact invocation, policy, Subject, Snapshot, selected results,
missing reviewer names and optional canonical sets. A complete Review can satisfy the generic
Review Task goal while its domain conclusion remains `changes_requested`. Incomplete Review
cannot satisfy that goal. CLI exit precedence follows the recorded Review domain conclusion,
including replay through `af task run`.

The `--file` adapter is the first standalone command-Worker integration. Historical CLI formats
and their persisted artifacts are unchanged at this checkpoint. Their full admission migration,
production model bindings and embedded implementation acceptance remain required before the
corresponding plan packages can close. This ADR does not authorize a partial-release claim.
