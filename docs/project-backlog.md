# Project backlog operator runbook

Record a private Report from a Project Workspace and continue the current Task as soon as
AF returns its local receipt. The designated authority receives Reports through signed file
transfer, freezes daily batches, and prepares an independently verified Scenario. A daily Ant
can run the prepared Scenario. Publication to GitHub Issues requires a separately configured
receiver, destination and credential.

The privacy and receipt boundaries are fixed by
[ADR-2026-11-20](adr/2026-11-20-store-project-backlog-reports-privately.md). Project and Workspace
identity and the Store location are defined in [Projects and Workspaces](project.md).

## Offline local capture and inspection

For a copyable development-session command, see the [Kimi capture runbook](project-backlog-kimi.md).

`af backlog record` requires a nonblank `--title` and `--observed` fact. `--report-id` is optional;
supplying a stable producer ID lets an identical retry recover a lost receipt without duplicating
the Report. `--repro` is optional. Each `--evidence-file` adds private, untrusted UTF-8 text; at
most eight files and 65,536 bytes in total are accepted. Evidence is validated before capture.

The Report and its evidence are committed to the local Project Store at
`$XDG_STATE_HOME/af/project/<hex>/`, where `<hex>` is the Project ID digest; AF's ordinary local
Store registry is also below `$XDG_STATE_HOME/af/`. Worktrees with the same repository root
commits resolve the same Project Store. No Provider, model, credential, remote repository, or
network service is needed.

### Disposable two-Workspace demonstration

Use Bash, Git, and the `af` binary built from this source. The block uses
`set -euo pipefail`: Bash's `pipefail` option makes a failed AF command fail its pipeline even
when `tee` succeeds. Put that binary on `PATH`, or set `AF` to its absolute path before running
the block. The block creates one local sample repository and two clean, detached worktrees. It
also creates a fresh private home and XDG directories, so it neither reads nor changes an
operator's installed AF state. Every AF command below is expected to exit zero; `tee` preserves
its JSON result under the disposable directory.

```bash
set -euo pipefail
AF=${AF:-af}
scratch=$(mktemp -d "${TMPDIR:-/tmp}/af-backlog.XXXXXX")
trap 'rm -rf -- "$scratch"' EXIT

export HOME="$scratch/home"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_STATE_HOME="$HOME/.local/state"
export XDG_CACHE_HOME="$HOME/.cache"
export XDG_DATA_HOME="$HOME/.local/share"
export XDG_BIN_HOME="$HOME/.local/bin"
mkdir -p "$XDG_CONFIG_HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME" \
  "$XDG_DATA_HOME" "$XDG_BIN_HOME"
export AF_SELF_OFFLINE=1
export NO_COLOR=1
export GIT_CONFIG_GLOBAL="$scratch/gitconfig"
export GIT_CONFIG_NOSYSTEM=1

git init -q "$scratch/repository"
git -C "$scratch/repository" config user.name "AF Backlog Example"
git -C "$scratch/repository" config user.email "af-backlog@example.invalid"
git -C "$scratch/repository" commit --allow-empty -q -m "sample root"
git -C "$scratch/repository" worktree add --quiet --detach \
  "$scratch/producer-a" HEAD
git -C "$scratch/repository" worktree add --quiet --detach \
  "$scratch/producer-b" HEAD

printf '%s\n' 'synthetic evidence: local setup reached the stop marker' \
  > "$scratch/evidence.txt"

(
  cd "$scratch/producer-a"
  "$AF" backlog record \
    --report-id producer-a \
    --title "Synthetic planner stop" \
    --observed "The synthetic planner stopped after local setup." \
    --evidence-file "$scratch/evidence.txt" \
    --json
) | tee "$scratch/receipt-a.json"

(
  cd "$scratch/producer-b"
  "$AF" backlog record \
    --report-id producer-b \
    --title "Synthetic planner stop" \
    --observed "The synthetic planner stopped after local setup." \
    --evidence-file "$scratch/evidence.txt" \
    --json
) | tee "$scratch/receipt-b.json"

cd "$scratch/producer-b"
"$AF" backlog inbox --json | tee "$scratch/inbox.json"
"$AF" backlog show producer-a --private --json | tee "$scratch/report-a.json"
"$AF" backlog list --json | tee "$scratch/list.json"
```

