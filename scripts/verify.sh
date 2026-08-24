#!/usr/bin/env bash
set -euo pipefail

# The Review Kernel gives gates a read-only source tree. Reuse build products in
# a stable external cache; set AFACTORY_REVIEW_TARGET_DIR for isolation or a cold run.
cache_home="${XDG_CACHE_HOME:-${HOME:-/tmp}/.cache}"
target_dir="${AFACTORY_REVIEW_TARGET_DIR:-$cache_home/afactory/review-target}"
mkdir -p "$target_dir"
CARGO_TARGET_DIR="$target_dir" make check
