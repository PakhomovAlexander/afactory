#!/usr/bin/env bash
set -euo pipefail

# The Review Kernel gives gates a read-only source tree. Reuse build products in
# a stable external cache; set AFACTORY_REVIEW_TARGET_DIR for isolation or a cold run.
# Tests that read checked-in fixtures must resolve them at runtime: cached test
# binaries may have been compiled in an earlier, already-destroyed gate sandbox.
cache_home="${XDG_CACHE_HOME:-${HOME:-/tmp}/.cache}"
target_dir="${AFACTORY_REVIEW_TARGET_DIR:-$cache_home/afactory/review-target}"
mkdir -p "$target_dir"
AF_WORKSPACE_ROOT="$PWD" CARGO_TARGET_DIR="$target_dir" make check
