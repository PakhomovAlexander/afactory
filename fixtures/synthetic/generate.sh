#!/usr/bin/env bash
# generate.sh — rebuild the synthetic Phase 0 fixtures by RUNNING the real harness.
#
#   tools/review-kernel/fixtures/synthetic/generate.sh [--check]
#
# The legacy corpus under ../legacy/ only ever exercised report -> fixed (see its
# README). Every other case Phase 0 needs is produced here, by driving the actual
# ledger.sh / checks.sh rather than hand-writing JSONL: the harness is the
# specification the kernel must reproduce, and a hand-authored fixture would
# encode our reading of it instead of its behavior.
#
# Each case captures the exact command sequence, stdout, stderr and exit code in
# transcript.txt, plus the resulting ledger. Every case asserts what it claims to
# prove, so this script fails loudly if the harness ever changes underneath it.
#
# --check regenerates into a temp dir and diffs against the committed fixtures
# instead of rewriting them (use in CI / before trusting a replay result).
set -euo pipefail
# Deterministic collation and numeric formatting: this corpus must reproduce on any
# machine, and glob/sort order under en_US differs from C on hyphenated names.
export LC_ALL=C

HERE="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
HUB="$(cd -- "$HERE/../../../.." && pwd)"
LEDGER="$HUB/.agents/skills/self-review-heavy/scripts/ledger.sh"
CHECKS="$HUB/.agents/skills/self-review-heavy/scripts/checks.sh"
[ -x "$LEDGER" ] || { echo "generate.sh: not found: $LEDGER" >&2; exit 2; }
[ -x "$CHECKS" ] || { echo "generate.sh: not found: $CHECKS" >&2; exit 2; }
command -v jq >/dev/null || { echo "generate.sh: jq is required" >&2; exit 2; }

MODE="write"
[ "${1:-}" = "--check" ] && MODE="check"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
OUT="$WORK/out"; mkdir -p "$OUT"
FAILURES=0

fail() { echo "generate.sh: FAIL: $*" >&2; FAILURES=$((FAILURES + 1)); }

# ---------------------------------------------------------------- case helpers
CASE=""; CASE_DIR=""; LAST_RC=0
new_case() {  # new_case <name> <one-line what this pins>
  CASE="$1"; CASE_DIR="$WORK/run/$CASE"
  mkdir -p "$CASE_DIR/input"
  : > "$CASE_DIR/transcript.txt"
  printf '%s\n' "$2" > "$CASE_DIR/.pins"
}

# Write a findings file the way a Reviewer stage emits one.
findings() {  # findings <file> then heredoc of the .findings array body
  cat > "$CASE_DIR/input/$1" <<EOF
{
  "verdict": "request-changes",
  "summary": "synthetic fixture input",
  "findings": [
$(cat)
  ],
  "benchmark_demands": [],
  "disputes": []
}
EOF
}
finding() {  # finding <severity> <file> <title> <body> [fix]
  # `fix` is REQUIRED by findings.schema.json. ledger.sh's own validation checks only
  # severity/file/title/body, so it accepts a finding without one — but a reviewer never
  # emits that (63 of 63 in the frozen corpus carry a fix), and a fixture that violates the
  # schema it claims to exercise is not representative input.
  printf '    {"severity": "%s", "file": "%s", "line": null, "title": "%s", "body": "%s", "fix": "%s", "confidence": 0.9}' \
    "$1" "$2" "$3" "$4" "${5:-minimal change at $2}"
}

