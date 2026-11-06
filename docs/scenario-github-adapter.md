# GitHub Scenario adapter

The optional production adapter is `scripts/scenario-github-adapter.py`, installed separately
with `scripts/install-scenario-github-adapter.py`. It implements the existing
[Scenario adapter protocol](scenarios.md#machine-policy-and-bounded-adapters) and
[ADR-0275](adr/0275-install-a-durable-github-scenario-adapter.md). Nothing installs credentials,
registers an Ant, schedules work or grants publication merely by reading these files.
Service behavior is tested with mocked transport; live GitHub operation has not been validated.

## Installation and machine authority

V1 requires Unix, the trusted system `/usr/bin/python3` with isolated mode and site imports
disabled (`-IS`), its standard library and certificate store, and HTTPS access to `api.github.com`. The installed executable is
one complete application file: it imports no adjacent application modules or downloaded packages.
The interpreter, stdlib and repository administrators remain trusted machine/service boundaries.
The adapter clears the environment. It ignores proxies, ambient GitHub tokens, Python paths and
TLS environment settings; it never follows redirects, including repository renames. GitHub.com
is the only service. GitHub Enterprise, fork heads, teams, arbitrary comments and emails are
unsupported.

Create a private directory outside **every** Git checkout, mode 0700, and an owner-only config
JSON, mode 0600. All paths must be canonical absolute paths, with no symlinks. Ancestors must
belong to the account or root and cannot be writable by others, except sticky directories such
as `/tmp`. The config and token each have a private parent. State and executable paths are new,
inside private parents; the installer refuses existing state rather than resetting receipts.
The example `fixtures/scenarios/github/config.example.json` is inert: replace its paths,
repository name/ID, actor ID/login, branch mapping, check identities and reviewer identities after
operator review. IDs in examples are test data, never evidence about a real account.

The config contract is `af/github-adapter-config@1`:

| Field | Meaning |
| --- | --- |
| `repository`, `repository_id` | Exact canonical `OWNER/REPO` and positive GitHub numeric repository ID. One repository per installation. |
| `actor` | Exact `{id, login}` of the machine-owned bot user; `/user` must match on every invocation. |
| `token_file` | Explicit owner-only file outside Git; ASCII token only, optionally followed by a newline. Never a workflow input or environment variable. |
| `state_dir` | New private directory outside Git, created by the installer. Keep it for the entire lifetime of all intents. |
| `heads` | Nonempty exact map of pre-provisioned same-repository head branches to allowed base branches. |
| `required_checks` | Nonempty set of `{kind:"check", name, app_id}` or `{kind:"status", context, creator_id}`. No wildcard identities. |
| `reviewers` | At most 15 machine-authorized user `{id, login}` entries. No teams or arbitrary destinations. |
| `operations` | Explicit subset of `publish_pr`, `followup`, `notify`, `observe`. |

Provision a repository-scoped fine-grained personal access token for that bot user separately.
V1 verifies the user through `/user`; installation tokens without a user identity are unsupported.
Grant only the selected operations' permissions: Metadata read, Administration read (to inspect
head protection), Contents read, Pull requests read/write (create and request reviews), Checks
read, Commit statuses read and Issues read/write if follow-ups are enabled. The repository
administrator provisions and locks branches separately; this adapter never changes protection,
pushes, creates/deletes refs or merges. Token rotation in the same file is allowed only while
`/user` retains the pinned actor. Neither token bytes nor their digest enter receipts or stdout.

After reviewing those files, install offline. The following paths are placeholders:

```sh
/usr/bin/python3 -IS scripts/install-scenario-github-adapter.py \
  --config /ABSOLUTE/PRIVATE/github-config.json \
  --output /ABSOLUTE/PRIVATE/github-adapter
```

The installer prints the **executable** SHA-256 to paste into the existing Scenario machine
policy. It embeds the exact config path and config-byte digest into that executable; the
Scenario policy's executable digest therefore also pins configuration identity without pinning
secret material. Editing config, scope, paths or application code refuses existing dispatch.
Finish/cancel old Runs under their original installation before approving a different installation.
Do not discard uncertain receipts or reuse an old intent with a new installation.

The existing `af/scenario-policy@4` adapter entry is authored by the operator, for example:

```toml
[adapters.github]
executable = "/ABSOLUTE/PRIVATE/github-adapter"
digest = "sha256:REPLACE_WITH_INSTALLER_OUTPUT"
operations = ["publish_pr", "followup", "notify", "observe"]
destinations = ["github:OWNER/REPO"]
max_wall_ms = 30000
max_output_bytes = 65536
```

These fields are in addition to the policy's `schema`, `task_plans`, `task_templates`, `decision_keys`, and
`file_roots`. Grants in the Scenario must be subsets. No definition supplies an executable,
credential path, URL, environment or interpreter argument. Install neither the mock transport
nor a simulator in production. Re-pinning an executable is a new authority review.

