.PHONY: prereqs release-check preflight-check check fmt lint test-build test test-repeat stress-proof release review-kernel-container-probes

# What this machine needs before `make check`, one line per tool with the command that installs
# a missing one (issue #161): python3, git 2.30+, the rustup toolchain rust-toolchain.toml pins,
# cargo-nextest, cc, npx, minisign and 30 GiB free where target/ builds. It changes nothing.
prereqs:
	@python3 -I scripts/prereqs.py

# nextest is the gate (ADR-0124): one process per test, scheduled across every test binary.
# `TEST_RUNNER=cargo` keeps the sequential libtest path for comparison. TEST_THREADS bounds
# concurrent tests, not compiler jobs: four on a four-core runner, half the cores elsewhere,
# because these tests spawn real process trees. After nextest, whatever its status, the gate
# entry prints scripts/test-time-report.py's summary of the JUnit it just wrote into its own
# private store, then publishes it to target/nextest/ci/junit.xml (removed when the run starts,
# and when it ends without a JUnit). Before it fails, it reruns each test that failed in the
# parallel run once, alone (at most 25 tests, 15 minutes): a test that passes alone is printed
# `flaky:` and does not fail the step, one that fails twice does, with nextest's exit status.
# Every step names its stage (`--stage`), which a Task check's report records (CONTRIBUTING.md).
# scripts/test-classes.py starts that run without the tests .config/test-quarantine.json lists
# (ADR-0268); with the file empty the selection is unchanged.
TEST_RUNNER ?= nextest
TEST_THREADS ?= $(shell python3 -c 'import os; print(max(4, (os.cpu_count() or 4) // 2))')
CI_STEP = python3 scripts/ci-step.py
BASE_EVIDENCE = $(if $(AF_GATE_NEXTEST_RUN),$(AF_GATE_NEXTEST_RUN)/base-evidence,target/base-evidence)

# Run unprivileged: preflight proves a chmod-sealed source rejects writes.
# Root/CAP_DAC_OVERRIDE bypass that seal and intentionally fail the precondition.
check: preflight-check fmt lint test perf-python-tests perf-feature-tests release-check

fmt:
	$(CI_STEP) --stage lint fmt cargo fmt --all -- --check

# The default-off diagnostic features (ADR-0174) and the calibration loop (ADR-0181) are compiled
# and linted as the laboratory's diagnostic and calibration builds use them, and the trace
# recorder's and the loop's feature-on tests run through perf-feature-tests (full gate) or the
# selected perf class; no ordinary product build enables them.
# The calibration builds of the probe and of `af` are built and executed too (ADR-0182).
lint:
	$(CI_STEP) --stage lint lint cargo clippy --all-targets --locked -- -D warnings
	$(CI_STEP) --stage lint lint-diagnostic-features cargo clippy --locked -p review-pipeline -p af --example perf-probe --bin af --features review-pipeline/perf-trace,review-pipeline/perf-control,review-pipeline/perf-calibration,af/perf-trace,af/perf-control,af/perf-calibration -- -D warnings

test-build:
	$(CI_STEP) --stage build test-build cargo test --locked --no-run

test: test-build
ifeq ($(TEST_RUNNER),nextest)
	$(CI_STEP) --stage tests test python3 scripts/test-classes.py nextest-gate -- --locked --profile ci --test-threads $(TEST_THREADS)
else ifeq ($(TEST_RUNNER),cargo)
	$(CI_STEP) --stage tests test cargo test --locked -- --test-threads=$(TEST_THREADS)
else
	$(error TEST_RUNNER must be nextest or cargo)
endif

# Open the release PR for VERSION (bump + CHANGELOG section). Merging it is the release: the
# workflow tags, checks, builds, signs, and publishes. COMPAT states authority compatibility.
release:
	scripts/release.sh "$(VERSION)" --compat "$(COMPAT)" --profile "$(or $(PROFILE),fast)"

