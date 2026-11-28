- Allow `strict_required_status_checks_policy: false` on the active `main` ruleset in
  `scripts/dev-release.py` while continuing to require an active repository-owned
  `required_status_checks` ruleset with no bypass actors, unchanged `main` preflight, and
  tightened post-merge tree and exact first-parent verification matching
  `scripts/resolve-release.sh`.
