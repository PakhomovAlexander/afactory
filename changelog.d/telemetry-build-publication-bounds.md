- Separate locked telemetry dependency fetching from offline, network-disabled compilation;
  bound all writable build state on a verified 8 GiB filesystem within one deadline. Admit
  only confined bounded binary bytes, and keep tool provenance outside candidate mounts.
  Include collection status, position and incomplete records in locked peak-capacity
  reservations and final accounting. Extend the real Docker CI smoke with small-budget
  storage, network and output attacks. Hosted CI remains required before rollout
  ([ADR-0215](docs/adr/0215-contain-telemetry-experiments-and-collect-only-results-verified-under-their-authority.md)).