# Run the tests NEXTEST_FILTER selects NEXTEST_REPEAT times in a row under the gate's scheduling,
# stopping at the first run that fails and never rerunning anything: the proof that a flake is
# fixed (CONTRIBUTING.md), e.g. make test-repeat NEXTEST_FILTER='test(/::task_heavy::/)'.
NEXTEST_REPEAT ?= 20
NEXTEST_FILTER ?= all()
test-repeat:
	python3 scripts/nextest-gate.py --repeat $(NEXTEST_REPEAT) --locked --profile ci --test-threads $(TEST_THREADS) -E "$(NEXTEST_FILTER)"

# The concurrent-load stress proof of the recurring flakes (issue #331, CONTRIBUTING.md): every
# test scripts/stress-proof.py names runs STRESS_RUNS times through the gate's profile beside
# STRESS_LOAD CPU-bound and STRESS_SPAWN process-spawning workers it starts and kills by pid,
# never rerun or stopped early, and it prints each test's passes, load range and wall.
STRESS_RUNS ?= 20
STRESS_LOAD ?= $(shell python3 -c 'import os; print(os.cpu_count() or 4)')
STRESS_SPAWN ?= 2
stress-proof:
	python3 -I scripts/stress-proof.py --runs $(STRESS_RUNS) --load-workers $(STRESS_LOAD) --spawn-workers $(STRESS_SPAWN) --test-threads $(TEST_THREADS)

# Live containment and the v3 Gate route. These stay outside `make check` because a missing
# daemon is a hard failure here, never a skip disguised as success.
review-kernel-container-probes:
	cargo test --locked -p review-sandbox --test container_probes -- --ignored
	cargo test --locked -p review-sandbox container::tests::a_timed_out_container_is_removed_before_execution_returns -- --ignored --exact
	cargo test --locked -p review-pipeline --test it task_campaign_review::host::domain::a_container_gate_executes_on_the_task_host -- --ignored --exact

# Telemetry's overhead gate (design §12, docs/telemetry.md "Performance"): `off` against `spans`
# on the three pinned bench fixtures with a release build, 20 randomized pairs each; the script
# exits 3, failing the target, when a median overhead passes 2%, an interval's upper bound 3% or
# the drain latency's median difference 10 ms. `make check` runs only its one-pair smoke, the nextest test
# `telemetry_overhead_smoke`, so the full gate stays outside the check's wall.
.PHONY: telemetry-gate
telemetry-gate:
	cargo build --release --locked --bin af
	mkdir -p target/telemetry-gate
	python3 scripts/telemetry-overhead.py --af target/release/af \
		--report target/telemetry-gate/overhead.json

# Exercise release selection and tag races against disposable local Git remotes, check that
# CHANGELOG.md's [Unreleased] holds only its pointer and every changelog.d/ note only bullets,
# exercise the test quarantine scan on test-local task lists (ADR-0268),
# and keep this repository's own .af/af.lock on the newest release (ADR-0138).
release-check:
	$(CI_STEP) --stage release release-resolution python3 scripts/test-class-python.py test-release-resolve.py "$(BASE_EVIDENCE)/test-release-resolve.py.xml"
	$(CI_STEP) --stage release changelog-notes-tests python3 scripts/test-class-python.py test-changelog-notes.py "$(BASE_EVIDENCE)/test-changelog-notes.py.xml"
	$(CI_STEP) --stage release changelog-notes python3 scripts/changelog-notes.py --check
	$(CI_STEP) --stage release quarantine-scan-tests python3 scripts/test-class-python.py test-test-quarantine.py "$(BASE_EVIDENCE)/test-test-quarantine.py.xml"
	$(CI_STEP) --stage release af-pin-tests python3 scripts/test-class-python.py test-af-pin.py "$(BASE_EVIDENCE)/test-af-pin.py.xml"
	$(CI_STEP) --stage release af-pin python3 scripts/af-pin.py --check

