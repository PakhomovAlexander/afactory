# ADR-0139: Deliver native login challenges to a verified private requester

**Status:** accepted (2026-10-04). Partially supersedes
[ADR-0137](0137-permit-provider-logins-through-private-host-capabilities.md)'s blanket exclusion
of personal assistant transcripts and its Codex app-server login mechanism. All other credential,
context, lifetime, Task and execution-authority fences remain in force.

## Context

The first implementation provided the af side of an anonymous-pipe protocol but no runnable
presentation host. A user of a personal headless installation still could not receive a browser
challenge. Its blanket transcript prohibition also conflated a short-lived, provider-issued
pairing challenge with a reusable access credential. A verified one-to-one coordinator can obtain
action-time consent, deliver the former to its original requester and leave browser consent to the
human, without receiving native access/refresh tokens.

This is an explicit disclosure boundary change, not a claim that tool output is secret or that a
CLI flag authenticates a human. The short-lived challenge and a returned one-time Claude response
may be visible to the coordinating assistant and retained in its private conversation/tool
transcript. A shared conversation, public log, Task input or arbitrary stdout consumer is still
not an authorized recipient. If this private-transcript disclosure is unacceptable, use another
real secure presentation/input host or stop; never fabricate one.

Official Codex documentation supports `codex login --device-auth` on personal remote/headless
machines. Its app-server auth endpoints have separate hosted-service restrictions. Use the native
login command for this personal installation rather than relying on an ambiguous hosted
app-server exception. This does not authorize a commercial service otherwise forbidden by the
provider. Unmodified native CLIs own the OAuth exchange and credential storage.

## Decision

Ship `scripts/provider-auth-host.py`, a one-session Linux presentation adapter. It creates and
retains the existing private anonymous-pipe capabilities and supervised af process. It adds no
network listener, remote server, credential copy, persistent challenge file or provider API.
The coordinator retains its command session across turns, not VM restarts.

The coordinator must independently verify the original human's one-to-one destination and get
action-time permission for the selected Provider and native auth directory, disclosing persistent
access and where credentials will remain. Only then may it submit a grant bound to the current
recovery ID, context ID and opaque requester handle, with the actual consent-message reference.
Launching the bridge, selecting `--delivery verified-private-chat`, inventing a reference or
writing `approved: true` does not establish those facts. The adapter does not pretend to
authenticate a messaging platform or validate a consent receipt it cannot access.

Only closed, validated protocol fields cross its explicitly selected private presentation output:
an official authorization URL, Codex's short-lived user code, and opaque/status metadata. It
rejects unexpected endpoints, token-bearing query parameters, other client IDs, additional scopes,
unrecognized fields, duplicate keys, malformed framing, recipient/session mismatches and replay.
It never reflects provider stdout/stderr, error text, account identity or credentials. Input echo
is disabled when a retained PTY is used. Invalid input yields only a closed bridge error.

The coordinator sends the challenge once to that same verified requester, preserving the
provider's phishing warning and identifying the receiving installation. It acknowledges delivery
only after an accepted private send and supplies the returned message reference. An uncertain or
failed send is not an acknowledgement: cancel or expire without blind resend. The adapter does not
infer delivery from printing output. Cancellation immediately revokes further challenge
presentation, including a challenge already in flight on the af pipe.

Claude's one browser-returned `CODE#STATE` response is admitted only after delivery, for the same
requester/session, with the exact active URL state. It is bounded data on native stdin, never an
argument, shell command, diagnostic or persistent bridge artifact. Passwords, MFA codes, API keys,
setup-token output and access/refresh tokens are not this interface. Browser consent and native
credential storage remain outside the coordinator's input handling.

Codex uses the dedicated official `login --device-auth` operation. Its bounded, version-supported
prompt is parsed as a complete frame; successful native exit and the existing token-free status
probe are both required. There is no app-server model thread, turn or inference. Native cancellation
may leave an old context signed out because native device login first clears that context.

Claude's supported parser includes 2.1.289's exact unterminated manual-code prompt after one
complete URL. It does not treat an arbitrary partial prompt as complete. A changed native CLI
protocol fails closed until characterized and tested; URLs and parameters are never reconstructed.

## Consequences and verification

This closes the presentation gap for a real coordinator with verified private messaging and a
retained command session. It is not an out-of-the-box chatbot service or identity provider. Both
af-side and concrete bridge tests remain credential-free, with actual anonymous pipes and PTY echo
checks. Live proof separately requires real human permission and browser consent.

Successful login remains `authenticated_unverified`: no paid admission, implementation, plan
approval, Task resume, publication or new retry allowance is implied. Issue #122's runtime recovery
stage remains separate. Ordinary af stdout and durable recovery records remain challenge-free.

Sources checked against Codex 0.159.2 and Claude Code 2.1.289:

- [Codex headless authentication](https://learn.chatgpt.com/docs/auth#login-on-headless-devices)
- [Codex 0.159.2 device login](https://github.com/openai/codex/blob/rust-v0.159.2/codex-rs/login/src/device_code_auth.rs)
- [App-server constraints](https://learn.chatgpt.com/docs/app-server#auth-endpoints)
- [Claude authentication](https://code.claude.com/docs/en/authentication)
- [Claude hosted CLI conditions](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use)
