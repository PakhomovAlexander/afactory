- `report --diagnostic` now admits a child run's views against the trusted renderer, not their
  hashes. The child page must be exactly the page drawn from its admitted profiles and traces.
  Each sanitized flamegraph must be what the sanitizer makes of its original, and each Perfetto
  export must be its trace's. The frozen child plan must carry the parent's sources, selectors
  and authority, and the profiles must name that plan's sources, recipe, capture bounds and
  cases. An inconsistent child, or one with active HTML or SVG, is refused, and no child bundle
  is rewritten. A dispatched `refs/heads/NAME` now resolves where the workflow's checkout
  fetched it, `refs/remotes/origin/NAME`, in the control step and the scheduler alike, and is
  refused as ambiguous when a local branch of that name disagrees
  ([ADR-0178](docs/adr/0178-admit-linked-diagnostic-views-by-their-renderer-and-read-dispatched-branches-where-the-checkout-fetched-them.md)).
