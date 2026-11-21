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

Every JSON result has `schema = "af/backlog@2"`, a `view`, and a `data` object. The two capture
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
defined by [`backlog-v2.json`](../schemas/backlog-v2.json) and
[`backlog-record-v2.json`](../schemas/backlog-record-v2.json).

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
Save a machine-local `backlog-batch-policy-v2.json` in the Project Store shown by
`af project show --json`. The policy uses the same Project ID and the receipt's
`body.authority_id`. For a manual accounting exercise, the sample Scenario and destination digests suffice.
For a prepared Scenario, bind the actual artifact and destination identities as shown below.
The first tick pins the policy and timezone transition table. A later tick can pin a changed policy for the same authority after the active batch
closes. Finish an active batch under its original policy before installing the replacement.
The policy lives outside the checkout.

```json
{
  "schema": "af/backlog-batch-policy@2",
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
closing. A batch closes at admission when it has neither assessment members nor pending delivery. The atomically sampled `high_water`
is the last accepted Report sequence at admission, even if a tick arrives within the allowed
lateness window. `closed_high_water` advances only after every frozen member has a disposition.
The audit lists each accepted Report once in `intake` with its original sequence and digest.
Each batch takes at most 64 members, leaving room for a distinct repository citation per
member, and bounds their encoded size in the portable Task template. Excess work stays queued across restarts.
A deferred member queues behind work already waiting and ahead of later arrivals. The audit's
`pending_member_ids` also includes arrivals after the cutoff.
Retrying the same tick timestamp recovers the original occurrence and the current audit.

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
[Admission and policy decision](adr/2026-11-20-bound-backlog-assessment-and-repin-between-batches.md)

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
as shown below. Set `evidence_access` to `private_report` to admit private facts, reproduction and bounded
evidence excerpts to both assessment Workers. Reserve that setting for approved private Providers.
`metadata_only` and `evidence_ids` withhold report prose and cannot support a positive disposition.
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
After admitting an active batch, use its audit to write the reference. This example uses
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
Store. It collects at most 64 frozen members. The collected input contains accepted Report
identities, source revisions and evidence allowed by policy. With `private_report` the
private source includes the observed fact, reproduction, attachment excerpts of at most 256
UTF-8 bytes each, and a bounded set of verified tracked revisions with provenance. An oversized
member gets bounded, labeled excerpts of report facts and reproduction so it can still be
assessed. Full attachments, late arrivals and local-only Reports stay in the inbox. Keep prepared
bundles private.
A local duplicate outcome names the exact matched item and revision in `local_match`. If the
tracked view is truncated, exact matches in the supplied view can still settle as already
tracked. Hold otherwise verified decisions until equivalence can be ruled out.

Revisions above the publication cap appear in `pending_deliveries`. A later due batch can
dispatch them even with no assessment members. Dispatch checks the exact approved revision
and decision against the current source, pinned policy and destination. Changed source or a
superseding revision records `requires_reverification` under `delivery_resolutions`. A tick
that pins a changed policy moves pending deliveries to the same outcome under the Project
Store lock. Use explicit reconsideration to assess that item again. Audit records retain the
original policy ID on each batch and rolling reservations retain their original token amounts.

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

The offline regressions exercise signed intake and the normal prepare, validate, start and
resume commands with fresh receivers and a deterministic Provider. They check duplicate
grouping, distinct source defects, held and insufficient evidence, bounded admission and
policy rotation. They also check receipt and sink delivery across interruption and resume.

```sh
cargo nextest run --workspace --locked --profile ci \
  -E 'binary_id(af::it) & test(portable_backlog_)'
