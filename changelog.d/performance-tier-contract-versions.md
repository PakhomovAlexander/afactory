- Version the expanded performance tier contracts together: `af.perf-executor/5`,
  `af.perf-history/2`, `af.perf-equivalence/2` and `af.perf-schedule/3`. Readers reject prior
  identities, including standard-tier records, under
  [ADR-0145](../docs/adr/0145-read-only-what-this-release-writes-and-version-every-changed-shape.md).
  Synchronize schemas, fixtures and release/harness declarations without changing tier rules,
  measurement algorithms or publication authority. No performance improvement is claimed.
- Admit exact project-owned native and performance schema identities in release-controller
  preflight; malformed and foreign declarations still fail before candidate or remote effects.
- Make cleanup test HOME directories explicitly owner-private, including interrupted
  recreation, with a child-process umask 022 regression. Keep watcher test state and sockets
  in short canonical private `/var/tmp` fixtures, independently of ambient temporary-directory
  checkout markers; retain production account, checkout and audit Store guards.
