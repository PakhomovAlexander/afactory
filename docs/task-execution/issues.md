# Captured issue requirements

Issue adapters produce business input before Pipeline selection. The same implementation and
embedded Review definitions run after either local or Jira capture.

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

[jira.personal]
site = "example.atlassian.net"
email = "developer@example.invalid"
token_file = "/absolute/local/path/jira-token"
```

The token file contains the API token. Configure the Task file's issue selector:

```json
{
  "kind": "jira",
  "binding": "personal",
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

Explicit ticket refresh with a new Task revision is still under implementation. `af task run`
continues captured work and does not fetch the latest ticket.
