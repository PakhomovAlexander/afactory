- The performance laboratory now bounds its work by absolute deadlines that a slow step cannot
  stretch. A scratch filesystem check fixes one deadline before its first step. A capacity
  refusal that arrives after the fill's bound no longer verifies the scratch. A fill still
  blocked at the deadline is abandoned, and its file is kept and named. A sample container is
  removed once, within one 60 s allowance that also ends by the run's deadline. A seed's
  directories are refused while they are read, as soon as they exceed the entry bound. Each
  HTTPS request of the archive collector, its redirects included, is refused at its deadline
  however slowly the server sends, with TLS verification unchanged. A cancellation admitted
  before a sample's launch now always prevents the spawn; one that arrives during it cancels the
  launched child. The supervisor's integration test runs its own copy of the example, so a
  parallel test's rebuild can no longer remove it mid-run
  ([ADR-0173](docs/adr/0173-own-the-supervisor-a-test-runs-and-bound-admission-collection-scratch-and-cancellation-by-absolute-deadlines.md)).
