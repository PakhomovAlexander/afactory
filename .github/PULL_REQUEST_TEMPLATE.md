## What

<!-- One topic per pull request. Say what changes, as a user or a maintainer would see it. -->

## Why

<!-- The problem, run, or decision that motivates it. Link the issue or discussion if there is one. -->

## Checklist

- [ ] `make check` passes locally (fmt, clippy `-D warnings`, `cargo test --locked`, release check).
- [ ] `make review-kernel-container-probes` passes, if `crates/review-sandbox` changed (needs Docker).
- [ ] A change note `changelog.d/<topic>.md`, if the change is user-visible (never an edit to `CHANGELOG.md`).
- [ ] A design change references its ADR: <!-- docs/adr/NNNN-….md, or "not a design change" -->
- [ ] No contract, fixture, gate, budget, or sandbox boundary was weakened to make a test pass.
