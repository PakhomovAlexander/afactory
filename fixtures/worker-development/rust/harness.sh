#!/bin/sh
set -eu
# Both invocations keep the same private HOME, dependency view and build directory.
cargo test --offline --locked --manifest-path Cargo.toml >&2
printf 'first command completed\n' > "$HOME/first-command"
cargo test --offline --locked --manifest-path Cargo.toml >&2
read -r first < "$HOME/first-command"
test "$first" = 'first command completed'
printf '%s\n' '{"schema":"af.worker-reply/1","outputs":{"report":[{"summary":"Rust fixture compiled and tested twice in one private Attempt"}]}}'
