#!/bin/bash
# The p8 closeout's real local laboratory workflow (ADR-0172), on this macOS arm64 host in the
# named macos_arm64_local environment. Every step's argv, exit status and wall time is appended to
# $WORK/steps.jsonl and its public stdout and stderr kept below $WORK/logs. No step is retried.
#
# Usage: workflow.sh SOURCE_CHECKOUT WORK STAGE
#   SOURCE_CHECKOUT  the source to measure (copied into a private Git repository, never changed)
#   WORK             a new task-owned work directory (private HOME, XDG, scratch, Cargo home)
#   STAGE            snapshot | plan | fetch | build | run | analyse | profile | report |
#                    archive | rerun | reproduce
#
# A stage stops at its first command or step that fails and exits with that status, so a later
# success never hides it; a verdict step's nonzero exit (4, incomplete, for instance) is recorded
# in steps.jsonl and ends its stage too. The snapshot stage copies SOURCE without its `.git`
# directory or worktree file into a new $WORK/repo, which must not exist, and commits it in a
# fresh repository of its own, never SOURCE's.
set -u -o pipefail
SOURCE=$1
WORK=$2
STAGE=$3
REAL_HOME=${REAL_HOME:-$HOME}
mkdir -p "$WORK/logs" || exit 1
REPO=$WORK/repo
LAB="python3 -B $REPO/scripts/performance/cli.py"
export PYTHONDONTWRITEBYTECODE=1
export PATH="$REAL_HOME/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin"
export RUSTUP_HOME=$REAL_HOME/.rustup
unset CARGO_TARGET_DIR AF_GATE_TARGET_DIR CARGO_HOME
export HOME=$WORK/home XDG_CONFIG_HOME=$WORK/home/.config XDG_DATA_HOME=$WORK/home/.local/share
export XDG_STATE_HOME=$WORK/home/.local/state XDG_CACHE_HOME=$WORK/home/.cache TMPDIR=$WORK/tmp/
mkdir -p "$HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME" "$WORK/tmp" \
  || exit 1

# step NAME ARGV...: run one step from $REPO and record it; a nonzero exit ends the stage with it.
step() {
  local name=$1; shift
  local start end status
  start=$(python3 -c 'import time; print(int(time.time() * 1000))') || exit 1
  (cd "$REPO" && "$@") >"$WORK/logs/$name.out" 2>"$WORK/logs/$name.err"
  status=$?
  end=$(python3 -c 'import time; print(int(time.time() * 1000))') || exit 1
  python3 - "$WORK/steps.jsonl" "$name" "$status" "$start" "$end" "$@" <<'EOF' || exit 1
import json, sys
path, name, status, start, end = sys.argv[1:6]
with open(path, "a") as stream:
    stream.write(json.dumps({"step": name, "argv": sys.argv[6:], "exit": int(status),
                             "started_unix_ms": int(start), "ended_unix_ms": int(end)},
                            sort_keys=True) + "\n")
EOF
  echo "$name: exit $status ($(( (end - start) / 1000 )) s)"
  [ "$status" -eq 0 ] || exit "$status"
}

git_env() {
  env GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_AUTHOR_NAME="af p8 workflow" \
    GIT_AUTHOR_EMAIL=p8@example.invalid GIT_COMMITTER_NAME="af p8 workflow" \
    GIT_COMMITTER_EMAIL=p8@example.invalid GIT_AUTHOR_DATE=2026-10-21T00:00:00Z \
    GIT_COMMITTER_DATE=2026-10-21T00:00:00Z "$@"
}

case $STAGE in
snapshot)
  # The measured source: every file of SOURCE but its build target and Git metadata, committed
  # once in a repository this stage creates.
  fail() { echo "snapshot: $1" >&2; exit 1; }
  [ -e "$REPO" ] && fail "$REPO already exists"
  mkdir "$REPO" || fail "cannot create $REPO"
  (cd "$SOURCE" && tar --exclude ./target --exclude ./.git -cf - .) \
    | (cd "$REPO" && tar -xf -) || fail "copying $SOURCE failed"
  [ -e "$REPO/.git" ] && fail "the copy kept Git metadata of $SOURCE"
  git_env git -C "$REPO" init -q -b main || fail "git init failed"
  git_env git -C "$REPO" add -A || fail "git add failed"
  git_env git -C "$REPO" commit -q -m "p8 measured source" || fail "git commit failed"
  [ "$(git -C "$REPO" rev-list --count HEAD)" = 1 ] || fail "the repository is not fresh"
  left=$(git -C "$REPO" status --porcelain --ignored) || fail "git status failed"
  [ -z "$(printf '%s\n' "$left" | grep -v -e '^!! target/$' -e '^$')" ] \
    || fail "files outside the commit: $left"
  git -C "$REPO" rev-parse HEAD > "$WORK/commit" || fail "git rev-parse failed"
  git -C "$REPO" rev-parse 'HEAD^{tree}' > "$WORK/tree" || fail "git rev-parse failed"
  ;;