# Run a harness command inside the case dir, recording it verbatim.
emit() {
  local rc=0
  printf '### %s\n' "$(printf '%s ' "$@" | sed "s|$LEDGER|ledger.sh|; s|$CHECKS|checks.sh|; s| $||")" \
    >> "$CASE_DIR/transcript.txt"
  ( cd "$CASE_DIR" && "$@" ) > "$CASE_DIR/.out" 2> "$CASE_DIR/.err" || rc=$?
  LAST_RC=$rc
  {
    printf 'exit=%d\n--- stdout\n' "$rc"
    cat "$CASE_DIR/.out"
    printf -- '--- stderr\n'
    sed "s|$LEDGER|ledger.sh|g; s|$CHECKS|checks.sh|g" "$CASE_DIR/.err"
    printf '\n'
  } >> "$CASE_DIR/transcript.txt"
  rm -f "$CASE_DIR/.out" "$CASE_DIR/.err"
}
led()  { emit "$LEDGER" "$1" . "${@:2}"; }
chk()  { emit "$CHECKS" "$@"; }

assert_rc()  { [ "$LAST_RC" = "$1" ] || fail "$CASE: expected exit $1, got $LAST_RC"; }
assert_led() {  # assert_led <jq filter over the ledger as an array> <description>
  jq -se "$1" "$CASE_DIR/ledger.jsonl" >/dev/null 2>&1 \
    || fail "$CASE: ledger assertion failed: $2"
}
assert_out() {  # assert_out <fixed string> <description>
  grep -qF "$1" "$CASE_DIR/transcript.txt" || fail "$CASE: transcript missing '$1' ($2)"
}

close_case() {
  local d="$OUT/$CASE"
  mkdir -p "$d"
  cp -R "$CASE_DIR/input" "$d/"
  cp "$CASE_DIR/transcript.txt" "$d/"
  [ -f "$CASE_DIR/ledger.jsonl" ] && cp "$CASE_DIR/ledger.jsonl" "$d/"
  [ -f "$CASE_DIR/round" ] && cp "$CASE_DIR/round" "$d/"
  if [ -f "$CASE_DIR/checks.tsv" ]; then
    # column 3 is elapsed seconds — the only nondeterministic field the harness
    # writes. Normalized so the corpus is byte-reproducible; the kernel's Check
    # contract does not depend on it.
    awk -F'\t' 'BEGIN{OFS="\t"} {if (NF>=3) $3="<secs>"; print}' "$CASE_DIR/checks.tsv" > "$d/checks.tsv"
    if [ -d "$CASE_DIR/checks" ]; then
      cp -R "$CASE_DIR/checks" "$d/"
      # checks.sh creates its log directory before it knows whether any check will
      # run, so a vacuous run leaves it empty — and git cannot store an empty
      # directory. Without this marker the directory survives locally, vanishes on
      # clone, and --check then fails everywhere except the machine that generated
      # it. A fixture that cannot round-trip through a clone is not a fixture.
      [ -n "$(ls -A "$d/checks")" ] || : > "$d/checks/.gitkeep"
    fi
  fi
  { printf '# Case: %s\n\n%s\n\n' "$CASE" "$(cat "$CASE_DIR/.pins")"
    printf 'Generated by generate.sh from the real harness — do not hand-edit.\n'
    printf 'The transcript is the fixture: every command, its stdout, its stderr, its exit code.\n'
  } > "$d/CASE.md"
}

# =============================================================== ledger cases
# ---------------------------------------------------------------------------
new_case duplicate-same-round \
"Two reviewers report the same (file, title) in one round. The ledger keeps ONE entry
credited to the first reporter and drops the second report entirely — no second source,
no second body, no corroboration count. The kernel must instead keep both reports
immutable and treat the fingerprint as a grouping hint, never as a merge."
led init
findings r1-deep.json <<EOF
$(finding major src/a.rs "Retry loop can spin forever" "deep: no backoff, no cap")
EOF
findings r1-cross.json <<EOF
$(finding major src/a.rs "Retry loop can spin forever" "cross: independently found, different evidence")
EOF
led add --source deep-r1 input/r1-deep.json
assert_out "new=1 dup=0 reopened=0 escalated=0 open=1" "first report is new"
led add --source cross-r1 input/r1-cross.json
assert_out "new=0 dup=1 reopened=0 escalated=0 open=1" "second report is a silent dup"
assert_led 'length == 1' "one entry"
assert_led '.[0].source == "deep-r1"' "first reporter keeps the entry"
assert_led '.[0].body | test("^deep:")' "second reporter's evidence is discarded"
assert_led '.[0].round == 1 and .[0].last_seen_round == 1' "no news"
close_case

