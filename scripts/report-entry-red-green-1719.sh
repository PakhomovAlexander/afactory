#!/usr/bin/env bash
set -euo pipefail
out="$RUNNER_TEMP/load-evidence"
mkdir -p "$out"
report=crates/review-pipeline/src/task/report.rs
lease=crates/review-pipeline/src/task/lease.rs
cp "$report" "$RUNNER_TEMP/report-green.rs"
cp "$lease" "$RUNNER_TEMP/lease-green.rs"
restore() { cp "$RUNNER_TEMP/report-green.rs" "$report"; cp "$RUNNER_TEMP/lease-green.rs" "$lease"; }
trap restore EXIT
old=d1ec098c2659f81bc2a08a795dca58fab7c467e5
# Exact old production implementation, with only the identical new test module appended.
git show "$old:$report" > "$report"
printf '
#[cfg(test)]
#[path = "report_tests.rs"]
mod tests;
' >> "$report"
git show "$old:$lease" > "$lease"
{ date -Is; echo "red_production=$old"; git diff -- "$report" "$lease"; sha256sum "$report" "$lease"; } > "$out/red-identity.txt"
set +e
cargo test --locked -p review-pipeline --lib task::report::tests::due_report_entry_renews_before_prefix_without_reexecuting_or_recharging -- --exact --nocapture > "$out/red.log" 2>&1
rc=$?
set -e
echo "red_exit=$rc" > "$out/red-exit.txt"
# A compiler/environment failure is NOT a RED regression.
test "$rc" -eq 101
grep -q 'the due renewal must precede the newly read report prefix' "$out/red.log"
grep -q 'test result: FAILED. 0 passed; 1 failed' "$out/red.log"
restore
trap - EXIT
git diff --exit-code -- "$report" "$lease"
{ date -Is; git rev-parse HEAD HEAD^{tree}; sha256sum "$report" "$lease"; } > "$out/green-identity.txt"
cargo test --locked -p review-pipeline --lib task::report::tests -- --nocapture > "$out/green.log" 2>&1
grep -q 'test result: ok. 3 passed; 0 failed' "$out/green.log"
