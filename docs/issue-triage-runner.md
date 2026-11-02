# Issue triage on claw-3

`scripts/backlog-triage.py` implements [the accepted triage design](design/issue-triage.md)
§6.2–§7. It defaults to dry-run. The units in
[`fixtures/issue-triage/service/`](../fixtures/issue-triage/service/) are staged installation
examples: this change installs nothing, runs no live triage or backfill, releases no held
issue and migrates no plugin. Hermetic tests establish coordinator behavior, not live GitHub,
Provider, Docker, systemd or claw-3 coverage.

## Installation prerequisites

Use the reviewed control checkout at `/opt/af-perf/control`, Python 3.9 or newer, the pinned af
release, and the owner's already reviewed `backlog/triage` packages and report policy. The
[package README](../fixtures/issue-triage/README.md) describes their installation and catalog
check. Do not regenerate authority, widen a budget or add a remote check to make admission
pass. `codex-main` and `claude-main` must resolve to the intended registered personal logins
on claw-3; use the installation's reviewed bindings, never a token in a Task file.

Provision a local Git repository at `/opt/af-triage/repo` whose `refs/heads/main` is refreshed
by the operator's trusted source-refresh process. The runner never fetches, pulls, checks out,
commits or pushes. It captures that commit with `af task plan --authority COMMIT`, verifies the
Task Snapshot's origin and the captured sources, and confirms the exact resulting plan. The
repository's working tree is not evidence. Excerpts refuse blobs larger than 2 MiB as missing
evidence before reading their contents; binary/nontext blobs, unsafe paths, out-of-range lines
and excerpt count/byte limits also leave recorded refusals. Each Task keeps the accepted 150,000 tokens,
1,200,000 ms and eight Attempts, including both admissions and retry reserves. Neither the
runner nor its tests change the triagers or the check's interpretation of needs-info.

Create `/var/lib/af-triage` as a real directory, mode 0700, owned by the service account
(`af-perf-exec` in the examples). It must have no symlink component. Keep its journal and its
`task-store/` together and private. Back them up together while both timers are stopped and
no runner holds the journal lock. The af Store is a child of the state root, never the root
itself; af's storage cleanup cannot own the coordinator journal. State is bounded at 32 MiB;
capacity exhaustion defers new work and needs operator inspection, not automatic deletion of
unresolved generations or reservations.

The runner needs a distinct GitHub principal from the owner, because label events must
distinguish owner overrides from runner writes. Provision a fine-grained token scoped to this
repository, with issues read/write and pull requests read, and no contents write or
administration permission. Store it in `/etc/af-triage/github-token`, readable only by the
service account or root (0600). The staged units use systemd `LoadCredential` to hand it
privately to the service without widening file permissions. The API's authenticated `/user` login must match `--principal`; a
mismatch refuses work, and the principal is rechecked immediately before each public
mutation. Token values never belong in argv, environment files, journals, comments
or logs. The runner uses the nightly's bounded API transport; it never invokes gh or a remote
helper, follows a redirect, or edits an issue body or state.

Use the nightly's existing owner Telegram channel and token file. The staged units use
`LoadCredential=triage-telegram:/etc/af-nightly/telegram-token` so the service manager hands
that same secret privately to the triage account; do not make the original file group- or
world-readable or provision another bot/channel. Both token arguments name private files
below systemd's `CREDENTIALS_DIRECTORY`, never token values. Put only these non-secret settings in
`/etc/af-triage/runner.env`:

```text
AF_TRIAGE_PRINCIPAL=<dedicated-runner-login>
AF_TRIAGE_TIMEZONE=Europe/Moscow
AF_TRIAGE_CHAT_ID=<existing-nightly-owner-destination>
```

Confirm the installed claw-3 timezone and use its IANA name; the example freezes Europe/Moscow.
The journal refuses a different timezone, repository, owner or runner principal on reuse. A security hold
stores the existing `af/issue-triage-hold@1` privately, sends only an issue number and a request
to inspect that journal through the existing nightly notifier, and makes no public label,
comment or needs-info effect. Failed notification retains the hold and is retried privately
on discovery. Dry-run sends no notification. A model hold requires an owner `triage/held`
removal event later than the private hold record to release it; removing some other label does not release it. Existing security
labels still block release. This runbook authorizes no release of #271, #298 or #390.

## One shared lease before enabling timers

Install and verify ADR-0240's root-owned `/etc/af-perf-machine-lock.json` inode anchor as in the
[executor guide](performance-local-executor.md). All services must see the same system
`/tmp/af-perf-machine.lock` inode and sibling `.fence`, with `PrivateTmp=no` and no private
mount namespace. Exclude both from age-based temporary cleanup. Never replace or unlink the
lock. Busy, absent-anchor, changed-inode or fenced ownership defers before Task planning.