# ---------------------------------------------------------------------------
new_case duplicate-later-round \
"An open finding re-reported at the SAME severity in a later round bumps last_seen_round
only. .round is untouched, so it is not convergence news and does not reset the clean
window. Kernel equivalent: a repeat observation must not restart convergence."
led init
findings r1.json <<EOF
$(finding major src/b.rs "Unbounded allocation on parse" "r1 evidence")
EOF
findings r2.json <<EOF
$(finding major src/b.rs "Unbounded allocation on parse" "r2 evidence, same rank")
EOF
led add --source deep-r1 input/r1.json
led bump
led add --source cross-r2 input/r2.json
assert_out "new=0 dup=1 reopened=0 escalated=0 open=1" "same-severity re-report is a dup"
assert_led '.[0].round == 1' "round untouched — not news"
assert_led '.[0].last_seen_round == 2' "still seen"
assert_led '.[0].severity == "major"' "rank unchanged"
close_case

# ---------------------------------------------------------------------------
new_case escalation \
"An open finding re-reported at HIGHER severity is escalated: the new rank, evidence and
source are adopted in place, .round moves to the current round (it IS news), and a note
records the transition. Kernel: severity is monotone and an escalation must force
another clean round."
led init
findings r1.json <<EOF
$(finding major src/c.rs "Token is logged on auth failure" "r1: looks like noise")
EOF
findings r2.json <<EOF
$(finding blocker src/c.rs "Token is logged on auth failure" "r2: the log ships to a third party")
EOF
led add --source deep-r1 input/r1.json
led bump
led add --source cross-r2 input/r2.json
assert_out "new=0 dup=0 reopened=0 escalated=1 open=1" "counted as an escalation"
assert_led '.[0].severity == "blocker"' "new rank adopted"
assert_led '.[0].round == 2' "escalation is news"
assert_led '.[0].source == "cross-r2"' "current truth wins"
assert_led '.[0].note | startswith("escalated:")' "transition noted"
close_case

# ---------------------------------------------------------------------------
new_case reopen-after-fix \
"A finding resolved FIXED in an earlier round and re-reported later is reopened with the
new evidence adopted and .round advanced. Note the loss: the reopen note OVERWRITES the
resolution note, so the ledger can no longer say what the fix claimed. This is the case
the append-only event log exists for."
led init
findings r1.json <<EOF
$(finding major src/d.rs "Off-by-one truncates the last row" "r1 evidence")
EOF
findings r2.json <<EOF
$(finding major src/d.rs "Off-by-one truncates the last row" "r2: the fix only moved the boundary")
EOF
led add --source deep-r1 input/r1.json
FP="$(jq -r '.fp' "$CASE_DIR/ledger.jsonl")"
led resolve "$FP" fixed --note "fixed by clamping the range in commit abc1234"
assert_led '.[0].status == "fixed"' "resolved"
assert_led '.[0].note | test("abc1234")' "resolution note recorded"
led bump
led add --source deep-r2 input/r2.json
assert_out "new=0 dup=0 reopened=1 escalated=0 open=1" "counted as a reopen"
assert_led '.[0].status == "open"' "back to open"
assert_led '.[0].round == 2' "reopen is news"
assert_led '.[0].note | startswith("reopened:")' "reopen noted"
assert_led '.[0].note | test("abc1234") | not' "the resolution note is GONE — projection is lossy"
close_case

