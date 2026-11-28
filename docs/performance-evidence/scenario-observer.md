# Scenario observer timing on a shared Mac

## Revisions and runner

- Workload `scenario-observer`
- Baseline commit `f97ac450d3bfd95f6537ebbdc744cd1479db4677`
- Measured candidate commit `f3408f630cb20bb4e129a13e3e9b0b3f0f904a32`
- Measured candidate Git tree `056e89a27ef8eed055b2313cde7edaca8f3cbfdd`
- Measured native Snapshot `sha256:b72dcf53e813de207cab06ff5bca4cab98beb77d2282556be1f93c48827a948c`

The operator's sealed benchmark-tree record binds these measurements to the candidate above.
The earlier series measured commit `125e7eab863aacdf76a4bd3b19498a32c421856d` and tree
`e2d4c93594b293e62fe4393a85167a834f7f304d`. The supplied revision comparison lists this evidence
file as the sole difference between those measured candidate trees. This correction starts from
reviewed Snapshot `sha256:9a2503f15c973477a865fde0db6b6030f79126ab1ce7980cc69df0ca44ed42ca`.
The successor changes only this Markdown file, so the publication tree will reflect a
documentation delta. The measurements remain bound to the exact measured revision above.

All four trials used the same shared Apple Silicon Mac, warmed builds and uninstrumented nextest
with four slots under the `ci` profile. The exclusive overrides in
[nextest configuration](../../.config/nextest.toml) give each selected case all four slots.
The selection covers the five longest exclusive Scenario cases identified for this comparison.
It uses the original public integration test entrypoints and exercises the real 30-second driver
cadence. The profile sets automatic retries to zero.

## Reproduction

Use the repository root at each exact revision and warm both builds before timing. Run the
following operator command first in baseline-then-candidate order and then in
candidate-then-baseline order. Record the command's wall time and exit status, and save
`target/nextest/ci/junit.xml` after each first pass before it can be overwritten by a retry.
Use the per-case elapsed time from JUnit and keep any retry wall time and outcome separately.

```sh
case_prefix='scenario::performance::'
case_1='native_scoped_default30s_service_publishes_one_pr_with_exact_native_accounting'
case_2='native_scoped_default30s_routes_gate_and_review_findings_through_two_fresh_repairs'
case_3='native_scoped_default30s_repairs_rejected_whole_goal_verification_before_publication'
case_4='native_scoped_default30s_service_recovers_a_killed_driver_reservation'
case_5='native_scoped_driver_recovers_transient_preflight_after_process_restart'
filter="test(=${case_prefix}${case_1}) | test(=${case_prefix}${case_2}) | \
test(=${case_prefix}${case_3}) | test(=${case_prefix}${case_4}) | test(=${case_prefix}${case_5})"
cargo nextest run --locked --offline -p af --test it --profile ci --test-threads 4 -E "$filter"
```

For trial 1, the operator ran one manual retry using the same command with
`-E "test(=${case_prefix}${case_4})"`.

## First-pass case and suite wall times

Times are in seconds. Case rows use the exact test name suffixes from the command above.
Suite rows are the recorded command wall times and include runner overhead.

| Case | 1 · Baseline | 2 · Candidate | 3 · Candidate | 4 · Baseline |
| --- | ---: | ---: | ---: | ---: |
| `native_scoped_default30s_service_publishes_one_pr_with_exact_native_accounting` | 212.191 | 177.641 | 175.606 | 178.066 |
| `native_scoped_default30s_routes_gate_and_review_findings_through_two_fresh_repairs` | 161.202 | 129.149 | 143.873 | 129.447 |
| `native_scoped_default30s_repairs_rejected_whole_goal_verification_before_publication` | 152.335 | 130.284 | 131.722 | 134.880 |
| `native_scoped_default30s_service_recovers_a_killed_driver_reservation` | 253.561 (failed) | 203.142 | 203.921 | 203.092 |
| `native_scoped_driver_recovers_transient_preflight_after_process_restart` | 254.227 | 169.408 | 171.842 | 168.872 |
| First-pass suite | 1034.0965380412526 | 810.0902996659279 | 827.2393810832873 | 814.6535048750229 |
| First-pass outcome | Failed | Passed | Passed | Passed |
| Manual retry | 258.5521234171465 (exit 100) | None | None | None |
| Suite including manual retry | 1292.6486614583991 | 810.0902996659279 | 827.2393810832873 | 814.6535048750229 |
| Final outcome | Failed after retry | Passed | Passed | Passed |

