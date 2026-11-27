use std::fs::File;
use std::io;
use std::path::Path;

use nix::sys::statfs::fstatfs;

#[derive(Clone, Copy)]
pub(super) struct Allocation {
    unit: u64,
    ext_block: Option<u64>,
}

impl Allocation {
    pub(super) fn new(unit: u64) -> Self {
        Self {
            unit,
            ext_block: None,
        }
    }

    #[cfg(any(target_os = "linux", test))]
    pub(super) fn ext(block: u64, cluster: u64) -> Self {
        Self {
            unit: cluster,
            ext_block: Some(block),
        }
    }

    pub(super) fn file_bytes(self, logical: u64) -> Option<u64> {
        let data = logical.checked_next_multiple_of(self.unit)?;
        let Some(block) = self.ext_block else {
            return Some(data);
        };
        let blocks = logical.div_ceil(block);
        let mapping = indirect_blocks(blocks, block)?.max(extent_blocks(blocks, block)?);
        data.checked_add(mapping.checked_mul(self.unit)?)
    }
}

fn indirect_blocks(blocks: u64, block: u64) -> Option<u64> {
    // The first twelve data blocks are addressed in the inode. Each external block
    // can store block_size / 4 addresses. Count nonempty blocks at each of the
    // three levels after removing the ranges covered by the preceding levels.
    let pointers = block / 4;
    let single = blocks.saturating_sub(12).div_ceil(pointers);
    let double = single.saturating_sub(1).div_ceil(pointers);
    let triple = double.saturating_sub(1).div_ceil(pointers);
    single.checked_add(double)?.checked_add(triple)
}

fn extent_blocks(blocks: u64, block: u64) -> Option<u64> {
    // One extent per logical block covers arbitrary fragmentation. The inode holds
    // four entries. Its first overflow moves the root to one external block, which
    // only splits on overflow of its own 12-byte entries (after the 12-byte header).
    // Beyond that point assume no minimum fill: each of at most five external
    // levels has at most one node per logical block, including the inserted extent.
    match blocks {
        0..=4 => Some(0),
        n if n <= (block - 12) / 12 => Some(1),
        n => n.checked_mul(5),
    }
}

fn unmeasured() -> io::Error {
    io::Error::other(
        "cannot bound the observation snapshot allocation on the destination filesystem",
    )
}

pub(super) fn for_destination(directory: &Path) -> io::Result<Allocation> {
    let allocation = destination_allocation(&File::open(directory)?)?.ok_or_else(unmeasured)?;
    #[cfg(debug_assertions)]
    let allocation = {
        let mut allocation = allocation;
        // Simulate larger clusters or 4 KiB ext mappings without lowering the real bound.
        match std::env::var("AF_TEST_OBSERVATION_ALLOCATION_UNIT") {
            Ok(value) => {
                let simulated = value.parse::<u64>().map_err(io::Error::other)?;
                if !simulated.is_power_of_two() {
                    return Err(unmeasured());
                }
                allocation.unit = allocation.unit.max(simulated);
            }
            Err(std::env::VarError::NotPresent) => {}
            Err(error) => return Err(io::Error::other(error)),
        }
        if std::env::var_os("AF_TEST_OBSERVATION_EXT_MAPPING").is_some() {
            allocation.ext_block = Some(allocation.ext_block.unwrap_or(4096).min(4096));
        }
        allocation
    };
    Ok(allocation)
}

#[cfg(target_os = "linux")]
fn destination_allocation(directory: &File) -> io::Result<Option<Allocation>> {
    use std::os::unix::fs::MetadataExt;

    use nix::sys::statfs::{EXT4_SUPER_MAGIC, TMPFS_MAGIC};

    let volume = fstatfs(directory)?;
    #[allow(clippy::useless_conversion)]
    let block = u64::try_from(volume.block_size()).map_err(io::Error::other)?;
    if !block.is_power_of_two() {
        return Ok(None);
    }
    match volume.filesystem_type() {
        EXT4_SUPER_MAGIC if (1024..=65_536).contains(&block) => {
            let metadata = directory.metadata()?;
            Ok(ext_cluster_bound(block, metadata.len(), metadata.blocks())
                .map(|cluster| Allocation::ext(block, cluster)))
        }
        TMPFS_MAGIC if tmpfs_uses_bounded_pages(directory)? => Ok(Some(Allocation::new(block))),
        _ => Ok(None),
    }
}

