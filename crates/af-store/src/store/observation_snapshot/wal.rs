use std::io;

const HEADER_BYTES: usize = 32;
const FRAME_HEADER_BYTES: usize = 24;
const FORMAT_VERSION: u32 = 3_007_000;
const INDEX_HEADER_BYTES: usize = 48;
pub(super) const INDEX_PREFIX_BYTES: usize = 2 * INDEX_HEADER_BYTES + 4;

/// SQLite's native-endian WalIndexHdr pair followed by WalCkptInfo.nBackfill.
#[derive(PartialEq, Eq)]
pub(super) struct LiveIndex([u8; INDEX_PREFIX_BYTES]);

impl LiveIndex {
    pub(super) fn parse(bytes: &[u8; INDEX_PREFIX_BYTES]) -> io::Result<Self> {
        let header = &bytes[..INDEX_HEADER_BYTES];
        let checksum = checksum_words([0, 0], &header[..40], cfg!(target_endian = "big"));
        if header != &bytes[INDEX_HEADER_BYTES..2 * INDEX_HEADER_BYTES]
            || native_word(&header[..4]) != FORMAT_VERSION
            || header[12] != 1
            || header[13] > 1
            || checksum != [native_word(&header[40..44]), native_word(&header[44..48])]
            || native_word(&bytes[96..100]) > native_word(&header[16..20])
        {
            return Err(io::Error::other(
                "invalid live SQLite WAL index; retry after SQLite has recovered the Store",
            ));
        }
        Ok(Self(*bytes))
    }

    pub(super) fn commit<'a>(
        &self,
        wal: &'a [u8],
        page_size: usize,
        database_size: u64,
    ) -> io::Result<Option<Commit<'a>>> {
        let header = &self.0[..INDEX_HEADER_BYTES];
        let frames = native_word(&header[16..20]) as usize;
        if frames == 0 {
            // A WAL restart publishes an empty index before replacing old WAL bytes.
            return Ok(None);
        }
        let inconsistent = || {
            io::Error::other(
                "live SQLite WAL index does not match the committed WAL prefix; retry after SQLite has recovered the Store",
            )
        };
        let encoded_size = u16::from_ne_bytes(header[14..16].try_into().unwrap());
        let index_page_size = if encoded_size == 1 {
            65_536
        } else {
            usize::from(encoded_size)
        };
        let frame_bytes = page_size + FRAME_HEADER_BYTES;
        let end = frames
            .checked_mul(frame_bytes)
            .and_then(|bytes| HEADER_BYTES.checked_add(bytes))
            .ok_or_else(inconsistent)?;
        let prefix = wal.get(..end).ok_or_else(inconsistent)?;
        let last = &prefix[end - frame_bytes..];
        if index_page_size != page_size
            || word(&prefix[8..12]) as usize != page_size
            || header[13] != prefix[3] & 1
            || header[32..40] != prefix[16..24]
            || native_word(&header[20..24]) != word(&last[4..8])
            || [native_word(&header[24..28]), native_word(&header[28..32])]
                != saved_checksum(&last[16..24])
        {
            return Err(inconsistent());
        }
        let mut commit = last_commit(prefix, page_size)?
            .filter(|commit| commit.frames.len() == end - HEADER_BYTES)
            .ok_or_else(inconsistent)?;
        let backfilled = native_word(&self.0[96..100]) as usize;
        if backfilled == frames && commit.image_size() > database_size {
            return Err(inconsistent());
        }
        // SQLite reads checkpointed pages from the database and uses frames after
        // nBackfill for the overlay. Validation still covers the entire prefix.
        commit.frames = &commit.frames[backfilled * frame_bytes..];
        Ok(Some(commit))
    }
}

fn native_word(bytes: &[u8]) -> u32 {
    u32::from_ne_bytes(bytes.try_into().expect("four-byte WAL index word"))
}

pub(super) struct Commit<'a> {
    frames: &'a [u8],
    page_size: usize,
    pages: u32,
}

