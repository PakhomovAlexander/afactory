# Private-host Provider authentication

This is the explicit host integration for browser-only Provider setup and reauthentication
([ADR-0137](adr/0137-permit-provider-logins-through-private-host-capabilities.md), refined by
[ADR-0138](adr/0138-deliver-native-login-challenges-to-a-verified-private-requester.md)). It partially
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

A host must have verified private delivery. ADR-0138 permits a narrow personal-chat route: the
original requester explicitly approves the exact native login, and the coordinator delivers only
the validated short-lived browser challenge in that requester's verified one-to-one conversation.
The challenge and one-time response may be visible to the assistant and retained in that private
conversation/tool transcript. This is not secret-free messaging. Shared conversations, ordinary
logs, issue text, Task inputs, Worker context, CAS and notes remain excluded. If no private route
exists, stop. Never publish a challenge into a group, or disclose passwords, reusable tokens,
native credential files or account identities through this interface.

Permission is scoped to one login session. It neither approves model inference nor grants new
implementation, plan, publication or continuation authority. Do not run a paid doctor/admission
implicitly. Status and login completion are not usability proof.

## Concrete personal-chat host

`scripts/provider-auth-host.py` is the executable presentation adapter (Linux, Python 3 standard
library). It does not contact a messaging service itself. The coordinator supplies that real
capability and owns human identity, action-time consent, destination and accepted-send receipts.
It launches the command in a retained command session; the human needs only their browser:

```
python3 scripts/provider-auth-host.py codex-main --kind codex \
  --auth-dir /private/codex --af /installed/af \
  --requester-ref ORIGINAL_REQUESTER_64_HEX --coordinator-ref COORDINATOR_64_HEX \
  --delivery verified-private-chat --timeout-secs 600
```

Use actual random 64-character lowercase hexadecimal handles mapped by the coordinator, not the
illustrative placeholders, names or guessed platform IDs. The delivery flag is an explicit
output-routing contract, not evidence of consent. Do not use a shared CI logger or save stdout
to artifacts. This selected output intentionally presents short-lived challenge fields; native
output and the internal af pipes remain separate. PTY input echo is disabled before any request.

1. Verify the original requester's one-to-one destination and obtain specific action-time consent
   for this Provider/native directory, disclosing persistent account access until revoked.
2. Read the bridge's `schema: af/provider-auth-chat@1`, `action: request_permission` record. No
   native login has started. Bind the real approval to the current context and send on stdin:

   ```json
   {"action":"approve","recovery_id":"EXACT_ID","requester_ref":"EXACT_HANDLE","context_id":"EXACT_CONTEXT","approval_ref":"ACTUAL_CONSENT_MESSAGE_ID"}
   ```

3. Receive one `action: challenge`. Send its exact validated URL and Codex code only to that
   original requester. Identify the personal cloud CLI receiving access and preserve the warning:
   continue only if they initiated this login; cancel if an unrelated site/person supplied it.
   The user signs in and approves on the official provider page in their own browser.
4. Only after an accepted private send, acknowledge the actual returned message reference:

   ```json
   {"action":"delivered","recovery_id":"EXACT_ID","requester_ref":"EXACT_HANDLE","delivery_ref":"ACTUAL_SENT_MESSAGE_ID"}
   ```

   A failed or uncertain send is not a receipt; cancel/expire without blind resend. The script
   checks bounded reference syntax, not the messaging platform's truth. That is the coordinator's
   responsibility. Printing a challenge does not automatically acknowledge delivery.
5. Codex needs no response to the bridge. Claude may show one `CODE#STATE` browser response.
   Accept it only from the same requester for this active session and send the `action: code`
   frame below on stdin. The bridge checks the exact active state and never echoes the code.
   Never request passwords, MFA, setup-token output or reusable access/refresh tokens.
6. Report non-secret completion only after native completion. `setup_completed` and `result`
   refer to one result, not separate notifications. Keep the command session running while
   waiting; it survives turns, not VM restarts. Closing stdin cancels ownership. Send the existing
   `action: cancel` frame after approval; before approval close stdin without granting permission.

