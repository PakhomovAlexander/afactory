# Opt-in original-session Task wakes

This candidate implements local Linux routes for Pi 1.4.0 interactive extensions and the
Codex 0.167.2 existing interactive CLI queue. Final live certification is pending. All repository
acceptance uses recording stand-ins. Claude automatic wake is unavailable; its live tests and
sign-in remain deferred. `af task wake capabilities` reports these distinctions without contacting
any host. No daemon, scheduler, host session or service is installed automatically.

`af help task wake --json` lists all seven commands and their effects under the existing
command catalog contract. `capabilities` and `inspect` read only. `grant` writes the Task Store,
the private grant file and the machine session binding; `register`, `revoke`, `drain` and
`pi-next` write notification state in the Task Store. Codex `drain` may also change the granted
local host's queue/session state. All seven commands are AF token-free: any follow-up inference
uses the original host's separate grant and limits, never an AF Task budget. The grant command
does not require a signed plan approval or a Provider login.

## Owner setup

Plan the Task normally first. A route grants follow-up inference in exactly one existing session,
under that host's existing limits. It grants no AF execution, continuation, login or publication.
Use an owner-private directory outside the repository and Task Store for grant files. Supply
canonical paths: no symlinks, writable installation binaries, network URLs or shell callbacks.
The Store itself must satisfy Linux machine/owner/device/inode incarnation admission.
The same-UID owner is trusted; this facility does not isolate hostile processes with that owner's
privileges. Worker sandboxes must not have access to owner files or the Store. An original host/session is
bound to one authoritative Task Store in a separate private machine control Store under
`$XDG_STATE_HOME/af/task/local/`. The existing machine inventory accounts and retains that Store.
A grant in another Task Store refuses, preventing cross-Store overtaking of uncertain admission.
There are at most 4096 such machine session bindings; neither expiry nor a replacement grant
rebinds them. Removing machine control state is destructive removal of recovery authority.

For Codex, explicitly select the original interactive thread and its existing local daemon socket.
The canonical socket parent must be owner-owned and private (mode `0700`, with no group/other
access). The socket must also be owner-owned, but may have mode `0777`: Linux pathname access
is protected by the private parent directory. AF still rejects symlinks, pins socket device/inode
and authenticates the connected peer's UID. Do not change the daemon's socket permissions.
Obtain the thread and endpoint from the original host, rather than Task text or an environment
session string. Example (replace the paths and UUID):

```sh
af task plan --file ../ticket.json
af task wake --state /owner/task-store grant TASK_ID --host codex \
  --session ORIGINAL_THREAD_UUID --executable /owner/bin/codex \
  --socket /owner/codex/daemon.sock --grant-file /owner/wakes/task-grant.json \
  --expires-secs 86400
af task run TASK_ID --execute --detach --state /owner/task-store \
  --wake-route /owner/wakes/task-grant.json
af task wake --state /owner/task-store drain --max-batches 16 --timeout 30
af task wake --state /owner/task-store inspect
```

The grant file is created absent-only, fsynced and its directory fsynced before the route is
installed. Its capability is never printed or included in a notification. An interrupted grant
installation can leave an inert file; inspect before explicitly replacing it. AF pins the
installation's bytes and socket device/inode, checks the local peer's UID, and invokes only
`codex queue --thread UUID --message FIXED_METADATA --remote unix:///SELECTED_SOCKET`.
It never invokes resume, starts an app-server or creates a conversation. A replaced daemon socket
requires a fresh explicit owner grant; pending work is never redirected automatically.

For Pi, independently install the reviewed `scripts/task-wake-extension.ts` in the original
interactive host. This is the production pull hook; `scripts/task-wake-pi.ts` remains a separate
experimental probe. Grant the original session reported by `ctx.sessionManager.getSessionId()`:

```sh
af task wake --state /owner/task-store grant TASK_ID --host pi \
  --session ORIGINAL_SESSION --executable /owner/bin/node \
  --extension /owner/pi/task-wake-extension.ts \
  --grant-file /owner/wakes/pi-grant.json
```

Here `--executable` is the canonical executable of Pi's actual interactive process, which Linux
reports at `/proc/PARENT_PID/exe`, usually Node. Do not select an interpreter alias or a launcher
script. AF verifies that parent at every Pi pull, in addition to the private capability, installed
extension digest, exact session and current grant. There is no unauthenticated listener.
The local receiver is the Task Store, shared by the installed hook and the explicit foreground
driver; its SQLite FULL-synchronous transactions are the packet and deduplication boundary.

Create a private `0600` Pi configuration file in that same private directory. `af_digest` is the
SHA-256 of the reviewed installed AF executable's bytes; all paths are canonical:

```json
{
  "schema": "af/task-wake-pi-config@1",
  "af": "/owner/bin/af",
  "af_digest": "sha256:REPLACE_WITH_64_LOWERCASE_HEX_DIGITS",
  "repo": "/owner/repository",
  "state": "/owner/task-store",
  "route": "/owner/wakes/pi-grant.json"
}
```

