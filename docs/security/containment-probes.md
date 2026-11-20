# Containment probes: the malicious check

The record of which containment probes are discharged, which do not apply to what GA ships, and
which are still open (none at the time of writing), kept so that a probe nobody has run is never
mistaken for a probe that passed.
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

## Status — provider boundary and native Task routing exercised

Two test files carry this case. `crates/af-sandbox/tests/malicious_check.rs` runs against
the `trusted_local` provider — a materialized copy of a snapshot in a temporary directory,
**not security isolation**, and its tests do not pretend otherwise.
`crates/af-sandbox/tests/container_probes.rs` runs the probes that need real isolation
against `ContainerProvider` with a live daemon: one bind (the sandbox), `--network=none`, no
inherited environment, image pinned by manifest digest. The
`make review-kernel-container-probes` target, full `make check` and the selected e2e gate run them
and the native Task check route, including a timeout probe that asserts the daemon-owned container
is gone before execution returns; they are `#[ignore]`d in the ordinary test run.
The `container-probes` job is green on
the historical baseline: Release run `39396278192` at `c79f03c0` reports
`check / container-probes: success`. That result does not prove the current native replacement. Where
the target is invoked, a missing daemon is a hard failure, never a skip — an unrun probe must not
look like a passed one.

| Probe | State | Why |
|---|---|---|
| review input immutability | **discharged** | a node runs against a materialized copy and capture already happened, so the snapshot being reviewed cannot be altered by anything the node does |
| canonical checkout on disk | **discharged (container)** | the same hostile command as the `trusted_local` test, and this time the assertion is real: the absolute-path write does not reach the checkout — `worktree_state` identical before and after |
| credentials | **discharged** | the check runner (`af_check::CheckRunner`), not the sandbox, clears the environment and rebuilds it from an allowlist, so a token in the kernel's own environment cannot leak by being forgotten in a denylist — asserted on what a real check process receives, and for a container run by `an_external_provider_gets_only_portable_and_explicit_environment` in `crates/af-check/src/runner.rs` plus the runtime's `--env-file /dev/null` |
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

Native Task Pipeline `/6` uses `CodeTaskDomain::checks`, also delegated by `ReviewTaskDomain`.
`require_container = true` selects the existing shared `ContainerProvider`, pins `DEFAULT_IMAGE`,
probes usability within the Attempt deadline and admits its actual isolation with `Policy::safe()`.
An absent/unusable runtime refuses before any project command; there is no local fallback.
`CheckRunner::run_with_observed` keeps typed argument validation, then dispatches through
`exec_evidenced_controlled`. The shared provider retains no-network, no-credential, capability,
process and cleanup restrictions. Private container HOME, TMPDIR, scratch and cache are tmpfs
mounts. `[warm]` and native host-toolchain mappings remain refused with required containment.
Worker environment authority remains separate from check containment.

The live native regression is
`task_review_execute_checks::a_container_check_executes_on_the_native_task_host` in
`crates/af-runtime/tests/it`. It requires `Availability::Usable`, runs a real check through the
native Task host and Review domain, asserts satisfied acceptance, and reads durable
`af/TaskCheckContainment@1` evidence linked to the exact `af/TaskCheckReceipt@2`, captured policy
and Snapshot. That evidence states admitted Container isolation, the pinned default image and
observed execution. Failed-command and source-mutation controls use the same live route and
cannot produce a passing Review. The command checks private writable runtime paths and an
unreachable host marker. The captured source remains unchanged. Its reviewer is an in-process
fixture; this proves contained check dispatch, not contained model execution.

`make review-kernel-container-probes` explicitly selects the native regression with nextest
`--no-tests fail` through `scripts/nextest-gate.py --repeat 1`. This existing single-run mode
preserves the live proof's no-rerun policy. Inside `scripts/verify.sh`, the existing
runner places reports in the gate-owned writable store outside the read-only source Snapshot;
in a writable checkout it publishes the normal `target/nextest/ci/junit.xml`. The sealed-source
fixture in `scripts/test-nextest-gate.py` executes the exact Makefile recipe against a tiny
ignored test and checks its JUnit and source immutability. That fixture proves report routing,
not containment. The test catalog audits that exact selector as an ignored `live-container`
e2e test. The target still runs all existing sandbox-level live probes and the timeout/reap
control. Full `make check` invokes it after all full prerequisites succeed, including the
ordinary Rust build/test and Python suites; this ordering holds under parallel Make.
`scripts/verify.sh --profile full` and bare `verify.sh` both reach that Make graph. Standalone
`make test` remains Rust-only, while FAST and selective e2e execution retain their existing
single invocation. A failed prerequisite, unavailable runtime or failed probe fails the gate;
no second ordinary full Rust run is added.

The shared tiny-fixture regression also executes the actual FULL wrapper/Make graph, with
controlled sandbox bodies and real ordinary/ignored Rust selections. It proves sequential and
parallel ordering, exact single execution, missing-runtime/timeout/native failure propagation,
gate-owned JUnit and unchanged source bytes/modes. The class-runner regression separately checks
standalone `make test` in both runner modes and failed-build ordering. These routing fixtures
do not prove containment; the captured FULL gate must execute the real live test.

The ordinary non-container regression
`a_required_container_check_refuses_a_missing_runtime_before_any_local_command` gives a child
process a PATH without any runtime and proves hard admission refusal, no local check receipt and
no reviewer dispatch. It is negative admission evidence, never positive containment proof.

A successful provider return means observed execution, not a successful check. Check results
still determine acceptance; source mutation is refused by sealing. Provider execution failure
records refusal only after confirmed cleanup. Unconfirmed cleanup fails the Attempt and preserves
its private roots for the operator without sealing or deleting a possibly live bind.

The native live test must actually execute with a usable runtime in the host gate. Listing,
mocking, static inspection and the historical provider receipt above do not discharge that
obligation. Local repair validation ran `make review-kernel-container-probes` against a usable
runtime: seven sandbox probes, one timeout-cleanup control and one native Task/Review regression
passed. The native regression executed three real contained checks; its in-process reviewer
fixture does not prove model containment. This is actual live execution evidence, separate from
listing and negative missing-runtime proof. A preceding full attempt passed 3,758 ordinary Rust
tests but was cancelled with Python interrupted and this live target omitted. It is not a passed
FULL check. The repaired candidate requires its own complete captured FULL and Markdownlint
receipts, actual live native execution and independent correctness/evaluation before delivery.