The bridge's URL parser is intentionally tied to the characterized native endpoints/client/scope
set below. Codex pairing codes use a bounded uppercase/digit/hyphen presentation alphabet, without
assuming a fixed provider code length. Unknown framing fails closed. Both inbound frames and
outbound writes are bounded; a blocked private output has a one-second write timeout and cancels
ownership rather than hanging indefinitely.

This is a host for coordinators with actual private messaging, not a chatbot identity service.
Credential-free checks run with `python3 scripts/test-provider-auth-host.py` and in `make check`.

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

Do not redirect either private pipe raw to a logger or model transcript. The concrete bridge
validates and presents only explicitly authorized fields. A broker must keep the
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
maximum lifetime, not the provider's advertised expiry. Codex's native prompt announces 15-minute
validity; af's deadline can be shorter. No approval, wrong Provider/context, expired grant or non-private delivery
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

Failed native Codex login also has closed, non-secret diagnostic states (all exit 6):
`authentication_invalid_token_response`, `authentication_proxy_configuration_failed`,
`authentication_tls_configuration_failed`, `authentication_rejected` and
`authentication_transport_failed`. The lifetime guard drains bounded native stderr privately
and recognizes only anchored, source-characterized error prefixes. It passes the category through
reserved internal guard exit codes, never native text, response bodies, URLs or tokens. Unknown,
conflicting and oversized errors remain `authentication_failed`; a category is evidence of the
observed native failure class, not authorization to alter network/security settings or retry an
OAuth exchange manually. No native exit value alone can manufacture successful authentication.

A cancelled, expired, interrupted, unsupported or private-route-unavailable session is blocked,
never authenticated. Restart starts a fresh official flow and requires a fresh host permission;
af never restores a secret challenge from disk or trusts a stale PID. The live process and its
private pipes survive agent-turn interruption, **not a VM restart**. Credentials remain in the
same native auth directory, with the same Provider ID. Native account changes remain subject to
existing Task identity fences.

## Verified provider surfaces and limitations

- Codex 0.159.2's official `login --device-auth` command is documented for
  [headless personal installations](https://learn.chatgpt.com/docs/auth#login-on-headless-devices).
  The adapter validates its bounded, version-supported prompt and requires successful native
  exit; no app-server auth endpoint or model turn is used. Codex clears old authentication when
  starting device login, so cancellation can leave an existing context signed out. Native Codex
  also owns its normal diagnostic log. The host never reads or forwards that log. This personal
  CLI flow is not blanket permission to offer provider OAuth as a third-party hosted service.
- Claude 2.1.289 supports the dedicated `auth login --claudeai` command and documented code-on-stdin
  fallback for SSH/container hosts. See [official troubleshooting](https://code.claude.com/docs/en/troubleshoot-install#oauth-login-fails-in-wsl2-ssh-or-containers)
  and [network origins](https://code.claude.com/docs/en/network-config#network-access-requirements).
  The provider publishes no versioned JSON login-output protocol. The adapter supports a complete
  bare URL followed by a newline-terminated browser-code instruction or an OSC-8 hyperlink target, including wrapped visible
  labels. Ambiguous/truncated bare URL wrapping is rejected; parameters are never reconstructed.
  The exact 2.1.289 trailing `Paste code here if prompted > ` prompt is also supported without
  a newline; arbitrary partial prompts remain unsupported. The concrete bridge checks the native
  subscription endpoint `https://claude.com/cai/oauth/authorize`, native client ID
  `9d1c250a-e61b-44d9-88ed-5944d1962f5e`, manual platform callback and known scope set. Custom
  login hints/SSO routes fail closed. The native URL builder and `CODE#STATE` input handler were
  statically characterized in installed 2.1.289. CLI changes need recharacterization. Offline
  checks are not live authentication evidence.

## Credential-free checks and opt-in live proof

CI uses deterministic CLI stand-ins, anonymous host pipes, synthetic challenge markers and
failure cases. It never initiates OAuth, reads live credentials or calls a paid model.

A live proof is a separate operator-authorized action: verify the installed official CLI version,
use an explicitly selected auth directory and authorized private host, start one grant, privately
complete browser consent, and inspect the non-secret result. Test cancellation and expiry without
publishing the challenge. Do not include URLs, codes, account identities, screenshots of private
consent, raw native output or credential files in a PR. A bounded real inference is separate paid
verification and requires its own already-approved captured allowance.