```

[Native settlement and delivery decision](adr/2026-11-20-settle-and-dispatch-verified-backlog-locally.md)

## Enroll once for recurring daily processing

On the receiver, configure the signed intake authority, the approved Provider registry and
the committed backlog Scenario packages. Install the [daily batch policy](#manually-admit-a-daily-batch)
and choose its destination before enrollment. The synthetic local sink, GitHub Issues, Markdown
and Jira use the same scheduler. Set `scenario_id` to the digest of the reviewed committed artifact
bytes. Set the daily time, timezone, start, lateness and spending limits in the batch policy.
Schedule the first cutoff after enrollment and use a checkout at the reviewed commit.
Keep all state paths and any Provider alias bindings local to the receiver.
Use `af help ant backlog` for the command list and `af help ant backlog --json` for the
options and effects of each command.

### One-time setup

Run this from the registered receiving checkout. Change the owner label to your own and the
artifact path if you installed the reviewed package elsewhere. Pass `--bindings /private/bindings.toml`
to both commands if this receiver uses an explicit Provider alias file.

```bash
umask 077
receiver=$(pwd -P)
project_id=$(af project show --json | jq -er .project_id)
private="${XDG_STATE_HOME:-$HOME/.local/state}/af/backlog-receiver/${project_id#sha256:}"
mkdir -p "$private"
scenario_state="$private/scenarios"
task_state="$private/tasks"
artifact=fixtures/project-backlog/scenario.json
owner=backlog-owner
scope=(--repo "$receiver" --file "$artifact" --owner "$owner"
       --task-state "$task_state" --state "$scenario_state")