In that Pi conversation run `/af-wake-connect /owner/wakes/pi-config.json`. The hook checks
`ctx.mode`, the original session and `ctx.isIdle()`. It polls for at most five minutes and 128
polls, with five-second bounded child calls, and stops after one native submission or any
uncertainty. Reconnect explicitly for another bounded observation window, including after an
exact-session reopen. `/af-wake-disconnect` stops the hook. Busy delivery stays in the durable AF
inbox until an idle poll, then uses `pi.sendMessage` with `triggerTurn: true` and
`deliverAs: 'followUp'`; it never steers or runs a parallel turn. Each authenticated idle poll also extracts one checked
history page and accepts at most one packet for its route. No foreground drain is required during
that attachment window; `wake drain` remains available for larger catch-up and after hook expiry.
Closing Pi does not erase that inbox or its native intent. Disconnect, shutdown and reconnect
invalidate pending callbacks: an old callback cannot send or change a replacement attachment.
Any already committed native intent stays uncertain.

## Registration and finite driving

`run --detach --wake-route FILE` authenticates the original route and commits registration,
checked current prefix and launch intent in the parent before any detached spawn. Failure launches
nothing. The child receives neither the grant path nor a registration flag. A crash before spawn
leaves inspectable intent; a crash after spawn is reconciled from ordinary Task history. The driver
never respawns. Immediate completion before detach returns remains extractable. Ordinary detach
keeps its existing output and behavior when no route is selected.

For an already recorded Task, explicit catch-up is:

```sh
af task wake --state /owner/task-store register TASK_ID --route /owner/wakes/task-grant.json
```

Catch-up starts at its checked opening and is bounded to 128 observations. The raw Store API also
supports exact idempotent registration keys and refuses changed configuration. A new CLI registration
is a distinct subscription, not an ambiguous-send recovery command.

`drain` bounds each stage independently to `--max-batches` (at most 128): source visits,
transport claims and native route visits. Empty or unavailable visits count too. Extraction and
native traversal positions persist in the Store, so repeated `--max-batches 1` invocations reach
later subscriptions/routes across restart. These positions do not bypass expiry or uncertainty
fences. The driver stops at its selected wall deadline, at most 300 seconds. Each extraction page examines at most 4096
new history rows, retaining the checked checkpoint across restart. Ordinary canonical validation
can revisit earlier history. Each native request has at most five seconds plus shared supervisor
cleanup. Extraction/outbox/cursor commit atomically under the Store writer serialization.
Attempts, thirty-second sender leases, fencing epochs and capped exponential backoff survive
restart. Eight transport attempts exhaust automatic delivery; restart never resets them. Route
refusal backs off without calling a host. Run drain again explicitly when its bounded batch ends.
No running driver is implied by a pending row or by a detached run returning.

## Receipts, uncertainty and recovery

Inspection distinguishes the retained outbox, durable inbox acceptance, native intent and
`admission_unknown`. Native turn admission, turn start and user-visible reporting require their
own authoritative evidence; these adapters currently establish none of those later stages.
A queue exit code, a void Pi return and unrelated host lifecycle events are never promoted to
success. The fixed wake contains only Task/Store/receipt references and an instruction to inspect
current evidence under existing host authority. It excludes Task descriptions and diagnostics and
discloses that the observation may be historical, superseded or collected.

Before returning a Pi packet or invoking Codex, AF commits one native intent and a sticky original
session fence. A crash or lost response cannot cause another native call. A prior uncertain intent
also prevents later subscriptions or replacement routes for that same host/session from overtaking
it. Without demonstrated native idempotency or authoritative reconciliation, inspection/reporting
is manual. Reconnecting, expiry, a fresh delivery key or a fresh grant cannot clear that fence.
This conservative behavior can block later automatic wakes even when a native call returned
normally. It does not claim exactly-once model execution. No command fabricates turn/report receipts
or clears unknown admission. The user may still inspect the Task and communicate normally in the
host; a wake never authorizes another AF action.

Revoke future route use explicitly:

```sh
af task wake --state /owner/task-store revoke --route /owner/wakes/task-grant.json
```

Wrong capability, Task opening, Store incarnation, peer, session, installation, endpoint, expired
route and unsupported host refuse. Revocation does not recall a native request already admitted.

## Expiry, collection and capacity

Grants/subscriptions expire after at most seven days. Expiry persists closure, including an
unextracted subscription; it creates no delivery success. Collection's own writer transaction closes
expired subscriptions before checking its extraction barrier. A live subscription still blocks
collection until extraction reaches the exact current checked prefix. Once extracted or closed,
actual collection may remove the CAS closure. `source_available: false` identifies retained
historical metadata; a notification never replaces the unavailable result or retains its full CAS
closure. Unknown evidence and changed source identities refuse rather than advance a cursor.

Bodies, packets, receipts and expired grants retain a twenty-four-hour post-expiry replay horizon.
`drain` persists expiry and compacts eligible bodies in the Store transaction. Expired capabilities
and envelopes are rejected, even after their bodies are forgotten. Native uncertainty compacts to
small permanent session/identity/provenance tombstones, retaining source availability and never
permitting resend. There are at most 128 active retained subscriptions/routes, 4096 packets and
4096 uncertainty fences, with a 16 MiB logical control bound. Exhausting the uncertainty bound
refuses further admission; there is no automatic unsafe reset. The database/WAL, including these
rows and tombstones, are already measured by AF's machine Store inventory. Whole-Store removal
is destructive removal of recovery evidence, never successful delivery.
