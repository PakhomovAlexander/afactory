.PHONY: check fmt lint test fixtures build pilot-check consumer-check review-kernel-container-probes

check: fmt lint test fixtures

fmt:
	cargo fmt --all -- --check

lint:
	cargo clippy --all-targets --locked -- -D warnings

test:
	cargo test --locked

fixtures:
	fixtures/synthetic/generate.sh --check

build:
	cargo build --release --locked --bin af

pilot-check:
	cargo test --locked -p reviewctl --test task_implement

# Plan every consumer fixture with the release binary — what the release workflow runs before a
# release leaves draft. The same check runs inside `make check` through the reviewctl tests.
consumer-check: build
	fixtures/consumers/check.sh target/release/af

# Live containment and the v3 Gate route. These stay outside `make check` because a missing
# daemon is a hard failure here, never a skip disguised as success.
review-kernel-container-probes:
	cargo test --locked -p review-sandbox --test container_probes -- --ignored
	cargo test --locked -p review-sandbox container::tests::a_timed_out_container_is_removed_before_execution_returns -- --ignored --exact
	cargo test --locked -p review-pipeline --test end_to_end a_v3_container_gate_executes_through_the_pipeline -- --ignored --exact
