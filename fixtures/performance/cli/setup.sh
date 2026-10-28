#!/bin/sh
# Fixture setup of the performance laboratory's CLI cases (fixtures/performance/cli.toml,
# ADR-0168, ADR-0169) and seeded native samples (ADR-0170). The authority supervisor runs it
# under the same containment and environment as the measured command, in the sample's private
# scratch: as a sample's untimed setup step, or, once per side and seed, to write and publish
# that side's seed. It is the authority's reviewed code; every product write it makes goes
# through the side's own binaries.
#
#   setup.sh SETUP SCRATCH AF PROBE ARGUMENT
#
# task_repository      a fresh Git repository of task-roots/repo at SCRATCH/repo, and the Task
#                      file at SCRATCH/task.json; ARGUMENT is the side's check-receipt
#                      generation (ADR-0255): `receipt-v2`, or `receipt-v1` for a side before
#                      ADR-0255, whose repository gets task-roots/receipt-v1 laid over it so its
#                      evaluator declares the receipt that side's AF writes
# task_started         the same, then the Task captured by the side's own `af task start`
# seed_copy            a private copy of the read-only seed ARGUMENT at SCRATCH/seed
# native_seed_copy     a private, writable copy of the read-only native seed ARGUMENT at
#                      SCRATCH/seed, and the probe's own empty scratch SCRATCH/probe
# seed_task_state      the side's own probe writes the Task state seed below SCRATCH/seed; its
#                      message (request ARGUMENT) must report exactly 10,000 events
# seed_campaign_state  the side's own `af review run` writes the fixture Campaign of
#                      campaign-roots/repo at SCRATCH/seed/campaign, then the side's own probe
#                      (request ARGUMENT) tops its log up to exactly 10,000 events
# seed_publish         copy SCRATCH/seed, written by a seed writer whose processes have all
#                      exited, to the empty directory ARGUMENT
# registry_empty       create private HOME/XDG directories without a Store registry
# registry_stores_20x10000  copy the Task seed to 20 explicit Stores and register each
#                      through the side's own `af task gc` preview: a command that names a
#                      --state Store records it (ADR-0144) and the preview writes nothing
#                      else; `af task list --state` only reads, and records nothing
#
# Any failure exits nonzero and the supervisor measures nothing.
set -eu

setup=$1
scratch=$2
af=$3
probe=$4
argument=$5
fixtures=$(dirname "$0")

git_fixture() {
    repository=$1
    shift
    GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_TERMINAL_PROMPT=0 \
        GIT_AUTHOR_NAME=fixture GIT_AUTHOR_EMAIL=fixture@example.invalid \
        GIT_COMMITTER_NAME=fixture GIT_COMMITTER_EMAIL=fixture@example.invalid \
        GIT_AUTHOR_DATE=2026-01-01T00:00:00Z GIT_COMMITTER_DATE=2026-01-01T00:00:00Z \
        git -C "$repository" "$@"
}

# Copy a tree as fresh directories and files of this user's default modes, never the source's:
# the authority fixtures and sealed seeds are read-only, and a directory created read-only cannot
# be made writable again on every filesystem a scratch may live on.
copy_tree() {
    (cd "$1" && find . -type d) | while IFS= read -r directory; do
        mkdir -p "$2/$directory"
    done
    (cd "$1" && find . -type f) | while IFS= read -r file; do
        cat "$1/$file" >"$2/$file"
    done
}

repository() {
    source=$1
    target=$2
    mkdir "$target"
    copy_tree "$source" "$target"
    git_fixture "$target" init -q -b main
    git_fixture "$target" add -A
    git_fixture "$target" commit -q -m "performance CLI fixture"
}

task_repository() {
    mkdir "$scratch/repo"
    copy_tree "$fixtures/task-roots/repo" "$scratch/repo"
    case "$argument" in
    receipt-v2) ;;
    receipt-v1) copy_tree "$fixtures/task-roots/receipt-v1" "$scratch/repo" ;;
    *)
        echo "setup.sh: unknown check-receipt generation $argument" >&2
        exit 2
        ;;
    esac
    git_fixture "$scratch/repo" init -q -b main
    git_fixture "$scratch/repo" add -A
    git_fixture "$scratch/repo" commit -q -m "performance CLI fixture"
    cat "$fixtures/task-roots/task.json" >"$scratch/task.json"
}

probe_writer() {
    printf '%s' "$argument" | "$probe" >"$scratch/writer.json"
    grep -q '"correctness":{"status":"passed"' "$scratch/writer.json"
    grep -q '"count":10000,' "$scratch/writer.json"
}

registry_home() {
    mkdir -p "$scratch/home/state" "$scratch/home/config" "$scratch/home/cache" \
        "$scratch/home/data"
}

case "$setup" in
task_repository)
    task_repository
    ;;
task_started)
    task_repository
    "$af" task start --file "$scratch/task.json" --repo "$scratch/repo" --json \
        >"$scratch/start.json"
    ;;
seed_copy)
    test -d "$argument"
    mkdir "$scratch/seed"
    copy_tree "$argument" "$scratch/seed"
    ;;
native_seed_copy)
    # A native seed may hold 10,000 files, so it is copied in one process and only then made
    # writable by its owner: new directories and files, never links to the sealed seed.
    test -d "$argument"
    mkdir "$scratch/probe"
    cp -R "$argument" "$scratch/seed"
    chmod -R u+w "$scratch/seed"
    ;;
seed_task_state)
    mkdir "$scratch/seed"
    probe_writer
    test -f "$scratch/seed/probe-work/events.sqlite"
    ;;
seed_campaign_state)
    repository "$fixtures/campaign-roots/repo" "$scratch/campaign-repo"
    mkdir "$scratch/seed"
    (cd "$scratch/campaign-repo" &&
        "$af" review run --campaign perf-campaign --state "$scratch/seed/campaign" \
            --pipeline .af/pipelines/review.toml --policy-rev HEAD --json \
            >"$scratch/review.json")
    mkdir "$scratch/seed/campaign-probe"
    probe_writer
    rm -r "$scratch/seed/campaign-probe"
    test -f "$scratch/seed/campaign/events.sqlite"
    ;;
seed_publish)
    test -d "$scratch/seed"
    copy_tree "$scratch/seed" "$argument"
    ;;
registry_empty)
    registry_home
    ;;
registry_stores_20x10000)
    test -f "$argument/probe-work/events.sqlite"
    registry_home
    number=1
    while test "$number" -le 20; do
        target="$scratch/home/explicit/$number"
        mkdir -p "$target"
        copy_tree "$argument/probe-work" "$target"
        (cd "$fixtures" && "$af" task gc --older-than 36500 --keep 1000 --state "$target" \
            >"$scratch/registry-gc.txt")
        number=$((number + 1))
    done
    test -f "$scratch/home/state/af/stores.toml"
    test "$(grep -c '^\[\[store\]\]$' "$scratch/home/state/af/stores.toml")" -eq 20
    ;;
*)
    echo "setup.sh: unknown setup $setup" >&2
    exit 2
    ;;
esac
