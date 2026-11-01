- `docs/test-quarantine-claw.md`: the Claw confirmation runbook now builds a small image with the
  prebuilt cargo-nextest 0.9.132, runs the clone and the confirmation as uid 1000, holds the
  performance laboratory's machine lock with `flock -n`, and runs detached as `afq-confirm`, as
  its first real run required.