The killed-driver reservation case was the only first-pass failure and failed again on the
manual retry. The operator summary supplies pass or fail for each first pass and a numeric exit
code for the manual retry only.

In baseline-then-candidate order the baseline remained unsettled after its retry. In
candidate-then-baseline order both trials passed, with the candidate taking 12.586 s longer.
The passing baseline falls between the two passing candidate times. The paired-timings record
reports a baseline median of 1053.651083166711 s and a candidate median of 818.6648403746076 s.
Its reported difference of 234.98624279210344 s (22.302092841385463%) includes the failed
baseline trial and its retry in the baseline median. The passing pair and the small sample on a
shared host leave aggregate improvement and hosted validation time unproven.

## Record provenance

The following paths are relative to the operator's evidence packet. The packet supplies the
digests below. The raw measurement files and retry JUnit digest were unavailable for independent
verification in this checkout.

| Trial | Raw measurement path | First-pass JUnit SHA-256 | Raw measurement SHA-256 |
| --- | --- | --- | --- |
| 1 | `measurements/scenario-observer-f97ac450d3bf-20261126T170815Z/measurement.json` | `1040d28cf70c203f852a4118e6f45c43ff3d40f08a58c7bd4a054d71b41f82a9` | `005eec987fb31b662f23894c571de4981976c6161b1f2b4ee44a06447628de94` |
| 2 | `measurements/scenario-observer-f3408f630cb2-20261127T003724Z/measurement.json` | `11eb00b57b8a63fd2a2527acdb48ce9f1be96c0316ddd4ae296a8406cc3196b5` | `626fe27b797bb26ac7363b7e75261f20b927b50a4edd6c74c6ec78aa929959ec` |
| 3 | `measurements/scenario-observer-f3408f630cb2-20261127T005142Z/measurement.json` | `150b8d62464e9238a8569bcae1dc2f0f2686c25ad5d396c59c1b369877a3cbae` | `3b5b412be8af28dbcaf5d3ef0ab9a88d360456c3643b581fe02cfc77f4b65bab` |
| 4 | `measurements/scenario-observer-f97ac450d3bf-20261127T010530Z/measurement.json` | `b4f6cb2513e5370367e7a158256dc2d2a629a45b48a601704a6f76a2be05a3da` | `107dae80ebed4dda3087f1cf8b0555fe110782c5678a4bd1a229dcc772c9f6a6` |

| Primary record | SHA-256 |
| --- | --- |
| `continuation-retry1/benchmark-tree.json` | `37f5ecbcdc79c6dde65b0dd1aa565c82d65765292c6267f6b0f659cb328b5014` |
| `continuation-retry1/paired-timings.json` | `e12cbc6fa51799579338c728551ca44d431ce57829ee57e37cbd2ed4e557e1da` |
| `continuation-retry1/show-timings.json` | `35f2c8f6d7109806f2d195588a2ce94eb8e694bb957b1dc744df872ad1a0ee1d` |

## Standalone Show microbenchmark

The standalone CLI Show measurement used one invocation at a time, with 12 samples and 11,979
bytes of output per invocation on each revision. The median fell from 0.028978354297578335 s
at baseline to 0.020760000683367252 s at the measured candidate.

| Sample | Baseline (s) | Candidate (s) |
| ---: | ---: | ---: |
| 1 | 0.03025970794260502 | 0.021055040881037712 |
| 2 | 0.028998833615332842 | 0.020158458966761827 |
| 3 | 0.02884062472730875 | 0.020936000160872936 |
| 4 | 0.029502625111490488 | 0.02020200015977025 |
| 5 | 0.028351375367492437 | 0.02033341722562909 |
| 6 | 0.029227042105048895 | 0.020179999992251396 |
| 7 | 0.028830207884311676 | 0.020387332886457443 |
| 8 | 0.02908304100856185 | 0.02137441700324416 |
| 9 | 0.02910049958154559 | 0.02142737479880452 |
| 10 | 0.02879662485793233 | 0.020777542144060135 |
| 11 | 0.028957874979823828 | 0.020935208071023226 |
| 12 | 0.028690916020423174 | 0.02074245922267437 |

To repeat the standalone measurement, time each revision's
`af scenario show RUN --json --state STATE` in the same isolated fixture and capture stdout
for the byte count. The operator record omits the original fixture recipe and output content
digest, so an exact replay of that microbenchmark requires the original fixture. Show
invocation counts within the five uninstrumented Scenario tests were unavailable.