Run the whole block again in a fresh Bash shell to repeat the demonstration. `mktemp` selects a
new repository, pair of worktrees, and Project Store, so the new inbox again contains exactly
these two Reports. Reusing an old scratch directory is not a fresh-workspace run.

### Recorded fresh-workspace validation

The block above (SHA-256
`0c05d174dc56ae03f45a7052072cfd20cb17f44e8a25f5ea69f54571e3bce961`) was executed unchanged
with `/bin/bash` against the built AF CLI (binary SHA-256
`b5088f9370b57417a232efbed3fd12952a6e9c951b56a16d376a2b33b557b9cb`). Each run used a new
temporary repository, two clean worktrees, and private XDG state. The command-exit columns below
follow the block's order: producer A's `record`, producer B's `record`, `inbox`, private `show`,
and `list`.

| Run | Project ID | Overall exit | Record A | Record B | Inbox | Private show | List | Wall seconds |
|---|---|---|---:|---:|---:|---:|---:|---:|
| 1 | `sha256:9b8133a24591715685914f67fb654375b1775704078358c32a20147403dc1807` | 0 | 0 | 0 | 0 | 0 | 0 | 1.30 |
| 2 | `sha256:5e7827ec0a07a189249ebd5acd082356c1552b7cbb8e1df25853b99cb9da8777` | 0 | 0 | 0 | 0 | 0 | 0 | 0.46 |

The distinct Project IDs record the two fresh repositories. In both runs the observed views were
`capture`, `capture`, `inbox`, `report`, and `list`, and the Report IDs were `producer-a` and
`producer-b`. Each run produced two receipts with `local_capture = true` and zero authority
receipts. The private inbox contained two Reports, private `show` returned `producer-a`, and the
verified item count was zero (`data.items = []`). Both runs used zero Provider tokens.

### Read the two capture receipts

Every JSON result has `schema = "af/backlog@1"`, a `view`, and a `data` object. The two capture
commands produce `view = "capture"` and these receipt fields:

| Field | What the two results show |
|---|---|
| `data.project_id` | The same repository-derived Project ID in both worktrees. |
| `data.report_id` | The distinct stable producer IDs `producer-a` and `producer-b`. |
| `data.captured_unix_ms` | The first durable capture time for each Report, as a decimal string. |
| `data.local_capture` | `true`: the Report and its evidence committed to the local Project Store before the receipt was printed. |
| `data.authority_receipt` | `null`: no designated authority has received or acknowledged either Report. |

Thus, a successful local receipt is not an authority receipt, an independent verification, or a
publication receipt. The identical title, fact, and evidence do not merge the Reports: their
Report IDs and Workspace IDs remain distinct. An identical retry with the same producer ID keeps
the original capture time and returns the local receipt again; reusing that ID for different
content is refused.

### Inspect private and verified views

The last three commands run in `producer-b` but read the Project shared with `producer-a`:

- `inbox.json` has `view = "inbox"`. Its `data.reports` array contains both private Reports.
  Each Report includes `report_id`, `workspace_id`, `captured_unix_ms`, `title`, `observed`,
  `source_revision`, `privacy = "private"`, and `evidence`. Task and Run provenance are `null`
  when AF has no trusted values. Each evidence entry has `content_id`, `media_type`, and `text`.
- `report-a.json` has `view = "report"`; `data` is the single `producer-a` Report. The explicit
  `--private` selects local operator inspection and includes the private fact and evidence.
- `list.json` has `view = "list"` and `data.items = []`. Local capture alone creates no verified
  item. Ordinary `af backlog list` and `af backlog show` expose only verified item and revision
  IDs and approved public fields; they do not disclose raw Reports or evidence.

Do not treat an empty verified list as a lost Report: use `af backlog inbox` or
`af backlog show REPORT_ID --private` for the private intake view.

### Regression coverage for these semantics

