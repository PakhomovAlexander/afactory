# Native child cancellation verification, 2026-11-23

Built the candidate on Linux with `cargo build --locked --bin af` using Rust 1.88.0.
The SHA-256 of `target/debug/af` was
`ad1cc3dd2dbcf4785404975a9b52df8d177eb1f0de8b1835832887143a5e09d4`.

Ran the unchanged manual fixture from the repository root:

```sh
TMPDIR=/dev/shm/af-native-child-cancel.u486M7St \
AF_DURABILITY_SCRATCH=/dev/shm/af-native-child-cancel.u486M7St \
python3 -B scripts/durability-e2e/native_child_cancel.py \
  /opt/tasks/af-sandboxes/af-sandbox-514352-CspMSM/tree/target/manual-cancel-runtime.vOh5lQXa/af "$PWD"
```

The scratch directory was fresh and isolated. The launcher exported
`AF_STORAGE__SANDBOX_ROOT` and `XDG_CACHE_HOME` to private directories on the
checkout's filesystem, then used `exec` to run the locally built binary. This
kept both volumes above the existing free-space floor. The fixture used its
existing mock Provider and isolated home and Task and Scenario Stores.

The command exited **0** and all seven fixture invariants passed. The stopped
coordinator made no Scenario progress during cancellation. Its replacement
settled the original Run as `failed`, retained the child's `host_cancelled`
outcome and did not redispatch that Task. The next distinct Ant occurrence
succeeded. The fixture reported zero known chargeable tokens.

| Observation | Task Attempts |
| --- | ---: |
| Original child before cancellation | 1 |
| Original child after cancellation, before replacement | 1 |
| Original child after replacement | 1 |
| Child of the next Ant occurrence | 3 |

## Missing-Task settlement follow-up

Built with `cargo build --locked --bin af` on Linux using Rust 1.88.0.
The tested binary's SHA-256 was
`9df8d2da8aab659f0174f86f3627248479a14c22c9abd76958eea2a442c13292`.

Ran the unchanged fixture from the checkout root with a private scratch directory
under `/dev/shm` and a launcher that redirected the sandbox and cache roots to
private directories on the checkout's filesystem. The fixture ran its existing
mock Provider and used fresh Task and Scenario Stores.

```sh
TMPDIR="$scratch" AF_DURABILITY_SCRATCH="$scratch" \
  python3 -B scripts/durability-e2e/native_child_cancel.py "$launcher" "$PWD"
```

The command exited **0** and all seven invariants passed. The original Run ended
as `failed` and the original child remained `abandoned/cancelled/host_cancelled`
before and after replacement. Its Attempt count stayed at **1** before
cancellation, after cancellation and after replacement. The next distinct Ant
occurrence succeeded with **3** Attempts. The fixture reported zero known
chargeable tokens across both children.

## Required CAS object permission follow-up

On Linux, the unprivileged worker prepared a pinned Task intent, stopped its
coordinator at the pre-admission barrier, and set the exact execution plan's
CAS file to mode `000`. The Task event log still replayed, while opening the
plan file returned `EACCES`. Before the fix, `af scenario resume` exited 1
with `event store artifact: cas io: Permission denied (os error 13)` and left
the prepared intent unsettled. The native CLI regression covers recovery after
restoring the original file, integrity refusals for missing or corrupt plan
bytes, and Scenario settlement at its deadline, driver reservation cap, and
operator cancellation while the plan is unreadable.

Built with `cargo build --locked --bin af` on Linux using Rust 1.88.0.
The tested binary's SHA-256 was
`6c398beb0ea56cdb96e99e95320702fc04535f922042d6c4a88abda3d3701094`.
The unchanged fixture's SHA-256 was
`4a358c55e66322011de04e6c0ec6c9cc77b2de45c14218319bb984c1665171af`.
The run used fresh private Stores and the same scratch and launcher arrangement
as the missing-Task follow-up.

```sh
TMPDIR="$scratch" AF_DURABILITY_SCRATCH="$scratch" \
  python3 -B scripts/durability-e2e/native_child_cancel.py "$launcher" "$PWD"
```

The fixture exited **0** and reported **7/7** invariants true. The stopped
coordinator made no progress during Task cancellation. Its replacement settled
the same Scenario Run as `failed`. The original child remained
`abandoned/cancelled/host_cancelled` with one Attempt before cancellation,
after cancellation, and after replacement. The next distinct Ant occurrence
succeeded with three Attempts. The fixture recorded zero known chargeable tokens
across both children.
