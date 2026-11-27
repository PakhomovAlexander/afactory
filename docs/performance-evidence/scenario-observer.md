# Scenario observer timing on a shared Mac

## Revisions and runner

- Workload `scenario-observer`
- Baseline commit `f97ac450d3bfd95f6537ebbdc744cd1479db4677`
- Measured candidate commit `125e7eab863aacdf76a4bd3b19498a32c421856d`
- Measured candidate Git tree `e2d4c93594b293e62fe4393a85167a834f7f304d`
- Reviewed native Snapshot `sha256:935ae84ac4611ba6ac7d1042ea2bdff30f756c69c74fa613e10c5b723bbd440a`

The owner supplied these timing records. Before adding this record, rebuilding the Git tree from
the sandbox files with a scratch index seeded at the baseline reproduced the measured tree ID.
That comparison binds the received source to the reported measured content even though the
sandbox lacks the candidate commit object. This record is a documentation addition after timing.

All four trials used the same shared Apple Silicon Mac (14 logical CPUs), a warm build for each
revision and uninstrumented nextest with four slots under the `ci` profile. The exclusive
overrides in [nextest configuration](../../.config/nextest.toml) give each selected case all four
slots. The selection covers the five longest exclusive Scenario cases identified for this
comparison and uses the public integration test entrypoints, including the real 30-second driver
cadence. Retry policy in the profile is zero. Trial 1 had one separate manual retry.

## Reproduction

From the repository root of each named revision, with a warmed build in each checkout, use the
same runner configuration and the following command. Run in baseline, candidate, candidate,
baseline order. Preserve first-pass JUnit and elapsed wall time before retrying a failure.

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

Trial 1's manual retry used the same command with `-E "test(=${case_prefix}${case_4})"`.

## First-pass case and suite wall times

Times are in seconds. Case rows use the exact test name suffixes from the command above.
Suite rows are the recorded command wall times and include runner overhead.

| Case | 1 · Baseline | 2 · Candidate | 3 · Candidate | 4 · Baseline |
| --- | ---: | ---: | ---: | ---: |
| `native_scoped_default30s_service_publishes_one_pr_with_exact_native_accounting` | 212.191 | 209.807 | 187.166 | 180.003 |
| `native_scoped_default30s_routes_gate_and_review_findings_through_two_fresh_repairs` | 161.202 | 154.843 | 137.782 | 132.499 |
| `native_scoped_default30s_repairs_rejected_whole_goal_verification_before_publication` | 152.335 | 188.973 | 143.801 | 132.553 |
| `native_scoped_default30s_service_recovers_a_killed_driver_reservation` | 253.561 (failed) | 231.838 | 214.521 | 205.198 |
| `native_scoped_driver_recovers_transient_preflight_after_process_restart` | 254.227 | 184.131 | 169.237 | 170.219 |
| First-pass suite | 1034.097 | 971.958 | 852.942 | 820.770 |
| Suite including manual retry | 1292.649 | 971.958 | 852.942 | 820.770 |
| Final outcome | Failed after retry | Passed | Passed | Passed |

Trial 1's killed-driver reservation case failed again on the single manual retry, which took
258.552 s and exited with code 100.
All other first-pass cases passed, and trials 2–4 had no retries.

The two trials per revision are a small sample from a shared host with uncontrolled background
load. Both candidate suite times fall between the two baseline times, and the only passing
baseline was faster than either candidate run. The records cannot establish a reliable aggregate
time reduction or whether hosted validation meets a one-hour threshold.

## Record provenance

The owner supplied the following first-pass JUnit and raw timing record SHA-256 digests.
The raw files and any retry JUnit digest were unavailable for independent digest verification
in this checkout.

| Trial (record timestamp, UTC) | First-pass JUnit SHA-256 | Raw measurement SHA-256 |
| --- | --- | --- |
| 1 (20261126T170815Z) | `1040d28cf70c203f852a4118e6f45c43ff3d40f08a58c7bd4a054d71b41f82a9` | `005eec987fb31b662f23894c571de4981976c6161b1f2b4ee44a06447628de94` |
| 2 (20261126T173102Z) | `a3054dbe572aeef78fde60da06329e9f483d96672fa9ffff654ad65fd0eca643` | `8f92f645632fa93a922aad831366d5be45438c49494e0ac9182ef7764830ab7f` |
| 3 (20261126T175118Z) | `c9deef6d07969f0b96bbef0ed0e05d8fe49c36b2ce66d66ed525221d4c57b2d7` | `9fd4538bd25addfe564d9eb6a3a5e922fa7b0e666186f5b219d5b9d202122bbe` |
| 4 (20261126T180531Z) | `80a9880fd0427ac3575e52d357e6b13ef1cebb7dde801a6abf0446cfa24662b3` | `42d557a700c31aca75e5fe7a045517ea31f4dc7b4e59e5e6b3c92d46ddf56c90` |

Owner-supplied paired timings SHA-256
`cf612a69c29092a9febab0d6b6f06419ce12becdd5646337ba7406c1ce68bb5a`.

## Standalone Show microbenchmark

A separate CLI Show workload sampled one invocation at a time, with 12 samples per revision.
Both revisions reported 11,979 bytes of output. The median was 40.120 ms at baseline
(39.313–40.967 ms) and 28.154 ms at the measured candidate (27.538–28.837 ms).
The record provides output size and timings, with no fixture recipe or output content digest.

The timed Scenario cases did not instrument Show subprocess counts. A separate paired count
was unavailable here. This Linux sandbox cannot replay the same Mac configuration, and its
process-tracing probe was denied. The microbenchmark's 12 samples per revision count standalone
Show invocations only. They give no estimate of the subprocess counts in the five Scenario cases.