# ---------------------------------------------------------------------------
new_case rejected-re-report-same-severity \
"A rejected finding re-reported at the same severity is NEVER auto-reopened: reviewers
only see open claims, so they rediscover rejections forever and auto-reopening would loop
the run to exhaustion. It stays a dup, the rejection reason survives, and the only signal
is a re-triage warning on STDERR. Kernel: that warning must become a typed event, not a
line nobody parses."
led init
findings r1.json <<EOF
$(finding major src/e.rs "Mutex is held across await" "r1 evidence")
EOF
findings r2.json <<EOF
$(finding major src/e.rs "Mutex is held across await" "r2: same claim, same rank")
EOF
led add --source deep-r1 input/r1.json
FP="$(jq -r '.fp' "$CASE_DIR/ledger.jsonl")"
led resolve "$FP" rejected --note "not a defect: the executor is single-threaded here"
led bump
led add --source cross-r2 input/r2.json
assert_out "new=0 dup=1 reopened=0 escalated=0 open=0" "stays a dup, nothing reopened"
assert_out "re-triage manually if the rejection no longer holds" "warning is stderr-only"
assert_led '.[0].status == "rejected"' "still rejected"
assert_led '.[0].round == 1' "not news"
assert_led '.[0].last_seen_round == 2' "but seen again"
assert_led '.[0].note | test("single-threaded")' "rejection reason survives"
close_case

# ---------------------------------------------------------------------------
new_case rejected-re-report-higher-severity \
"A rejected finding re-reported at HIGHER severity keeps its rejected status but adopts
the new rank, evidence and source in place, and .round advances — so a later manual
re-triage inherits the real severity instead of the stale one it was rejected at.
Deliberately not round-guarded: stages ingest separately within a round."
led init
findings r1.json <<EOF
$(finding major src/f.rs "Config parser accepts a negative timeout" "r1: cosmetic")
EOF
findings r2.json <<EOF
$(finding blocker src/f.rs "Config parser accepts a negative timeout" "r2: it disables the deadline entirely")
EOF
led add --source deep-r1 input/r1.json
FP="$(jq -r '.fp' "$CASE_DIR/ledger.jsonl")"
led resolve "$FP" rejected --note "rejected at major: clamped downstream"
led bump
led add --source cross-r2 input/r2.json
assert_out "at HIGHER severity (major → blocker; evidence adopted)" "escalated-in-place warning"
assert_led '.[0].status == "rejected"' "status untouched"
assert_led '.[0].severity == "blocker"' "rank adopted"
assert_led '.[0].round == 2' "adoption is news"
assert_led '.[0].source == "cross-r2"' "evidence adopted"
close_case

# ---------------------------------------------------------------------------
new_case wontfix-re-report \
"wontfix follows the same never-auto-reopen rule as rejected — pinned separately so a
kernel port cannot implement one branch and miss the other."
led init
findings r1.json <<EOF
$(finding major src/g.rs "Legacy codec has no length check" "r1 evidence")
EOF
findings r2.json <<EOF
$(finding major src/g.rs "Legacy codec has no length check" "r2: rediscovered")
EOF
led add --source deep-r1 input/r1.json
FP="$(jq -r '.fp' "$CASE_DIR/ledger.jsonl")"
led resolve "$FP" wontfix --note "codec is deleted in the next release"
led bump
led add --source deep-r2 input/r2.json
assert_out "new=0 dup=1 reopened=0 escalated=0 open=0" "dup, not reopened"
assert_led '.[0].status == "wontfix"' "still wontfix"
close_case

# ---------------------------------------------------------------------------
new_case contested-escalation \
"contested behaves like open: it blocks convergence and it escalates on a higher-severity
re-report, without losing the contested status."
led init
findings r1.json <<EOF
$(finding major src/h.rs "Cache invalidation misses the tombstone" "r1 evidence")
EOF
findings r2.json <<EOF
$(finding blocker src/h.rs "Cache invalidation misses the tombstone" "r2: stale reads are user-visible")
EOF
led add --source deep-r1 input/r1.json
FP="$(jq -r '.fp' "$CASE_DIR/ledger.jsonl")"
led resolve "$FP" contested --note "author disputes: claims the tombstone is written first"
led bump
led add --source cross-r2 input/r2.json
assert_out "new=0 dup=0 reopened=0 escalated=1 open=0" "escalated while contested"
assert_led '.[0].status == "contested"' "status kept"
assert_led '.[0].severity == "blocker"' "rank adopted"
led converged --clean-rounds 1 --max-rounds 3 --gate major
assert_rc 1
assert_out "NOT CONVERGED" "contested still blocks"
close_case

