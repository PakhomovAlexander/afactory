# Detached Task wake host experiments

Authored 2026-11-12 for issue #765 milestone 1. These are future, separately authorized disposable
host trials. None was run during implementation. The
[proposed ADR](../adr/2026-11-12-deliver-durable-task-wakes-to-originating-host-sessions.md)
retains the accepted research Document's provenance and records the source drift check.
There is no production subscription, driver, provider authentication change or automatic wake.
The subsequent [foundation milestone](task-wake-foundations.md) adds closed metadata contracts and
read-only checked lifecycle projection, without activating delivery or changing these trial limits.

## Primary references and support matrix

Documentation checked on the authoring date; no installed host version or API compatibility is
asserted. The coordinator must record exact executable/extension digests, host version, OS,
frontend, session identity, host policy and allowed turn budget privately before any live trial.
Do not put raw version output, paths, credentials or transcripts into the evidence packet.

- [Pi extension API](https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/docs/extensions.md):
  documents `registerCommand`, `ctx.sessionManager.getSessionId()`, `ctx.isIdle()` and
  `sendMessage` with `triggerTurn`/`followUp`. The probe uses these interfaces, including the
  documented `@earendil-works/pi-coding-agent` type import; their presence does not prove durable
  native admission. `followUp` is documented to wait for the agent's tools to finish.
