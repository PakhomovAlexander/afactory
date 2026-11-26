# Fleet visibility

Use `af infra --json` on a fresh Mac to see whether a fleet context is configured. Get the
connection descriptor and SSH host keys from the owner through a trusted channel. The descriptor
contains public pins and reader locations. Install the management SSH identity under
`${XDG_CONFIG_HOME:-$HOME/.config}/af/fleet/credentials/<ref>`. This entry can link to your private
identity file or to a public identity whose private key is available in your SSH agent. Use a
restricted read-only SSH grant and keep the private key on your machine.

```sh
af infra connect --context lab --from ./lab.afcontext --credential-ref operator
af infra --context lab --json
af infra status --context lab --json
af infra allocations --context lab --json
af cluster --infra lab --json
af cluster inbox --context lab --json
```

A sole context is selected automatically. With multiple contexts, select one explicitly.
Connecting checks the pinned host and reads both management capabilities concurrently under one
30-second deadline. It reports the cause for each failed capability and saves a private binding
only after both reads succeed. Repeat the same
command to verify it again. A different descriptor or credential
reference requires a different alias. Fleet commands use the running release independently of
the current checkout. Ordinary Task and Review commands continue to use local selection.

## Owner configuration

A descriptor is a JSON [fleet context](../schemas/fleet-context-v1.json). Set `schema` to
`af/fleet-context@1`, set `scope` to the stable owner-chosen identifier, and supply `management`
with `host`, `port`, `user` and `host_key`. The pin has the OpenSSH public-key format
`<algorithm> <base64>`, omitting the comment. The optional `cluster` field accepts `absent` when
the owner confirms the cluster has yet to be initialized and otherwise defaults to `unknown`.

Authorize the management account to run only these two commands. Both are GETs. Loopback resolves
on the management host through SSH.

```text
control-api http://localhost/pools
control-api http://localhost/dynamic/allocations
```

The broker's `static` and `dynamic` lists contain VM names from resource pool membership.
It reports SSD headroom at the host level and leaves storage pool inventory unknown. Static
availability is a separate view of the same host capacity and is excluded from the host's
headroom total. Physical totals, reservations and desired footprint are unknown. A stopped
allocation remains visible and can still carry a disk charge. For example, a broker-reported
`charged.ssd_gib` of `74.50580596923828` is displayed as `80 GB disk` on the stopped VM's row.
JSON retains the charge in GiB. If the broker omits the charge, the row says “charges unknown”
and the JSON field is null. The executor remains unknown. The infrastructure view withholds any
Task hint in the management response.

### Project access

Each descriptor `origins` entry is keyed by an owner-chosen origin label. A Project operator
supplies its `target` in the same pinned SSH format and the exact `project_id` and `store_id`.
Install a separately authorized identity under
`${XDG_CONFIG_HOME:-$HOME/.config}/af/fleet/project-credentials/<context>/<origin>`.

On the origin, the Project operator binds that SSH public key to a restrictive forced command
using `authorized_keys` options `restrict,command="…"` or the equivalent server policy. Use the
absolute path to the installed af release that wrote the Store. The command and all its options
are selected by the server. An example with placeholders follows.

```sh
/path/to/af cluster origin-read --scope SCOPE --origin ORIGIN --project PROJECT_ID \
  --workspace WORKSPACE_LABEL --principal PRINCIPAL --state-home /path/to/xdg-state
```

The SSH account needs read access to the Project's registered Workspace and Task Store,
including existing SQLite WAL and shared-memory files when present. The reader takes a private
snapshot under SQLite's local file locks and replays committed frames from that snapshot. The
snapshot directory and the database and WAL copies are created with owner-only permissions.
SQLite can rebuild its shared-memory index inside that directory. The reader leaves source
Store files and permissions untouched and needs temporary space for the snapshot.
An active write, checkpoint, or inaccessible snapshot can make the origin
temporarily unavailable. Running the command on the origin also shows the precise Project and
Store identities to put in the descriptor. For a principal separately authorized to take native
plan Decisions, add
`--allow-plan-decisions` and map `--principal` to their captured native developer identity.
An undecided, unexpired outer plan or prepared experimental child plan waiting for review can
then show a Decision. A principal with
only read access can see waiting Tasks. The native signed Decision mechanism remains in charge
of approval and rejection. The reader does not need access to the signing key.

