.PHONY: check fmt lint test fixtures build pilot-check

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
