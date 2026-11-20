# Project backlog operator runbook

This page covers the current offline checkpoint: private Report capture and inspection in one
local Project Store. It does not configure a designated authority, transfer Reports between
machines, run collection or verification, schedule an Ant, or publish tickets. Those integration
steps are not available through this CLI checkpoint and are intentionally not described here.

The privacy and receipt boundaries are fixed by
[ADR-2026-11-20](adr/2026-11-20-store-project-backlog-reports-privately.md). Project and Workspace
identity and the Store location are defined in [Projects and Workspaces](project.md).

## Offline local capture and inspection

`af backlog record` requires a nonblank `--title` and `--observed` fact. `--report-id` is optional;
supplying a stable producer ID lets an identical retry recover a lost receipt without duplicating
the Report. `--repro` is optional. Each `--evidence-file` adds private, untrusted UTF-8 text; at
most eight files and 65,536 bytes in total are accepted. Evidence is validated before capture.

The Report and its evidence are committed to the local Project Store at
`$XDG_STATE_HOME/af/project/<hex>/`, where `<hex>` is the Project ID digest; AF's ordinary local
Store registry is also below `$XDG_STATE_HOME/af/`. Worktrees with the same repository root
commits resolve the same Project Store. No Provider, model, credential, remote repository, or
network service is needed.

### Disposable two-Workspace demonstration

Use Bash, Git, and the `af` binary built from this source. The block uses
`set -euo pipefail`: Bash's `pipefail` option makes a failed AF command fail its pipeline even
when `tee` succeeds. Put that binary on `PATH`, or set `AF` to its absolute path before running
the block. The block creates one local sample repository and two clean, detached worktrees. It
also creates a fresh private home and XDG directories, so it neither reads nor changes an
operator's installed AF state. Every AF command below is expected to exit zero; `tee` preserves
its JSON result under the disposable directory.

```bash
set -euo pipefail
AF=${AF:-af}
scratch=$(mktemp -d "${TMPDIR:-/tmp}/af-backlog.XXXXXX")
trap 'rm -rf -- "$scratch"' EXIT

export HOME="$scratch/home"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_STATE_HOME="$HOME/.local/state"
export XDG_CACHE_HOME="$HOME/.cache"
export XDG_DATA_HOME="$HOME/.local/share"
export XDG_BIN_HOME="$HOME/.local/bin"
mkdir -p "$XDG_CONFIG_HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME" \
  "$XDG_DATA_HOME" "$XDG_BIN_HOME"
export AF_SELF_OFFLINE=1
export NO_COLOR=1
export GIT_CONFIG_GLOBAL="$scratch/gitconfig"
export GIT_CONFIG_NOSYSTEM=1

git init -q "$scratch/repository"
git -C "$scratch/repository" config user.name "AF Backlog Example"
git -C "$scratch/repository" config user.email "af-backlog@example.invalid"
git -C "$scratch/repository" commit --allow-empty -q -m "sample root"
git -C "$scratch/repository" worktree add --quiet --detach \
  "$scratch/producer-a" HEAD
git -C "$scratch/repository" worktree add --quiet --detach \
  "$scratch/producer-b" HEAD

printf '%s\n' 'synthetic evidence: local setup reached the stop marker' \
  > "$scratch/evidence.txt"

(
  cd "$scratch/producer-a"
  "$AF" backlog record \
    --report-id producer-a \
    --title "Synthetic planner stop" \
    --observed "The synthetic planner stopped after local setup." \
    --evidence-file "$scratch/evidence.txt" \
    --json
) | tee "$scratch/receipt-a.json"

(
  cd "$scratch/producer-b"
  "$AF" backlog record \
    --report-id producer-b \
    --title "Synthetic planner stop" \
    --observed "The synthetic planner stopped after local setup." \
    --evidence-file "$scratch/evidence.txt" \
    --json
) | tee "$scratch/receipt-b.json"

cd "$scratch/producer-b"
"$AF" backlog inbox --json | tee "$scratch/inbox.json"
"$AF" backlog show producer-a --private --json | tee "$scratch/report-a.json"
"$AF" backlog list --json | tee "$scratch/list.json"
```

Run the whole block again in a fresh Bash shell to repeat the demonstration. `mktemp` selects a
new repository, pair of worktrees, and Project Store, so the new inbox again contains exactly
these two Reports. Reusing an old scratch directory is not a fresh-workspace run.