## Exact objects and operation bodies

All operations use destination `github:OWNER/REPO`. Revision is the approved **40 lowercase hex
commit SHA**, never a branch name, PR number or merge commit. Unknown fields, duplicate JSON
keys, extra input ports and old versions fail closed. The core's strict port maps are unchanged.

Action `body` is a text datum containing one typed JSON payload. Canonical payload bytes are
UTF-8 JSON with sorted ASCII field names, no whitespace, literal Unicode, no nonfinite numbers;
use `json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=False)`.
Compute the subject **before** pinning the Scenario:

| Operation | Payload schema and fields (all required) | Subject |
| --- | --- | --- |
| `publish_pr` | `af/github-publish-pr@1`: `schema`, `head`, `base`, `title`, `body` | `github-pr:sha256:` + hex SHA-256 of canonical payload |
| `followup` | `af/github-followup@1`: `schema`, `publication`, `title`, `body` | `github-followup:sha256:` + hex SHA-256 of canonical payload |
| `notify` | `af/github-notify@1`: `schema`, `publication`, `reviewers` | `github-notify:sha256:` + hex SHA-256 of canonical payload |

`publication` is the original `github-pr:sha256:...` subject. Reviewers are exact `{id, login}`
entries, sorted by ID, unique, nonempty and within machine scope. This digest separately binds
notification target and recipient set; follow-up approval separately binds its full content.
Titles are single-line, at most 200 UTF-8 bytes; bodies are nonempty, at most 48000 UTF-8 bytes.
The reserved `<!-- af-github:` marker prefix is forbidden in approved content. Branch names use
a restricted Git-safe ASCII alphabet. Titles, bodies and recipient lists supply data only.

Publish and follow-up require their own signed Decisions through the kernel. Notify may also
require a Decision; the shipped inert example does. Decisions use each operation's exact subject
and revision, and the pin binds its consumer, grant and payload. The adapter trusts af's verified
minisign receipt; it rechecks object binding but is not an independent signature verifier. An
owner who can run or edit the trusted extension can already use its credentials outside af.