impl Commit<'_> {
    pub(super) fn image_size(&self) -> u64 {
        u64::from(self.pages) * self.page_size as u64
    }

    pub(super) fn apply(self, image: &mut [u8]) {
        for frame in self
            .frames
            .chunks_exact(self.page_size + FRAME_HEADER_BYTES)
        {
            let page = word(&frame[..4]);
            if page <= self.pages {
                let offset = (page as usize - 1) * self.page_size;
                image[offset..offset + self.page_size]
                    .copy_from_slice(&frame[FRAME_HEADER_BYTES..]);
            }
        }
        // The WAL commit supplies the authoritative page count, including truncation.
        image[28..32].copy_from_slice(&self.pages.to_be_bytes());
    }
}

/// Recover the durable prefix as SQLite's walIndexRecover does. An incomplete frame,
/// stale salt after WAL reuse, or invalid checksum ends the prefix. Valid frames after
/// the last commit are uncommitted. Checksum words on disk are always big-endian.
pub(super) fn last_commit(wal: &[u8], database_page_size: usize) -> io::Result<Option<Commit<'_>>> {
    if wal.len() <= HEADER_BYTES {
        return Ok(None);
    }
    let (header, frames) = wal.split_at(HEADER_BYTES);
    let magic = word(&header[..4]);
    let page_size = word(&header[8..12]) as usize;
    if magic & !1 != 0x377f_0682
        || !page_size.is_power_of_two()
        || !(512..=65_536).contains(&page_size)
    {
        return Ok(None);
    }
    let big_endian = magic & 1 != 0;
    let mut checksum = checksum_words([0, 0], &header[..24], big_endian);
    if checksum != saved_checksum(&header[24..]) {
        return Ok(None);
    }
    if word(&header[4..8]) != FORMAT_VERSION {
        return Err(io::Error::other("unsupported SQLite WAL format version"));
    }

    let frame_bytes = page_size + FRAME_HEADER_BYTES;
    let mut commit = None;
    for (index, frame) in frames.chunks_exact(frame_bytes).enumerate() {
        if frame[8..16] != header[16..24] || word(&frame[..4]) == 0 {
            break;
        }
        checksum = checksum_words(checksum, &frame[..8], big_endian);
        checksum = checksum_words(checksum, &frame[FRAME_HEADER_BYTES..], big_endian);
        if checksum != saved_checksum(&frame[16..24]) {
            break;
        }
        let pages = word(&frame[4..8]);
        if pages != 0 {
            commit = Some(Commit {
                frames: &frames[..(index + 1) * frame_bytes],
                page_size,
                pages,
            });
        }
    }
    if commit.is_some() && page_size != database_page_size {
        return Err(io::Error::other(
            "SQLite WAL and database page sizes differ",
        ));
    }
    Ok(commit)
}

fn word(bytes: &[u8]) -> u32 {
    u32::from_be_bytes(bytes.try_into().expect("four-byte WAL word"))
}

fn saved_checksum(bytes: &[u8]) -> [u32; 2] {
    [word(&bytes[..4]), word(&bytes[4..8])]
}