# Offline advisory orchestration checks against the native inspection fixture, the pull
# request description check against the `af task report` renderer's fixture (ADR-0142),
# performance schema parity (ADR-0162), and the guarded kernel experiments' protected-harness
# digest against the staged policy and any installed guarded measure (ADR-0185).
preflight-check:
	$(CI_STEP) --stage preflight task-preflight python3 scripts/test-task-preflight.py
	$(CI_STEP) --stage preflight prereqs python3 scripts/test-prereqs.py
	$(CI_STEP) --stage preflight gate-owner python3 scripts/test-gate-owner.py
	$(CI_STEP) --stage preflight nextest-gate python3 scripts/test-nextest-gate.py
	$(CI_STEP) --stage preflight stress-proof python3 scripts/test-stress-proof.py
	$(CI_STEP) --stage preflight test-time-report python3 scripts/test-test-time-report.py
	$(CI_STEP) --stage preflight provider-auth-host python3 scripts/test-provider-auth-host.py
	$(CI_STEP) --stage preflight pr-report python3 scripts/test-check-pr-report.py
	$(CI_STEP) --stage preflight pr-lines python3 scripts/test-pr-lines.py
	$(CI_STEP) --stage preflight test-classes python3 scripts/test-class-python.py test-test-classes.py "$(BASE_EVIDENCE)/test-test-classes.py.xml"
	$(CI_STEP) --stage preflight test-catalog python3 scripts/test-classes.py audit
	$(CI_STEP) --stage preflight performance-schemas python3 scripts/performance/schemas.py
	$(CI_STEP) --stage preflight --fix 'python3 -I scripts/performance/cli.py harness-digest --write' performance-kernel-harness python3 scripts/performance/cli.py harness-digest --check

# The laboratory suite executes tests, so it belongs after the real Rust build receipt,
# under the test deadline (ADR-0231). Depending on test also enforces this with parallel make;
# standalone make test retains its Rust-only behavior and both runner modes.
.PHONY: perf-python-tests
perf-python-tests: test
	$(CI_STEP) --stage tests performance-lab python3 scripts/test-performance.py

# Shared selective entry points. Full `check` above is what main validation, unbound releases
# and `make check` run; the Task gate runs `scripts/verify.sh --profile fast` (ADR-0251) and
# every pull request defaults to the same fast selection (ADR-0239). Labels are
# JSON data in a file; no Make/shell expansion of label strings is permitted.
.PHONY: base-check check-fast check-full check-classes test-plan perf-feature-tests perf-kernel-measure
TEST_PLAN ?= target/test-classes/plan.json
TEST_LABEL_FILE ?=
base-check: release-check
	$(CI_STEP) --stage preflight gate-owner python3 scripts/test-class-python.py test-gate-owner.py "$(BASE_EVIDENCE)/test-gate-owner.py.xml"
	$(CI_STEP) --stage preflight test-classes python3 scripts/test-class-python.py test-test-classes.py "$(BASE_EVIDENCE)/test-test-classes.py.xml"
	$(CI_STEP) --stage preflight test-catalog python3 scripts/test-classes.py audit

# Run before a PR changing scripts/performance/, crates/review-pipeline/examples/perf-probe/,
# crates/review-pipeline/build.rs, the experiment runtime, or kernel receipts (ADR-0252).
perf-kernel-measure:
	$(CI_STEP) --stage tests perf-kernel-measure cargo nextest run --workspace --locked --profile ci --run-ignored only -E 'test(=perf_kernel_measure::guarded_kernel_experiments_measure_one_judged_sample)'

perf-feature-tests:
	$(CI_STEP) --stage tests test-perf-trace cargo test --locked -p review-core --features perf-trace,perf-calibration --lib perf_
	$(CI_STEP) --stage tests test-perf-calibration cargo test --locked -p review-pipeline --features perf-calibration --test perf_probe calibration_build


test-plan:
	python3 scripts/test-classes.py plan --context local $(if $(TEST_LABEL_FILE),--event "$(TEST_LABEL_FILE)",) --output "$(TEST_PLAN)"

check-fast:
	python3 scripts/test-classes.py plan --context local --label release:fast --output "$(TEST_PLAN)"
	$(MAKE) check-classes

check-full:
	python3 scripts/test-classes.py plan --context local --label release:full --output "$(TEST_PLAN)"
	$(MAKE) check-classes

check-classes: base-check fmt lint
	python3 scripts/test-classes.py execute --plan "$(TEST_PLAN)" --threads $(TEST_THREADS)

.PHONY: test-labels
test-labels:
	python3 scripts/test-labels.py $(if $(filter 1,$(APPLY)),--apply,)