`publish_pr` creates a non-draft PR with `maintainer_can_modify:false` and the exact approved title
and body plus a machine marker. It does not push. GitHub's
[REST create API](https://docs.github.com/en/rest/pulls/pulls#create-a-pull-request) accepts a branch
name without an expected-head SHA; a preflight GET is not an atomic publication guard.
V1 requires server-enforced
[branch protection](https://docs.github.com/en/rest/branches/branch-protection#get-branch-protection)
on the provisioned **head**: `lock_branch.enabled` and `enforce_admins.enabled` true;
`allow_deletions.enabled`, `allow_force_pushes.enabled` and `allow_fork_syncing.enabled` false.
Missing, unlocked or unsupported protection is refused before mutation. The branch must be
protected at the exact approved SHA. Protection and exact head are checked again after creation
and during recovery. Administrators must preserve the lock and protection while publication is
in flight: changing protection is outside the adapter's authority and breaks this trusted
administrator assumption. A post-mutation race remains pending, never successful or definitively
failed.

`followup` creates exactly one issue with approved content and a marker. It requires this
installation's completed publication receipt and reads the same PR at the approved head before
and after the issue creation. An arbitrary existing issue or PR cannot supply that provenance.

`notify` requests the authorized user reviewers on that recorded, open, non-draft PR. It checks
exact head, protection, current required CI and each recipient's numeric identity first. A fixed
[GraphQL `requestReviews` mutation](https://docs.github.com/en/graphql/reference/pulls#requestreviewsinput)
uses the verified PR and user node IDs with `union:true`; mutable usernames are never mutation
targets, so rename/reuse cannot notify a different account. Only User accounts are supported as
recipients. No workflow supplies GraphQL text. There is no generic message or comment endpoint.
A successful mutation response and REST readback are durably recorded before returning done.
An uncertain review request stays pending forever unless its successful response was already saved: GitHub has no intent marker for it, and presence or
absence of requested reviewers cannot identify this invocation after reviewer removal. It is
never resent just because a reviewer disappeared.

## PR waits without changing a pin

The inert `fixtures/scenarios/github/bundle.json` demonstrates signed publication → exact-head
green → signed review request → merged → separately signed follow-up. It has no Task or active
Ant. Replace its fixture values and compute subjects once before validating/confirming the pin.
Each publication Action's existing `action` output retains subject/revision and adapter evidence;
the adapter retains the numeric GitHub PR ID and number. Later Waits use the **same original
publication subject and approved SHA**, with only the existing `subject` and `revision` text
ports and `condition` `checks_passed` or `merged`. The adapter resolves the PR from its durable
local publication receipt. No PR-number projection, invented runtime port or manual pin rewrite
is required. Observe never adopts a PR from remote search alone.

CI requires every machine-authorized identity at that exact SHA. Checks use exact name and app
ID, latest run ID and latest check suite per app; both suite and run must have explicit completed
success. A required name emitted by another app is refused. V1 conservatively supports required
names in the newest suite for each app; multiple independent workflows using the same app may
remain unsatisfied until all required names occur in that suite. This conservative scope avoids
accepting old suite success while a new suite has not emitted its required job. Statuses use
exact context and creator ID, selecting the newest status ID from the exact-SHA endpoint.
Missing, pending, stale, failing, skipped, neutral, cancelled or incomplete evidence is never
green. Both APIs paginate completely within the configured bounds; HTTP errors, auth failures,
rate limits and shifted/partial listings remain unknown. The PR identity and head are read
before and after observation. This is an observation of current service evidence, not an
immutable guarantee about future checks or a substitute for Task acceptance.

Merged observation requires the same recorded PR ID, destination, author, content, branches and
approved head, plus `state:closed`, `merged:true`, a merge timestamp and merge commit SHA.
An open PR waits. Closed without merge returns definitive failure. Merge itself is never
performed by the adapter. Changed head or content remains unknown and cannot complete a Wait.

## Recovery, bounds and evidence

Before any possible POST the adapter fsyncs `dispatched` in its private journal. A nonblocking
exclusive lock serializes all invocations against one retained lock inode. The `af/github-adapter-state@3` journal binds
intent ID, opaque kernel input digest, entire exact request, config identity, phase and remote
object ID/number; checksums detect corruption. Every invocation validates the journal and exact
input match. A different intent cannot publish the same subject/revision while another one is
prepared, dispatched or done. Pre-dispatch definitive failure is durably marked `failed`; only
that proven no-effect failure allows another bounded intent. A missing or corrupt journal is
never reinitialized at runtime.

Reconcile may return `absent` only for a durably prepared, never-dispatched local intent. After
possible dispatch, even a complete empty remote listing means **pending**, not absent. Recovery
uses exact marker, authenticated actor, full approved content, object IDs, repository and SHA;
a spoofed marker or unrelated issue cannot settle the intent. Confirmed object IDs are fetched
directly. Done is persisted before stdout. A crash after effect but before response can recover
PRs/issues from positive remote evidence; execute on an uncertain intent only reconciles.

The application wall is 24 seconds (inside af's at-most-30-second supervision), each HTTP call
at most 5 seconds and at most 100 calls. Input/config are at most 256 KiB, each response 2 MiB,
listing at most 10 pages of 100 objects. Output is one bounded existing response. State admits
at most 4096 receipts and 16 MiB of journal bytes, plus one bounded 16 MiB atomic-write scratch
slot. These are fixed code bounds, not workflow knobs. State writes fsync the file and directory;
unknown shapes/versions, checksum errors, partial responses and unavailable locks fail closed.

Receipts, including uncertain intents, have no automatic retention expiry or collection. At a
budget limit new intents are refused; existing receipts still reconcile. Keep original state,
config and executable while any Run may dispatch or reconcile. Never delete receipts, initialize
a replacement journal or create a new intent to retry an ambiguous effect. Inspect
`af scenario show RUN --json`, its waiting evidence and the private journal's recorded object
IDs/marker; compare the exact GitHub object with the owner. Restore unavailable service access
and resume the same intent. If positive evidence cannot establish the effect within bounds,
leave it pending or cancel the Run and retain the receipts for manual service handling.
Cancellation does not roll back an effect. There is no automatic unsafe “force retry” escape.

The mock service, private fake token and installed transport fixture in
`test-scenario-github-adapter.py` and `scenario::production_github_adapter_*` test the production
stdin/CLI path, crashes, concurrency, stale checks, scope, receipts and ambiguous recovery without
using developer credentials or real external mutations. Schemas live under `schemas/github-*-v1.json`;
`python3 -I scripts/scenario-github-schemas.py` checks their parity with the adapter contract.

The current adapter reads `af/scenario-adapter-request@3` and writes its embedded requests in
`af/github-adapter-state@3`; earlier request/journal generations fail closed. Config and operation
payloads retain generation 1. Finish old Runs under their original release/installation before
upgrading; there is no journal migration or automatic replay through a replacement installation.
The [performance loop integration decision](adr/2026-11-03-native-performance-scenario-loops.md)
records this protocol integration without changing publication authority.

The optional [Git workspace capability](scenario-git-workspace.md) reuses this production adapter
for accepted-candidate publication and waits. Its explicit exclusive writer mode is separate
from this installer's unchanged server-locked-head contract.