af ant backlog validate "${scope[@]}" --json > "$private/review.json"
jq . "$private/review.json"
```

Review the Project, checkout commit, artifact, intake and batch policy pins, destination, native
Task authority and limits. Confirmation authorizes fresh private assessments and publication
within that scope for future Project batches. The review spends no model tokens.
`validate` records the native template pin in the Task Store. `enroll` rechecks the scope,
prepares the Scenario Store and records the confirmed enrollment in the Project Store.
The foreground scheduler or an explicit tick drives the enrolled batches.

```bash
pin=$(jq -er .revision "$private/review.json")
af ant backlog enroll "${scope[@]}" --confirm-pin "$pin" --json
```

### Opt in to recurring GitHub issue indexing

Complete the GitHub destination setup first. On each receiving machine, package the
[read-only observer](issue-triage-runner.md#read-only-open-issue-index) from the reviewed
Afactory source corresponding to the installed release. Use a trusted Python 3 on the
scheduler's `PATH`. Set `af_source` to the absolute path of that source. Run from the
receiving checkout after setting the `scope` array above, before validating and enrolling.

```bash
umask 077
store=$(af project show --json | jq -er .store)
build=$(mktemp -d) || exit 1
mkdir -p "$build/observer/performance"
cp "$af_source/scripts/backlog-open-issues.py" "$build/observer/__main__.py"
cp -R "$af_source/scripts/performance/perflab" "$build/observer/performance/"
python3 -I -m zipapp "$build/observer" -p '/usr/bin/env python3' -o "$build/backlog-github-observer"
install -m 700 "$build/backlog-github-observer" "$store/backlog-github-observer"
rm -r "$build"
```

Place a fine-grained token for the exact configured repository with Issues read permission
in the owner-only `$store/backlog-github-read-token`. Use a separate credential from the
publisher's `backlog-github-token`. The reader only receives the read credential, and
the Tasks only receive the bounded observation. Use the same index flag for validation and
confirmation, and review the `github_observer` executable digest in the preview.

```bash
scope+=(--github-issue-index)
af ant backlog validate "${scope[@]}" --json > "$private/review.json"
jq . "$private/review.json"
pin=$(jq -er .revision "$private/review.json")
af ant backlog enroll "${scope[@]}" --confirm-pin "$pin" --json
```

For an existing enrollment, settle outstanding work and pause it in its original Scenario
Store before confirming the replacement. Enrollment envelopes with schema
`af/backlog-schedule@1` must be retired after settling work with the release that wrote
them. Archive the old private enrollment file `backlog-schedule-v1.json` and validate and
enroll the new scope with this release. The private file slot now holds the
`af/backlog-schedule@2` envelope.

For each frozen batch that needs a Run, the scheduler checks the configured GitHub repository
name and stable numeric ID, then validates the complete page walk. The accepted
observation must fit the [existing Project limits](#use-a-private-provider-index-for-daily-duplicate-checks).
Before preparation the scheduler retains the exact bytes in the private Project Store,
addressed by digest, and records one immutable pin for that batch. The two assessment Workers
use the same observation. A pre-existing issue independently verified as an exact match
settles as already tracked without a publication intent. Local, Markdown and Jira destinations
need no observer. GitHub enrollment without this flag uses only local Project evidence.

### Continuous runtime

Run the normal foreground scheduler with the same Scenario Store and receiver Provider registry.
The Project Store records each occurrence, the accepted intake high-water mark and exact
membership in one transaction. Intake committed after that transaction waits for a later batch.
The scheduler prepares a new native Scenario and Task grant for each batch that has work. The
assessment must pass independent review before settlement or delivery. Empty batches with no
pending delivery close without sending private data to a Provider.

```bash
af ant scheduler run --state "$scenario_state"
```

The daily policy controls missed, busy and over-budget slots. Skipped slots preserve intake
for later admission. An unfinished batch keeps its membership and reservation across restarts.
A restarted scheduler reconciles the same Scenario Run and delivery intents. The schedule
uses the batch policy's pinned timezone rules, including daylight-saving gaps and folds.
Rolling reservations are measured from batch closure. A daily slot can be too early to admit
work if the previous batch closed less than a rolling window ago. Size the reviewed rolling
allowance for the number of batches that can overlap that window.

### Inspection and recovery

```bash
af ant scheduler status --state "$scenario_state" --json
af ant backlog show --repo "$receiver" --json
af backlog batch audit
af scenario list --state "$scenario_state" --json
af ant backlog pause --repo "$receiver"
af ant backlog resume --repo "$receiver"
```

Pause suppresses future cutoffs while already-frozen work can settle. Resume records any due
paused slots before enabling admission. To stop the foreground process, use
`af ant scheduler stop --state "$scenario_state"`. Start it again with the same private Stores
and receiver inputs. `af ant tick --state "$scenario_state" --json` runs one scheduler cycle
and reports refusals and busy Runs. Inspect a recorded Run with
`af scenario show RUN_ID --state "$scenario_state" --json`.

If a pending delivery cannot be reconciled, restore the authorized destination configuration,
adapter or credential and resume the same Run. Restore the enrolled checkout commit, Provider
registry, bindings or policy if they drift during a batch. To intentionally change the reviewed
scope, settle the active batch and Run, pause enrollment, and repeat validation and
confirmation in the original Scenario Store. For a failed or cancelled terminal Run, inspect the
batch journal and retained Task and delivery receipts before making an explicit recovery
decision. The scheduler reports the blocked batch and retains its membership.

For an indexed batch, a refused observer read leaves the frozen batch private. Check
`af ant tick --state "$scenario_state" --json` and `af backlog batch audit`.
For an unavailable or incomplete read, restore the Issues-read service or rotate the read token
in its owner-only file, then retry with the same scheduler. For a scope mismatch, check the
reviewed GitHub repository name and ID and the token's repository permissions. An oversized
walk stays blocked until a complete observation fits the input bounds. If a retained pin or its
observation is unavailable, restore the exact private files from backup. Preserve
`backlog-provider-index-<batch digest>.json` and its corresponding
`backlog-open-issues-<observation digest>.json`; replacing a bound observation would change
the batch's evidence. Once bound, reconciling the same Run does not contact the observer.

### Optional manual mode

For a Project without recurring enrollment, [admit the batch manually](#manually-admit-a-daily-batch)
and use the [preparation and execution commands](#prepare-and-run-the-portable-daily-scenario).
The optional `backlog-provider-index-v1.json` pin belongs to manual preparation. Archive that
pin outside the Project Store before enrolling a recurring schedule. The scheduler preserves any
manual pin it encounters and reports a conflict.

### Reproduce the scheduler evidence

```sh
cargo nextest run --workspace --locked --profile ci \
  -E '(binary_id(af::it) & test(portable_backlog_)) | (binary_id(af-contracts::schema_parity) & test(backlog_batch_journal))'
