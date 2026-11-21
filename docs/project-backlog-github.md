# Project backlog GitHub Issues destination

Install the checked-in [Python executable](../scripts/project-backlog-github.py) as
`backlog-github-adapter` following the [receiver runbook](project-backlog.md#github-issues-destination).
AF pins the executable's bytes and supplies the configured Project, destination, repository,
publisher and credential in its private subprocess environment. The adapter uses isolated system
Python and the standard library.

The coordinator commits each attempt before launch. The adapter authenticates the GitHub account
and repository, then uses the same Project/item marker across revisions. A fresh create can send
one issue POST. An uncertain create or a later mapped revision can only read. An empty lookup
leaves an uncertain create pending. Reconciliation observes the title, body and status without
changing them and returns only their digest and provider identities to AF.

The offline adapter and Scenario regressions exercise the installed entrypoint with a fake HTTPS
connection.

```sh
python3 -I scripts/test-project-backlog-github.py
cargo nextest run --workspace --locked --profile ci \
  -E 'binary_id(af::it) & test(portable_backlog_github_)'
```
