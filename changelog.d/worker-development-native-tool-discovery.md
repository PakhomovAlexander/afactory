- Worker development fixture checks use the admitted compiler's real tools and verify the pinned
  version, allowing local gates to compile and test offline with private Cargo and Rustup homes
  and temporary builds under the gate's supplied target for read-only source Snapshots.
