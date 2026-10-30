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

## Lines added

<!-- Run, from this repository on this branch, with its base fetched:

       python3 scripts/pr-lines.py

     (`--base REF` for a base other than origin/main) and replace the whole block below, both
     markers included, with its output: the lines this pull request adds, per directory, as a
     title with the total and a table (ADR-0195). The `PR report` check renders the block again
     from the pull request's commits and refuses a description whose block is missing or
     differs; after a push, run it again and replace the block when it changed. -->

<!-- af-pr-lines:v1 -->
<!-- /af-pr-lines -->

## Test quarantine

<!-- Run, from this repository on this branch, with its base fetched:

       python3 scripts/pr-quarantine.py

     (`--base REF` for a base other than origin/main) and paste its output here, both markers
     included: the tests this pull request adds to `.config/test-quarantine.json`, with their
     state, expiry and why (ADR-0268). When the script says no block is needed, delete this
     section. The `PR report` check renders the block again from the pull request's commits and
     refuses a description whose block is missing, differs, or is there when no quarantined
     test is added. -->

## Checklist

- [ ] `make check` passes locally (fmt, clippy `-D warnings`, `cargo test --locked`, release check).
- [ ] `make review-kernel-container-probes` passes, if `crates/review-sandbox` changed (needs Docker).
- [ ] This change was made through af Tasks (implementation and verification pipelines), and their `af task report` is included above.
- [ ] The `Lines added` block above is `scripts/pr-lines.py`'s output for this branch as pushed.
- [ ] A change note `changelog.d/<topic>.md`, if the change is user-visible (never an edit to `CHANGELOG.md`).
- [ ] A design change references its ADR: <!-- docs/adr/NNNN-….md, or "not a design change" -->
- [ ] No contract, fixture, gate, budget, or sandbox boundary was weakened to make a test pass.
