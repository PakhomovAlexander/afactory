- Add `./dev release` planning, RC execution, status and resume with exact fetched-source
  identity, isolated preparation, durable receipts and normal protected PR/check/merge semantics.
  `make release` and the shell entry point share this implementation. Replace the RC Python
  allowlist with an exact schema/fixture declaration generated during preparation; retain
  signed publication and separate human-reviewed pin adoption. Publication retries reconcile
  matching drafts/assets and signed provenance instead of clobbering releases.
- Drive default RC and resume invocations through pending checks, merge and signed publication
  with bounded polling and a six-hour timeout. Preserve recoverable waiting state on interruption
  or timeout; fail promptly on release workflow failures while keeping pin adoption independent.
- Refresh the exact release capability declaration for the captured current-main schemas and
  fixtures; retain contract drift refusal and executable contract/harness preflight checks.
- Bind exact release PR CI independently of formally required statuses, retain optional class
  exemptions, and refuse cuts without server-enforced strict base freshness. Require separate
  administrator setup; never change protections. Retain original signed bundles across publication
  retries, report missing pin credentials independently, and validate version-independent fixtures
  plus the real telemetry contract regression in the release gate.
- Bind release PR acceptance to the digest-verified exact-attempt validation plan, approved
  base/candidate merge tree, source catalog and authorized profile; wait through opened-fast versus
  labeled-full races. Require the complete Linux/macOS class and conditional job roster with exact
  skip semantics and paginated collection completeness. Support bare and ref-qualified canonical
  workflow paths with immutable workflow identity in CI and signed-bundle restoration.
- Recheck the complete exact-source CI run collection before accepting release checks, waiting
  when newer evidence appears. Wait through cancellation before a plan is retained; reject
  cancelled or failed validation once its plan establishes the authorized profile.
