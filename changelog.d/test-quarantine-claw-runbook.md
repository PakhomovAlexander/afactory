- `docs/test-quarantine-claw.md`: the Claw confirmation runbook now builds a small image with the
  prebuilt cargo-nextest 0.9.132, runs the clone and the confirmation as uid 1000, holds the
  performance laboratory's anchored production `MachineLock` across Docker in a host Python
  parent, and runs detached as `afq-confirm`. Host imports use a fresh, verified `COMMIT` archive
  of the whole authority package outside container mounts; dirty authority bytes or executable
  bits refuse before import or Docker. Cleanup stays inside the fresh host-only directory without
  following links.
  Preparation uses `sudo chown` only for non-root SSH users other than uid 1000. The launch checks
  the required root-owned inode anchor, binds its exact identity directly to acquisition without
  rereading or enabling lock creation, and checks the experiment fence under the lock, refusing
  missing, unsafe or replaced coordination state and requiring operator provisioning/recovery
  ([ADR-0240](../docs/adr/0240-execute-performance-experiments-independently-on-a-locked-vm.md));
  a busy-lock retry repeats only the launch, preserving the prepared checkout.
- Both production executor lock paths bind the required anchor identity without rereading or
  creating a lock; explicit test directories still need no installed anchor. Confirmation fences
  before Docker and settles its fence only after a successful inventory confirms `afq-confirm`
  is absent, including stopped containers and `--rm` cleanup. Parent death, Docker launch failure,
  inspection failure or a remaining container retains the fence for operator recovery; a nonzero
  confirmation result with confirmed container absence releases it and keeps that exit status.
