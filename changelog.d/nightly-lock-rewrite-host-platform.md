- The nightly build's lock-rewrite test resolves `cargo metadata --locked --offline` for the
  host platform only, through a private Cargo home that holds only the host's crates. It no
  longer fails offline on a cache filled only for Linux. It still requires Cargo's stale-lock
  refusal (both the Cargo 1.88 and the newer wording) when only `Cargo.toml` moves.
