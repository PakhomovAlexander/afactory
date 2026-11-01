- `docs/test-quarantine-claw.md`: the Claw confirmation runbook now builds a small image with the
  prebuilt cargo-nextest 0.9.132, runs the clone and the confirmation as uid 1000, holds the
  performance laboratory's anchored production `MachineLock` across Docker in a host Python
  parent, and runs detached as `afq-confirm`. Host imports use a fresh, verified `COMMIT` archive
  of the whole authority package outside container mounts; dirty authority sources refuse, and
  cleanup stays inside the fresh host-only directory without following links.
  Preparation uses `sudo chown` only for non-root SSH users other than uid 1000. The launch checks
  the required root-owned inode anchor, binds its exact identity directly to acquisition without
  rereading or enabling lock creation, and checks the experiment fence under the lock, refusing
  missing, unsafe or replaced coordination state and requiring operator provisioning/recovery
  ([ADR-0240](../docs/adr/0240-execute-performance-experiments-independently-on-a-locked-vm.md));
  a busy-lock retry repeats only the launch, preserving the prepared checkout.
