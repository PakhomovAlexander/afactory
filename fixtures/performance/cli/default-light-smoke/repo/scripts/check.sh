#!/bin/sh
set -eu

: "${CARGO_TARGET_DIR:?the Task check must bind its declared target}"
export CARGO_NET_OFFLINE=true
artifacts="$CARGO_TARGET_DIR/dev-flow-cargo.jsonl"
cargo test --workspace --locked --no-run --message-format=json-render-diagnostics >"$artifacts"
cargo test --workspace --locked