python3 -I scripts/contract-inventory.py
```

The isolated fixture runs one cutoff through the foreground scheduler and advances the next
daily slot through the same tick path. It checks late intake, independent reviews, distinct
Task and Scenario pins, scope drift, recovery without duplicate delivery, and paused and missed
cutoffs. The remaining portable backlog tests exercise the four delivery adapters with private
fixtures.

[Recurring schedule decision](adr/2026-11-21-enroll-recurring-project-backlog-processing.md)

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

In an earlier fixture Run with digest-only evidence access, the signed exchange accepted seven
Reports. Six were frozen for
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
the assessment input nor the outgoing ticket. In the current private-report fixture, the
reproduction is part of the private assessment input. The fake destination still receives only
approved public fields. Fixture identities change with each fresh repository and batch.

[GitHub publication decision](adr/2026-11-20-coordinate-github-backlog-publication.md)

### Use a private provider index for daily duplicate checks

For manual batches, run the
[Issues-read observer](issue-triage-runner.md#read-only-open-issue-index) against the exact
GitHub destination. Use a fresh observation for each batch. Keep the manual observer's read
credential outside the Project and Workers. The receiver admits at most 128 issues and 64 KiB of
observation JSON. The entire accepted walk must also fit in the encoded portable Task input
alongside the frozen members. Preparation refuses an observation that cannot fit intact.
The observer marks truncated issue bodies individually. Such a body cannot support
an exact provider match, and a batch with any truncated body cannot propose public fields.

After freezing the batch and before running `af scenario prepare`, use the `batch.json`
constructed above and the installed GitHub batch policy. Set `observation` to the private
output of the read-only observer. Run this from the checkout whose committed HEAD will be
assessed. The batch pin selects exact observation bytes and must be owned by the receiving
user with no group or other permissions.

```bash
store=$(af project show --json | jq -r .store)
source_revision=$(git rev-parse --verify HEAD)
python3 - "$store" "$scratch/batch.json" "$source_revision" "$observation" <<'PYTHON'
import hashlib
import json
import os
import pathlib
import sys
import tempfile

store, reference, commit, observation = sys.argv[1:]
store = pathlib.Path(store)
batch = json.loads(pathlib.Path(reference).read_text())
policy = json.loads((store / "backlog-batch-policy-v2.json").read_text())
with open(observation, "rb") as file:
    raw = file.read(65537)
if len(raw) > 65536:
    raise SystemExit("Observation exceeds the Project collection bound")
digest = hashlib.sha256(b"review.kernel/content-id/v1\0" + raw).hexdigest()
output = store / f"backlog-open-issues-{digest}.json"
if not output.exists():
    with tempfile.NamedTemporaryFile(dir=store) as file:
        file.write(raw)
        file.flush()
        os.fsync(file.fileno())
        os.link(file.name, output)
pin = dict(schema="af/backlog-provider-index-pin@1", project_id=batch["project_id"],
           batch_id=batch["batch_id"], source_revision=commit,
           destination_id=policy["destination_id"], observation_id="sha256:" + digest)
with tempfile.NamedTemporaryFile(mode="w", dir=store, delete=False) as file:
    json.dump(pin, file)
    file.flush()
    os.fsync(file.fileno())
os.replace(file.name, store / "backlog-provider-index-v1.json")
PYTHON
```

Continue with manual Scenario preparation and exact pin approval. Keep the pin and its bytes
in place through first settlement. A mismatched pin, wrong repository name or numeric ID,
missing bytes, unsafe file, invalid JSON or incomplete page walk prevents acceptance. If the
pin or observation changes before settlement, restore the captured scope and bytes for the
same Run. Once settled, the recorded native receipt replays without rereading the observer's
files. Private `af backlog show ID --private --json` output traces the observation digest and
the stable provider issue ID and display number beside the disposition. The Scenario retains
the private assessment and its independent verification. Never share the prepared bundle or
assessment as public output.

With no `backlog-provider-index-v1.json` installed, collection uses the existing local Project
item view. Hold a suspected duplicate whose identity remains uncertain. A complete live
walk can miss concurrent provider changes and does not prove that no duplicate exists.
The Scenario uses only the pinned observation and committed source. The operator can obtain
a fresh read-only export and prepare a new batch assessment when current evidence is needed.
External observations do not import tickets, add intake Reports or make existing tickets
publication targets.

Run the focused offline proof from a fresh checkout with no provider credentials:

```sh
cargo nextest run --workspace --locked --profile ci --success-output immediate \
  -E 'binary_id(af::it) & test(portable_backlog_github_index_prevents_a_duplicate)'
