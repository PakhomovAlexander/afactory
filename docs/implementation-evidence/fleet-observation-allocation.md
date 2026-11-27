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

## Earlier Linux rounding and overlap observations

On this Task's Linux host, e2fsprogs 1.47.2 reported a 4,096-byte block and 65,536-byte cluster
in an unmounted ext4 bigalloc image. Its fresh directory had logical size 4,096 and block count
128. A 1,024-byte regular file had block count 128 as well. Both accounted for 65,536 bytes.
This reproduces the supplied w7/r-401 image measurement for the regular file and measures the
fresh directory the earlier implementation used to bound the cluster size. The Linux ext4 implementation
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

## Historical mapping metadata repair

This repair starts from Snapshot
`sha256:c2649219ca13c144c4a8f43d8d434cf1f9aab61a7db2992b4709297dfb826a4d`.
The two native findings cite Snapshot
`sha256:200bb283ca333b959b375bbdd1139f1631514b8e40b013242223244f8ba5b770`.

For ext2, ext3 and ext4, the reader rounds each of the database, WAL and rebuilt index to
its own destination allocation unit. It then adds the larger cost of the indirect map and
the extent tree, charging a full destination cluster for every possible mapping block.
The indirect map holds 12 direct addresses in the inode and four bytes per address in
external blocks. Three rounds of ceiling division count the single, double and triple
indirect blocks. The extent map can require one extent per logical filesystem block, even
when a bigalloc cluster contains multiple blocks. Four entries fit in the inode. One
external node suffices until its own entries can overflow. For larger files the reader
allows one nonempty node per logical block at each of five possible external levels.
This bound allows uneven splits and does not assume that extents are contiguous or that
nodes stay half full. The larger-file reservation can substantially exceed real use.
[Ext4 on-disk mapping formats](https://docs.kernel.org/filesystems/ext4/ifork.html)
[Ext4 split and growth implementation](https://github.com/torvalds/linux/blob/v6.12/fs/ext4/extents.c)

An empty file reserves zero data and mapping blocks. Missing WAL skips both sidecars.
A present WAL reserves its own bytes and the index allowance of WAL length plus one
32 KiB region. Rounding, multiplication and addition refuse overflow before copying.
Linux settles file and rebuilt index allocation while holding the existing account-wide
copy lock. The snapshot directory is created before measuring its destination.

Tmpfs counts allocated pages and has no disk mapping blocks to charge to its files. The
reader admits only the inherited bounded-page modes. On APFS, file extent records are
stored in shared volume trees. Its per-file reservation covers the rounded data stream.
Shared volume metadata, including APFS copy-on-write tree updates, is outside AF's
regular-file inventory. Measuring free bytes and subtracting this reservation did not
bound those updates. The calculation also missed external attribute blocks charged to
files on ext2. The in-memory repair below supersedes that reservation. No APFS execution
was performed for the mapping repair.
[Linux tmpfs huge-page policies](https://docs.kernel.org/admin-guide/mm/transhuge.html#hugepages-in-tmpfs-shmem)
[Apple File System Reference](https://developer.apple.com/support/downloads/Apple-File-System-Reference.pdf)

### Unmounted ext2 measurement

This Linux host reproduced the supplied w7 measurement with e2fsprogs 1.47.2. The 12-page
file used 96 sectors. The 13-page file used 112 sectors and reported an indirect block.
Both dumped databases matched their input files byte for byte and passed
`PRAGMA integrity_check`.

| SQLite pages | Logical bytes | Allocated bytes | Indirect block |
| ---: | ---: | ---: | --- |
| 12 | 49,152 | 49,152 | Absent |
| 13 | 53,248 | 57,344 | Present |

Run from the checkout with Python 3, SQLite support in its standard library and e2fsprogs
on PATH. The recipe uses a new disposable image and does not mount it.

```sh
mkdir -p target/observation-evidence
work=$(mktemp -d target/observation-evidence/ext2.XXXXXX)
python3 - "$work" <<'PY'
import sqlite3
import sys
from pathlib import Path

for rows in (10, 11):
    with sqlite3.connect(Path(sys.argv[1]) / f"{rows}.sqlite") as db:
        db.execute("PRAGMA page_size=4096")
        db.execute("CREATE TABLE items (id INTEGER PRIMARY KEY, value BLOB)")
        db.executemany("INSERT INTO items(value) VALUES (?)", [(b"x" * 3500,)] * rows)
PY
image=$work/volume.img
truncate -s 16M "$image"
mke2fs -q -t ext2 -b 4096 -F "$image"
for rows in 10 11; do
    file=$work/$rows.sqlite
    debugfs -w -R "write $file /$rows.sqlite" "$image"
    debugfs -R "stat /$rows.sqlite" "$image"
    debugfs -R "dump /$rows.sqlite $file.dump" "$image"
    cmp "$file" "$file.dump" || exit 1
done
python3 - "$work" <<'PY'
import sqlite3
import sys
from pathlib import Path

for rows in (10, 11):
    with sqlite3.connect(Path(sys.argv[1]) / f"{rows}.sqlite.dump") as db:
        print(db.execute("PRAGMA page_count").fetchone()[0])
        assert db.execute("PRAGMA integrity_check").fetchone() == ("ok",)
PY
```

A separate unmounted 4,096-byte-block ext4 image also used 57,344 bytes for a
53,248-byte file. Interleaving single-block `debugfs fallocate` calls with writes to
separate filler files produced thirteen unwritten extents and one external extent-tree
block (`ETB0`). This was a fragmented file allocation witness. The SQLite integrity
checks above were on the two ext2 database files.

### Public reader comparison

The real `af cluster origin-read` fixture used a valid 1,024-byte-page Project database
at the measured mapping boundary. On both tmpfs and ordinary mounted ext4 the Project
database measured 53,248 logical and allocated bytes, the Workspace database measured
147,456 logical and allocated bytes, and the eligible inventory was 524,288 bytes.
The debug fixture adds at least the 4 KiB ext mapping reservation and supplies the free
bytes for testing the floor. The inherited executable ignores the mapping fixture.

| Policy | Settings | Inherited executable | Candidate executable |
| --- | --- | --- | --- |
| Machine budget | `max_bytes=577536` | Project copied, Workspace refused | Project refused before copy |
| Free-space floor | Free 2,306,048, `min_free_bytes=2252800` | Project copied, Workspace refused | Project refused before copy |
| Sufficient capacity | `max_bytes=2826240`, free 2,306,048, `min_free_bytes=4096` | Complete reader succeeded | Complete reader succeeded |

Both budget and floor cases supplied 53,248 bytes of headroom. The candidate also
refused both at 57,343 bytes. In the control the live private Project copy measured
53,248 allocated bytes on both mounted filesystems. The witnessed 57,344-byte
allocations came from the unmounted images. The larger-cluster fixture still refused
both limits at 55,296 bytes of headroom. Its positive control now supplies 4,194,304
bytes of headroom so the independent mapping allowance can also fit. The ordinary
small-page fixture still refused both limits at 44,544 bytes. Its positive control
now supplies 2,289,664 bytes.

For the telemetry test, the measured Project and Workspace inventory was 618,496 bytes.
The registered external Store went from 466,944 to 1,515,520 bytes after adding
1,048,576 allocated bytes of telemetry inside `cas`. Each source database was
147,456 logical and allocated bytes. The test determines the first complete budget
before adding the payload, then adds its measured allocation once. That exact budget
succeeds and one byte less refuses. A separate unowned Store subtree remains excluded
until it is explicitly selected as the telemetry directory.

| Destination and executable | First budget with empty telemetry | First budget with payload |
| --- | ---: | ---: |
| Tmpfs, inherited and candidate | 1,380,352 | 2,428,928 |
| Ordinary ext4, inherited | 1,380,352 | 2,428,928 |
| Ordinary ext4, candidate | 1,384,448 | 2,433,024 |

The mounted ext4 threshold rose by 4,096 bytes for the Workspace copy's mapping
allowance while the already-copied Project was counted at its observed allocation.
The telemetry increment remained exactly 1,048,576 bytes. To reproduce the public
comparison, use the focused command above with `TMPDIR` on the desired filesystem.
For the inherited comparison substitute the inherited binary as described above and run
`fleet::origin_reserves_ext_mapping_before_copying` and
`fleet::origin_counts_telemetry_inside_a_registered_external_store_once` separately.
The inherited mapping regression is expected to fail after printing its control and
the Workspace refusals.

## Bounded in-memory repair

This repair starts from Source Snapshot
`sha256:d33fa95a3d0d2c7e2e90ccd4abdee956af5bad8225332aed68067c07e7a90dc3`.
The APFS findings identify reviewed Snapshot
`sha256:fd58c831ba25a44696567f810f897f013d75c713ae23b92978549d0de6baefec`.

The reader creates no snapshot files, directories or indexes when it checks the selected
volume or captures and replays the Stores. The storage budget charges existing AF-owned
disk usage. The separate volume check admits at exact equality with the configured floor.
These checks measure current conditions. The read does not control allocations by other
processes or the operating system.

Before reading either file into a buffer, the reader checks the combined logical lengths
against 64 MiB, including holes. It checks again with any WAL-driven expansion before
allocating the database image. Capture keeps at most 64 MiB of database and WAL buffers.
After dropping the WAL, SQLite deserialization can use one further copy of the image.
The Project image can remain live while replaying the Task Store, yielding at most 192 MiB
in snapshot buffers per account, with ordinary SQLite and projection working memory in
addition. An existing account inode carries the exclusive lock across both reads. The
kernel releases that lock and reclaims the images if the process exits or is killed.

The database shared lock and the WAL write, checkpoint and recovery byte ranges exclude
native mutations during capture. The reader verifies file identities and refuses a rollback
journal or a newly appeared sidecar. For a live WAL, the Unix VFS dead-man switch lock
identifies an attached index. The reader validates both index header copies and their
checksums and checks the published frame count, page count, salt and final checksum against
the WAL. It overlays published frames after the checkpoint's backfill boundary. Once all
SQLite connections release the dead-man switch, the reader recovers the valid on-disk
commit prefix, including when stale shared-memory bytes remain. A read-only
SQLite connection inspects the private image with memory-only temporary storage. The same
path runs on macOS and Linux.

### Interrupted live WAL publication

Run the public regression with the bundled SQLite:

```sh
cargo nextest run --workspace --locked --profile ci \
  -E 'binary_id(af::it) & test(/^fleet::native_origin_respects_interrupted_live_wal_publication$/)'
```

The fixture creates a prior committed frame and keeps a SQLite read transaction open.
An ordinary Task transition writes and publishes the later frames. A separate fixture process
restores the prior pair of index headers while preserving SQLite's WAL bytes, producing the
state a writer can leave when it stops before publishing its new frame count. A fresh native
SQLite reader sees the prior event count. The origin must omit the waiting Task during that
window and expose it after the last SQLite connection closes, both with the stale index bytes
present and after the fixture removes them. Native SQLite subsequently recovers the later
event count from the same WAL.

The fixture compares SHA-256, logical lengths, allocated blocks and modes of the source files
for each observation and checks for observation runtime files. To replay the before case,
build the inherited `af` executable and substitute it for the candidate CLI in the same
test, with other tests stopped. On Linux, the inherited executable exited the fixture with
the waiting `pagination-cli` Task on the live result side of the assertion and an empty
expected list. The native-reader assertion and source-file checks had passed. Restoring the
candidate executable passed the live, stale-index and missing-index assertions.

### External ACL block measurement

On Linux, a directory with a named default ACL passed the ACL to a file created with
owner-only mode `0600`. Linux kept the named entry and masked its access.
Separately, an unmounted ext2 image with 4,096-byte blocks and 128-byte inodes stored a
four-page SQLite database with and without that access ACL. Both files used exactly four
direct data blocks. The ACL-bearing file charged one additional block. Its retrieved
bytes matched the original, and SQLite reported `ok` from `PRAGMA integrity_check`.

| SQLite file | Logical bytes | Allocated bytes | External ACL block |
| --- | ---: | ---: | --- |
| No ACL | 16,384 | 16,384 | Absent |
| Named access ACL | 16,384 | 20,480 | Present |

The inherited calculation allowed four data blocks and zero mapping blocks for this file.
Its reservation is 16,384 bytes. The ACL witness requires 20,480. The new reader creates
no destination files that could inherit an ACL, including the database, WAL, index and
runtime directory. The ext2 exercise sets the inherited ACL explicitly with `debugfs` on
an unmounted image.

Run with Python 3 and e2fsprogs on a Linux filesystem supporting POSIX ACLs.

```sh
mkdir -p target/observation-evidence
work=$(mktemp -d target/observation-evidence/acl.XXXXXX)
python3 - "$work" <<'PY'
import os
import sqlite3
import struct
import sys
from pathlib import Path

root = Path(sys.argv[1])
with sqlite3.connect(root / "four.sqlite") as db:
    db.execute("PRAGMA page_size=4096")
    db.execute("CREATE TABLE items (id INTEGER PRIMARY KEY, value BLOB)")
    db.executemany("INSERT INTO items(value) VALUES (?)", [(b"x" * 3500,)] * 2)
    assert db.execute("PRAGMA page_count").fetchone() == (4,)
entries = [(1, 7, -1), (2, 4, os.getuid()), (4, 5, -1), (16, 5, -1), (32, 0, -1)]
acl = struct.pack("<I", 2) + b"".join(struct.pack("<HHi", *entry) for entry in entries)
directory = root / "inheriting"
directory.mkdir()
os.setxattr(directory, "system.posix_acl_default", acl)
child = directory / "file"
os.close(os.open(child, os.O_CREAT | os.O_WRONLY, 0o600))
actual = os.getxattr(child, "system.posix_acl_access")
assert child.stat().st_mode & 0o777 == 0o600
assert (2, 4, os.getuid()) in struct.iter_unpack("<HHi", actual[4:])
assert (16, 0, -1) in struct.iter_unpack("<HHi", actual[4:])
(root / "acl").write_bytes(actual)
PY
image=$work/ext2.img
truncate -s 16M "$image"
mke2fs -q -t ext2 -b 4096 -I 128 -F "$image"
debugfs -w -R "write $work/four.sqlite /plain.sqlite" "$image"
debugfs -w -R "write $work/four.sqlite /acl.sqlite" "$image"
debugfs -w -R 'set_inode_field /plain.sqlite mode 0100600' "$image"
debugfs -w -R 'set_inode_field /acl.sqlite mode 0100600' "$image"
debugfs -w -R "ea_set -f $work/acl /acl.sqlite system.posix_acl_access" "$image"
debugfs -R 'stat /plain.sqlite' "$image"
debugfs -R 'stat /acl.sqlite' "$image"
debugfs -R "dump /acl.sqlite $work/recovered.sqlite" "$image"
cmp "$work/four.sqlite" "$work/recovered.sqlite"
python3 - "$work/recovered.sqlite" <<'PY'
import sqlite3
import sys

with sqlite3.connect(sys.argv[1]) as db:
    assert db.execute("PRAGMA integrity_check").fetchone() == ("ok",)
PY
```

### Paired public reader measurements

The Linux public CLI comparison used the inherited executable and the patched executable
on the same fixtures. Only the reported free bytes in the disk-boundary fixture were
simulated. Both executables read a 1,024-byte-page Project database with 44,032 logical
and 45,056 allocated bytes. The Workspace database measured 147,456 logical and allocated
bytes. The eligible inventory was 516,096 bytes. The configured runtime directory started
absent.

| Machine limit | Free bytes | Free floor | Inherited | In-memory |
| ---: | ---: | ---: | --- | --- |
| 516,095 | 4,194,304 | 4,096 | Refused | Refused |
| Unlimited | 4,194,304 | 4,194,305 | Refused | Refused |
| 516,096 | 4,194,304 | 4,096 | Refused | Succeeded |
| Unlimited | 4,194,304 | 4,194,304 | Refused | Succeeded |
| 516,096 | 4,194,304 | 4,194,304 | Refused | Succeeded |
| 4,710,400 | 4,194,304 | 4,096 | Succeeded | Succeeded |

At both exact limits the new reader returned the expected observation with unchanged
source bytes and inventory. It left the runtime directory and the old admission registry
absent. This models a volume with no spare bytes above the floor. It does not measure
APFS copy-on-write growth. The earlier macOS observations in this document are historical.
No current macOS host was available to run the new reader.

The geometry fixture replays the 53,248-byte Project database at the indirect boundary.
The source and Workspace allocations were 53,248 and 147,456 bytes, and eligible inventory
was 524,288. The new reader succeeded with 20,479, 57,343 and 65,535 bytes of headroom under
both disk policies and created zero snapshot files. The cluster and ext2 image recipes
above reproduced the earlier allocation witnesses on Linux. The ACL recipe reproduced the
additional 4,096-byte charge that the old file calculation omitted.

The external Store fixture measured 618,496 owned Project and Workspace bytes and 466,944
owned external Store bytes before adding telemetry. The latter rose to 1,515,520 after
adding 1,048,576 measured bytes inside `cas`. The old and new thresholds on the checkout's
ext filesystem both increased by that single telemetry allocation.

| Executable | Empty telemetry threshold | Populated telemetry threshold |
| --- | ---: | ---: |
| Inherited | 1,384,448 | 2,433,024 |
| In-memory | 1,085,440 | 2,134,016 |

The new exact total admits, and a limit one byte lower refuses. A separately configured
unowned subtree adds its own charge. Unmeasurable overlapping telemetry still refuses.

Run the public boundary and WAL fixtures on a volume with room for the default free-space
floor. The concurrency fixture starts a second process while the first holds the account
lock and repeats for normal close and forced interruption.

```sh
mkdir -p target/origin-tmp
TMPDIR="$PWD/target/origin-tmp"   cargo nextest run --workspace --locked --profile ci --success-output immediate   -E '(binary_id(af::it) & test(/^fleet::/)) | (package(af-store) & test(/observation_snapshot/))'
```

For the paired Linux comparison retain the inherited executable before building the
candidate. With other test runs stopped, substitute the inherited executable as described
above and run
`fleet::origin_admits_small_sqlite_pages_at_independent_disk_limits` and
`fleet::origin_counts_telemetry_inside_a_registered_external_store_once` separately.
The fixtures print the inherited outcomes and thresholds before they fail on a new
expectation. Restore the candidate executable before re-running the filtered tests.
