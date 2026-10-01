.PHONY: release-check preflight-check check fmt lint test release review-kernel-container-probes

# nextest is the gate (ADR-0124): one process per test, scheduled across every test binary.
# `TEST_RUNNER=cargo` keeps the sequential libtest path for comparison. TEST_THREADS bounds
# concurrent tests, not compiler jobs: four on a four-core runner, half the cores elsewhere,
# because these tests spawn real process trees.
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

# Exercise release selection and tag races against disposable local Git remotes.
release-check:
	$(CI_STEP) release-resolution python3 scripts/test-release-resolve.py
	$(CI_STEP) changelog-notes python3 scripts/changelog-notes.py --check

# Offline advisory orchestration checks against the native inspection fixture.
preflight-check:
	$(CI_STEP) task-preflight python3 scripts/test-task-preflight.py
	$(CI_STEP) nextest-gate python3 scripts/test-nextest-gate.py