```

The synthetic read-only observer returns issues 1073 (#73) and 1074 (#74) with the same title.
Issue 1073 gives the precise behavior of `blue-boundary.py` at inputs 3 and 4. The other issue
describes a string input failure. The daily Ant records the exact issue as already tracked
for both Blue Reports and keeps a third Report with missing evidence private. The fake
publisher receives one POST for the distinct, verified Green defect. Replaying the settled Run
after rotating the observer input does not add another POST. The fixture prints the actual
Ant Run and observation identities.

## Markdown and Jira destinations

Use the same signed intake, batch policy, portable Scenario and independent verification steps.
Configure one destination in the receiving Project Store before its batch policy is pinned.
The prepared grant names the Project and exact destination digest. Both destinations receive
only the approved title and body and the metadata needed to bind the publication.

### Markdown

Markdown appends independently verified items to one existing UTF-8 Markdown file selected
by the receiving operator. The file can already contain prose and headings. AF keeps its
existing bytes and appends an entry with a stable Project and item marker, the approved title and
body, and a closing marker. Raw reports, evidence files and credentials stay private. Later
revisions observe the current entry and record whether its text matches the new approved text.
To preserve human content, AF leaves existing managed entries in place.

Use a dedicated, owner-writable regular `.md` file on a local filesystem with append and file-lock
semantics. The file may be readable by other users. The receiver must own the file and its
containing directory. Group or public write access, symlinks and hard links are refused. Keep the AF markers and the text between them intact. Edit surrounding
prose between deliveries. Writers that edit concurrently should take an exclusive file lock and
keep the pathname in place until they finish. AF takes a non-blocking lock, checks the pathname,
file identity, contents, size and both file timestamps (status change and modification) at
nanosecond precision, and appends without replacing the document. An edit or replacement
that races the append can leave publication pending until the receiver can observe the exact
entry. A short or ambiguous append is never repeated. A confirmed prewrite conflict can retry
within the same Run's limits once the operator clears the obstruction.

The `unversioned_file` scope refuses files under Git checkouts, linked worktrees, submodules and
bare repositories. To track the file in Git, use a separately authorized repository publication
workflow that binds the repository, ref, path and expected revision. The Markdown receiver has
no repository write authority.

From the registered receiver checkout, set `backlog_file` to the existing file's absolute path
using its real, symlink-free directory names. Create the private configuration and use the shared
pin step below to compute its destination ID.

```bash
project=$(af project show --json)
store=$(jq -r .store <<< "$project")
project_id=$(jq -r .project_id <<< "$project")
backlog_file=/absolute/path/to/team-backlog.md
(
  umask 077
  set -o noclobber
  jq -n --arg project_id "$project_id" --arg path "$backlog_file" \
    '{schema:"af/backlog-markdown-destination@2", $project_id, scope:"unversioned_file", $path}' \
    > "$store/backlog-markdown-destination-v2.json"
)
```

### Jira Cloud

Grant a dedicated Jira account Browse Projects and Create Issues for the pinned project.
Use the canonical `<site>.atlassian.net` host, project and issue type IDs from Jira, and the
publisher's `accountId`. Jira must allow Summary, Description and Labels on that issue type's
create screen. The adapter maps the approved body to plain-text ADF paragraphs and writes a
stable AF label. It uses the issue's stable numeric ID for later reads, records the current
display key and regards status category Done as closed. The transport follows the Jira Cloud
REST v3 [Issues](https://developer.atlassian.com/cloud/jira/platform/rest/v3/api-group-issues/)
and [enhanced issue search](https://developer.atlassian.com/cloud/jira/platform/rest/v3/api-group-issue-search/) contracts.

Set `jira_site`, `jira_project_id`, `issue_type_id` and `publisher_account_id` before running
the following from the registered receiver checkout. Run one receiver per designated destination.

```bash
project=$(af project show --json)
store=$(jq -r .store <<< "$project")
project_id=$(jq -r .project_id <<< "$project")
test ! -e "$store/backlog-jira-adapter" && test ! -L "$store/backlog-jira-adapter" &&
  install -m 600 scripts/project-backlog-jira.py "$store/backlog-jira-adapter"

