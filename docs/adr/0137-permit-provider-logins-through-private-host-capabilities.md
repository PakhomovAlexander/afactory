# ADR-0137: Permit Provider logins through private host capabilities

**Status:** accepted (2026-10-04). Partially supersedes
[ADR-0112](0112-refuse-agent-mediated-provider-logins.md)'s unconditional refusal of automated
login. Extends ADR-0111's machine-local bootstrap dispatch exemption.

## Context

A human using an assistant-managed headless host cannot use its private terminal. Missing and
stale credentials both require an official browser login on the same native auth directory.
Forwarding raw login stdout would disclose OAuth material to transcripts and group conversations.
Issue #122 therefore needs a separate, explicitly permissioned host boundary.

The first boundary is authentication and setup. It is not paid Provider Admission or Task
continuation: a successful login and a token-free status probe do not prove runtime usability.
A terminal Task cannot resume, and an existing captured admission with a one-Attempt cap has no
remaining retry authority. This change does not alter those facts or close the runtime part of
#122. A future runtime decision must capture recovery allowance before spending it, retain all
failed Attempts, and preserve exact approval, identity, deadline and budget fences.

## Considered options

- **Return URLs/codes on ordinary stdout or JSON.** Rejected: every automated reader and log would
  become a credential recipient. Redaction after publication cannot repair disclosure.
- **Treat a CLI flag, environment override or status success as permission.** Rejected: none
  authenticates the requesting human or establishes a private delivery route.
- **Keep terminal-only login for every caller.** Rejected: it forces a headless user to obtain
  shell access and transfers recovery work to them.
- **Invent one shared OAuth exchange.** Rejected: Codex device approval needs no pasted response;
  Claude supports browser callback or a code on stdin. Native credential stores remain native.
- **Persist raw challenges or restore a native login by PID.** Rejected: secret persistence and
  PID reuse create a larger boundary than required. An interrupted owner requires a fresh grant.
- **Automatically run an inference after login.** Rejected: login permission grants neither a
  model call nor a new Task Attempt. Status remains explicitly unverified.

## Decision

Add the machine-local `af provider auth begin/status/cancel` namespace. Generic
`af provider setup --login` remains terminal-only. `begin` always means explicit reauthentication,
including when local status claims authenticated; it never takes the stale-status shortcut.
Provider ID, kind and an explicit native auth directory are mandatory. Existing registry conflict
checks, directory ownership checks and the native context setup lock remain authoritative. An
existing binding removed or rebound while the browser login is waiting is not recreated by the
older login permission; completion reports a registry conflict under the registry lock.

The Linux trusted host passes two dedicated inherited anonymous pipes, each with a descriptor
above 2. Kernel `/proc/self/fd` provenance is required; macOS currently fails closed for this
new path while retaining terminal setup.
af duplicates only the supplied pipe objects, validates type and ownership, consumes the inherited
descriptors, and leaves only close-on-exec handles before starting any native child. Ordinary
stdin/stdout/stderr, regular files, sockets and a self-loop pipe are refused. The host must isolate
these capabilities from untrusted code and keep its process and pipes alive across agent turns.
No local listener, network server, shell hook, recipient platform or persistent credential is added.

Before starting a CLI, af sends a non-secret permission request bound to a random recovery ID,
Provider ID, kind, opaque canonical-directory/inode identity and a bounded host deadline. The host
must establish action-time human authorization and private recipient delivery, returning opaque
requester/coordinator references and an expiry no later than the request. Merely launching `begin`
does not grant permission. A false, malformed, misbound, expired or non-private grant starts no
login. A host outside this trust boundary cannot truthfully provide this capability.

Only this private transport carries the validated challenge and, for Claude, one browser-returned
code. Every response is bound to recovery ID and requester. A delivery acknowledgement is required;
wrong-session, wrong-recipient, duplicate, stale, wrong-mode and replayed responses fail closed.
Codes are bounded single-line data written to the dedicated native stdin, never shell commands.
The host must never route password, API-key, access/refresh-token or MFA requests to this interface.

Provider adapters use supported official mechanisms:

- Codex uses app-server initialization and `account/login/start` with `chatgptDeviceCode`, validates
  the official verification URL, retains the native login ID privately, and requires the matching
  `account/login/completed` success. Cancel terminates the owned process after a best-effort native
  cancellation. No model thread or turn is created.
- Claude uses only `auth login --claudeai`, not a general terminal session. It extracts one complete
  official-origin URL or OSC-8 hyperlink target from bounded output; visible hyperlink wrapping
  does not truncate its target. Ambiguous physical bare-URL wrapping, extra URLs, unsupported
  prompts and terminal controls are refused rather than guessing parameters. It accepts one code
  or native callback completion and requires successful native exit. This bounded adapter follows
  the documented stdout/stdin fallback, not an invented JSON protocol.

All native login output is private, bounded memory. No raw text is reflected into exceptions or
normal JSON. Native failed model output also gains closed failure classification before capture:
recognized auth failures and obvious credential/challenge-bearing failed streams are replaced by
one non-secret summary before CAS publication. Parsed exact usage is retained separately. This is
not a general detector of secrets in arbitrary successful model output, and a textual contention
report is not proof that another refresh process exists. Classification never grants login or retry.

An independent native-lifetime guard inherits the already-locked file capability through a
private internal spawn, moves it to a close-on-exec handle before any ordinary logging and
retains it until the native process group is reaped. Parent EOF or owner death cancels the native
flow before that guard releases the lock; a new login cannot race an orphaned CLI.

A single owner holds the native auth-context lock. Concurrent `begin` calls return its same opaque
non-secret status and cannot start another challenge. State is atomically persisted under a private
machine-local directory, never Task inputs/CAS. Permission, challenge delivery, response consumption
and terminal result have durable closed states; no challenge, code, native login ID, account
identity or credentials are persisted there. Cancellation names the exact active session. TTL,
interrupt, host disconnect, invalid response and context replacement stop the native child.
A lost owner is reported as interrupted; restart is a new grant and native flow, never restoration
of stale secret state. Host deadline is a local upper bound, not a provider-advertised expiry.

After official completion, af performs only the existing token-free status check and idempotent
registration. It durably records `authenticated_unverified`, then notifies the exact opaque
coordinator through the host. Status can recover a missed completion notification. The host must
idempotently consume this recovery ID and notify the originating conversation without secrets.
No verified/resumed claim, paid call, Task dispatch or publication follows from login authority.

## Consequences

The new host protocol and status schema are versioned independently from ordinary setup/status.
A host integration can provide browser-only setup while generic automation retains its safe
terminal-only refusal. An unavailable private host is a narrowly identified blocker, never a reason
to publish a challenge into a group or ask the user to repair credentials through SSH.

Deterministic tests use synthetic official-command stand-ins and real private pipes, including
callback/device completion, explicit stale-auth recovery, permission refusal, concurrency, recipient
fences, cancellation, process loss, code replay, output limits and secret-free ordinary artifacts.
Live proof is separately opt-in and requires host/user authorization; it is not part of CI.
