# ADR-0112: Refuse agent-mediated Provider logins and separate status from usage

**Status:** accepted (2026-09-21)

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
  action, and can never cause an OAuth exchange to be started on its behalf.
- Operators who relied on `af provider setup` logging them in add `--login` once; the help, the
  logged-out status hint, and the printed next action all spell it.
- `af provider status` is faster by default and no longer shows subscription or quota windows
  without `--usage`; the TUI's explicit refresh still probes them.
- Exit codes 3–7 are part of the `af provider` contract and are pinned by CLI tests; the two JSON
  documents are pinned by schemas and schema-parity fixtures.
