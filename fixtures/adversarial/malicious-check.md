# Case: malicious check

**Phase 2.** Discharges: *"Every executable node uses a safe Execution Binding; a malicious
check/helper cannot touch a host marker, credentials, the canonical checkout, or undeclared
network"* and *"Safe bindings expose no reusable credential bytes."*

## Why there is nothing to capture

`checks.sh` runs `bash -c "$cmd"` in the working directory with the caller's full environment.
Every command in a check list already has whatever the operator has. There is no boundary to
test, so this case describes one the kernel must create.

## Setup

A project check list whose commands are hostile in the five ways that matter. The check is
declared as an ordinary build/test step and *also does its job*, so a run that only looks at
pass/fail sees nothing wrong.

| Probe | Attempt |
|---|---|
| host marker | read and modify a file planted outside the sandbox root (`$HOME/.review-host-marker`) |
| canonical checkout | write to the source checkout the review targets |
| credentials | read the token used for authenticated acquisition; re-emit it base64'd and split across two artifacts |
| undeclared network | connect to a host not in the binding's allowlist, and exfiltrate over an allowed one |
| escape via argument | pass `--config`, a leading option, and `@response-file` through an untrusted artifact value |

## Required behavior

- Every probe **fails at the boundary**, not by convention: the host marker is unreadable and
  unmodified, the checkout is unwritable, the undeclared connection is refused.
- The credential is never present as reusable bytes inside the sandbox. The transformed-secret
  attempt produces no artifact containing it in any encoding, and the allowed-egress attempt
  carries no derivative of it.
- The privileged operation the check legitimately needs is performed by the **broker**, which
  emits a receipt proving only allowed operations ran.
- Argument injection is rejected by the typed argument slots — `--config`, leading options and
  `@...` are refused as *option* positions while the same bytes remain acceptable as a value.
- The check's own pass/fail result is still recorded normally. Containment is not a verdict.
- The run fails closed if the sandbox provider cannot enforce any of the above — a
  `trusted_local` worktree must not be silently accepted where isolation was required.

## What failure looks like

A green run. The check passes, the review converges, and the only evidence of the escape is on
a host nobody inspects. This is the case that decides whether project-supplied check commands
can be treated as data.

## Status — discharged at the provider boundary; routing and broker still open

Two test files carry this case. `crates/review-sandbox/tests/malicious_check.rs` runs against
the `trusted_local` provider — a materialized copy of a snapshot in a temporary directory,
**not security isolation**, and its tests do not pretend otherwise.
`crates/review-sandbox/tests/container_probes.rs` runs the probes that need real isolation
against `ContainerProvider` with a live daemon: one bind (the sandbox), `--network=none`, no
inherited environment, image pinned by manifest digest. Those probes run in CI
(`make review-kernel-container-probes`) and are `#[ignore]`d in the ordinary test run; where
they are invoked, a missing daemon is a hard failure, never a skip — an unrun probe must not
look like a passed one.

| Probe | State | Why |
|---|---|---|
| review input immutability | **discharged** | a node runs against a materialized copy and capture already happened, so the snapshot being reviewed cannot be altered by anything the node does |
| canonical checkout on disk | **discharged (container)** | the same hostile command as the `trusted_local` test, and this time the assertion is real: the absolute-path write does not reach the checkout — `worktree_state` identical before and after |
| credentials | **discharged** | the environment is cleared and rebuilt from an allowlist, so a token in the kernel's own environment cannot leak by being forgotten in a denylist |
| argument injection | **discharged** | typed slots refuse an untrusted value in an option position, asserted end-to-end through the check runner |
| host marker | **discharged (container)** | a marker planted outside the sandbox is unreadable and unmodified — the absolute path names nothing inside the container |
| undeclared network | **discharged (container)** | `--network=none` leaves no route out and no resolver, so the refusal is immediate rather than a timeout |

Each container probe is paired with a control: the same provider runs a typed check command —
resolved exactly as the check runner validates it — inside the container, the check does its
work, and the work lands in the sandbox bind. So the probes fail for isolation reasons, not
because the container runs nothing.

What "discharged (container)" claims is the *provider boundary*, no more. `trusted_local`
still provides none of it, and `admit` still refuses a safe pipeline on that provider — that
refusal remains tested. Open here, by name:

- **Routing.** The kernel does not yet execute project checks through the container provider
  inside a pipeline run; the probes drive the provider directly. That wiring is Phase 4, and
  this row is where it gets checked off.
- **The broker half.** The transformed-secret probe (re-emit the token base64'd across two
  artifacts) and exfiltration-over-allowed-egress need a broker that holds credentials outside
  the sandbox and emits receipts. There is no broker yet.

**Do not weaken this case as the wiring lands.** Marking the routing or broker rows satisfied
on the strength of the provider probes would be exactly the quiet redefinition this file warns
about.
