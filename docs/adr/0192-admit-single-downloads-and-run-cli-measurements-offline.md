# Admit single downloads and run CLI measurements offline

Status: accepted, 2026-10-21.

## Context

The first hosted partial A/A run, Actions `39633877665`, built both sides in the verified GNU
image, verified its bounded scratch and recorded every one of its 216 planned slots. It also
exposed two integration defects: aggregation refused the flat directory written by the pinned
download-artifact v8 action for one matching artifact, and all 28 CLI Task slots refused setup
because AF's detached release update check outlived the fixture command. The supervisor's
refusal was correct. The original run and failures remain retained.

## Considered options

- Require two shards or relax descendant checks: rejected; a single shard is supported, and
  an unexpected surviving process invalidates the measurement.
- Assume a flat download is shard zero: rejected; the only surviving artifact may be any
  planned shard, while the others remain missing.
- Disable product self-management globally: rejected; the laboratory controls its own command
  environment and must not change ordinary AF behavior.

## Decision

Aggregation admits the pinned action's two directory layouts: one flat transport artifact, or
separate named artifact directories. The flat transport is read with the same bounded regular
file admission as every other artifact. Its validated transport document supplies the shard
identity. Foreign runs, unplanned or duplicate shards and mixed layouts remain refused;
absent shards remain incomplete. No persisted record changes shape or version.

The stable status check likewise reads a flat single verdict or the named primary report.
It rejects mixed layouts and still binds the verdict to the exact run, attempt, plan and
required-check eligibility.

CLI measurement, setup, seed writing and diagnostic runs share one environment declaration
with `AF_SELF_OFFLINE=1`. These workloads measure the frozen product without a release lookup
or install. Their private HOME/XDG state, independent oracles, containment, deadlines and
descendant checks are unchanged. The authority's harness digest changes with this declaration;
earlier and later harnesses are not silently treated as one calibrated authority.

## Consequences

Tests round-trip actual transport bundles through the flat download layout, including a sole
nonzero shard, missing evidence and mixed-layout refusal. Status tests cover both layouts and
foreign identity. CLI stand-ins require the offline setting in the processes they execute,
covering propagation through setup and samples. A new hosted run must demonstrate the repaired
path before calibration begins; a locally regenerated report is not a repaired Actions result.
