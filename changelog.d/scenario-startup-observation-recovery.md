- Fix scoped implementation driver recovery after positively typed transient startup observations.
  Retain the exact retry cadence, consumed resume allowance and original deadlines across restart;
  keep credentials, authority, integrity and resource refusals fail closed. Validate publication
  cadence against every enclosing invocation path, including shared children. Driver 2 and Store 16
  refuse prior shapes without migration. Generic signed and nightly publication remain unchanged.
  See the [decision](../docs/adr/2026-11-17-retry-typed-startup-observations-within-enclosing-bounds.md).