### Recorded fresh-workspace validation

The block above (SHA-256
`0c05d174dc56ae03f45a7052072cfd20cb17f44e8a25f5ea69f54571e3bce961`) was executed unchanged
with `/bin/bash` against the built AF CLI (binary SHA-256
`a268d42be418c04ebd85610aaaee7defbf293a77f55ad529a3902d93825ff3ab`). Each run used a new
temporary repository, two clean worktrees, and private XDG state. The command-exit columns below
follow the block's order: producer A's `record`, producer B's `record`, `inbox`, private `show`,
and `list`.

| Run | Project ID | Overall exit | Record A | Record B | Inbox | Private show | List | Wall seconds |
|---|---|---|---:|---:|---:|---:|---:|---:|
| 1 | `sha256:983728c7134e59d9c3496a449e74c7ed871e4ebf230d84e11f958c5ed2803500` | 0 | 0 | 0 | 0 | 0 | 0 | 2.233 |
| 2 | `sha256:b0971b7c13448d60eb002928cf9bf585398b3b4aca6827768ef8266a59b3beb9` | 0 | 0 | 0 | 0 | 0 | 0 | 0.414 |

The distinct Project IDs record the two fresh repositories. In both runs the observed views were
`capture`, `capture`, `inbox`, `report`, and `list`, and the Report IDs were `producer-a` and
`producer-b`. Each run produced two receipts with `local_capture = true` and zero authority
receipts. The private inbox contained two Reports, private `show` returned `producer-a`, and the
verified item count was zero (`data.items = []`). Both runs used zero Provider tokens.

### Read the two capture receipts

Every JSON result has `schema = "af/backlog@1"`, a `view`, and a `data` object. The two capture
commands produce `view = "capture"` and these receipt fields:

| Field | What the two results show |
|---|---|
| `data.project_id` | The same repository-derived Project ID in both worktrees. |
| `data.report_id` | The distinct stable producer IDs `producer-a` and `producer-b`. |
| `data.captured_unix_ms` | The first durable capture time for each Report, as a decimal string. |
| `data.local_capture` | `true`: the Report and its evidence committed to the local Project Store before the receipt was printed. |
| `data.authority_receipt` | `null`: no designated authority has received or acknowledged either Report. |

Thus, a successful local receipt is not an authority receipt, an independent verification, or a
publication receipt. The identical title, fact, and evidence do not merge the Reports: their
Report IDs and Workspace IDs remain distinct. An identical retry with the same producer ID keeps
the original capture time and returns the local receipt again; reusing that ID for different
content is refused.

### Inspect private and verified views

The last three commands run in `producer-b` but read the Project shared with `producer-a`:

- `inbox.json` has `view = "inbox"`. Its `data.reports` array contains both private Reports.
  Each Report includes `report_id`, `workspace_id`, `captured_unix_ms`, `title`, `observed`,
  `source_revision`, `privacy = "private"`, and `evidence`. Task and Run provenance are `null`
  when AF has no trusted values. Each evidence entry has `content_id`, `media_type`, and `text`.
- `report-a.json` has `view = "report"`; `data` is the single `producer-a` Report. The explicit
  `--private` selects local operator inspection and includes the private fact and evidence.
- `list.json` has `view = "list"` and `data.items = []`. Local capture alone creates no verified
  item. Ordinary `af backlog list` and `af backlog show` expose only verified item and revision
  IDs and approved public fields; they do not disclose raw Reports or evidence.

Do not treat an empty verified list as a lost Report: use `af backlog inbox` or
`af backlog show REPORT_ID --private` for the private intake view.

### Regression coverage for these semantics

The integration test
[`backlog_recovers_a_lost_receipt_and_shares_privately_across_worktrees`](../crates/af/tests/it/project.rs)
uses two worktrees and distinct producer IDs, checks both capture-receipt fields, recovers a lost
receipt, reads both Reports from the shared inbox, checks private `show`, and verifies that the
verified list is empty. The adjacent `backlog_refuses_bad_fact_and_evidence_before_capture` test
covers blank facts, invalid UTF-8, oversized evidence, and symlink refusal. The JSON shapes are
defined by [`backlog-v1.json`](../schemas/backlog-v1.json) and
[`backlog-record-v1.json`](../schemas/backlog-record-v1.json).
