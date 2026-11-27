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

## Mapping metadata repair

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
regular-file inventory. The free-space check uses the volume's measured available headroom
for the copy reservation. It does not reserve the volume against concurrent activity or
quantify growth of shared APFS trees. No APFS execution was performed for this repair.
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
