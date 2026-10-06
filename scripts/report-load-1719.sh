#!/usr/bin/env bash
set -euo pipefail
out="$RUNNER_TEMP/load-evidence"
export AF_WORKSPACE_ROOT="$PWD"
export AF_GATE_NEXTEST_TARGET="$CARGO_TARGET_DIR"
export CARGO_BUILD_JOBS="$(nproc)"
{
 date -Is; git rev-parse HEAD HEAD^{tree}; uname -a; lscpu; free -b; df -h; mount | grep ' / '; cat /proc/loadavg
 echo "CARGO_BUILD_JOBS=$CARGO_BUILD_JOBS TEST_THREADS=$(nproc)"
} > "$out/environment.txt"
binary="$(find "$CARGO_TARGET_DIR/debug/deps" -maxdepth 1 -type f -name 'it-*' -executable | head -1)"
test -n "$binary"
sha256sum "$binary" > "$out/binary-sha256.txt"
probe() {
 local phase="$1" nodes="$2" reps="$3"
 echo "probe $phase nodes=$nodes repetitions=$reps $(date -Is)" | tee -a "$out/timeline.txt"
 AF_REPORT_LOAD=1 AF_REPORT_NODES="$nodes" AF_REPORT_REPETITIONS="$reps"  timeout --signal=TERM --kill-after=10s 600 "$binary" task_runtime::report_load::report_lock_load_measurement --exact --ignored --nocapture  > "$out/$phase-$nodes.log" 2>&1
}
probe admission 200 1
probe baseline 1 5
probe baseline 50 5
# Genuine full suite on every core, plus bounded CPU indexing surrogate to reproduce contention.
# No loop restarts of the full suite and no test gate receipt is inferred from this load process.
TEST_THREADS="$(nproc)" make check > "$out/make-check.log" 2>&1 & check_pid=$!
stress-ng --cpu "$(( $(nproc) * 2 ))" --timeout 900s --metrics-brief > "$out/stress.log" 2>&1 & stress_pid=$!
(while kill -0 "$check_pid" 2>/dev/null; do date -Is; cat /proc/loadavg; free -b; sleep 5; done) > "$out/load-timeseries.txt" & monitor_pid=$!
trap 'kill "$stress_pid" "$monitor_pid" "$check_pid" 2>/dev/null || true' EXIT
sleep 15
probe loaded 1 20
probe loaded 50 20
set +e
wait "$check_pid"; check_rc=$?
echo "make_check_exit=$check_rc" | tee "$out/make-check-exit.txt"
kill "$stress_pid" "$monitor_pid" 2>/dev/null
wait "$stress_pid"; wait "$monitor_pid"
set -e
trap - EXIT
date -Is >> "$out/timeline.txt"