The existing `scripts/nightly/build.py` takes no machine lock. Before enabling either triage
timer, install the staged `nightly-lease.conf` as the build service's drop-in. Its fixed
`backlog-triage.py nightly-build --config /etc/af-nightly/nightly.toml` entrypoint calls the
existing nightly builder in process with its existing configuration, plan, outputs and
handoff. It adds the same anchored nonblocking lease and durable fence; its host adapter tracks
only containers it reserved by the builder's fixed names, refuses existing names, and confirms
owned container/process cleanup before clearing the fence. It also requires the unified
`af-nightly-build.service` cgroup (including descendants) to contain only the wrapper itself,
so native mirror/export helpers cannot outlive exclusion. Missing or uncertain cgroup evidence
keeps the fence; the staged wrapper is a systemd entrypoint, not a standalone build command. It is not an arbitrary command
wrapper. No existing nightly or performance file is changed by this rollout.

Have the operator inspect the effective unit definitions and inode on claw-3, and prove both
directions of contention with the timers stopped: a held nightly lease defers triage, and a
held triage lease defers the build. A deferred nightly build does not start measurement; the
operator starts the build service again after the holder finishes. The measurement executor
already uses the same anchored lease. Installing timers without the nightly drop-in leaves a
race and is not a supported installation. No claim is made that this wiring has been deployed
or proved on claw-3 by the hermetic tests.

## First week: half-hour dry-run

After reviewing the above prerequisites, install the staged `af-backlog-triage.service` and
`.timer`, `af-backlog-sweep.service` and `.timer`. The units run under the same account as the
registered Providers, with a private umask and control-group cleanup. Do not enable a second
copy from cron. systemd does not overlap a running oneshot with its next tick; the shared
nonblocking lease and private journal lock also reject overlap across invocations.

The services require `ConditionHost=claw-3`. The triage timer runs at minute 00 and 30 of every hour. The sweep runs Sunday at 12:15 UTC.
Both defaults are dry-run. For seven full days, keep `--apply` absent, capture stdout in private
service logs, and review the exact proposed label set and canonical comment. Dry-run still
runs bounded report Tasks and reserves their spend; “dry” means zero GitHub mutations and no
owner notification, not zero model calls. Do not share dry-run logs as public reports: issue
text and proposed classifications remain unreviewed evidence.

A tick admits at most four distinct issues and has a 90-minute wall, including API discovery
and publication. Every generation is claimed before planning. Every Task reserves its entire
150k allowance durably before planning/admission. The 2M daily cap counts settled usage for the
configured local day, all outstanding reservations (including earlier days), and the new
reservation. A settlement spanning midnight is conservatively counted on both its reservation
day and its settlement day, so late accounting cannot open an uncharged slot after midnight. Only terminal `af task show --json` cumulative usage settles spend. Unknown usage,
a lost admission or unavailable receipts keeps the reservation and produces no public result.
No new Task is started to recover an uncertain launch. Checked report, evaluation, source and
receipt identities must agree before the existing check can merge them.

The weekly sweep calls no Task or Provider. Only an explicit completed-fix citation on captured
main or a merged, reachable closing PR can propose `triage/maybe-fixed`; bare, negated,
partial or future mentions go into `owner_review` in the private journal. The sweep preserves
needs-info, disputed and held states and never closes an issue. It also defers issues with
uncertain Tasks, unresolved publication intents or conflicting owner labels, including reminders;
a durable dry-run proposal remains unresolved after enabling `--apply`, until publication or
explicit supersession. A temporary private hold keeps the proposal unresolved after release.
On recovery, the ordinary tick keeps the original proposal fenced until a fresh checked
replacement successfully publishes; the durable handoff records that publication and marks
the superseded proposal stale atomically. Completed or explicitly aborted label/comment effects
do not block that replacement. An unresolved write intent still requires reconciliation and
blocks both replacement publication and auxiliary writes.
A token-free sweep cannot bypass a publication hold. A needs-info generation gets
one reminder after 30 days without a reply, deduplicated by the recorded request time.
An existing request can use the trusted owner/runner label-event time; absent a known request
time, the sweep never guesses its age.

## Recovery and publication

Stop both timers before inspecting or recovering state. Read `journal.json` as
`af.issue-triage-runner/1`, checked against
[`schemas/issue-triage-runner-v1.json`](../schemas/issue-triage-runner-v1.json). Older or malformed
shapes fail closed; do not migrate or silently reset them. Preserve the original state for
inspection. In particular, deleting the journal loses comment authority and the spend ledger.
The runner refuses an unjournaled existing canonical comment rather than making a second one.