#[cfg(target_os = "linux")]
fn ext_cluster_bound(block: u64, directory_len: u64, sectors: u64) -> Option<u64> {
    // A new, non-inline ext directory contains a real data block for "." and "..".
    // Ext4 charges at least its full allocation cluster to i_blocks, including on
    // bigalloc. An inline directory uses less than one block of logical space and
    // cannot establish this bound. Ext2/3 share the magic and use single blocks.
    let allocated = sectors.checked_mul(512)?;
    if directory_len != block || allocated < block {
        return None;
    }
    // A cluster is a power of two. Other blocks charged to the directory can make
    // this bound larger, and the largest power of two <= allocated is still safe.
    let exponent = u64::BITS - 1 - allocated.leading_zeros();
    Some(1_u64 << exponent)
}

#[cfg(target_os = "linux")]
fn tmpfs_uses_bounded_pages(file: &File) -> io::Result<bool> {
    use std::fs;
    use std::os::fd::AsRawFd;

    let fdinfo = fs::read_to_string(format!("/proc/self/fdinfo/{}", file.as_raw_fd()))?;
    let mount_id = fdinfo
        .lines()
        .find_map(|line| line.strip_prefix("mnt_id:").map(str::trim))
        .ok_or_else(unmeasured)?;
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")?;
    let options = mountinfo
        .lines()
        .find(|line| line.split_whitespace().next() == Some(mount_id))
        .and_then(|line| line.split_once(" - "))
        .and_then(|(_, filesystem)| filesystem.split_whitespace().nth(2))
        .ok_or_else(unmeasured)?;
    // No huge page advice is given to these files. With within_size, a huge page
    // must fit within the file or current write, so it cannot grow the reservation.
    if options.split(',').any(|option| {
        option.starts_with("huge=")
            && !matches!(
                option,
                "huge=never" | "huge=within_size" | "huge=advise" | "huge=deny"
            )
    }) {
        return Ok(false);
    }
    match fs::read_to_string("/sys/kernel/mm/transparent_hugepage/shmem_enabled") {
        Ok(policy) => Ok(policy.split_whitespace().any(|value| {
            matches!(
                value,
                "[always]" | "[within_size]" | "[advise]" | "[never]" | "[deny]"
            )
        })),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // On a kernel with no shmem THP support the knob is absent from sysfs.
            use nix::sys::statfs::{SYSFS_MAGIC, statfs};
            Ok(statfs("/sys/kernel/mm")?.filesystem_type() == SYSFS_MAGIC)
        }
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "macos")]
fn destination_allocation(file: &File) -> io::Result<Option<Allocation>> {
    let volume = fstatfs(file)?;
    // APFS charges the file its data stream blocks. Its extent records live in
    // the shared filesystem tree, outside the file's allocation count.
    let block = u64::from(volume.block_size());
    Ok(
        (volume.filesystem_type_name() == "apfs" && block.is_power_of_two())
            .then(|| Allocation::new(block)),
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn destination_allocation(_file: &File) -> io::Result<Option<Allocation>> {
    Ok(None)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn ext_cluster_bound_uses_the_fresh_destination_directory() {
        let block = 4096;
        assert_eq!(ext_cluster_bound(block, block, 8), Some(block));
        assert_eq!(ext_cluster_bound(block, block, 128), Some(65_536));
        assert_eq!(ext_cluster_bound(block, block, 136), Some(65_536));
        assert_eq!(ext_cluster_bound(block, 60, 128), None);
        assert_eq!(ext_cluster_bound(block, block, 0), None);
        assert_eq!(ext_cluster_bound(block, block, u64::MAX), None);
    }

    #[test]
    fn ext_indirect_mapping_covers_the_first_block_at_each_level() {
        let block = 4096;
        let pointers = block / 4;
        assert_eq!(indirect_blocks(12, block), Some(0));
        assert_eq!(indirect_blocks(13, block), Some(1));
        assert_eq!(indirect_blocks(12 + pointers, block), Some(1));
        assert_eq!(indirect_blocks(13 + pointers, block), Some(3));
        assert_eq!(
            indirect_blocks(12 + pointers + pointers * pointers, block),
            Some(pointers + 2)
        );
        assert_eq!(
            indirect_blocks(13 + pointers + pointers * pointers, block),
            Some(pointers + 5)
        );
    }

    #[test]
    fn ext_extent_mapping_covers_tree_growth_under_fragmentation() {
        let block = 4096;
        assert_eq!(extent_blocks(0, block), Some(0));
        assert_eq!(extent_blocks(4, block), Some(0));
        assert_eq!(extent_blocks(5, block), Some(1));
        assert_eq!(extent_blocks(340, block), Some(1));
        assert_eq!(extent_blocks(341, block), Some(1705));
        assert_eq!(extent_blocks(u64::MAX, block), None);
    }
}