fn checksum_words(mut sums: [u32; 2], bytes: &[u8], big_endian: bool) -> [u32; 2] {
    let read_word = if big_endian {
        u32::from_be_bytes
    } else {
        u32::from_le_bytes
    };
    for pair in bytes.chunks_exact(8) {
        let a = read_word(pair[..4].try_into().expect("four-byte checksum word"));
        let b = read_word(pair[4..].try_into().expect("four-byte checksum word"));
        sums[0] = sums[0].wrapping_add(a).wrapping_add(sums[1]);
        sums[1] = sums[1].wrapping_add(b).wrapping_add(sums[0]);
    }
    sums
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: usize = 512;

    fn wal_header(big_endian: bool, version: u32) -> Vec<u8> {
        let mut header = Vec::new();
        for value in [
            0x377f_0682 | u32::from(big_endian),
            version,
            PAGE as u32,
            0,
            123,
            456,
        ] {
            header.extend(value.to_be_bytes());
        }
        header.extend(
            checksum_words([0, 0], &header, big_endian)
                .map(u32::to_be_bytes)
                .concat(),
        );
        header
    }

    fn append_frame(wal: &mut Vec<u8>, page: u32, pages: u32, fill: u8, big_endian: bool) {
        let mut prefix = [0; 8];
        prefix[..4].copy_from_slice(&page.to_be_bytes());
        prefix[4..].copy_from_slice(&pages.to_be_bytes());
        let last_checksum = if wal.len() == HEADER_BYTES {
            &wal[24..32]
        } else {
            let offset = wal.len() - PAGE - 8;
            &wal[offset..offset + 8]
        };
        let data = [fill; PAGE];
        let checksum = checksum_words(saved_checksum(last_checksum), &prefix, big_endian);
        let frame_checksum = checksum_words(checksum, &data, big_endian);
        let salt: [u8; 8] = wal[16..24].try_into().unwrap();
        wal.extend(prefix);
        wal.extend(salt);
        wal.extend(frame_checksum.map(u32::to_be_bytes).concat());
        wal.extend(data);
    }

    fn index_prefix(wal: &[u8], frames: u32, backfilled: u32) -> [u8; INDEX_PREFIX_BYTES] {
        let mut header = [0; INDEX_HEADER_BYTES];
        header[..4].copy_from_slice(&FORMAT_VERSION.to_ne_bytes());
        header[12] = 1;
        header[13] = wal[3] & 1;
        header[14..16].copy_from_slice(&(PAGE as u16).to_ne_bytes());
        header[16..20].copy_from_slice(&frames.to_ne_bytes());
        header[32..40].copy_from_slice(&wal[16..24]);
        if frames != 0 {
            let start = HEADER_BYTES + (frames as usize - 1) * (PAGE + FRAME_HEADER_BYTES);
            let frame = &wal[start..];
            header[20..24].copy_from_slice(&word(&frame[4..8]).to_ne_bytes());
            header[24..28].copy_from_slice(&word(&frame[16..20]).to_ne_bytes());
            header[28..32].copy_from_slice(&word(&frame[20..24]).to_ne_bytes());
        }
        seal_index_header(&mut header);
        let mut bytes = [0; INDEX_PREFIX_BYTES];
        bytes[..INDEX_HEADER_BYTES].copy_from_slice(&header);
        bytes[INDEX_HEADER_BYTES..2 * INDEX_HEADER_BYTES].copy_from_slice(&header);
        bytes[96..].copy_from_slice(&backfilled.to_ne_bytes());
        bytes
    }

    fn seal_index_header(header: &mut [u8]) {
        let sums = checksum_words([0, 0], &header[..40], cfg!(target_endian = "big"));
        header[40..44].copy_from_slice(&sums[0].to_ne_bytes());
        header[44..48].copy_from_slice(&sums[1].to_ne_bytes());
    }

    #[test]
    fn observation_snapshot_honors_the_live_frame_limit_and_backfill() {
        for big_endian in [false, true] {
            let mut wal = wal_header(big_endian, FORMAT_VERSION);
            append_frame(&mut wal, 1, 2, 1, big_endian);
            append_frame(&mut wal, 2, 2, 2, big_endian);
            append_frame(&mut wal, 1, 2, 3, big_endian);

            let index = LiveIndex::parse(&index_prefix(&wal, 2, 1)).unwrap();
            let mut image = [9; 2 * PAGE];
            index
                .commit(&wal, PAGE, image.len() as u64)
                .unwrap()
                .unwrap()
                .apply(&mut image);
            assert_eq!(image[0], 9);
            assert_eq!(image[PAGE], 2);

            // A live restart publishes zero frames even if old WAL bytes remain.
            let empty = LiveIndex::parse(&index_prefix(&wal, 0, 0)).unwrap();
            assert!(empty.commit(&wal, PAGE, 0).unwrap().is_none());
        }
    }

    #[test]
    fn observation_snapshot_refuses_inconsistent_live_headers_and_frames() {
        let mut wal = wal_header(false, FORMAT_VERSION);
        append_frame(&mut wal, 1, 1, 1, false);
        append_frame(&mut wal, 1, 1, 2, false);
        let valid = index_prefix(&wal, 2, 0);

        let mut torn = valid;
        torn[INDEX_HEADER_BYTES + 8] ^= 1;
        seal_index_header(&mut torn[INDEX_HEADER_BYTES..2 * INDEX_HEADER_BYTES]);
        assert!(LiveIndex::parse(&torn).is_err());
        let mut checksum = valid;
        checksum[40] ^= 1;
        checksum[INDEX_HEADER_BYTES + 40] ^= 1;
        assert!(LiveIndex::parse(&checksum).is_err());
        assert!(LiveIndex::parse(&index_prefix(&wal, 2, 3)).is_err());

        let index = LiveIndex::parse(&valid).unwrap();
        assert!(index.commit(&wal[..wal.len() - 1], PAGE, 0).is_err());
        assert!(index.commit(&wal, 2 * PAGE, 0).is_err());
        let mut mixed_salts = wal.clone();
        mixed_salts[HEADER_BYTES + 8] ^= 1;
        assert!(index.commit(&mixed_salts, PAGE, 0).is_err());
        let mut mismatched_generation = valid;
        mismatched_generation[32] ^= 1;
        seal_index_header(&mut mismatched_generation[..INDEX_HEADER_BYTES]);
        let (first, rest) = mismatched_generation.split_at_mut(INDEX_HEADER_BYTES);
        rest[..INDEX_HEADER_BYTES].copy_from_slice(first);
        let index = LiveIndex::parse(&mismatched_generation).unwrap();
        assert!(index.commit(&wal, PAGE, 0).is_err());
    }

    #[test]
    fn observation_snapshot_recovers_the_last_commit_with_either_checksum_order() {
        for big_endian in [false, true] {
            let mut wal = wal_header(big_endian, 3_007_000);
            append_frame(&mut wal, 3, 0, 1, big_endian);
            append_frame(&mut wal, 1, 3, 2, big_endian);
            append_frame(&mut wal, 1, 2, 3, big_endian);
            append_frame(&mut wal, 2, 0, 4, big_endian);
            append_frame(&mut wal, 2, 2, 5, big_endian);
            // A full frame with a bad checksum must not promote either part of its transaction.
            *wal.last_mut().unwrap() ^= 1;
            wal.extend_from_slice(&[0; 40]);

            let commit = last_commit(&wal, PAGE).unwrap().unwrap();
            assert_eq!(commit.image_size(), 2 * PAGE as u64);
            let mut image = [9; 2 * PAGE];
            commit.apply(&mut image);
            assert_eq!(image[0], 3);
            assert_eq!(word(&image[28..32]), 2);
            assert!(image[PAGE..].iter().all(|&byte| byte == 9));
        }
    }

    #[test]
    fn observation_snapshot_refuses_unsupported_wal_version_and_page_size_mismatch() {
        let mut wal = wal_header(false, 3_007_001);
        append_frame(&mut wal, 1, 1, 0, false);
        assert!(
            last_commit(&wal, PAGE)
                .err()
                .unwrap()
                .to_string()
                .contains("version")
        );

        let mut wal = wal_header(false, 3_007_000);
        append_frame(&mut wal, 1, 1, 0, false);
        assert!(
            last_commit(&wal, 2 * PAGE)
                .err()
                .unwrap()
                .to_string()
                .contains("page sizes differ")
        );
        // The first frame has an old salt. SQLite ignores this entire recycled tail.
        wal[HEADER_BYTES + 8] ^= 1;
        assert!(last_commit(&wal, PAGE).unwrap().is_none());
    }
}