executable_digest=$(python3 - "$store/backlog-jira-adapter" <<'PYTHON'
import hashlib
from pathlib import Path
import sys

payload = Path(sys.argv[1]).read_bytes()
print("sha256:" + hashlib.sha256(b"review.kernel/content-id/v1\0" + payload).hexdigest())
PYTHON
)
(
  umask 077
  set -o noclobber
  jq -n --arg project_id "$project_id" --arg site "$jira_site" \
    --arg jira_project_id "$jira_project_id" --arg issue_type_id "$issue_type_id" \
    --arg publisher_account_id "$publisher_account_id" --arg executable_digest "$executable_digest" \
    '{schema:"af/backlog-jira-destination@1", $project_id, $site, $jira_project_id,
      $issue_type_id, $publisher_account_id, $executable_digest}' \
    > "$store/backlog-jira-destination-v1.json"
)
```

Provision `backlog-jira-credential` as an owner-only file containing a JSON object with
`email` and `api_token`. Use a Jira Cloud token that supports HTTP Basic authentication at
the pinned site. Follow the receiver account and Worker isolation procedure above. The adapter
authenticates the account and project before contacting the issue create endpoint and talks
only to the configured HTTPS host. Keep the credential absent during assessment when Workers
share the receiver UID. When provisioning is complete, resume the already-admitted Scenario.

### Pin and recover a configured destination

Set `destination_config` to the configuration created above, then copy its digest into the batch policy's
`destination_id` before ticking. Review the `<kind>-backlog:<project_id>:<destination_id>`
grant before confirming the prepared Scenario.

```bash
# For Jira use "$store/backlog-jira-destination-v1.json".
destination_config="$store/backlog-markdown-destination-v2.json"
python3 - "$destination_config" <<'PYTHON'
import hashlib
import json
import sys

with open(sys.argv[1], encoding="utf-8") as config:
    payload = json.dumps(json.load(config), sort_keys=True, ensure_ascii=False,
                         separators=(",", ":")).encode("utf-8")
print("sha256:" + hashlib.sha256(b"review.kernel/content-id/v1\0" + payload).hexdigest())
PYTHON
```

An interrupted attempt leaves the batch waiting if the receiver cannot confirm the result.
Restore the exact configuration and adapter bytes if they were removed. Inspect the existing
Markdown marker or Jira label, issue scope and author before resuming. A missing Jira search
result can reflect indexing delay. A conflicting Markdown entry should be preserved while the
operator investigates its origin. Resume with the original Run ID and Scenario Store.

```bash
af backlog batch audit
af scenario resume "$run_id" --state "$scratch/scenarios" --json
af backlog list --json
```

Private publication rows reference [configured destination state](../schemas/backlog-configured-state-v2.json).
For Markdown, `remote_id` binds the managed item, first intent and content digest.
For Jira, `remote_id` is `jira:<issue-id>`
and the private observation includes the display key. An observed digest mismatch records
the discrepancy without replacing text or reopening an issue.

### Offline checks for all four destinations

This command exercises the CLI and portable Ant with isolated Stores and deterministic
Providers. GitHub and Jira use fake transports. The Markdown fixture runs with a disposable Project Store, recovers an append after a lost
receipt and verifies two entries in the configured file. It also checks that the receiver
preserves existing prose and leaves damaged or duplicate markers pending.

```sh
cargo nextest run --workspace --locked --profile ci \
  -E 'binary_id(af::it) & test(/^scenario::portable::portable_backlog/)'
python3 -I scripts/test-project-backlog-github.py
python3 -I scripts/test-project-backlog-jira.py
```

[Markdown and Jira publication decision](adr/2026-11-21-bind-markdown-and-jira-backlog-destinations.md)
