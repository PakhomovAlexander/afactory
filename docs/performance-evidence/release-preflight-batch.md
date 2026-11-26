# Release preflight batch reads

Measured on 26 November 2026 UTC on one Linux x86-64 worker with four visible Intel Core
i5-13400 CPUs, Git 2.47.3 and Python 3.13.5. The parent was
`f97ac450d3bfd95f6537ebbdc744cd1479db4677` (tree
`70cba1c32d9c39a8fbc13901ec58fafe22f3f01a`). The candidate implementation patch SHA-256
is `c957f7d67ddf241675d93b1e9eab44306780992228f455b71998e4a56be719cd`.
The driver hashes the unified diff of `scripts/dev-release.py` with fixed path labels, so
the identity is independent of this evidence file and of the eventual candidate commit.

## Reproduce

From the candidate checkout, set `parent_repo` to a local Git repository containing the parent
object, then run the commands below. The driver also accepts the parent and candidate as two
separate extracted trees.

```bash
set -o pipefail
scratch=$(mktemp -d)
mkdir "$scratch/parent"
git -C "$parent_repo" archive f97ac450d3bfd95f6537ebbdc744cd1479db4677 |
  tar -xf - -C "$scratch/parent"
python3 -I docs/performance-evidence/release-preflight-batch.py "$scratch/parent" "$PWD" |
  tee "$scratch/results.jsonl"
```

The driver uses the existing `ReleaseCLI` test fixture, with a fresh source repo, bare local
remote, state directory, GitHub fixture and private home for every successful plan. It calls
the real `dev release plan --repo o/r --remote <remote.git> --state <state> --format json`.
Git runs for real, including the fetch and temporary worktree. The fixture supplies tool and
GitHub responses and a harness-check stand-in. Each plan's elapsed time starts after fixture
setup. Git process counts are Trace2 `start` events, including Git children that inherit the
trace. The isolated read uses that same plan's fetched commit and mirror. It times only
retrieving and normalizing the 54 file contents, then verifies those contents and the plan
receipt against separate real `git show` reads outside the timed interval.

## Results

| Pair | Side | Plan seconds | Git processes | Exit | Isolated 54 seconds | Git processes | Exit |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | Parent | 1.095331 | 141 | 0 | 0.149554 | 54 | 0 |
| 1 | Candidate | 1.033655 | 35 | 0 | 0.017352 | 2 | 0 |
| 2 | Candidate | 0.796094 | 35 | 0 | 0.010588 | 2 | 0 |
| 2 | Parent | 0.946497 | 141 | 0 | 0.088371 | 54 | 0 |
| 3 | Parent | 0.949353 | 141 | 0 | 0.086318 | 54 | 0 |
| 3 | Candidate | 0.778932 | 35 | 0 | 0.010206 | 2 | 0 |
| Median | Parent | 0.949353 | 141 | 0 | 0.088371 | 54 | 0 |
| Median | Candidate | 0.796094 | 35 | 0 | 0.010588 | 2 | 0 |

All 54 path-to-digest receipts matched individually on every run. The sorted JSON map SHA-256
was `4d57b1b9874877ebfea4c3c38b7c103758d8d8ec560d2303126eb80ab1ac41f5`.
The plan projection covering the policy, version, profile, pin status and preflight receipt
matched at `bfdde023cab05c4e040400f77811675f2d992b292b5002145fe4c4542d17a89e`.
The driver separately checked each run's actual fetched source and executor identity.

The first pair also replanned using the same mirrors after moving the local remotes' main refs.
Both sides refused malformed JSON at `schemas/backlog-signed-submission-v1.json` with
`source_inclusion: invalid required schema` and refused the absent path with the same Git exit
128 diagnostic. Neither case retained a plan or performed a publication effect.

| Case | Parent seconds | Parent Git | Parent exit | Candidate seconds | Candidate Git | Candidate exit |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Malformed | 0.221697 | 17 | 1 | 0.220685 | 18 | 1 |
| Missing | 0.265560 | 17 | 1 | 0.264345 | 19 | 1 |

These observations describe the local release-plan fixture and the isolated read. The Mac
operator's test of the actual PR commit and the native performance comparison are separate runs.
The release contract checker also verified the existing declaration against the workspace
version and exact capabilities. The declaration's Git blob ID is
`d5e0a3d589c47b62e180274babcd2c8880d7df5e` on both sides.
