- Separate locked telemetry dependency fetching from offline, network-disabled compilation;
  bound all writable build state on a verified 8 GiB filesystem within one deadline. Admit
  only confined bounded binary bytes, and keep tool provenance outside candidate mounts.
  Include collection status, position and incomplete records in locked peak-capacity
  reservations and final accounting. Extend the real Docker CI smoke with small-budget
  storage, network and output attacks. Hosted CI remains required before rollout
  ([ADR-0215](docs/adr/0215-contain-telemetry-experiments-and-collect-only-results-verified-under-their-authority.md)).

Reserve rounded history allocation before downloaded staging, authority expansion and retained/import
copies, including directories, parent growth and bindings. Validate bounded regular authority Git
objects and the complete path set before materializing. Keep interrupted owned bytes accounted until
locked cleanup, without evicting results. Preserve diagnostic reader uncertainty from launch through
namespace release and profile shutdown; retain scratch and writable evidence and prevent later
execution when any reader may remain live. Add actual-filesystem collection and every-reader
interruption regressions without changing persisted formats, caps, verdicts or release metadata.

Keep allocation fixtures in owned temporary scratch outside the source tree, including when
source is read-only. Preserve the shared authority-materialization interruption boundary after
the complete coexistence reservation, using the same bounded immutable Git-object admission.

Reuse the bounded immutable Git authority buffers within one collection operation after the full
coexistence reservation, preserving the materialization interruption entry point. Import captures
fresh confined authority bytes after verification, checks their complete digest/count/registry
identity, and retains those immutable buffers through the retention entry point. Unbound or
corrupted prepared evidence is refused; retained and imported output are still read back. No
preparation survives an operation. During each write-free locked allocation calculation, examine
each parent once and use its single filesystem allocation unit for unchanged rounding. All root
scans, simultaneous-copy reservations, terminal reserve and cleanup boundaries remain enforced.
These changes address measured redundant admission work before the unchanged one-second expiry
test's retention phase; hosted Python and committed whole-source verification remain required.
