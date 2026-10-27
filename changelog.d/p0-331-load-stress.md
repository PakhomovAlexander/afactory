- Test children no longer die of a sibling's stale kill under load (issue #331 item 7): no test
  guard or fixture signals a process or group it does not hold as its own unreaped child any
  more. Inspecting an ID and then signalling it is a race no start time closes (the process can
  be reaped and its ID reused in between), and on a shared gate host the ID space wraps within
  minutes while nextest makes every test a group leader, so such a kill could end a whole sibling
  test with its `af` and `git` children. A test ends what it holds; the fixture Workers, the
  parked `git` and the held Codex stand-in end their own child and themselves once the `af` that
  started them is gone; a laboratory fixture's escaped child ends when its test releases it
  (`tests.support.release_recorded`); the docker stand-in's init ends its container's group, not
  `docker rm`, and a group that ended before its signal is still reaped and reported ended; the
  laboratory's fake supervisor forwards a cancellation only while its child is unreaped (it reaps
  with SIGTERM blocked and forgets the child first); and the shared `Leftovers` guard only waits
  for and reports what a test recorded.
  The telemetry tail's real-time test joins the exclusive tests of `.config/nextest.toml`; no
  wall, retry bound, assertion or fixture budget changed. `make check` keeps running the stress
  proof's own tests (`scripts/test-stress-proof.py`) in its preflight, besides their e2e class.
- `make stress-proof` (`scripts/stress-proof.py`) is the repeatable concurrent-load proof of the
  recurring flakes (issue #331 item 8): it runs the named tests 20 times (`STRESS_RUNS`) through
  the gate's profile beside CPU-bound and process-spawning load workers it starts and kills by
  pid on every exit path, and prints each test's passes, load range and wall time
  (CONTRIBUTING.md).
- The stress proof counts a skipped case and a run nextest ended nonzero as not passed, and a
  load worker that ends on its own, printed with its status and stderr, ends the proof unproven;
  a spawn worker waits out a host's refusal to spawn instead of ending.
- The stress proof of issue #331 item 8, `scripts/stress-proof.py --runs 20 --test-threads 7` on
  a 14-CPU macOS gate host shared with other gates, 14 CPU and 2 spawn load workers beside it,
  run on the rebuilt #331 tree before its last close-out round (that round changed none of
  these tests): every test passed every run, over 42.4 min at a 1-minute load of 27.7 to 191.1.

  | Test | Passes | Load (1-min avg) | Wall s min/median/max |
  | --- | --- | --- | --- |
  | `task_starters::heavy_starter_repair_uses_current_evidence` | 20/20 | 27.7-178.9 | 30.2/39.1/91.3 |
  | `task_repair::interrupted_fix_verification_resumes_same_continuation_and_charges_the_lost_attempt` | 20/20 | 39.1-191.1 | 73.6/77.6/85.9 |
  | `campaign_loop::trusted_reuse_keeps_evidence_satisfaction_current_after_head_change` | 20/20 | 39.1-191.1 | 5.5/8.0/12.2 |
  | `campaign_loop::waiver_remains_current_when_the_subject_advances` | 20/20 | 39.1-191.1 | 5.6/7.8/12.9 |
  | `telemetry_runs::telemetry_tail_follows_a_live_run_within_the_flush_latency` | 20/20 | 39.1-191.1 | 3.9/4.5/6.8 |
  | `task_file::captured_task_kind_packages_keep_business_names_and_domain_acceptance` | 20/20 | 39.1-191.1 | 9.2/12.3/20.9 |
  | `review_plan_refusals::first_run_refusals_name_the_fix` | 20/20 | 39.1-191.1 | 2.4/2.5/3.7 |
