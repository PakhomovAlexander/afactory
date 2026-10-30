- The nightly build's test that resolves a rewritten copy of this tree now runs
  `cargo metadata --locked --offline --filter-platform <host>`, with the host triple from
  `rustc -vV`, so it needs only the host platform's crates in the cargo cache and passes on
  Linux CI, whose cache holds only the Linux build's (#483).
