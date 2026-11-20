# Project backlog GitHub Issues destination

The isolated [Python destination](../scripts/project-backlog-github.py) is ready for an
authorized dispatch caller. Run its fake transport tests from the checkout root without
credentials or a network connection.

```sh
python3 -I scripts/test-project-backlog-github.py
```

## Dispatch contract

Call `publish_issue(intent, authority, service, commit_send=...)` with the exact public
projection of an already committed Project Store write intent. The input contains
`intent_id`, `item_id` and `revision_id` as SHA-256 identities, the approved `title`
and `body`, and the `destination` (`github:Owner/Repo`), numeric `repository_id` and
`publisher` (`id` and `login`). Supply the destination, repository ID and publisher
independently from machine-local authority. Keep Report evidence, verification reasons and
credentials outside the intent passed to this module.

Omit `commit_send` for read-only reconciliation. For a fresh send, the caller's
`commit_send(intent_id)` must recheck eligibility and atomically acquire a durable, exclusive
send claim for this intent and its destination, item and revision. Return `True` only once the
claim commits. After a claim, resume using reconciliation, even if the process ended before
returning a result. The caller owns the Store transaction, the process deadline and receipt
persistence. Tests supply a fake service. The optional `IssueTransport(token_file)` uses an
explicit private credential file supplied by the authorized caller.

The marker includes the stable item and approved revision IDs and a digest of the approved
title and body. The destination checks the authenticated account and immutable repository ID.
It walks both open and closed issues in bounded, sequential pages and rechecks a unique match
at its exact issue URL. It never follows pagination URLs. An incomplete listing or missing
marker can remain pending across calls. Human edits around an intact marker and closure survive
reconciliation. An independently approved later revision can create its own issue.

| Status | Caller interpretation |
|---|---|
| `done` | Persist the receipt with the intent, item, revision, destination, publisher, GitHub issue ID and number, observed state and whether the title and body still match. |
| `retry` | A transient transport error occurred before this invocation reached the send guard. A fresh attempt still needs an exclusive Store claim. |
| `pending` | Reconcile again if the Store shows a claim, or evaluate the unresolved observation under the existing intent if it is still unclaimed. |
| `conflict` | A publisher, repository or issue identity contradicted the intent, or the exact marker was ambiguous. |
| `refused` | Review the local intent, authority or pre-dispatch failure before trying again. |