- [Codex app-server](https://developers.openai.com/codex/app-server/): documents application-owned
  thread/turn operations. It does not establish an authenticated attachment to the particular
  interactive frontend selected for this experiment. A separate app-server or resumed stored
  thread is not evidence that the original frontend receives a turn.
- [Claude channels reference](https://code.claude.com/docs/en/channels-reference): documents
  channel notifications over MCP and the need to gate senders. Transport acknowledgement alone
  does not demonstrate a durable inbox, native turn admission or original-session restart recovery.

| Host / concrete frontend | Kit entry point | Current result | What remains unproved |
| --- | --- | --- | --- |
| Pi interactive CLI, POSIX local filesystem | `scripts/task-wake-pi.ts`, explicit in-process command | Implemented, live **untested** | Installed API, busy command availability, native receipts, turn/report correlation, restart |
| Pi SDK-owned application / AF Worker | Excluded | **Unsupported** for original frontend criterion | A new SDK session cannot pass it |
| Codex already open CLI/Desktop conversation | `task-wake-probe.py probe --live`, host `codex` | **Unsupported route**, live **untested** | Authenticated attachment to that exact frontend, preserved policy and queue semantics |
| Codex separate app-server / SDK application | Excluded | **Unsupported** for original frontend criterion | Stored history resume is not frontend attachment |
| Claude Code already open interactive session, including enabled channels | `task-wake-probe.py probe --live`, host `claude` | **Unsupported route**, live **untested** | Exact channel-to-session binding, authenticated reconnect and native admission reconciliation |
| Claude SDK app / closed interactive session | Excluded | **Unsupported** by this kit | No original-session connection established |
| Any host absent or required Pi APIs missing | Refusal | **Unavailable** | Install/environment evidence belongs to later trials |

Unsupported, unavailable and untested never pass. No public socket, HTTP listener, webhook, SDK
session creation or credential read is implemented. Pi's endpoint is the operator-selected command
inside the original process, authenticated by that interactive user's access to the frontend and
trusted loaded extension. Opaque IDs alone are not authentication. This assumes a trusted local
user; it does not protect against hostile code running with the same permissions. Remote and
unverified platform transports are refused rather than inferred from the host name.

## Offline reproduction

Python uses only its standard library. The extension's deterministic stand-ins require Node 24.3+
with built-in TypeScript stripping; they import no Pi runtime package and perform no model calls.
No dependency installation is required. From the checkout root:

```sh
python3 -I scripts/test-task-wake-probe.py
python3 -I scripts/task-wake-probe.py probe \
  --packet fixtures/task-wake-probe/pi.json --session-id session_1
python3 -I scripts/task-wake-probe.py validate \
  --packet fixtures/task-wake-probe/pi.json --session-id session_1 \
  --evidence fixtures/task-wake-probe/pi.jsonl
```

The fixture is manufactured recording evidence. Validation prints
`structurally consistent only; not independent live proof`. Dry-run emits `dry_run/offline`,
creates no journal and sends nothing. Even `probe --live` in Python refuses; Pi live work must
use the reviewed extension explicitly. Refusals exit 2, dry-run/structural checks exit 0; zero is
not a live capability pass. The fast functional catalog selects these tests, including the
native TypeScript entry point with recording stand-ins. They prove probe behavior only.

## Pi disposable-session trial

The commands below are for the later operator, never this implementation Task. Select a disposable
interactive session with an already authorized, finite marker-only turn allowance. Record its
frontend instance and session-file binding privately; never use a real working conversation.
With an existing trusted Pi installation, deliberately open the test frontend with the reviewed
extension (no installation, service or background watcher):

```sh
pi -e ./scripts/task-wake-pi.ts
```

In that frontend, run `/af-wake-identity`. It displays the session ID locally without inference.
Copy that exact ID into `WAKE_SESSION_ID` below; do not guess it or use the newest/default session.
The following shell values are operator selections, never loaded from an evidence packet. All
paths in the extension command must be absolute and contain no whitespace. Use a private journal
per new trial; preserve it for duplicate/reconnect/restart trials of that same delivery.

```sh
WAKE_SESSION_ID=REPLACE_WITH_EXACT_DISPOSABLE_SESSION_ID
mkdir -m 700 -p target/wake-idle
chmod 700 target/wake-idle
python3 -I scripts/task-wake-probe.py packet --host pi --run-id trial_1 \
  --session-id "$WAKE_SESSION_ID" --probe-id idle_1 --delivery-id delivery_1 \
  --scenario idle --marker AF_WAKE_0123456789abcdef > target/wake-idle/packet.json
```

First invoke the dry command in the selected Pi frontend, substituting the checkout's actual
absolute paths and the exact displayed ID:

```text
/af-wake-probe dry EXACT_SESSION /ABS/CHECKOUT/target/wake-idle/packet.json /ABS/CHECKOUT/target/wake-idle
```

Only after separate live-trial authorization, change `dry` to `live`. This is the explicit send
opt-in and exact session/endpoint binding. The handler checks both the packet and the current
native session ID at invocation, retains/fsyncs the canonical packet as `inbox.json` and fsyncs
the leaf journal and then each ancestor through the filesystem root before writing
`inbox_accepted/persisted` to `events.jsonl`. This persists the packet's entry and the selected
journal's own entry, including newly created ancestors from the setup above. The receipt file
and leaf journal are then fsynced before the handler can return acceptance or send
one fixed request to echo the marker without tools. Idle requires actual idle; a busy case
requires actual busy. It always uses `followUp`, never steering or interruption. If a frontend
only dispatches commands once idle, the busy case is unavailable there; do not label it passing.

The journal is owner-only, no-follow, bounded, locked and bound to one exact packet. The retained
`inbox.json` is an exclusive, single-link, mode-0600 file containing the complete canonical packet,
including its marker, within the same 2048-byte bound. Replay validates it against the selected
packet, exact delivery/session identities and every receipt's digest. Missing/corrupt retained
payload refuses even if the receipts survive; digest-only journals are refused. A complete packet
left before its receipt by a crash is validated and fsynced again, followed by the full leaf-to-root
directory sync sequence before continuing. Any open or sync failure in that packet/ancestry sequence
refuses before writing acceptance or calling the native API; a subsequent explicit retry must
establish all that durability again. An incomplete packet refuses. The retained packet survives
loss of the external input; the operator may explicitly select `inbox.json` as the packet file for
replay without creating another delivery.

Complete retained writes may be visible even when their fsync failed or the process stopped
before syncing. After validating the packet, all receipt rows and any claim, replay fsyncs
`inbox.json`, `events.jsonl` and any `admission.claim`, then the leaf directory and every ancestor
through the filesystem root, before returning retained evidence or continuing pre-intent work.
Any recovery persistence failure refuses without returning success evidence or making a native
call. Retained admission intent still blocks resending even if recovery fails. A complete orphan
packet follows the same file/ancestry durability sequence before its first receipt. Recording
filesystem regressions cover these boundaries, recovery failures and simulated power loss after
successful recovery; they establish probe behavior, not live host admission or storage reliability.

This guarantee requires a local filesystem that honors file and directory fsync; an unsupported
ancestor sync fails closed. It assumes the trusted local user preserves the canonical directory
ancestry and retained journal. It does not protect against that user's hostile code replacing or
deleting them, nor make a temporary filesystem survive a reboot. Use durable local storage for
power-loss trials; the canonical temporary fixture roots prove ordering and refusal behavior only.

An immutable `admission.claim` also blocks replay if the receipt journal disappears or is truncated
after a send. A duplicate returns retained rows, not a new send receipt. A crash after inbox
persistence but before admission intent may continue on the next explicit invocation. Intent alone
blocks further native calls. A stale `.lock`, corrupt journal, changed payload or wrong session
refuses. Never delete or replace a journal to obtain another send for an ambiguous delivery.
Establish the old process is gone
before manually clearing a stale lock; clearing it never clears admission intent. Missing journal
history cannot prove an earlier delivery absent; do not retry with a fresh journal.

Pi's void native return supplies no native admission identity. The kit therefore records
`native_returned` separately and leaves `admission_unknown`; it does not infer turn start/report
from unrelated lifecycle events. Maximum native sends per retained delivery is one; automatic
retries are zero. One command is foreground and finite in work; it relies on the installed host's
synchronous `sendMessage` contract, with no retry timer or long-running receiver.

## Codex and Claude original-session refusals

For each deliberately selected disposable frontend, privately record its native session/thread
identity from the host itself and the owning frontend/installation. For Codex, retain both the
thread ID and native session identity when exposed; a fork/root/session ID or history file alone
cannot identify an authenticated live frontend endpoint. For Claude, retain the actual open-session
identity and owning enabled channel, not a chat room or SDK conversation. If the host cannot expose
these facts, record unavailable and stop; do not invent a session identity to pass a trial.

Create one packet per host with an actual captured session ID, using the same `packet` command
above with `--host codex` or `--host claude`, then run:

```sh
python3 -I scripts/task-wake-probe.py probe --live \
  --packet target/wake-codex/packet.json --session-id EXACT_CODEX_SESSION
python3 -I scripts/task-wake-probe.py probe --live \
  --packet target/wake-claude/packet.json --session-id EXACT_CLAUDE_SESSION
```

Both emit `unsupported/no_original_session_route` and exit 2. No endpoint parameter is accepted;
packet data cannot choose a network destination. These are runnable explicit capability refusals,
not partial emulations of `thread/resume`, `turn/start` or `notifications/claude/channel`. Enabling
an arbitrary webhook, starting a separate app-server, calling another CLI or opening an SDK session
cannot overcome them. Later authenticated original-session adapters need separate reviewed code;
this milestone neither requires nor silently substitutes them.

## Experiment sequence and exact evidence

Run this sequence independently for **each** host/frontend, stopping at the first unavailable or
unsupported prerequisite. For Codex and Claude, the current entry points terminate at route refusal
for every case below; retain that non-passing result. A future adapter must use the same cases and
identity discipline. For Pi, create each new scenario packet with its own probe/delivery ID and
private directory; exact duplicates and restarts reuse the original packet and directory unchanged.
The `scenario` field is a label for the controlled trial, never an executable fault instruction;
only `ambiguous` deliberately suppresses the Pi native return observation.

| Case | Pi stimulus | Codex stimulus once attachment is proved | Claude stimulus once channel binding is proved | Required observation |
| --- | --- | --- | --- | --- |
| Idle | `idle` packet; explicit live command while idle | Single marker `turn/start` in exact original thread | Single marker channel event in exact open session | Separate inbox, admission, turn and visible echo identities |
| Busy | `busy` packet during one bounded marker turn | Queue externally until verified idle; never steer | Send only if approved queue behavior is established | No interruption/concurrent turn; follow-up after active work |
| Duplicate | `duplicate` packet, invoke twice unchanged | Repeat same delivery through deduplicating receiver | Repeat same channel delivery key through receiver | One native call at most; matching retained inbox receipt |
| Reconnect | `reconnect` packet; reload extension in same session, repeat | Disconnect/reconnect bridge to same owning frontend | Disconnect/reconnect enabled channel to same open session | Exact session and retained receipt continuity; no resend |
| Restart | `restart` packet; close/reopen exact disposable session with extension and old journal | Restart frontend separately from bridge; no new thread | Close/reopen exact session separately from channel | Proven original session/policy continuity or non-passing limitation |
| Ambiguous | `ambiguous` packet discards return after one send; repeat after reload | Drop return after possible native turn acceptance | Drop return after possible channel acceptance | Unknown persists; no automatic replacement native request |

Also attempt wrong-session binding, changed marker under the same delivery key, missing receiver
state, missing API and oversized/unknown fields using stand-ins. They must cause no native call.
Never fault-inject another user's conversation. Stop when the approved turn allowance is exhausted;
do not log in, change provider policy, execute tools or resume any AF Task to finish this experiment.

Packet/evidence schemas are [probe](../../schemas/task-wake-probe-v1.json) and
[evidence](../../schemas/task-wake-evidence-v1.json). These are experimental script formats, not
Task runtime contracts. Canonical ASCII JSON with sorted keys, compact separators and exactly one
newline is required. Bounds: 2048 bytes per record, 128 evidence records / 256 KiB total; Pi's
receiver writes at most four records. Opaque IDs are 1–128 ASCII letters/digits/underscore/hyphen;
marker is exactly `AF_WAKE_` plus 16 lowercase hexadecimal digits. Every evidence row joins exact
run/host/session/probe/delivery IDs and the SHA-256 of canonical packet bytes **without** its newline.
A repeated stage key must preserve the entire row. Unknown fields, alternate encodings, wrong
sessions, altered payloads, dangling/oversized rows and invalid stage joins fail closed. JSON Schema
checks shape; the script additionally checks framing, byte bounds and cross-row joins.

| Stage | Evidence required; absent evidence stays unknown |
| --- | --- |
| `inbox_accepted` | Exact canonical packet in receiver `inbox.json`; packet, leaf journal and every ancestor through root fsynced before the receipt; receipt file and leaf fsynced before returning |
| `admission_intent` | Journaled before the single native call; not proof of admission |
| `native_returned` | Call returned; no native admission proof is implied |
| `native_admitted` | Native request identity/authoritative admission receipt tied to this delivery |
| `admission_unknown` | Call may have been accepted; suppress automatic resend |
| `turn_started` | Exact native turn plus original session and marker correlation, not just any agent start |
| `report_observed` | Marker-only assistant message in the original visible conversation with its message reference |

The extension records the first stages automatically; it intentionally makes no admission/turn/report
claim. The operator can export bounded opaque references from a separately observed trial, excluding
raw transcripts. For example, copy the journal to `evidence.jsonl`, then append explicitly observed
references (never append these to the receiver's journal):

```sh
cp target/wake-idle/events.jsonl target/wake-idle/evidence.jsonl
python3 -I scripts/task-wake-probe.py observe --packet target/wake-idle/packet.json \
  --session-id "$WAKE_SESSION_ID" --stage turn_started --reference EXACT_NATIVE_TURN_REF \
  >> target/wake-idle/evidence.jsonl
python3 -I scripts/task-wake-probe.py observe --packet target/wake-idle/packet.json \
  --session-id "$WAKE_SESSION_ID" --stage report_observed --reference EXACT_VISIBLE_MESSAGE_REF \
  >> target/wake-idle/evidence.jsonl
python3 -I scripts/task-wake-probe.py validate --packet target/wake-idle/packet.json \
  --session-id "$WAKE_SESSION_ID" --evidence target/wake-idle/evidence.jsonl
```

`observe` performs no host operation: `observer_supplied` explicitly marks a local assertion.
Structural consistency is never independent live proof, even if all stages are locally supplied.
A passing host result requires separate review of actual native receipts, authenticated identity
correlation and original visible marker evidence, linked by the retained opaque references. Never
promote the manufactured fixture, a model's claim to have notified, an RPC acknowledgement, an
unobserved report or a resumed SDK history into that proof.