Inbox results include Task ID, waiting phase and reason, log sequence and age, and placement when
known. Each origin authenticates independently. The caller sees an unavailable origin when current
read access cannot be established. Project observations are never cached by the client. An
origin that returns a timestamp more than five minutes old under live read authority is marked
stale and its Decisions are withheld. Discovery readiness does not indicate execution readiness.

## Reading the briefing

Text and JSON render the same [briefing](../schemas/fleet-briefing-v3.json), including the next
action, its reason and its success condition. JSON reports unknown storage pool inventory as
`inventory.data.pools: null` and a known empty inventory as `inventory.data.pools: []`. Text
displays “Storage pool inventory unknown” and “No storage pools” respectively. Host-level SSD
availability is reported independently. Infrastructure and Project reads run concurrently under
one 20-second deadline per invocation. A reading whose command exceeds this deadline has
`timed_out: true` in JSON and a deadline message on its text row. Its freshness is `unavailable`
and its data is null. Other readings retain their own outcomes, and identity, protocol or access
failures still determine the next action when present.

The infrastructure cache lives under `${XDG_STATE_HOME:-$HOME/.local/state}/af/fleet-cache`.
A successful management read can refresh its per-context observation if it fits the
machine's `[storage] max_bytes` budget. `af storage` reports these directories as fleet
cache entries and `af storage prune --apply` can reclaim them. A live observation still prints
when there is no room to cache it. An authenticated management command can return exit 75 to carry forward
a stale observation from the exact binding for up to 24 hours. A failed connection, refused
credential, host-key mismatch or invalid response cannot disclose a cached observation. Only
current [version 2 cache records](../schemas/fleet-cache-v2.json) are eligible. If the management
reader is temporarily unavailable after upgrade and only an obsolete record exists, the
observation is unavailable until a successful read refreshes it.

An origin reader replays private SQLite copies under the sandbox runtime root
(`[storage] sandbox_root` or the system temporary directory). It checks the storage limit and
free-space floor on the destination volume before copying. The reservation rounds the database,
any copied WAL and its rebuilt SQLite index separately to the destination allocation unit.
Readers under the same OS account serialize admission and copying through a private registry
in the account's state directory,
including when they use different sandbox roots or temporary directories. The lock is released
for replay. Admission uses the machine Storage Budget inventory, counts retained copies and
overlapping telemetry and cache directories once, and measures Store files without opening
their databases. For a registered external Task Store, admission counts its owned entries.
Telemetry inside one of those entries shares that entry's charge. An incomplete inventory or
insufficient space makes the origin unavailable.
Readers remove their copies on close. A subsequent origin read can reclaim abandoned copies
across registered roots and leaves live readers' copies alone.

The [fleet integration tests](../crates/af/tests/it/fleet.rs) exercise the six public commands with
local OpenSSH endpoints, disposable keys, the management wire formats and a native Task whose
waiting transition is still in the WAL. They also cover clean shutdown, read-only Store files
and directories, concurrent writes, and a fresh offline standalone Task workflow.
The [Store snapshot regression](../crates/af-store/src/store/observation_snapshot.rs) checks
permissions on the live snapshot under umasks `000` and `022` while replaying committed WAL frames.

Run the Fleet tests from an ordinary user shell.

```sh
cargo nextest run --workspace --locked --profile ci -E 'binary_id(af::it) & test(/^fleet::/)'
```

The authenticated test requires `/usr/bin/ssh`, `/usr/sbin/sshd`, and `ssh-keygen`. The native SSH
test hands each accepted loopback socket to a separate Apple inetd-mode `sshd` so both management
reads can proceed concurrently and create audit sessions for their forced commands. It requires
a shell where the daemon can initialize its own sandbox.
The Linux fixture uses stdio so it can authenticate in gates that
forbid TCP listeners. Both use isolated per-test configuration and require no running SSH service.
