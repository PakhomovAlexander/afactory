- A release now reads only the state, configuration and flags it writes itself: no reader,
  migration or fallback is kept for anything an earlier release wrote, and a changed persisted
  shape takes a new version whose predecessor is refused
  ([ADR-0145](docs/adr/0145-read-only-what-this-release-writes-and-version-every-changed-shape.md)).
  Finish or discard in-flight Campaigns and Tasks before upgrading. The Warm Set is now
  `review.kernel/WarmSet@2`, selected by `WarmSetSelected@2`, with no session layer; a Campaign
  that recorded `WarmSetSelected@1` is refused by its replay error. The never-hosted Claude
  session layer and Cold Closeout are removed with their events, so a pipeline that sets
  `warm.session`, `warm.session_max_age_secs` or `convergence.cold_closeout` no longer loads:
  delete the key. A Campaign the pre-Task executor wrote is refused by the same replay error as
  any other record this release does not write, instead of a dedicated message. The `.af/`
  layout no longer declares `task-compat/`, so a tracked copy is reported as undeclared.
