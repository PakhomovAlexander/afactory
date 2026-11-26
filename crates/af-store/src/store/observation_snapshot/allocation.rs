use std::fs::File;
use std::io;
use std::path::Path;

use nix::sys::statfs::fstatfs;

#[derive(Clone, Copy)]
pub(super) struct Allocation(u64);

impl Allocation {
    pub(super) fn new(unit: u64) -> Self {
        Self(unit)
    }

    pub(super) fn file_bytes(self, logical: u64) -> Option<u64> {
        logical.checked_next_multiple_of(self.0)
    }
}

fn unmeasured() -> io::Error {
    io::Error::other(
        "cannot bound the observation snapshot allocation on the destination filesystem",
    )
}

pub(super) fn for_destination(directory: &Path) -> io::Result<Allocation> {
    let unit = destination_unit(&File::open(directory)?)?.ok_or_else(unmeasured)?;
    // A debug fixture can model larger destination clusters without mounting a filesystem.
    // It can only increase the reservation on a destination whose bound was established.
    #[cfg(debug_assertions)]
    let unit = match std::env::var("AF_TEST_OBSERVATION_ALLOCATION_UNIT") {
        Ok(value) => {
            let simulated = value.parse::<u64>().map_err(io::Error::other)?;
            if !simulated.is_power_of_two() {
                return Err(unmeasured());
            }
            unit.max(simulated)
        }
        Err(std::env::VarError::NotPresent) => unit,
        Err(error) => return Err(io::Error::other(error)),
    };
    Ok(Allocation::new(unit))
}

#[cfg(target_os = "linux")]
fn destination_unit(directory: &File) -> io::Result<Option<u64>> {
    use std::os::unix::fs::MetadataExt;

    use nix::sys::statfs::{EXT4_SUPER_MAGIC, TMPFS_MAGIC};

    let volume = fstatfs(directory)?;
    #[allow(clippy::useless_conversion)]
    let block = u64::try_from(volume.block_size()).map_err(io::Error::other)?;
    if !block.is_power_of_two() {
        return Ok(None);
    }
    match volume.filesystem_type() {
        EXT4_SUPER_MAGIC => {
            let metadata = directory.metadata()?;
            Ok(ext_cluster_bound(block, metadata.len(), metadata.blocks()))
        }
        TMPFS_MAGIC if tmpfs_uses_bounded_pages(directory)? => Ok(Some(block)),
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
fn destination_unit(file: &File) -> io::Result<Option<u64>> {
    let volume = fstatfs(file)?;
    // APFS uses its fundamental filesystem block size for data extents.
    let block = u64::from(volume.block_size());
    Ok((volume.filesystem_type_name() == "apfs" && block.is_power_of_two()).then_some(block))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn destination_unit(_file: &File) -> io::Result<Option<u64>> {
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
}
