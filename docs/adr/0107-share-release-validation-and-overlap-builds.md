# Share release validation and overlap builds

**Status:** accepted (2026-09-17). Refines ADR-0045 execution scheduling; preserves
its tagged-source checks, consumer validation and signed publication requirements.

RC3 spent 28m 34s in the release workflow after two sequential PR gates. Release
builds waited for validation despite being independent, main repeated Linux checks,
and cached CI still reran long sequential test binaries.

Use one reusable validation workflow for PR, main and tagged release commits.
The release workflow owns main validation, including non-release pushes. For a
release, build all targets alongside Linux/macOS validation; require both branches
and Linux container probes before signing or publication. Use the triggering commit
throughout; the resolver tags that same commit. No PR check substitutes for validation
of the actual release commit. Main pushes are eligible to create a release tag only
when the workspace version differs from the first parent and the changelog section
exists. This refines ADR-0045 eligibility so a later ordinary commit cannot win a
concurrent tag race. A rejected push re-reads the remote: the same commit may resume,
a different commit selects ordinary validation, and an absent/unreadable tag fails.
No tag is overwritten. Workflow groups remain per commit; correctness does not depend
on job queue order.

Retain bounded nextest execution as an opt-in through `make check TEST_RUNNER=nextest`;
ordinary Cargo remains the local and CI default with four test threads, overridable through
`TEST_THREADS` for explicit local experiments. Doctests remain explicit and required,
ignored container probes retain their independent required job, and no retry masks
failed tests. Preserve real-time integration coverage with exclusive scheduling,
and use fixed observations to test comparison verdicts deterministically.

Cache compiled artifacts with compatible restore prefixes and revision-specific saves.
Cargo fingerprints still validate restored artifacts. Cache or test-runner changes
never authorize reuse of a prior verdict. Capture per-step durations, failure outcomes,
cache outcomes and JUnit results; report wall time separately from summed runner time
and leave unavailable billing/token counters unknown. `test-build` compiles ordinary
test targets. Cargo's `test` record also includes doctest compilation/execution;
nextest's separate `doctests` record does too. Neither record is pure execution time.

Required PR status contexts change from `check` and `container-probes` to
`validation / check (ubuntu-latest)` and `validation / container-probes`. Main/tag
contexts use `check / check (ubuntu-latest)` and `check / container-probes`, with
`check / check (macos-latest)` additionally required for releases. Update any branch
protection or ruleset that names the old PR contexts when rolling this out. The target
repository had no protection rules or rulesets when inspected for this PR; no remote
rule mutation is needed. The README badge follows the main Release workflow.

See [release performance](../development/release-performance.md) for the observed
baseline, workflow graph, measurement commands and limits of savings estimates.

Local native-provider probe timeouts block promoting nextest to the required CI gate.
Keep this rollout decision separate from the independent workflow/cache improvements.
