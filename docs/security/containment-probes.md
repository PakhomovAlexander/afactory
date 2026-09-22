# Containment probes: the malicious check

The record of which containment probes are discharged, which do not apply to what GA ships, and
which are still open, kept so that a probe nobody has run is never mistaken for a probe that
passed.
[`non-goals.md`](../non-goals.md) relies on it for the claim that the open probes are recorded
as open rather than claimed covered.

The probes below discharge *"Every executable node uses a safe Execution Binding; a malicious
check/helper cannot touch a host marker, credentials, the canonical checkout, or undeclared
network"* and *"Safe bindings expose no reusable credential bytes."*

## Setup

A project check list whose commands are hostile in the five ways that matter. The check is
declared as an ordinary build/test step and *also does its job*, so a run that only looks at
pass/fail sees nothing wrong.

| Probe | Attempt |
|---|---|
| host marker | read and modify a file planted outside the sandbox root (`$HOME/.review-host-marker`) |
| canonical checkout | write to the source checkout the review targets |
| credentials | read the token used for authenticated acquisition |
| undeclared network | connect to a host outside the sandbox |
| escape via argument | pass `--config`, a leading option, and `@response-file` through an untrusted artifact value |

## Required behavior

- Every probe **fails at the boundary**, not by convention: the host marker is unreadable and
  unmodified, the checkout is unwritable, the undeclared connection is refused.
- The credential never exists as reusable bytes inside the sandbox, so there is nothing in there
  to read, re-encode or carry out.
- GA performs no brokered privileged operation: GA ships without a Broker
  ([ADR-0113](../adr/0113-ga-reads-only-what-ga-writes.md)), so no credential is held outside the
  sandbox on a check's behalf and no receipt stands in for one.
- Argument injection is rejected by the typed argument slots — `--config`, leading options and
  `@...` are refused as *option* positions while the same bytes remain acceptable as a value.
- The check's own pass/fail result is still recorded normally. Containment is not a verdict.
- The run fails closed if the sandbox provider cannot enforce any of the above — a
  `trusted_local` worktree must not be silently accepted where isolation was required.

## What failure looks like

A green run. The check passes, the review converges, and the only evidence of the escape is on
a host nobody inspects. This is the case that decides whether project-supplied check commands
can be treated as data.

## Status — provider boundary discharged; live Gate routing still open

Two test files carry this case. `crates/review-sandbox/tests/malicious_check.rs` runs against
the `trusted_local` provider — a materialized copy of a snapshot in a temporary directory,
**not security isolation**, and its tests do not pretend otherwise.
`crates/review-sandbox/tests/container_probes.rs` runs the probes that need real isolation
against `ContainerProvider` with a live daemon: one bind (the sandbox), `--network=none`, no
inherited environment, image pinned by manifest digest. The
`make review-kernel-container-probes` target and dedicated CI job run them and the live v3 Gate
route, including a timeout probe that asserts the daemon-owned container is gone before execution
returns; they are `#[ignore]`d in the ordinary test run. The `container-probes` job has not yet
produced green evidence for this branch. Where the target is invoked, a missing
daemon is a hard failure, never a skip — an unrun probe must not look like a passed one.

| Probe | State | Why |
|---|---|---|
| review input immutability | **discharged** | a node runs against a materialized copy and capture already happened, so the snapshot being reviewed cannot be altered by anything the node does |
| canonical checkout on disk | **discharged (container)** | the same hostile command as the `trusted_local` test, and this time the assertion is real: the absolute-path write does not reach the checkout — `worktree_state` identical before and after |
| credentials | **discharged** | the check runner (`review_check::CheckRunner`), not the sandbox, clears the environment and rebuilds it from an allowlist, so a token in the kernel's own environment cannot leak by being forgotten in a denylist — asserted on what a real check process receives, and for a container run by `an_external_provider_gets_only_portable_and_explicit_environment` in `crates/review-check/src/runner.rs` plus the runtime's `--env-file /dev/null` |
| argument injection | **discharged** | typed slots refuse an untrusted value in an option position, asserted end-to-end through the check runner |
| host marker | **discharged (container)** | a marker planted outside the sandbox is unreadable and unmodified — the absolute path names nothing inside the container |
| undeclared network | **discharged (container)** | `--network=none` leaves no route out and no resolver, so the refusal is immediate rather than a timeout |
| transformed secret | **not applicable at GA** | the half that re-emits a credential base64'd across two artifacts was written for a Broker holding it outside the sandbox; GA ships without a Broker ([ADR-0113](../adr/0113-ga-reads-only-what-ga-writes.md)), and a check's environment carries no token to transform |
| exfiltration over allowed egress | **not applicable at GA** | there is no allowed-egress allowlist to exfiltrate over: a container run is `--network=none`, so every connection is undeclared and refused |

Each container probe is paired with a control: the same provider runs a typed check command —
resolved exactly as the check runner validates it — inside the container, the check does its
work, and the work lands in the sandbox bind. So the probes fail for isolation reasons, not
because the container runs nothing.

What "discharged (container)" claims is the *provider boundary*, no more. `trusted_local`
still provides none of it, and `admit` still refuses a safe pipeline on that provider — that
refusal remains tested.

Pipeline format v3 now routes every root Gate through its explicit Execution Binding. Container
bindings require a project image pinned by digest. Provider usability and isolation admission are
recorded durably before `RunReport@6`; either failure stops the Gate before a project command
runs. A recording runtime asserts that Gate dispatch builds the exact container invocation:
portable declared environment without the host `PATH`, the caller's numeric UID:GID, and a unique
reaping name. A timed-out runtime client is followed by bounded `rm -f`; if cleanup cannot be
confirmed, the Gate reports the preserved sandbox path and returns incomplete without sealing a
possibly live bind. The
ignored live pipeline control and lower-level probes use `CheckRunner::run_with`; the new CI job
must turn that wiring into live evidence before Gate routing is called discharged. Open here, by
name — one item:

- **Live Gate routing.** The target and CI job exist, but no green run of them is recorded for
  this branch. A green `container-probes` job closes this item.

**Do not weaken this case as the wiring lands.** Marking the live-routing row satisfied on the
strength of the provider probes would be exactly the quiet redefinition this file warns about.
A probe recorded as not applicable stays that way only while GA has no Broker and no allowed
egress; whichever lands first reopens its half here.