plan)
  SHA=$(cat "$WORK/commit") || exit 1
  step capabilities $LAB capabilities
  step list $LAB list
  step plan $LAB plan --repo "$REPO" --trigger manual --scope partial_scope --tier nightly \
    --base "$SHA" --head "$SHA" --control "$SHA" --authority "$SHA" \
    --environment macos_arm64_local --shards 1 --pairs "${PAIRS:-20}" --seed 20261021 \
    --run-id p8_local_aa --case cli_discovery__command_version \
    --case canonical_validate_small__package_small_envelope \
    --case cas_typed_get_small__package_small_envelope --output "$WORK/run.json"
  ;;
fetch)
  # The pre-fetched, task-owned Cargo home every build reads offline.
  mkdir -m 700 "$WORK/cargo-home" || exit 1
  step fetch env CARGO_HOME="$WORK/cargo-home" cargo fetch --locked --target aarch64-apple-darwin
  ;;
build)
  step build $LAB build --plan "$WORK/run.json" --repo "$REPO" --cargo-home "$WORK/cargo-home"
  ;;
run)
  mkdir -m 700 -p "$WORK/scratch" || exit 1
  BUILD=$REPO/target/performance/p8_local_aa/build
  step run-shard-0 $LAB run --plan "$WORK/run.json" --build "$BUILD" --shard 0 \
    --run-dir "$WORK/run" --repo "$REPO" --trusted-local --scratch "$WORK/scratch"
  ;;
analyse)
  BUILD=$REPO/target/performance/p8_local_aa/build
  step aggregate $LAB aggregate --run "$WORK/run" --output "$WORK/verdict-aggregate.json"
  step compare $LAB compare --run "$WORK/run" --output "$WORK/verdict-compare.json"
  ;;
profile)
  BUILD=$REPO/target/performance/p8_local_aa/build
  step profile-trace $LAB profile --plan "$WORK/run.json" --repo "$REPO" \
    --case cli_discovery__command_version --case canonical_validate_small__package_small_envelope \
    --recipe macos_arm64_trace --mode trusted_local --run-id p8_local_trace \
    --parent-build "$BUILD" --cargo-home "$WORK/cargo-home" --total-minutes 30 \
    --output "$WORK/diag"
  ;;
report)
  step report $LAB report --run "$WORK/run" --output "$WORK/report" --diagnostic "$WORK/diag"
  ;;
archive)
  step archive-init $LAB archive --root "$WORK/archive" --init --capacity-bytes 2147483648
  step archive-import $LAB archive --root "$WORK/archive" --import "$WORK/run"
  step archive-status $LAB archive --root "$WORK/archive" --status
  step history $LAB history --inputs "$WORK/archive" --output "$WORK/history"
  ;;
rerun)
  # The reproduced plan executed: built and sampled like the original, then archived beside it.
  step rerun-build $LAB build --plan "$WORK/rerun.json" --repo "$REPO" --cargo-home "$WORK/cargo-home"
  step rerun-shard-0 $LAB run --plan "$WORK/rerun.json" \
    --build "$REPO/target/performance/p8_local_aa_rerun/build" --shard 0 \
    --run-dir "$WORK/rerun" --repo "$REPO" --trusted-local --scratch "$WORK/scratch"
  step rerun-aggregate $LAB aggregate --run "$WORK/rerun" --output "$WORK/verdict-rerun.json"
  step archive-import-rerun $LAB archive --root "$WORK/archive" --import "$WORK/rerun"
  step archive-status-2 $LAB archive --root "$WORK/archive" --status
  step history-2 $LAB history --inputs "$WORK/archive" --output "$WORK/history-2"
  ;;
reproduce)
  # The report's own reproduction commands, placeholders replaced, run as printed.
  python3 - "$WORK/report/report.json" "$WORK" "$REPO" > "$WORK/reproduce.argv" <<'PY' || exit 1
import json, sys
doc = json.load(open(sys.argv[1]))
work, repo = sys.argv[2], sys.argv[3]
for item in doc["reproduction"]:
    argv = [{"RUN_DIR": work + "/run", "NEW_REPORT_DIR": work + "/report-repro",
             "NEW_RUN_ID": "p8_local_aa_rerun"}.get(arg, arg) for arg in item["argv"]]
    if item["purpose"] == "rerun":
        argv += ["--repo", repo, "--output", work + "/rerun.json"]
    if item["purpose"] == "recompute":
        argv += ["--output", work + "/verdict-recompute.json"]
    print(item["purpose"] + "\t" + json.dumps(argv))
PY
  while IFS=$'\t' read -r purpose argv; do
    quoted=$(python3 -c 'import json, shlex, sys; print(" ".join(shlex.quote(a) for a in json.loads(sys.argv[1])))' "$argv") || exit 1
    eval "set -- $quoted"
    step "reproduce-$purpose" "$@"
  done < "$WORK/reproduce.argv"
  ;;
*)
  echo "unknown stage: $STAGE" >&2
  exit 2
  ;;
esac
