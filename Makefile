.PHONY: release-check preflight-check check fmt lint test release review-kernel-container-probes \
	markdownlint-tool markdownlint-tool-e2e

# nextest is the gate (ADR-0124): one process per test, scheduled across every test binary.
# `TEST_RUNNER=cargo` keeps the sequential libtest path for comparison. TEST_THREADS bounds
# concurrent tests, not compiler jobs: four on a four-core runner, half the cores elsewhere,
# because these tests spawn real process trees. After nextest, whatever its status, the gate
# entry prints scripts/test-time-report.py's summary of the JUnit it just wrote (it removes an
# earlier run's JUnit first); the step keeps nextest's exit status.
TEST_RUNNER ?= nextest
TEST_THREADS ?= $(shell python3 -c 'import os; print(max(4, (os.cpu_count() or 4) // 2))')
CI_STEP = python3 scripts/ci-step.py

# Run unprivileged: preflight proves a chmod-sealed source rejects writes.
# Root/CAP_DAC_OVERRIDE bypass that seal and intentionally fail the precondition.
check: preflight-check fmt lint test release-check

fmt:
	$(CI_STEP) fmt cargo fmt --all -- --check

lint:
	$(CI_STEP) lint cargo clippy --all-targets --locked -- -D warnings

test:
ifeq ($(TEST_RUNNER),nextest)
	$(CI_STEP) test-build cargo test --locked --no-run
	$(CI_STEP) test python3 scripts/nextest-gate.py --locked --profile ci --test-threads $(TEST_THREADS)
else ifeq ($(TEST_RUNNER),cargo)
	$(CI_STEP) test-build cargo test --locked --no-run
	$(CI_STEP) test cargo test --locked -- --test-threads=$(TEST_THREADS)
else
	$(error TEST_RUNNER must be nextest or cargo)
endif

# Open the release PR for VERSION (bump + CHANGELOG section). Merging it is the release: the
# workflow tags, checks, builds, signs, and publishes. COMPAT states authority compatibility.
release:
	scripts/release.sh "$(VERSION)" --compat "$(COMPAT)"

# Live containment and the v3 Gate route. These stay outside `make check` because a missing
# daemon is a hard failure here, never a skip disguised as success.
review-kernel-container-probes:
	cargo test --locked -p review-sandbox --test container_probes -- --ignored
	cargo test --locked -p review-sandbox container::tests::a_timed_out_container_is_removed_before_execution_returns -- --ignored --exact
	cargo test --locked -p review-pipeline --test it task_campaign_review::host::domain::a_container_gate_executes_on_the_task_host -- --ignored --exact

# Exercise release selection and tag races against disposable local Git remotes, and keep
# this repository's own .af/af.lock on the newest release (ADR-0138).
release-check:
	$(CI_STEP) release-resolution python3 scripts/test-release-resolve.py
	$(CI_STEP) changelog-notes python3 scripts/changelog-notes.py --check
	$(CI_STEP) af-pin-tests python3 scripts/test-af-pin.py
	$(CI_STEP) af-pin python3 scripts/af-pin.py --check

# Offline advisory orchestration checks against the native inspection fixture, the pull
# request description check against the `af task report` renderer's fixture (ADR-0142), and the
# offline markdownlint tool's installer and verifier against a synthetic closure (ADR-0145).
preflight-check:
	$(CI_STEP) task-preflight python3 scripts/test-task-preflight.py
	$(CI_STEP) nextest-gate python3 scripts/test-nextest-gate.py
	$(CI_STEP) test-time-report python3 scripts/test-test-time-report.py
	$(CI_STEP) provider-auth-host python3 scripts/test-provider-auth-host.py
	$(CI_STEP) pr-report python3 scripts/test-check-pr-report.py
	$(CI_STEP) markdownlint-tool python3 scripts/test-markdownlint-tool.py

# The markdownlint gate's tool (ADR-0145). `markdownlint-tool` is the one networked step: it
# installs the pinned closure read-only below $XDG_DATA_HOME/af-tools (PREFIX overrides) and
# prints the bin directory to put on PATH. `markdownlint-tool-e2e` installs it into a temporary
# prefix and lints fixtures through it offline; it stays outside `make check` because it fetches.
markdownlint-tool:
	python3 scripts/markdownlint-tool.py install $(if $(PREFIX),--prefix "$(PREFIX)")

markdownlint-tool-e2e:
	AF_MARKDOWNLINT_E2E=1 python3 scripts/test-markdownlint-tool.py
