- The native performance probe no longer removes anything: it creates `probe-work` in the
  scratch it is given and leaves it for its caller. `cli.py probe` runs each sample in a private
  0700 namespace that it holds by a directory handle, below a `--scratch` parent owned by you or
  root and not writable by others unless sticky. It removes the namespace only through that
  handle, confirms the removal, and fails the sample when cleanup is not confirmed; it no longer
  ignores cleanup errors. A lease-bound sample is `lease_expired` only when the Store reports the
  lease expired or fenced. A SQLite, decoding or clock error keeps the operation's own failure,
  with both diagnostics. The state persistence sample finishes its Store reads before the lease
  refresh that starts its region
  ([ADR-0165](docs/adr/0165-own-probe-scratch-in-the-driver-and-take-lease-expiry-only-from-the-store.md)).
