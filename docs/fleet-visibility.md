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
Connecting checks the pinned host and reads both management capabilities before saving a private
binding. Repeat the same command to verify it again. A different descriptor or credential
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
It reports SSD headroom at the host level. Text shows storage pool inventory as unknown, and
an empty JSON `pools` list means the provider reported no storage pool identities. Static
availability is a separate view of the same host capacity and is excluded from the host's
headroom total. Physical totals, reservations and desired footprint are unknown. A stopped
allocation remains visible with unknown charges and executor. The infrastructure view withholds
any Task hint in the management response.

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

The SSH account needs read access to the Project's registered Workspace and Task Store. Running
that command on the origin also shows the precise Project and Store identities to put in the
descriptor. For a principal separately authorized to take native plan Decisions, add
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

Text and JSON render the same [briefing](../schemas/fleet-briefing-v1.json), including the next
action, its reason and its success condition. The infrastructure cache lives under
`${XDG_STATE_HOME:-$HOME/.local/state}/af/fleet-cache`. A successful management read refreshes its
per-context observation. An authenticated management command can return exit 75 to carry forward
a stale observation from the exact binding for up to 24 hours. A failed connection, refused
credential, host-key mismatch or invalid response cannot disclose a cached observation.

The [fleet integration tests](../crates/af/tests/it/fleet.rs) exercise the six public commands with
local OpenSSH endpoints, disposable keys, the management wire formats and a native Task waiting
for plan review. They also exercise a fresh offline standalone Task workflow.

Run the Fleet tests from an ordinary macOS user shell with `cargo test --locked -p af --test it
fleet::`. The native SSH test hands accepted loopback sockets to Apple's inetd-mode `sshd` so it
can create audit sessions for the forced commands. It requires a shell where the daemon can
initialize its own sandbox. The Linux fixture uses stdio so it can authenticate in gates that
forbid TCP listeners. Both use isolated per-test configuration and require no running SSH service.