The inclusive updated-issue cursor advances in the same atomic fsynced journal write as event
enqueueing. Body hashes, `/triage` and size-wrong comment ids, needs-info reply ids, owner
namespace label events and manual pending events are consumed once. A stale bundle or changed
main queues a new generation; the old generation cannot publish. Consumers use
`consume_comment` in the runner: only the journal-named comment, by the configured principal,
with the exact schema/issue/generation and current published generation has authority.

Label and comment intents/completions are separate checkpoints. A known precondition refusal
before any mutation records an aborted phase without claiming completion. A partial label write,
an ambiguous API response or a freshness refusal after labels were written retains the unresolved
intent and blocks replacement publication and auxiliary writes. A refusal on a later attempt cannot
abort an earlier unresolved intent. Ordinary tick recovery completes a retained label intent only
when it observes the intended runner-owned labels and hint removals, even if changed inputs prevent
that generation from publishing. Unrelated label changes still invalidate its freshness but do not
hide completed runner effects. This reconciliation precedes checked replacement publication,
retirement and sweep. Partial or unknown label results remain fenced. Fresh label recovery applies
only remaining authorized differences. Unrelated and unknown labels are preserved; only enumerated
runner values and pending/bug/enhancement hints may change.
`class/decision`, `class/security` and `triage/held` are never removed. Owner namespace label
provenance is retained in `owner_override` and effective JSON/labels; two owner values in one
namespace suspend that event until a resolving owner event arrives. Missing or incoherent label
provenance also suspends before Task admission; the runner never assumes an unattributed
namespace label was its own. The private `suspension` reason identifies that refusal.
Owner overrides do not establish agreement: differing author/verifier class or size still
produces `triage/disputed`, while preserving the owner's effective values in the record and labels.

An ambiguous comment create is reconciled by principal, recorded comment id and exact generation
marker/content. A found comment is adopted once. If no result can be observed, the create stays
suspended: the runner never blindly repeats it. The operator must establish its outcome before
resolving that intent; do not delete the intent to force progress. A later generation edits the
same journal-named comment. The canonical footer is exactly
`triage: af/issue-triage-merged@1 generation=<key> task=<id>`. Auxiliary sweep/reminder comments
use separate evidence checkpoints and retain their exact intended bodies for reconciliation
even if the next sweep no longer proposes that evidence; they never impersonate a merged triage record.

A Task with known terminal failed usage leaves pending on its first two failed generations;
the third asks the owner to inspect failed Task receipts under needs-info, without inventing a
classification. Unknown usage, uncertain admission, identity/integrity mismatches and failed
input reads do not authorize that public fallback. A security classification is held privately
even when verifier output is missing, the report check failed or native billing is unknown.
Use `af task show TASK --json --repo /opt/af-triage/repo --state /var/lib/af-triage/task-store`
to inspect the exact Task. Do not rerun an admitted Task to repair the runner's journal.

After interruption, the shared fence is operator evidence. Inspect all owned processes,
containers, mounts and Task receipts before settling it while holding the anchored lock, as
the executor guide describes. Never use a process-name kill or clear temporary directories.
A surviving or unconfirmed process/container keeps the fence and blocks subsequent work.

## Explicit transition to writes

After the seven-day review, the owner chooses when to enable writes. Stop both timers and wait
for the service cgroups to be empty; review holds, unknown reservations, failure counts,
conflicts, stale generations and every ambiguous intent first. Verify the existing nightly
channel and the separate issues token's scope. Keep the same state root and principals.

Replace each triage/sweep service's `ExecStart` in an operator-controlled systemd drop-in with
the reviewed original invocation plus `--apply`, then inspect the effective unit and restart
the timers. No date or environment variable turns writes on automatically. Dry-run generations
are rechecked before publication; changed inputs require new generations. To return to dry-run,
remove `--apply` and reload the units after draining the current run. This is a later operator
action; no such transition, live coverage or backfill is part of this change.

## Local evidence

Run `python3 -I scripts/test-backlog-triage.py` and
`python3 -I scripts/test-backlog-check-triage.py`; the runner test also works with
`/usr/bin/python3` 3.9.6. Fakes cover input bounds, malicious paths, event deduplication, claim
and reservation ordering, token caps and unknown spend, holds, dry-run, freshness, ambiguous
label/create/edit recovery (including input changes after a label write returns but before its
completion checkpoint, and partial/unknown label results), owner conflicts, sweep evidence and
reminder deduplication. A fake nightly host covers the staged shared-lease adapter without running
Docker or systemd. Existing Rust `task_triage` tests cover installed package planning and real
report-renderer bytes. These
are distinct from an unperformed live deployment and first-week observation.
