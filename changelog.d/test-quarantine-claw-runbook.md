- `docs/test-quarantine-claw.md`: the Claw confirmation runbook now builds a small image with the
  prebuilt cargo-nextest 0.9.132, runs the clone and the confirmation as uid 1000, holds the
  performance laboratory's machine lock with `flock -n`, and runs detached as `afq-confirm`.
  Preparation uses `sudo chown` only for non-root SSH users other than uid 1000. The launch checks
  the experiment fence under the lock and requires operator recovery when fenced
  ([ADR-0240](../docs/adr/0240-execute-performance-experiments-independently-on-a-locked-vm.md));
  a busy-lock retry repeats only the launch, preserving the prepared checkout.
