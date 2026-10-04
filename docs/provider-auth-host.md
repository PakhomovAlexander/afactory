# Private-host Provider authentication

This is the explicit host integration for browser-only Provider setup and reauthentication
([ADR-0137](adr/0137-permit-provider-logins-through-private-host-capabilities.md)). It partially
implements issue #122. It does not yet suspend, verify or continue an executing/terminal Task.

## Boundary and supported environments

The integration currently supports **Linux with `/proc/self/fd` available**. The host must be a
trusted local process running as the same user as af. af checks kernel anonymous-pipe provenance;
macOS fails closed until an equivalent supported provenance check exists. Generic terminal setup
remains available on both supported CLI platforms.

The human opens an official authorization link in their own browser and approves access. Claude
may additionally require a one-time browser-returned code through the same private route. The
human does not run shell commands, use SSH, inspect auth directories or paste passwords/tokens.
The host owns machine-side actions and the original requester's identity.

A host must have a separate sensitive/private delivery capability. Normal assistant transcripts,
ordinary stdout/stderr, shared conversations, issue text, Task inputs, Worker context, CAS, logs
and notes are not this capability. If no private route exists, stop with that narrow blocker.
Never fall back to publishing an authorization link in a group.

Permission is scoped to one login session. It neither approves model inference nor grants new
implementation, plan, publication or continuation authority. Do not run a paid doctor/admission
implicitly. Status and login completion are not usability proof.

## Launch and retain the owner

The trusted host creates two anonymous pipes. It passes the read end of the host-to-af pipe and
the write end of the af-to-host pipe as dedicated inherited descriptors, both above 2:

```
af provider auth begin codex-main --kind codex --auth-dir /private/codex \
  --host-read-fd 7 --host-write-fd 10 --timeout-secs 600
```

The numbers above are illustrative: pass the actual descriptors, not files or guessed fd numbers.
For Claude select `--kind claude` and the existing Claude auth directory. `begin` deliberately
starts a new official login after host permission, even if cheap local status still says logged
in. Concurrent calls on an already-owned context only return its non-secret recovery status.

In Python the descriptor ownership skeleton is:

```python
import os
import subprocess

request_read, request_write = os.pipe()
event_read, event_write = os.pipe()
process = subprocess.Popen(
    [af_binary, "provider", "auth", "begin", provider_id,
     "--kind", provider_kind, "--auth-dir", exact_auth_directory,
     "--host-read-fd", str(request_read), "--host-write-fd", str(event_write)],
    pass_fds=(request_read, event_write),
    stdin=subprocess.DEVNULL,
    stdout=subprocess.PIPE,       # Non-secret final status only.
    stderr=subprocess.PIPE,       # Non-secret diagnostics only.
    env=approved_machine_environment,
)
os.close(request_read)
os.close(event_write)
private_to_af = os.fdopen(request_write, "wb", buffering=0)
private_from_af = os.fdopen(event_read, "rb", buffering=0)
```

