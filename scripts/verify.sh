#!/usr/bin/env bash
set -euo pipefail

# Gates run against a read-only source tree, so reuse build products in a stable
# external cache; set AF_GATE_TARGET_DIR for isolation or a cold run. The common
# Task code gate gives each run its own HOME and XDG_CACHE_HOME, so the cache is
# warm only for manual runs and for gates that keep the caller's environment.
# Tests that read checked-in fixtures must resolve them at runtime: cached test
# binaries may have been compiled in an earlier, already-destroyed gate sandbox.
cache_home="${XDG_CACHE_HOME:-${HOME:-/tmp}/.cache}"
target_dir="${AF_GATE_TARGET_DIR:-$cache_home/af/gate-target}"
mkdir -p "$target_dir"
AF_WORKSPACE_ROOT="$PWD" CARGO_TARGET_DIR="$target_dir" make check