# ---------------------------------------------------------------------------
new_case fix-needs-a-clean-round \
"The apply-without-confirmation case. A finding fixed in the SAME round it was reported
does not converge: the fix has not survived a review. Only a later round that adds no
gate-severity news converges. Kernel: a patch application never resolves a Finding —
derived-snapshot evidence does."
led init
findings r1.json <<EOF
$(finding major src/i.rs "Writer drops the last batch on shutdown" "r1 evidence")
EOF
findings r2-empty.json <<EOF
$(printf '')
EOF
led add --source deep-r1 input/r1.json
FP="$(jq -r '.fp' "$CASE_DIR/ledger.jsonl")"
led resolve "$FP" fixed --note "flush on drop"
led converged --clean-rounds 1 --max-rounds 3 --gate major
assert_rc 1
assert_out "NOT CONVERGED" "a same-round fix is not convergence"
led bump
led add --source deep-r2 input/r2-empty.json
led converged --clean-rounds 1 --max-rounds 3 --gate major
assert_rc 0
assert_out "CONVERGED" "one clean round after the fix converges"
close_case

# ---------------------------------------------------------------------------
new_case max-rounds-exhausted \
"An open gate-severity finding at the round cap exits 3 — a THIRD verdict, distinct from
converged (0) and not-yet (1). It must surface as needs-human, never as a pass."
led init
findings r1.json <<EOF
$(finding blocker src/j.rs "Replica can serve reads before catching up" "r1 evidence")
EOF
led add --source deep-r1 input/r1.json
led bump
led bump
led converged --clean-rounds 1 --max-rounds 3 --gate major
assert_rc 3
assert_out "MAX-ROUNDS EXHAUSTED" "distinct exhausted verdict"
assert_led '.[0].status == "open"' "the finding is still open and must be reported"
close_case

# ---------------------------------------------------------------------------
new_case sub-gate-minor-never-blocks \
"An OPEN minor under a major gate neither blocks convergence nor counts as news. The run
converges with an open finding in the ledger — the kernel's convergence policy must
reproduce this, or every run will hang on cosmetics."
led init
findings r1.json <<EOF
$(finding minor src/k.rs "Comment says milliseconds, value is seconds" "r1 evidence")
EOF
led add --source deep-r1 input/r1.json
led converged --clean-rounds 1 --max-rounds 3 --gate major
assert_rc 0
assert_out "CONVERGED" "sub-gate findings never force a round"
assert_led '.[0].status == "open"' "and it is still open"
close_case

# ---------------------------------------------------------------------------
new_case malformed-severity-rejected \
"A finding whose severity is outside blocker|major|minor is rejected before ingestion —
the whole batch dies with exit 2 and the ledger is untouched. Without this, an unknown
severity would rank as minor in converged and slip under the gate."
led init
findings r1-bad.json <<EOF
$(finding critical src/l.rs "Out-of-enum severity" "should not be ingested")
EOF
led add --source deep-r1 input/r1-bad.json
assert_rc 2
assert_out "violates the findings schema" "rejected up front"
assert_led 'length == 0' "ledger untouched"
close_case

# ---------------------------------------------------------------------------
new_case empty-title-skipped-batch-survives \
"One unusable finding must not throw away a whole valid batch: an empty title is skipped
with a warning while its siblings ingest. Also pins the change-wide default — an empty
file becomes the literal path '(change-wide)', which shares the fingerprint namespace
with real paths."
led init
findings r1.json <<EOF
$(finding major src/m.rs "   " "empty title — must be skipped"),
$(finding major "" "Whole-change concern: no rollback path" "no file — change-wide"),
$(finding minor src/m.rs "Real finding" "must survive its bad sibling")
EOF
led add --source deep-r1 input/r1.json
assert_out "skipping finding with empty title" "warned"
assert_out "new=2 dup=0 reopened=0 escalated=0 open=2" "valid siblings survived"
assert_led 'length == 2' "two entries"
assert_led 'map(.file) | index("(change-wide)") != null' "change-wide default applied"
close_case

