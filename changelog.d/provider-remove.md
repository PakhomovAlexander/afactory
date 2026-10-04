- `af provider remove ID...` drops named Providers from the machine-local registry through the
  same locked, atomic publication `add` uses. Auth directories and their logins are left
  untouched, and the previous registry is preserved. It also repairs a registry made invalid by a
  deleted auth directory, runs in the invoking release from any repository, and refuses ambient
  IDs by name. In the browser, `d` on a registered Provider fills the `:` line with the command
  ([ADR-0136](docs/adr/0136-remove-a-registered-provider-by-id.md)).
