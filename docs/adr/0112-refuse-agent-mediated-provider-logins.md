# ADR-0112: Refuse agent-mediated Provider logins and separate status from usage

**Status:** accepted (2026-09-21). Partially superseded by
[ADR-0137](0137-permit-provider-logins-through-private-host-capabilities.md), which adds an
explicit private-host capability path while preserving the generic terminal-only default.

An interactive Provider login prints an OAuth URL and an authorization code. Both are credentials
in transit, and every reader of the process's standard streams gets them: a pipe, a CI log, a chat
window, an agent transcript. `af provider setup` previously started that login automatically
whenever the named auth context was not authenticated, which made the safe outcome depend on who
happened to be invoking it.

Provider setup therefore starts an official CLI login only when the operator passed `--login` *and*
this process owns an interactive terminal on stdin, stdout and stderr. Both conditions are
refusals, not preferences. Without them setup fails fast with a `human_action_required` result, exit
code 3, and the exact command a human runs at a private terminal; no login child is created. `af`
does not capture, parse, persist or re-emit anything such a login prints, and an already
authenticated context is still registered without any login at all. This narrows when
[ADR-0111](0111-keep-provider-bootstrap-machine-local-and-cross-release-safe.md)'s interactive path
runs; it changes nothing about credential ownership, auth-directory validation, the auth-context
lock, atomic cross-release registry publication, recovery, or machine-local dispatch.

`af provider status` is a fast registry and authentication check. Subscription and quota probing
moves behind `--usage`, and its outcome is a separate axis: an optional probe that times out, is
unsupported, or is absent leaves an authenticated Provider authenticated and exits 7. Status may
report `usable_or_untested` and never `usable`, because only the charged, explicit `af provider
doctor` dispatches and can prove end-to-end usability.

Both commands gain stable versioned JSON — `af/provider-status@1` and `af/provider-setup@1` — whose
closed vocabularies distinguish registration, authentication, usability and usage. Neither document
can represent an account email, an organization or account identity, a credential, OAuth material,
or raw Provider output; quota windows carry only numbers `af` derived. Exit codes 3 through 7 are
documented per condition, with results on stdout and progress and diagnostics on stderr.

## Considered options

- **Keep starting the login whenever authentication is missing.** Rejected: the caller that most
  needs a determinate answer — an automated one — is exactly the caller that must not receive an
  OAuth URL, and an automatic login hands it one.
- **Detect a terminal and carry on silently when there is none.** Rejected: silence turns a security
  boundary into an invisible behaviour difference. The refusal has to be a documented, machine-
  readable outcome with the next action in it.
- **Offer a headless flow: return the OAuth URL or a device code to the caller.** Rejected: that is
  the leak, spelled as a feature. An OAuth proxy or device broker would also make `af` a credential
  intermediary, which no other part of the system is.
- **Add an environment override for non-interactive login.** Rejected: an override exists to be set
  by the automation it protects against, and it would make the boundary untestable.
- **Leave status probing subscriptions by default.** Rejected: the slow, failure-prone half of
  status was demoting authenticated Providers for reasons that had nothing to do with their login.
- **Let status report `usable`.** Rejected: status dispatches nothing. Only the charged doctor
  check can make that claim, and conflating them would make the cheap command look authoritative.

## Consequences

- An agent, CI job, or any non-terminal caller gets a determinate result and a human-readable next
  action, and can never cause an OAuth exchange to be started on its behalf. A caller that holds a
  pseudo-terminal is not such a caller: see the amendment below.
- Operators who relied on `af provider setup` logging them in add `--login` once; the help, the
  logged-out status hint, and the printed next action all spell it.
- `af provider status` is faster by default and no longer shows subscription or quota windows
  without `--usage`; the TUI's explicit refresh still probes them.
- Exit codes 3–7 are part of the `af provider` contract and are pinned by CLI tests; the two JSON
  documents are pinned by schemas and schema-parity fixtures.

## Amended 2026-10-20

An independent audit (finding M8) showed what the terminal check is and is not. `isatty` on all
three standard streams tells a terminal from a pipe, a file or a CI log; it cannot tell a human
from a program. A caller that allocates a pseudo-terminal — `script`, `expect`, `tmux send-keys`,
`ssh -tt`, Python's `pty`, the `portable-pty` crate af's own tests use — passes it, reads the
OAuth URL the login prints and types the code back, and `af` continues to the post-login probe.
The test `a_caller_holding_a_pseudo_terminal_reaches_the_login` in
`crates/af/tests/it/provider_registry.rs` pins exactly that.

The decision stands, and its claim is narrowed to what the check enforces: the terminal-only
login prevents *accidental* capture of the OAuth URL and code — by a pipe, a CI log, a chat or
agent transcript, or a tool call that runs `af` without a terminal. A caller holding a
pseudo-terminal is indistinguishable from a human at a terminal and is out of this boundary's
scope; the consequence above that a non-terminal caller "can never cause an OAuth exchange to be
started on its behalf" holds for non-terminal callers only. A program that must start a login on
a person's behalf uses the private host protocol of
[ADR-0137](0137-permit-provider-logins-through-private-host-capabilities.md), whose permission and
private delivery are explicit capabilities rather than a property of the caller's streams. No
typed confirmation is added: a program holding the terminal could type it as well.
