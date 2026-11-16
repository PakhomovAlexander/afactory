- Recheck workspace publication identity and exact signed expiry after blocking observations,
  immediately before owned pushes and after HTTP connection establishment. Native accepted-Task
  regressions cover changed actors and expiry at both boundaries without external publication.
  See the [guard decision](../docs/adr/2026-11-16-recheck-owned-publication-after-identity-observations.md).
- Correct release-contract expectations for current Scenario/Ant generations and reject retired
  filenames and discriminators. Startup negative fixtures now include the immediately retired
  pin, Run and Store generations. No persisted format or authority is widened.
- Issue 834 Stage C remains incomplete: scoped accepted publication, durable preparation progress
  and continuous service driving are not implemented by this change.
