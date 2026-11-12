- Add `./dev release` planning, RC execution, status and resume with exact fetched-source
  identity, isolated preparation, durable receipts and normal protected PR/check/merge semantics.
  `make release` and the shell entry point share this implementation. Replace the RC Python
  allowlist with an exact schema/fixture declaration generated during preparation; retain
  signed publication and separate human-reviewed pin adoption. Publication retries reconcile
  matching drafts/assets and signed provenance instead of clobbering releases.
