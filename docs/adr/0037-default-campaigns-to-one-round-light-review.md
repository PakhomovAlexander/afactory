# Default Campaigns to one-Round light review

**Status:** accepted (2026-08-31); superseded in part by
[ADR-0113](0113-ga-reads-only-what-ga-writes.md): the explicit `--light` flag. Light review stays
the default, and `--heavy` is the only mode flag.

M6.3 dogfood exposed a process failure as well as code defects. Four successive correctness
Campaigns ran seven paid Terra/xhigh Attempts and spent 1,380,750 chargeable tokens. Each pass
found useful defects, but the operator had asked for lightweight dogfood; repeatedly opening a
fresh Campaign after fixing the prior pass was not authorized by that intent. Documentation alone
did not stop the agent from treating a clean follow-up Campaign as an implicit milestone gate.

`af review run` has two mutually exclusive Campaign modes. `--light` is the default. It resolves
the Campaign Manifest's effective convergence authority to one clean Round and one maximum Round,
while retaining the pipeline's severity gate, reviewers, checks, budgets, and all other authority.
An incomplete light Round may resume its exact inputs, but after one Round closes the CLI refuses
another dispatch. A finding-bearing light result remains a truthful non-zero review failure and
emits the next action in human and JSON output: fix the concrete Findings, run the deterministic
project gate, and stop; do not start another Campaign.

`--heavy` is deliberate opt-in. It preserves the complete convergence policy captured from the
pipeline and is used only when a human explicitly requests convergence review. The selected
effective convergence policy is already part of `CampaignManifest@1`, so no new persisted shape
is required. Resuming a Campaign with a mode different from the one that opened it fails before
dispatch. A heavy Campaign that reaches its pinned Round limit also refuses further dispatch and
requires an explicit human decision before any new Campaign.

## Considered options

- **Keep the distinction in agent instructions only.** Rejected because the M6.3 incident
  demonstrated that prose did not reliably stop repeated fresh Campaigns.
- **Make finding-bearing light review exit successfully.** Rejected because a concrete Finding is
  still a failed review result; spend policy must not turn it into approval.
- **Reduce reviewer reasoning or token reservations in light mode.** Rejected as the defining
  boundary because truncating one Attempt can weaken defect discovery. Light bounds the number of
  closed Rounds; ordinary pipeline budgets still bound each Attempt.
- **Let light mode run the pipeline's full clean window but merely recommend stopping.** Rejected
  because another invocation would still spend before the recommendation could protect the
  operator.
- **Persist a new mode field in a new Campaign Manifest version.** Postponed because the existing
  effective convergence fields express the complete execution difference. A future mode with
  behavior beyond convergence must introduce explicit versioned authority.
- **Default to one-Round light authority and require explicit heavy opt-in (chosen).** This makes
  the safe spend behavior executable while leaving deep convergence available.

## Consequences

- Plain `af review run` performs at most one closed review Round. Agents do not infer a second
  Campaign from a non-zero light result.
- Light mode does not claim convergence after Findings are fixed; the deterministic project gate
  is its closeout. Users who need reviewer-confirmed convergence explicitly choose `--heavy`.
- Existing Campaigns opened under full pipeline convergence must be resumed with `--heavy`; the
  default does not silently continue old expensive authority.
- A pipeline already configured for one Round has identical effective light and heavy convergence;
  this ambiguity grants no extra execution.
- The mode does not weaken Gates, sandboxing, credentials, fixtures, reviewer packages, severity,
  or token caps. It only bounds whether another closed Round may dispatch.
