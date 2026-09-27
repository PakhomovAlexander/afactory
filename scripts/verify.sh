#!/usr/bin/env bash
set -euo pipefail

# Gates run against a read-only source tree, so build products live outside it. An explicit
# AF_GATE_TARGET_DIR wins, for isolation or a cold run. Otherwise a CARGO_TARGET_DIR that is
# already set is honoured: a Task check sets it, to the Warm Check Cache its code policy's
# `[warm]` table grants (ADR-0123) or to a fresh private directory when it runs cold. A manual
# run without either reuses a stable external cache below XDG_CACHE_HOME.
# Tests that read checked-in fixtures must resolve them at runtime: cached test
# binaries may have been compiled in an earlier, already-destroyed gate sandbox.
if [[ -n "${AF_GATE_TARGET_DIR:-}" ]]; then
  target_dir="$AF_GATE_TARGET_DIR"
elif [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
  target_dir="$CARGO_TARGET_DIR"
else
  cache_home="${XDG_CACHE_HOME:-${HOME:-/tmp}/.cache}"
  target_dir="$cache_home/af/gate-target"
fi
mkdir -p "$target_dir"
AF_WORKSPACE_ROOT="$PWD" CARGO_TARGET_DIR="$target_dir" make check
