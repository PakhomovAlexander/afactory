## What

<!-- One topic per pull request. Say what changes, as a user or a maintainer would see it. -->

## Why

<!-- The problem, run, or decision that motivates it. Link the issue or discussion if there is one. -->

## af task report

<!-- Every change here is made through af Tasks (ADR-0142). Run, from this repository:

       af task report TASK_ID...

     naming the implementation and verification Tasks behind this change (add the same --state
     if you used one), and replace the whole block below, both markers included, with its output.
     The `PR report` check refuses a description without exactly one filled-in block. -->

<!-- af-task-report:v1 -->
<!-- /af-task-report -->

## Checklist

- [ ] `make check` passes locally (fmt, clippy `-D warnings`, `cargo test --locked`, release check).
- [ ] `make review-kernel-container-probes` passes, if `crates/review-sandbox` changed (needs Docker).
- [ ] This change was made through af Tasks (implementation and verification pipelines), and their `af task report` is included above.
- [ ] A change note `changelog.d/<topic>.md`, if the change is user-visible (never an edit to `CHANGELOG.md`).
- [ ] A design change references its ADR: <!-- docs/adr/NNNN-….md, or "not a design change" -->
- [ ] No contract, fixture, gate, budget, or sandbox boundary was weakened to make a test pass.
