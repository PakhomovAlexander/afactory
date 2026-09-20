# Self-optimizer Task Pipelines

Status: M1–M3 implementation map, 2026-09-17.

The light optimizer reuses one common Task and the M2 protected experimental slot:

```text
OptimizationHistory + captured Source + Requirements
       |
       +-- optimization_profile (kernel, data-only)
       |       |
       |       +-- diagnose Worker (no SourceTree, empty sandbox)
       |               |
       |               +-- propose Worker (one installed recipe, no SourceTree)
       |                       |
       +-----------------------+-- optimization_prepare (trusted candidate construction)
                                       |
                              separately signed experiment closure
                                  /                 \
                         baseline verifier     candidate verifier
                                  \                 /
                              exact comparison accounting
                                       |
                              independent evaluator
                                       |
                         verified Snapshot/local delivery
```

The proposal is data, not authority. Preparation checks the selected recipe against the exact
installed catalog and development profile, then applies its bounded edits only beneath captured
writable roots. The experiment still pauses before child registration and requires the existing
authenticated exact-plan decision. Baseline, candidate and evaluator retain separate packages and
all Attempts settle on the parent Task ledger.

The checked-in command Workers under `fixtures/self-optimizer/catalog` are credential-free
contract fixtures. Real projects may install model Workers with the same role-scoped contracts;
configured Worker authority permits delivery of those declared inputs, never the protected source
or holdout bodies.

See [Self-optimizer execution](../task-execution/self-optimizer.md) for the requirement-to-test map
and explicitly pending release gates.
