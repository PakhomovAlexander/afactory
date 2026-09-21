.PHONY: release-check check fmt lint test fixtures build pilot-check consumer-check release review-kernel-container-probes review-kernel-test-corpus

# Cargo remains the gate; nextest is an explicit cross-binary benchmark until validated in CI.
TEST_RUNNER ?= cargo
TEST_THREADS ?= 4
CI_STEP = python3 scripts/ci-step.py

check: fmt lint test fixtures release-check

fmt:
	$(CI_STEP) fmt cargo fmt --all -- --check

lint:
	$(CI_STEP) lint cargo clippy --all-targets --locked -- -D warnings

test:
ifeq ($(TEST_RUNNER),nextest)
	$(CI_STEP) test-build cargo test --locked --no-run
	$(CI_STEP) test-run cargo nextest run --locked --profile ci
	$(CI_STEP) doctests cargo test --locked --doc -- --test-threads=$(TEST_THREADS)
else ifeq ($(TEST_RUNNER),cargo)
	$(CI_STEP) test-build cargo test --locked --no-run
	$(CI_STEP) test cargo test --locked -- --test-threads=$(TEST_THREADS)
else
	$(error TEST_RUNNER must be cargo or nextest)
endif

fixtures:
	$(CI_STEP) fixtures fixtures/synthetic/generate.sh --check

build:
	cargo build --release --locked --bin af

pilot-check:
	cargo test --locked -p af --test task_implement

# Plan every consumer fixture with the release binary — what the release workflow runs before a
# release leaves draft. The same check runs inside `make check` through the af crate's tests.
consumer-check: build
	fixtures/consumers/check.sh target/release/af

# Open the release PR for VERSION (bump + CHANGELOG section). Merging it is the release: the
# workflow tags, checks, builds, signs, and publishes. COMPAT states authority compatibility.
release:
	scripts/release.sh "$(VERSION)" --compat "$(COMPAT)"

# Live containment and the v3 Gate route. These stay outside `make check` because a missing
# daemon is a hard failure here, never a skip disguised as success.
review-kernel-container-probes:
	cargo test --locked -p review-sandbox --test container_probes -- --ignored
	cargo test --locked -p review-sandbox container::tests::a_timed_out_container_is_removed_before_execution_returns -- --ignored --exact
	cargo test --locked -p review-pipeline --test end_to_end a_v3_container_gate_executes_through_the_pipeline -- --ignored --exact

# The legacy review corpus is private data (see fixtures/legacy/README.md). Its tests are
# `#[ignore]`d in `make check`; a project that captured a corpus runs them here, where a missing
# corpus is a failure, not a skip.
review-kernel-test-corpus:
	cargo test --locked -p review-core --test legacy_corpus -- --ignored
	cargo test --locked -p review-store --test legacy_ledgers -- --ignored

# Exercise release selection and tag races against disposable local Git remotes.
release-check:
	$(CI_STEP) release-resolution python3 scripts/test-release-resolve.py
