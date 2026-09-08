#!/usr/bin/env bash
set -euo pipefail

# The Review Kernel gives gates a read-only source tree. Reuse build products in
# a stable external cache; set AFACTORY_REVIEW_TARGET_DIR for isolation or a cold run.
# Tests that read checked-in fixtures must resolve them at runtime: cached test
# binaries may have been compiled in an earlier, already-destroyed gate sandbox.
# The /tmp fallback stays here deliberately: the Gate passes no HOME, and this Check is required,
# so it must still run. Nothing is executed out of this directory — cargo only writes build
# products into it — unlike scripts/markdownlint.sh, which execs its cache and therefore refuses
# a path it does not own. Set AFACTORY_REVIEW_TARGET_DIR to keep the cache elsewhere.
cache_home="${XDG_CACHE_HOME:-${HOME:-/tmp}/.cache}"
target_dir="${AFACTORY_REVIEW_TARGET_DIR:-$cache_home/afactory/review-target}"
mkdir -p "$target_dir"
AF_WORKSPACE_ROOT="$PWD" CARGO_TARGET_DIR="$target_dir" make check
