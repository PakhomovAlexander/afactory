.PHONY: check fmt lint test fixtures links installer-test installer-test-signed markdownlint build pilot-check consumer-check release review-kernel-container-probes review-kernel-test-corpus review-kernel-codex-smoke

# The debug binary the installer test drives: wherever cargo put it.
AF_DEBUG := $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR),target)/debug/af

check: fmt lint test fixtures links installer-test

fmt:
	cargo fmt --all -- --check

lint:
	cargo clippy --all-targets --locked -- -D warnings

test:
	cargo test --locked

fixtures:
	fixtures/synthetic/generate.sh --check

# Every relative link in every Markdown file names something in the tree.
links:
	scripts/check-links.sh

# install.sh end to end against a local fake release: bash, coreutils, and the debug binary.
installer-test:
	cargo build --locked -p reviewctl --bin af
	scripts/installer-test.sh $(AF_DEBUG)

# The same, plus the signature path. Needs a real minisign (MINISIGN=<path>, or on PATH), and a
# missing one is a failure, never a skip; CI fetches the pinned one with scripts/fetch-minisign.sh.
installer-test-signed:
	cargo build --locked -p reviewctl --bin af
	scripts/installer-test.sh --signed $(AF_DEBUG)

# markdownlint over **/*.md from the digest-pinned toolchain in tools/markdownlint/ — the review
# pipeline's second Gate Check (.af/pipelines/review.toml). Needs node and npm.
markdownlint:
	scripts/markdownlint.sh

build:
	cargo build --release --locked --bin af

pilot-check:
	cargo test --locked -p reviewctl --test task_implement

# Plan every consumer fixture with the release binary — what the release workflow runs before a
# release leaves draft. The same check runs inside `make check` through the reviewctl tests.
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

# The private legacy corpus under fixtures/legacy/ (real reviewer output a hub captured; see
# fixtures/legacy/README.md). Its tests are #[ignore]d because the corpus ships with no
# checkout; run explicitly, a missing corpus is a failure, never a skip.
review-kernel-test-corpus:
	cargo test --locked -p review-core --test legacy_corpus -- --ignored
	cargo test --locked -p review-store --test legacy_ledgers -- --ignored

# One real `codex exec` through the whole adapter stack (crates/review-runner-codex/tests/
# live_smoke.rs). It spends real tokens and needs the operator's Codex CLI and credentials, so
# it runs only under an explicit acknowledgement; without one, or without a usable provider,
# the target fails — it never reports a skip as success.
review-kernel-codex-smoke:
	@[ "$(AF_ACK_PAID_SMOKE)" = "1" ] || { echo "review-kernel-codex-smoke runs one real codex exec and spends real tokens: acknowledge with AF_ACK_PAID_SMOKE=1" >&2; exit 2; }
	@command -v codex >/dev/null 2>&1 || { echo "review-kernel-codex-smoke: codex is not on PATH; the smoke needs the operator's Codex CLI and credentials" >&2; exit 1; }
	cargo test --locked -p review-runner-codex --test live_smoke one_real_review_parses_and_reports_its_cost -- --ignored --exact --nocapture