The integration test
[`backlog_recovers_a_lost_receipt_and_shares_privately_across_worktrees`](../crates/af/tests/it/project.rs)
uses two worktrees and distinct producer IDs, checks both capture-receipt fields, recovers a lost
receipt, reads both Reports from the shared inbox, checks private `show`, and verifies that the
verified list is empty. The adjacent `backlog_refuses_bad_fact_and_evidence_before_capture` test
covers blank facts, invalid UTF-8, oversized evidence, and symlink refusal. The JSON shapes are
defined by [`backlog-v1.json`](../schemas/backlog-v1.json) and
[`backlog-record-v1.json`](../schemas/backlog-record-v1.json).

## Transfer a Report to the designated authority

[Enroll the producer and authority keys](project.md#enroll-a-producer-for-offline-transfer)
before exchanging files. Keep private keys on their owning machines and use a private file
channel for the submission and receipt. Run each command from a Workspace of the same Project.

```bash
# Producer checkout
umask 077
af backlog outbox --json
af backlog export "$report_id" --key "$producer_key" > "$exchange/submission.json"

# Authority checkout, after receiving submission.json
umask 077
af backlog accept "$exchange/submission.json" --key "$authority_key" > "$exchange/receipt.json"

# Producer checkout, after receiving receipt.json
af backlog ack "$exchange/receipt.json" --json
af backlog outbox --json
```

A lost authority receipt is recovered by accepting the same signed submission again. Successful
acknowledgement removes the Report ID from the producer's pending view. Intake and transfer can
continue while the Ant or publisher is paused. The offline acceptance fixture exercises this
exchange with generated Minisign keys, including an accept retry and acknowledgement.

## Manually admit a daily batch

Use a disposable authority Store with a signed intake policy and accepted sample Reports.
Save a machine-local `backlog-batch-policy-v1.json` in the Project Store shown by
`af project show --json`. The policy uses the same Project ID and the receipt's
`body.authority_id`. For a manual accounting exercise, the sample Scenario and destination digests suffice.
For a prepared Scenario, bind the actual artifact and destination identities as shown below. The first tick pins the policy and timezone transition
table. Subsequent ticks require the same policy. The policy lives outside the checkout.

```json
{
  "schema": "af/backlog-batch-policy@1",
  "project_id": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "authority_id": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
  "scenario_id": "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
  "destination_id": "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
  "timezone": "America/New_York",
  "hour": 2,
  "minute": 30,
  "start_unix_ms": 1772782200000,
  "lateness_ms": 60000,
  "run_tokens": 5,
  "run_wall_ms": 30000,
  "run_dispatches": 1,
  "rolling_tokens": 5,
  "rolling_window_ms": 345600000,
  "publication_limit": 1,
  "evidence_access": "metadata_only",
  "hold_policy": "hold_all"
}
```

Replace the sample Project and authority digests before ticking. `evidence_ids` exposes
only evidence digests in the private batch audit. `allow_local_reservations` permits
reservation accounting for the pinned destination. Provider calls, ticket writes and
installation of a scheduler are outside this command's effects.

```sh
af backlog batch tick --at-unix-ms 1772782200000
af backlog batch audit
af backlog batch settle "$batch_id" "$report_id" needs-evidence
af backlog batch close "$batch_id" --at-unix-ms 1772782200000
```

Take the batch ID from `active_batch_id` and settle every ID in its `members` map before
closing. A batch with no members closes at admission. The atomically sampled `high_water`
is the last accepted Report sequence at admission, even if a tick arrives within the allowed
lateness window. `closed_high_water` advances only after every frozen member has a disposition.
The audit lists each accepted Report once in `intake` with its original sequence and digest.
Reports accepted after the cutoff first appear in `pending_member_ids`. Retrying the same tick
timestamp recovers the original occurrence and the current audit.

After close, queue an explicit reconsideration of an accounted Report with
`af backlog batch reconsider --item-id "$item_id" --report-id "$report_id" --eligible-unix-ms "$due"`.
Use `--evidence-report-id "$later_report_id"` to link a later accepted Report. A future
occurrence takes the request when it is due and the evidence falls within its cutoff.

Under `allow_local_reservations`, settle with `reserve-publication --item-id "$item_id"` to
exercise the batch cap. A conflicting item claim or an exhausted cap produces a durable
deferral. The deferred member and its item identity carry into the next admitted batch.
Keep the same batch ID after an interruption. While its members are unfinished, later due
ticks record a busy skip, and the batch can still be settled and closed. Missed or nonexistent
local times and rolling-budget refusals appear in `occurrences`. The per-run reservation
remains in the rolling window from close and is distinct from actual Provider spend.
[Daily batch decision](adr/2026-11-20-freeze-project-backlog-at-authority-cutoff.md)

## Prepare and run the portable daily Scenario

The [daily backlog package](../fixtures/project-backlog/scenario.json) uses
[`af/backlog-scenario-artifact@1`](../schemas/backlog-scenario-artifact-v1.json). Copy the whole package directory
to the receiving checkout and commit it. Configure a report policy and Provider bindings for
`backlog/daily-author` and `backlog/daily-verifier` in the receiver's Task catalog. The Pipeline
requires independent verification under that catalog's policy. Package paths and package pins
travel with the artifact. The receiver selects its own private Project, Task and Scenario Stores.

For a disposable daily policy, set `scenario_id` to `sha256:` followed by the SHA-256 of the
committed artifact file. Give the daily budget at least the limits declared in the package.
For a synthetic local destination, compute the policy's `destination_id` from the Project ID
as shown below. Set `evidence_access` to `evidence_ids` to admit digest-matched source evidence.
Set `hold_policy` to `allow_local_reservations` to authorize local delivery and set
`publication_limit` to the number of revisions the native sink may accept per batch.

```bash
project_id=$(af project show --json | jq -r .project_id)
python3 - "$project_id" <<'PYTHON'
import hashlib
import json
import sys

scope = json.dumps(["af/backlog-local-sink@1", sys.argv[1]], separators=(",", ":"))
print("sha256:" + hashlib.sha256(b"review.kernel/content-id/v1\0" + scope.encode()).hexdigest())
PYTHON
```
After admitting a nonempty batch, use its audit to write the reference. This example uses
`jq` and sets restrictive permissions for the captured batch metadata and receipts.

```bash
umask 077
af backlog batch audit > "$scratch/audit.json"
jq '{
  schema: "af/backlog-batch-reference@1",
  project_id,
  batch_id: .active_batch_id,
  policy_id: .pin_id,
  high_water: .batches[.active_batch_id].high_water
}' "$scratch/audit.json" > "$scratch/batch.json"

af scenario prepare --file fixtures/project-backlog/scenario.json \
  --task-state "$scratch/tasks" --inputs "$scratch/batch.json" \
  --json > "$scratch/prepared.json"
```

Prepare checks the reference against the active batch and policy in the receiving Project
Store. It collects at most 128 frozen members. The collected input contains accepted Report
identities, source revisions and the evidence digests allowed by policy. Raw Report text,
their attachments, late arrivals and local-only Reports remain outside the prepared bundle.

After reviewing the prepared template, grant it in the receiver's machine policy. In a fresh
disposable Store, construct a policy containing just that Task template grant. The builtin
backlog Actions use the installed Project policy. The synthetic destination needs no executable.

```bash
jq '.definitions["daily-backlog"].authority.task_templates | {
  schema: "af/scenario-policy@8",
  task_plans: [], task_templates: ., adapters: {}, decision_keys: {}, file_roots: []
}' "$scratch/prepared.json" > "$scratch/policy.json"
```

Validate with that policy and review the resulting revision. Set `pin` to the approved revision before
starting. Start and resume use the same Scenario Store.

```bash
af scenario validate --file "$scratch/prepared.json" \
  --authority "$scratch/policy.json" --state "$scratch/scenarios" --json
```

```bash
printf '{}\n' > "$scratch/empty.json"
af scenario start daily --file "$scratch/prepared.json" \
  --authority "$scratch/policy.json" --inputs "$scratch/empty.json" \
  --state "$scratch/scenarios" --confirm-pin "$pin" --json
af scenario resume daily --state "$scratch/scenarios" --json
```

The `assessment` Task receipt identifies the sealed Document, its independent verification
and the observed Provider charge. The private Document's Decisions section covers each frozen
member exactly once. Its checks bind the structured assessment to the current source, frozen
evidence and exact proposed public fields before independent verification.

The `settle-backlog` output identifies the Project transaction for the accepted assessment.
Verified and already-tracked decisions need a policy-permitted evidence digest for each member
that matches a cited file in the committed Snapshot. Record that complete file as an evidence
attachment to opt into this match. The attachment itself stays private. A missing or mismatched
match cannot produce a visible item or a sink delivery.

The `dispatch-backlog` output identifies the resolved delivery intents. Approved revisions
beyond the cap stay visible through `af backlog list` and can be admitted by a later batch on
the same source revision. The synthetic sink is the receiving Store's `backlog-local-sink/`
directory. Each delivery contains the item ID, revision ID and approved public fields.
A resume with a pending intent reconciles the same delivery within the Scenario's remaining
wall time and dispatch limits. The native graph closes the batch when admitted deliveries
have receipts. `af backlog show ID --private --json` shows each Report's typed decision.

The offline regression exercises signed intake and the normal prepare, validate, start and
resume commands with two fresh receivers and a deterministic Provider. It checks duplicate
grouping, distinct source defects, held and insufficient evidence and preservation of the
original receipt and sink delivery across interruption and resume.

```sh
cargo nextest run --workspace --locked --profile ci \
  -E 'binary_id(af::it) & test(portable_backlog_settles_and_dispatches)'
```

[Native settlement and delivery decision](adr/2026-11-20-settle-and-dispatch-verified-backlog-locally.md)

## Schedule a prepared batch with an Ant

This example uses Bash and `jq`. The batch policy tick freezes a batch and produces the batch
reference. The Ant runs that **prepared batch**. The package binds its source, members, and policy,
so an existing Ant never silently switches to a later batch. For each later cutoff, create a
fresh batch reference, prepare it, and register a new reviewed Ant binding for that batch.

Use the `prepared.json` and `policy.json` from preparation above. Set `start_ms` to a Unix time
in milliseconds at or before the chosen daily UTC slot and set `hour` and `minute` to that slot.
Use an operator-selected owner and local notification route.

```bash
jq -n --slurpfile scenario "$scratch/prepared.json" \
  --arg id "$ant_id" --arg owner "$owner" --arg route "$notification" \
  --argjson start "$start_ms" --argjson hour "$hour" --argjson minute "$minute" '
  $scenario[0] as $bundle |
  $bundle.definitions["daily-backlog"].limits as $limits |
  {
    schema: "af/ant@12", id: $id, scenario: $bundle, inputs: {},
    schedule: {kind: "daily", timezone: "UTC", hour: $hour, minute: $minute,
               start_unix_ms: $start},
    owner: $owner, notification: $route,
    execution: {overlap: "skip", missed: "skip", lateness_ms: 120000,
                per_run: $limits, rolling_tokens: 150000, rolling_window_ms: 86400000}
  }
' > "$scratch/ant.json"

af ant validate --file "$scratch/ant.json" --authority "$scratch/policy.json" \
  --state "$scratch/scenarios" --json
```

Review the validated grant and set `ant_pin` to the returned `revision`. Before the next due slot,
register and enable it. Tick at the due slot, or run the foreground scheduler on the same Store.
Use `show` to retrieve the occurrence and its Run ID. To drive ticks continuously,
run `af ant scheduler run --state "$scratch/scenarios"` in the foreground.

```bash
af ant add --file "$scratch/ant.json" --authority "$scratch/policy.json" \
  --state "$scratch/scenarios" --confirm-pin "$ant_pin" --json
af ant enable "$ant_id" --state "$scratch/scenarios" --json
af ant tick --state "$scratch/scenarios" --json
af ant show "$ant_id" --state "$scratch/scenarios" --json
```

`af ant pause "$ant_id" --state "$scratch/scenarios"` pauses future launches. An existing
Run can still dispatch on a tick or resume. For a maintenance pause use
`af ant scheduler stop --state "$scratch/scenarios"` where a foreground scheduler is active
and refrain from manually ticking or resuming. Use `af scenario cancel RUN_ID --reason REASON`
with the same state path when the existing Run itself must stop. Cancellation preserves receipts
and effects already committed.

## GitHub Issues destination

The same daily Scenario can dispatch to a receiver-owned GitHub Issues destination. The
prepared Action grants show `github-backlog:<project_id>:<destination_id>`. The receiver
rechecks that scope against the frozen batch and the current machine policy. Use one designated
receiver and Project Store for a destination.

Before the first tick pins the batch policy, place these private, owner-only regular files in
the Project Store reported by `af project show --json`.

- `backlog-github-destination-v1.json` pins the exact receiver configuration shown below.
  Use the lowercase canonical GitHub repository name and its positive numeric repository and
  publisher account IDs.
- `backlog-github-adapter` holds the trusted standalone executable. AF verifies the exact bytes
  and stages them in a temporary directory under the private Project Store. Use the checked-in
  [standalone Python adapter](../scripts/project-backlog-github.py), which runs with
  `/usr/bin/python3 -IS` and requires no modules from the checkout.
- `backlog-github-token` holds the adapter credential as a single token, optionally followed by a
  newline. Keep it in the Project Store and out of the caller's environment and checkout. AF
  passes it to the adapter as `AF_BACKLOG_GITHUB_TOKEN` with a cleared environment and the
  process supervisor's sanitized `PATH`. AF also passes `AF_BACKLOG_GITHUB_SCOPE`, a JSON object
  containing the Project and destination IDs, repository name and numeric repository and
  publisher IDs from validated receiver authority. The adapter requires stdin to match that scope
  before opening a connection.

```json
{
  "schema": "af/backlog-github-destination@1",
  "project_id": "sha256:<Project ID>",
  "repository": "example/backlog",
  "repository_id": 23,
  "publisher_id": 42,
  "executable_digest": "sha256:<content identity of executable bytes>"
}
```

Use the config's canonical content identity as the batch policy's `destination_id`. Set
`hold_policy` to `allow_local_reservations` to admit publication, and set `publication_limit`
to bound the revisions admitted per batch. The following prints the executable identity and,
after the config contains that identity, its destination pin.

```bash
store=$(af project show --json | jq -r .store)
test ! -e "$store/backlog-github-adapter" && test ! -L "$store/backlog-github-adapter" &&
  install -m 600 scripts/project-backlog-github.py "$store/backlog-github-adapter"
python3 - "$store" <<'PYTHON'
import hashlib
import json
import pathlib
import sys

store = pathlib.Path(sys.argv[1])
def identity(data):
    return "sha256:" + hashlib.sha256(b"review.kernel/content-id/v1\0" + data).hexdigest()

print(identity((store / "backlog-github-adapter").read_bytes()))
config = json.loads((store / "backlog-github-destination-v1.json").read_bytes())
print(identity(json.dumps(config, sort_keys=True, ensure_ascii=False,
                          separators=(",", ":")).encode("utf-8")))
PYTHON
```

Review the prepared Scenario grant before confirming its pin. Removing the receiver
configuration, executable or credential prevents a new adapter call. A missing or changed
batch policy, hold, cancellation or expired deadline prevents dispatch. Restoring the exact
authority permits reconciliation within the admitted bounds. The batch policy and its
destination pin are immutable within this journal.

### Separate operator accounts

Use a dedicated receiver operating-system account and private Project Store for ticket
publication. Grant its destination principal Issues write for the pinned repository only.
Transfer the private submissions with the signed exchange. For an external agent launcher,
run it under a producer account without the publisher's file access, keychain session, CLI
login, credential-helper socket or environment. Keep developer and model Provider accounts
available under their existing approved bindings. Move any operator-managed write token out of
the agent's launch environment and auth configuration. Keep any separate read-only issue
observer under a principal with Issues read, as in the
[read-only index runbook](issue-triage-runner.md#read-only-open-issue-index).

Shell-capable Task Workers start with ambient GitHub CLI variables removed and Git credential
helper lookup disabled. These defaults are process controls. Use filesystem and account
isolation for the receiver's token file. If the receiver executes a Worker that can read
arbitrary files under the receiver UID, leave the token file absent during assessment.
After the Run waits on the publication intent, provision the receiver token privately and
resume that same Run. Keep it inaccessible to Workers while the file exists. Confinement
must cover alternate access such as the OS keychain and any exported auth helper.
Offline capture needs neither the receiver token nor a Provider login.

### Executable protocol

One invocation reads one UTF-8 JSON request from stdin through EOF and writes one UTF-8 JSON
response on stdout. The Python adapter limits requests to 64 KiB and its own wall time to
24 seconds. AF passes no arguments, limits responses to 4 KiB and supervises the process
under the Scenario Action deadline and cancellation. The executable, credential and
config reads are bounded at 8 MiB, 1 KiB and 4 KiB respectively. Adapter stdout, stderr and
credentials are excluded from logs. An unsuccessful, interrupted or malformed exchange
leaves the intent pending.

The request fields and types are as follows. IDs described as digests are lowercase
`sha256:` strings with 64 hex digits. Numeric IDs and issue numbers are positive JSON integers up to 9,007,199,254,740,991.

| Field | Value |
| --- | --- |
| `schema` | `af/backlog-github-request@1` |
| `request_id` | Unique durable AF attempt digest |
| `operation` | `create` or `reconcile` |
| `project_id`, `destination_id`, `item_id`, `revision_id` | Exact AF scope digests |
| `repository`, `repository_id`, `publisher_id` | The configured lower-case GitHub repository, repository ID and authenticated publisher ID |
| `marker` | The exact line `<!-- af-backlog-item:<project_id>:<item_id> -->`, stable across revisions |
| `issue` | `null` or the known object `{"id":444,"number":1}`, with the immutable issue ID and repository-local issue number |
| `fields` | `{"title":"…","body":"…"}` for `create`; `null` for `reconcile` |

Before any write the adapter verifies the authenticated principal and resolves the exact
repository on GitHub.com, checking both its name and numeric ID. A create allows at most one
issue POST with the approved title and body followed by two newlines and the exact marker.
A reconcile with a known issue reads that issue and checks both of its IDs. Without a known
issue, reconciliation looks for the exact marker and original publisher in the exact
repository, including closed issues, up to ten pages of 100 issues. Ambiguous, conflicting,
or incompletely observed matches
cannot resolve the intent. Reconciliation uses reads only.

The response repeats the verified `request_id`, `repository`, `repository_id` and
`publisher_id`, sets `schema` to `af/backlog-github-response@1`, and includes one `outcome`:

| Outcome object | Meaning |
| --- | --- |
| `{"kind":"observed","issue":{"id":444,"number":1},"marker":"…","author_id":42,"state":"open","fields_id":"sha256:…"}` | One observed issue, its actual marker, original author, `open` or `closed` state, and current public field digest |
| `{"kind":"absent"}` | The lookup did not observe a match |
| `{"kind":"conflict"}` | Multiple matches or contradictory identity evidence |
| `{"kind":"unavailable"}` | A bounded lookup or response could not establish the outcome |

Compute `fields_id` over the canonical UTF-8 JSON array `[title,body]` using the same
content-identity function above. Remove the transport's trailing two newlines and marker
before hashing the observed body. Return only identities and state. AF verifies the echoed
scope, the original author and exact marker, and requires the confirmed create fields to
match their approved digest. Reconciliation retains any human edit or closure and records
whether the observed fields match the approved revision.

### Recovery and local view

The native Project publication journal commits the revision and decision intent before an
attempt, and commits the attempt before launching its adapter. Once any attempt exists,
an unresolved create only reconciles. An empty search, including delayed indexing, leaves
the Scenario waiting. Each revision intent has at most 16 adapter attempts, further bounded
by the Scenario. Existing item mappings allow read-only checks for later verified revisions.
The `publication_limit` counts admitted intents, including unresolved ones.

Publication rows in the Project Store's `backlog-processing` stream retain the destination,
item, revision, decision, intent, attempts, remote ID and receipt. The `remote_id` is
`<issue-id>:<issue-number>`. Its `provider_state_id` references a private CAS JSON record
[`af/backlog-github-state@1`](../schemas/backlog-github-state-v1.json) with `attempt_id`,
`operation` and `outcome`. A preflight
deferral can have a null attempt or operation. Outcomes include `attempt_prepared`,
`adapter_unavailable`, `authority_unavailable`, `transport_unavailable`,
`invalid_response`, `identity_conflict`, `not_observed`, `attempt_limit` and
`observed`. The recorded observation includes issue identity, open or closed state, public
fields digest and an `approved_fields_match` boolean. The normal `af backlog list` and
`af backlog show` commands read the verified native projection without invoking the adapter.

The offline proof starts the real receiver Scenario through a daily Ant occurrence. It runs
the checked-in adapter entrypoint with a fake HTTPS connection. From a fresh checkout with
the pinned Rust toolchain, nextest, Git and Python 3, run the following copyable fixture.
The fixture creates disposable Workspaces, Stores, fake Provider credentials and a fake
destination. Its `Private captures` and `Offline cutover proof` lines contain the actual
fixture IDs and accounting. It covers adapter crashes
before the write and after remote creation, delayed lookup, identity conflicts, revoked authority,
concurrent resume, human closure and edit, and reconciliation of the same issue ID on a later
verified revision.

```sh
python3 -I scripts/test-backlog-open-issues.py --example
cargo nextest run --workspace --locked --profile ci --success-output immediate \
  -E 'binary_id(af::it) & test(portable_backlog_github_)'
```

### Recorded offline dispatch evidence

In one fresh fixture Run, the signed exchange accepted seven Reports. Six were frozen for
the first daily occurrence. The Project recorded two verified items, zero already tracked,
two holds (`hold`, `mismatch`) and one insufficient Report (`unknown`). The `late` Report
arrived after the cutoff. Both `blue-a` and `blue-b` had durable local receipts from distinct
Workspaces before verification. They resolved to the same item and revision. `green` was
verified and accounted, with no publication intent under the one-item cap.

The fixture kept the receiver credential file absent while the assessment ran. The synthetic
Provider retained its approved auth directory, while its Git helper lookup could not retrieve
the synthetic write credential. Before independent verification the fake provider received zero
requests. Once the receiver credential was installed, the pinned adapter sent one POST.
The fake transport killed the adapter immediately after recording the issue. A fresh
`af scenario resume` reconciled it by reading. The later revision reused the same issue
and preserved its simulated human edit and closure.

| Identity | Value from the fixture |
| --- | --- |
| Daily Ant Run | `ant-ded9899c25b183f9e018a684f78412c108b7ff695334c785dde5df726d24b54e` |
| Frozen batch | `sha256:b17a267a52d2e30d3b9a7da97c9e649c151b3c928680b9c5ff2951cbf2b50047` |
| Verified item | `sha256:ffa2f0bfd4a31f9443ce41aebdcb412f96acd9cfe12d02cfa0fe91754fa0c5b7` |
| Accepted revision | `sha256:e9742c9a6e953313e40611ec8a91394efc74220975e2b7c4e518183bb8c78483` |
| Publication intent | `sha256:64647b8387b0bd7510ae0527496a6bf6bb4a6f93d239f844293b5642ca0480d8` |
| Publication receipt | `sha256:1c44c5fa4b23accae74b388a66202c01ee8e8dd2106f1a6e1ecfe648134e4137` |
| Fake issue ID and number | `444:1` |
| Independent verification receipt | `sha256:a8c035a12b74e8186fb33d201b0935ef2d659e5e0aad281f3f4834c5f2ab3f77` |
| Deferred verified item | `sha256:486d6613e653de9631112447b285ae95472071da9a126454f5a817a3e661c4ea` |

The assessment charged 12 synthetic Provider tokens. The private `unknown` Report retained
its reproduction text and `needs_evidence` disposition. Its reproduction appeared in neither
the assessment input nor the outgoing ticket. Fixture identities change with each fresh
repository and batch.

The index example observes two existing fake provider issues and refuses an incomplete read.
Use a read-only index to nominate duplicate candidates and verify an exact issue identity
before linking it. The index does not import issues into the Project Store. The packaged
daily Scenario in this fixture takes the local frozen batch reference. The index example
is separate read-only evidence and the fake publication leaves existing remote issues alone.

[GitHub publication decision](adr/2026-11-20-coordinate-github-backlog-publication.md)
