- Keep CI Health unknown timing within its exact live workflow/job/platform/head scope, and
  preserve the latest actual known failure completion when an earlier unknown occurrence recovers.
  Older green jobs retain unresolved release blockers; unknown timing on another head no longer
  prevents valid closure. Capture time never becomes completion evidence. Signature state uses
  `af.ci-signature-state/5`; earlier versions are refused without migration under
  [ADR-0145](../docs/adr/0145-read-only-what-this-release-writes-and-version-every-changed-shape.md).
