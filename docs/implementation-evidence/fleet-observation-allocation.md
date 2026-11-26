# Project observation allocation evidence

All sizes are bytes. Allocated file sizes use 512-byte block counts. The Task began
from Source Snapshot `sha256:d9939810c92394b3a95a6b896d69ae5030d16fd79aa2aac5abf295c9048bb4e7`
and the allocation findings name reviewed Snapshot
`sha256:3f0389284a295f4d83991dbdacdcb09d45246cc1d99308947f9ac3cb87324f43`.

## Configuration and earlier macOS observations

The Task supplied these measurements from private macOS integrations using the public
`af cluster origin-read` command. Each database row shows the supplied sum of regular file
allocations under isolated `state/af` and the first `max_bytes` that admitted the Project
copy. That filesystem inventory sum and the reader's eligible inventory are distinct.

| Integration | SQLite logical | SQLite allocated | State allocated | First Project budget |
| --- | ---: | ---: | ---: | ---: |
| Before `bf7a6b86f6909b306beaded24e089b6eb948eda3` | 2,151,424 | 2,154,496 | 2,555,904 | 4,703,232 |
| Before, with a 1,024-byte increase | 2,152,448 | 2,154,496 | 2,555,904 | 4,704,256 |
| After `28598a15b2b590f4ff966bf36f0307046f394f45` | 2,151,424 | 3,198,976 | 3,600,384 | 5,750,784 |
| After, with a 1,024-byte increase | 2,152,448 | 3,198,976 | 3,600,384 | 5,750,784 |

The before integration raised the admission threshold by 1,024 for the logical growth.
The after integration kept its threshold fixed as both logical lengths fit the same allocation.
The supplied observations measured the thresholds and file allocations. The volume's allocation
unit was unspecified.

On the same integrations the Task measured the smallest budget for the complete reader before
and after adding 2,097,152 allocated telemetry bytes below `cas/telemetry` in a registered
external Task Store. The threshold succeeded and one byte less refused.

| Integration | Budget without payload | Budget with payload | Increase |
| --- | ---: | ---: | ---: |
| Before `bf7a6b86f6909b306beaded24e089b6eb948eda3` | 9,441,280 | 13,635,584 | 4,194,304 |
| After `28598a15b2b590f4ff966bf36f0307046f394f45` | 8,388,608 | 10,485,760 | 2,097,152 |

## Linux allocation observations

On this Task's Linux host, e2fsprogs 1.47.2 reported a 4,096-byte block and 65,536-byte cluster
in an unmounted ext4 bigalloc image. Its fresh directory had logical size 4,096 and block count
128. A 1,024-byte regular file had block count 128 as well. Both accounted for 65,536 bytes.
This reproduces the supplied w7/r-401 image measurement for the regular file and measures the
fresh directory the candidate uses to bound the cluster size. The Linux ext4 implementation
initializes a non-inline directory with a data block and charges the inode in whole clusters.
[Directory initialization](https://github.com/torvalds/linux/blob/v6.12/fs/ext4/namei.c)
[Cluster allocation](https://github.com/torvalds/linux/blob/v6.12/fs/ext4/mballoc.c)

Run these commands from the checkout root with e2fsprogs on PATH.

```sh
mkdir -p target/observation-evidence
image=target/observation-evidence/bigalloc.img
payload=target/observation-evidence/payload
truncate -s 64M "$image"
mkfs.ext4 -q -F -O bigalloc -b 4096 -C 65536 "$image"
head -c 1024 /dev/urandom > "$payload"
debugfs -w -R 'mkdir /snapshot' "$image"
debugfs -w -R "write $payload /snapshot/probe" "$image"
dumpe2fs -h "$image"
debugfs -R 'stat /snapshot' "$image"
debugfs -R 'stat /snapshot/probe' "$image"
```

The public reader fixture uses actual SQLite and tmpfs allocations on this host. The
debug fixture raises the destination unit to 65,536 and simulates available free bytes.
The public-reader tests used ordinary mounted filesystems. The bigalloc evidence comes
from the unmounted image.

In both the inherited and candidate runs of the cluster fixture, the Project database
had 44,032 logical bytes and 45,056 allocated bytes. The Workspace database had
147,456 logical and allocated bytes. The inventory was 516,096. With the simulated unit,
the required reservations were 65,536 for the Project and 196,608 for the Workspace.

| Policy | Settings | Inherited executable | Candidate executable |
| --- | --- | --- | --- |
| Machine budget | `max_bytes=571392`, giving 55,296 above inventory | Project admitted, Workspace refused | Project refused before copying |
| Free-space floor | Free 327,680, `min_free_bytes=272384`, giving 55,296 above the floor | Project admitted, Workspace refused | Project refused before copying |
| Sufficient capacity | `max_bytes=778240`, free 327,680, `min_free_bytes=65536` | Complete reader succeeded | Complete reader succeeded |

The ordinary tmpfs fixture used the same SQLite sizes and inventory with a measured
4,096-byte unit. The inherited and candidate readers both refused the Project at 44,544
bytes of headroom under each policy and completed at 192,512 bytes of headroom.
Those machine budgets were 560,640 and 708,608. The simulated free count was 196,608,
with floors of 152,064 and 4,096. The fixture measured 45,056 allocated bytes in the live
private Project copy at the sufficient capacity.

The local external Store fixture also passed on both executables. Its Project and Workspace
inventory was 618,496, its registered Store counted 1,515,520 including 1,048,576 telemetry
bytes, and each snapshot copy cost 147,456. The complete reader succeeded at 2,428,928 and
refused at 2,428,927. The fixture also checks an unowned external Store subtree that has to
be charged separately.

The candidate repeated the three public boundary fixtures with its temporary directory on
the checkout's mounted ordinary ext4 volume. They produced the same file sizes, inventory,
refusals, and sufficient-capacity results.

Run the focused fixtures on a debug build.

```sh
cargo nextest run --workspace --locked --profile ci --success-output immediate \
  -E 'binary_id(af::it) & test(/fleet::origin_(reserves|counts_telemetry)/)'
```

For the inherited cluster comparison, build the executable from the inherited Snapshot and
run the candidate's `fleet::origin_reserves_larger_clusters_before_copying` test against that
executable. With other test runs stopped, temporarily substitute the inherited executable
for the candidate's `target/debug/af` and invoke the candidate's
`target/debug/deps/it-<hash> --exact fleet::origin_reserves_larger_clusters_before_copying --nocapture`
using the `af::it` integration test binary. Restore the candidate executable afterward.
The inherited executable ignores the debug allocation-unit input. The test prints each
refusal path before asserting that both refusals name the Project. That assertion fails
with the inherited executable.