# =============================================================== checks cases
# ---------------------------------------------------------------------------
new_case check-vacuous-run-is-not-a-pass \
"A comment-only check list executes nothing and exits 1. 'No check failed' is not 'the
checks passed' — the kernel must treat an empty required check set as a failed gate."
printf '# every line is a comment\n# so nothing runs\n' > "$CASE_DIR/input/checks.render.tsv"
chk --file input/checks.render.tsv --out .
assert_rc 1
assert_out "no checks executed" "vacuous run refused"
close_case

# ---------------------------------------------------------------------------
new_case check-failure-is-recorded \
"A failing check is recorded as fail in checks.tsv with its log, and the run exits 1 even
though another check passed. The results file is the machine-readable gate input."
printf 'ok-check\ttrue\nbad-check\techo "boom" >&2; exit 7\n' > "$CASE_DIR/input/checks.render.tsv"
chk --file input/checks.render.tsv --out .
assert_rc 1
assert_out "checks: 1/2 passed" "one of two passed"
grep -q $'bad-check\tfail' "$CASE_DIR/checks.tsv" || fail "$CASE: failure not recorded in checks.tsv"
close_case

# ---------------------------------------------------------------------------
new_case check-with-no-command-fails-the-run \
"A malformed check line (no TAB, so no command) is not silently skipped: it fails the run
and is excluded from the executed total, so a typo cannot quietly shrink the gate."
printf 'has-command\ttrue\nno-command-here\n' > "$CASE_DIR/input/checks.render.tsv"
chk --file input/checks.render.tsv --out .
assert_rc 1
assert_out "has no command" "malformed line reported"
assert_out "checks: 1/1 passed" "malformed line excluded from the total, run still fails"
close_case

# ==================================================================== manifest
{
  printf 'case\tharness\tfinal_round\tledger_entries\tstatuses\tlast_exit\n'
  for d in "$OUT"/*/; do
    c="$(basename "$d")"
    if [ -f "$d/ledger.jsonl" ]; then
      h=ledger.sh
      n="$(jq -s 'length' "$d/ledger.jsonl")"
      st="$(jq -sr 'map(.status) | group_by(.) | map("\(.[0])=\(length)") | join(",") | if . == "" then "-" else . end' "$d/ledger.jsonl")"
      r="$(cat "$d/round" 2>/dev/null || echo -)"
    else
      h=checks.sh; n=-; st=-; r=-
    fi
    e="$(grep '^exit=' "$d/transcript.txt" | tail -1 | cut -d= -f2)"
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$c" "$h" "$r" "$n" "$st" "$e"
  done
} > "$OUT/MANIFEST.tsv"

if [ "$FAILURES" -gt 0 ]; then
  echo "generate.sh: $FAILURES assertion(s) failed — the harness no longer behaves as the corpus claims" >&2
  exit 1
fi

# ===================================================================== publish
if [ "$MODE" = check ]; then
  rc=0
  for d in "$OUT"/*/ ; do :; done
  diff -ru --exclude=README.md --exclude=generate.sh "$HERE" "$OUT" > "$WORK/diff.txt" 2>&1 || rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "generate.sh --check: committed fixtures differ from a fresh run:" >&2
    cat "$WORK/diff.txt" >&2
    exit 1
  fi
  echo "generate.sh --check: fixtures reproduce byte-identically"
else
  find "$HERE" -mindepth 1 -maxdepth 1 ! -name generate.sh ! -name README.md -exec rm -rf {} +
  cp -R "$OUT"/. "$HERE"/
  echo "generate.sh: wrote $(find "$HERE" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ') cases to $HERE"
fi