Login children start from an empty environment and the explicit native auth directory. The host's
existing proxy routing (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`, also lowercase) and
certificate paths (`SSL_CERT_FILE`, `SSL_CERT_DIR`, `REQUESTS_CA_BUNDLE`, `CURL_CA_BUNDLE`) are
preserved through the lifetime guard, matching terminal login. API keys, Node/browser hooks,
display/session state and TLS-verification-disable overrides are not inherited. af does not change
these settings or print their values.

Do not redirect either private pipe to a logger or model transcript. A broker must keep the
process and pipes alive across agent turns, bound its own buffers, read while writing, and close
them on cancellation. The owner consumes the inherited descriptors and keeps only close-on-exec
duplicates; native CLI children cannot impersonate host responses. Duplicates of ordinary
stdin/stdout/stderr, named FIFOs, files, sockets and self-loop pipes are rejected.

The private protocol is one JSON object per newline, at most 16 KiB per frame. It is a host
protocol, not a public command output format. A malformed, unknown-field or misbound response
fails closed. Unknown native output never becomes a public diagnostic.

## Permission handshake

Before any official CLI starts, af sends:

```json
{
  "schema": "af/provider-auth-host@1",
  "action": "request_permission",
  "recovery_id": "<64 lowercase hex characters>",
  "provider": "codex-main",
  "kind": "codex",
  "context_id": "<opaque 64-character context digest>",
  "host_deadline": 1900000000,
  "purpose": "provider_login_only"
}
```

The host resolves the original requesting human and originating coordinator, checks its current
login approval and private-delivery capability, and replies:

```json
{
  "schema": "af/provider-auth-host@1",
  "recovery_id": "<exact request ID>",
  "provider": "codex-main",
  "context_id": "<exact request context digest>",
  "requester_ref": "<host-generated opaque 64-character lowercase hex handle>",
  "coordinator_ref": "<host-generated opaque 64-character lowercase hex handle>",
  "approved": true,
  "private_delivery": true,
  "expires_at": 1900000000
}
```

These handles are not names, emails or channel IDs. The host retains their actual mappings
privately. The expiry must be in the future and no later than af's host deadline. It is a local
maximum lifetime, not the provider's advertised expiry. Codex's checked app-server login schema
has no expiry field. No approval, wrong Provider/context, expired grant or non-private delivery
starts a login. This acknowledgement must reflect real host authorization; a model deciding to
write `approved: true` is not authorization.

## Challenge and response

After validation of official Provider output, af sends `action: challenge`, carrying the same
recovery ID, Provider, requester reference, host deadline, and:

- Codex: `mode: device_code`, `url` and `user_code`. The human opens the URL and enters the
  displayed device code in their browser. Do not submit that code back to af.
- Claude: `mode: code_or_callback`, `url`, `user_code: null`. The browser may complete the native
  callback or show a one-time response for the host to return privately.

Only the actual trusted private-delivery method may consume these fields. Deliver once per
recovery ID and then acknowledge:

```json
{"action":"delivered","recovery_id":"<exact ID>","requester_ref":"<exact handle>"}
```

For a Claude browser-returned code, use the same approved human-private route and send:

```json
{"action":"code","recovery_id":"<exact ID>","requester_ref":"<exact handle>","code":"<one-time browser response>"}
```

A code is accepted only once, after delivery acknowledgement, for this active recipient/session
and the correct provider mode. It is bounded data on native stdin, never a command or argument.
Do not turn native requests for passwords, API keys, access/refresh tokens, MFA or permission
bypasses into this message. Wrong-recipient/session, replay, unknown messages and stale replies
terminate the owned flow without exposing the response.

To cancel through the host, send `action: cancel` with the same recovery and requester references.

## Completion, status and cancellation

Native completion plus the token-free status check and idempotent registry publication produce
`action: setup_completed`, carrying `coordinator_ref` and a non-secret `status`. The state is
`authenticated_unverified`, with `verified: false` and `continuation: not_authorized_by_login`.
The result is durable before notification. The host must idempotently record this recovery ID,
notify the originating coordinator/conversation without secrets, and re-read status if its
notification connection was interrupted. Delivery to the coordinator is not a claim that a Task
resumed. Any later paid verification must be covered by a separate existing authorization/budget.

Ordinary non-secret inspection and scoped cancellation use:

```
af provider auth status codex-main --kind codex --auth-dir /private/codex
af provider auth cancel codex-main --kind codex --auth-dir /private/codex --recovery-id RECOVERY_ID
```

The versioned status is [provider-auth-v1.json](../schemas/provider-auth-v1.json). Cancellation
returns `cancellation_requested` until the owner observes it. Repeated cancellation of a finished
matching session returns its final state. A stale recovery ID cannot cancel a replacement.

A cancelled, expired, interrupted, unsupported or private-route-unavailable session is blocked,
never authenticated. Restart starts a fresh official flow and requires a fresh host permission;
af never restores a secret challenge from disk or trusts a stale PID. The live process and its
private pipes survive agent-turn interruption, **not a VM restart**. Credentials remain in the
same native auth directory, with the same Provider ID. Native account changes remain subject to
existing Task identity fences.

## Verified provider surfaces and limitations

- Codex 0.159.2's offline app-server schema supports `account/login/start` with
  `chatgptDeviceCode`, `account/login/completed`, and `account/login/cancel`. Initialization comes
  first; no model thread or turn is created. The adapter requires a matching private login ID.
  See [official app-server auth documentation](https://learn.chatgpt.com/docs/app-server#authentication-endpoints).
  That documentation restricts these auth endpoints to local/open-source apps, not commercial
  hosted services. Hosts must assess applicability; this integration is not blanket permission
  to use provider OAuth in a hosted service.
- Claude 2.1.289 supports the dedicated `auth login --claudeai` command and documented code-on-stdin
  fallback for SSH/container hosts. See [official troubleshooting](https://code.claude.com/docs/en/troubleshoot-install#oauth-login-fails-in-wsl2-ssh-or-containers)
  and [network origins](https://code.claude.com/docs/en/network-config#network-access-requirements).
  The provider publishes no versioned JSON login-output protocol. The adapter supports a complete
  bare URL followed by a newline-terminated browser-code instruction or an OSC-8 hyperlink target, including wrapped visible
  labels. Ambiguous/truncated bare URL wrapping is rejected; parameters are never reconstructed.
  A bare URL followed only by an unterminated interactive prompt has no trusted frame boundary
  and is not supported. CLI changes may produce `unsupported` or expire without a challenge
  until the adapter is checked again. These variants have not been live-characterized here.

## Credential-free checks and opt-in live proof

CI uses deterministic CLI stand-ins, anonymous host pipes, synthetic challenge markers and
failure cases. It never initiates OAuth, reads live credentials or calls a paid model.

A live proof is a separate operator-authorized action: verify the installed official CLI version,
use an explicitly selected auth directory and authorized private host, start one grant, privately
complete browser consent, and inspect the non-secret result. Test cancellation and expiry without
publishing the challenge. Do not include URLs, codes, account identities, screenshots of private
consent, raw native output or credential files in a PR. A bounded real inference is separate paid
verification and requires its own already-approved captured allowance.
