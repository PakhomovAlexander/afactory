# Captured issue requirements

Issue adapters produce business input before Pipeline selection. The same implementation and
embedded Review definitions run after either local or Jira capture. Standalone Review can also
select an issue source; each reviewer receives and retains its exact captured requirements.

```text
Task file + explicit issue source
              |
       bounded read-only capture
              |
     exact raw bytes + field digests
              |
       normalized Requirements
              |
  shared implementation -> shared Review
              |
  evaluate exact requirements on final source
              |
       verified internal Snapshot
              |
     explicit local worktree delivery
```

## Account-free issue flow

Create and commit a [software starter](starters.md). Add `issue.json`:

```json
{
  "schema": "af.issue-input/1",
  "id": "10042",
  "key": "AF-42",
  "revision": "2026-09-12T10:00:00Z",
  "summary": "Offset and limit pagination",
  "description": "Preserve the original input values.",
  "acceptance": {
    "customfield_1": "Reject noninteger and negative bounds."
  }
}
```

Add `"issue": {"kind": "local", "path": "issue.json"}` to `implementation-reviewed.json` and
commit both files. The equivalent TOML issue format produces identical normalized requirements and
field digests while retaining its distinct raw bytes. The starter's explicit machine-readable
specification defines what the command substitutes implement and verify; replace those Workers
for broader prose interpretation.

```sh
af task plan --file implementation-reviewed.json --json
af task run implementation-reviewed --json
af task explain implementation-reviewed --json
af task deliver implementation-reviewed --branch af/pagination \
  --worktree ../pagination-result --confirm implementation-reviewed --json
```

Planning captures input but dispatches zero Workers. The Task's goal includes the original action
and normalized issue requirements. Later changes to the working issue file do not affect recorded
execution or replay. Delivery retains the existing clean-target and exact-source requirements.

## Explicit Jira Cloud binding

Keep local account settings in a machine-local file:

```toml
schema = "af.task-source-bindings/1"

[jira.cloud]
site = "example.atlassian.net"
email = "developer@example.invalid"
token_file = "/absolute/local/path/jira-token"
```

The token file contains the API token. Configure the Task file's issue selector:

```json
{
  "kind": "jira",
  "binding": "cloud",
  "key": "AF-42",
  "acceptance_fields": ["customfield_10001"]
}
```

Place this object in the Task file's `issue` property, then capture explicitly:

```sh
af task plan --file implementation-reviewed.json \
  --source-bindings /absolute/local/path/source-bindings.toml --json
```

The adapter uses Jira's read-only
[Get issue endpoint](https://developer.atlassian.com/cloud/jira/platform/rest/v3/api-group-issues/#api-rest-api-3-issue-issueidorkey-get)
and the local email/API-token
[authentication method](https://developer.atlassian.com/cloud/jira/platform/basic-auth-for-rest-apis/).
It selects summary, description, updated and the named custom fields. Supported
[ADF](https://developer.atlassian.com/cloud/jira/platform/apis/document/structure/) text, paragraphs,
headings, lists, quotes, code blocks and links retain their order and meaning in normalized text.
Unsupported semantic content causes refusal; links remain data. Missing selected requirements,
authentication errors, rate limits, oversized responses and timeouts never become empty accepted
requirements. There is no automatic retry or source write.

The adapter starts `/usr/bin/curl` with owned flags, an isolated environment and stdin credentials.
Its [curl configuration](https://curl.se/docs/manpage.html) disables ambient configuration and
redirects and enforces HTTPS, response-size and time bounds. Account configuration and raw Jira
responses are excluded from Worker payloads. A Task result retains exact captured field provenance.

## Refresh an updated issue

```sh
# Read the captured project-relative issue path again.
af task refresh implementation-reviewed --json

# Or explicitly read a fresh local JSON/TOML representation of the same issue.
af task refresh implementation-reviewed --source-file /absolute/path/updated-issue.toml --json

# Jira requires the explicit local binding again and retains its exact tenant/key/field selector.
af task refresh implementation-reviewed \
  --source-bindings /absolute/local/path/source-bindings.toml --json
```

Refresh captures and plans without running Workers. A changed selected field or source revision
creates a new Task revision. It preserves the original action, structured specification, code
Snapshot, permissions, verification, selection facts, total budget and absolute deadline. Live
edits to the Task file, source code or catalog do not become execution authority. Formatting or
unselected-field changes alone preserve the existing observation and approval.

```text
new issue observation
         |
   remaining-capacity selection
         |
   atomic revision + plan barrier
         |
         +-- existing plan ------> ready
         +-- generated plan ----> needs_plan_review (fresh exact signature)
         +-- no fit ------------> fixed Planner preparation (run explicitly)
         +-- insufficient ------> needs_resources (no plan, no dispatch)
```

Every earlier Attempt and token charge stays on the same ledger, including late usage. A fitting
previously generated definition is reused without another Planner call, but its new plan requires
fresh approval. A completed Task may receive an updated revision if its original allowance can
still fund the required work. Refresh does not extend a deadline or allocate more Attempts.

Inspect the new state with `af task explain`. Approve a generated plan through the existing
[signed decision flow](generated-plans.md), then run it explicitly. An unplanned waiting revision
makes `af task run` exit 4; it cannot fall back to the earlier approved plan. Existing results,
decisions and delivery receipts remain in history. A delivered worktree is unchanged. Each new
verified result can be delivered explicitly to another absent branch/worktree; inspection does
not present an earlier result's receipt as delivery of the current revision. An unfinished delivery
preparation must be reconciled before refreshing its Task.

`af task run` continues captured work and never fetches the latest ticket. The atomic barrier and
validation rules are recorded in [ADR-0062](../adr/0062-refresh-issue-revisions-without-resetting-execution-authority.md).
