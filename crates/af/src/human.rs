//! Figures as a person reads them: durations and sizes in units, times in the local zone.
//!
//! Human text uses these; the raw figure (milliseconds, bytes, Unix time) is printed only under
//! `--json`, where a script reads it.

use std::path::Path;

/// A duration in its two largest units: `850 ms`, `4.2 s`, `3 min 05 s`, `2 h 59 min`,
/// `3 d 4 h`. Truncated, never rounded up.
pub(crate) fn duration(ms: u64) -> String {
    const MINUTE: u64 = 60_000;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    match ms {
        0..1_000 => format!("{ms} ms"),
        1_000..MINUTE => format!("{}.{} s", ms / 1_000, ms % 1_000 / 100),
        MINUTE..HOUR => format!("{} min {:02} s", ms / MINUTE, ms % MINUTE / 1_000),
        HOUR..DAY => format!("{} h {:02} min", ms / HOUR, ms % HOUR / MINUTE),
        _ => format!("{} d {} h", ms / DAY, ms % DAY / HOUR),
    }
}

/// A size in binary units with one decimal: `512 B`, `1.2 GiB`.
pub(crate) fn bytes(bytes: u64) -> String {
    crate::storage::human_bytes(bytes)
}

/// The UTC offset in seconds and the zone's abbreviation at `unix_seconds`, from the zone
/// `TZ` names or the system's `/etc/localtime`; UTC when neither can be read.
fn zone_at(unix_seconds: i64) -> (i64, String) {
    let file = match std::env::var("TZ") {
        Ok(name) if name.is_empty() || name == "UTC" || name == "UTC0" || name == ":UTC" => {
            return (0, "UTC".into());
        }
        Ok(name) => {
            let name = name.trim_start_matches(':');
            if name.starts_with('/') {
                std::path::PathBuf::from(name)
            } else if name.split('/').all(|part| {
                !part.is_empty()
                    && part != ".."
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'+'))
            }) {
                Path::new("/usr/share/zoneinfo").join(name)
            } else {
                return (0, "UTC".into());
            }
        }
        Err(_) => std::path::PathBuf::from("/etc/localtime"),
    };
    std::fs::read(file)
        .ok()
        .and_then(|data| tzif_zone(&data, unix_seconds))
        .unwrap_or((0, "UTC".into()))
}

/// The offset and abbreviation a TZif file gives at `at`: its version 2+ 64-bit block when it
/// has one, else its version 1 block, the transition at or before `at`, else its first type.
fn tzif_zone(data: &[u8], at: i64) -> Option<(i64, String)> {
    fn counts(header: &[u8]) -> Option<[usize; 6]> {
        if header.get(..4)? != b"TZif" {
            return None;
        }
        let mut counts = [0usize; 6];
        for (index, count) in counts.iter_mut().enumerate() {
            let at = 20 + index * 4;
            *count = u32::from_be_bytes(header.get(at..at + 4)?.try_into().ok()?) as usize;
        }
        Some(counts)
    }
    let [isutc, isstd, leap, transitions, types, chars] = counts(data)?;
    let v1_length = transitions * 5 + types * 6 + chars + leap * 8 + isstd + isutc;
    let (block, time_size) = if data.get(4).copied().unwrap_or(0) >= b'2' {
        (data.get(44 + v1_length..)?, 8)
    } else {
        (data, 4)
    };
    let [_, _, _, transitions, types, chars] = counts(block)?;
    let body = block.get(44..)?;
    let times = body.get(..transitions * time_size)?;
    let indices = body.get(transitions * time_size..transitions * (time_size + 1))?;
    let infos =
        body.get(transitions * (time_size + 1)..transitions * (time_size + 1) + types * 6)?;
    let abbreviations = body.get(
        transitions * (time_size + 1) + types * 6
            ..transitions * (time_size + 1) + types * 6 + chars,
    )?;
    let time = |index: usize| -> Option<i64> {
        let raw = times.get(index * time_size..(index + 1) * time_size)?;
        Some(if time_size == 8 {
            i64::from_be_bytes(raw.try_into().ok()?)
        } else {
            i64::from(i32::from_be_bytes(raw.try_into().ok()?))
        })
    };
    let mut type_index = 0usize;
    for index in 0..transitions {
        if time(index)? <= at {
            type_index = usize::from(*indices.get(index)?);
        } else {
            break;
        }
    }
    let info = infos.get(type_index * 6..type_index * 6 + 6)?;
    let offset = i64::from(i32::from_be_bytes(info.get(..4)?.try_into().ok()?));
    let start = usize::from(info[5]);
    let end = abbreviations
        .get(start..)?
        .iter()
        .position(|b| *b == 0)
        .map_or(abbreviations.len(), |length| start + length);
    let name = String::from_utf8_lossy(abbreviations.get(start..end)?).into_owned();
    let name = if name.is_empty() || !name.bytes().all(|b| b.is_ascii_graphic()) {
        let sign = if offset < 0 { '-' } else { '+' };
        format!(
            "UTC{sign}{:02}:{:02}",
            offset.abs() / 3600,
            offset.abs() % 3600 / 60
        )
    } else {
        name
    };
    Some((offset, name))
}

/// The civil date of a day count since 1970-01-01 (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// A moment as local time with its zone, to the minute: `2026-10-23 06:12 CEST`.
pub(crate) fn local_time(unix_ms: u64) -> String {
    let seconds = i64::try_from(unix_ms / 1_000).unwrap_or(i64::MAX / 2);
    let (offset, zone) = zone_at(seconds);
    let local = seconds + offset;
    let (year, month, day) = civil(local.div_euclid(86_400));
    let of_day = local.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} {zone}",
        of_day / 3_600,
        of_day % 3_600 / 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_in_their_two_largest_units() {
        assert_eq!(duration(850), "850 ms");
        assert_eq!(duration(4_250), "4.2 s");
        assert_eq!(duration(185_000), "3 min 05 s");
        assert_eq!(duration(10_740_000), "2 h 59 min");
        assert_eq!(duration(3 * 86_400_000 + 4 * 3_600_000), "3 d 4 h");
    }

    #[test]
    fn civil_dates_and_utc_times() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_749), (2026, 10, 23));
        assert_eq!(civil(-1), (1969, 12, 31));
        assert_eq!(civil(11_016), (2000, 2, 29));
    }

    /// A version 1 TZif body with one transition: UTC before 1000 s, `XST` (+01:00) after.
    fn tzif() -> Vec<u8> {
        let mut data = b"TZif".to_vec();
        data.push(0);
        data.extend([0u8; 15]);
        for count in [0u32, 0, 0, 1, 2, 8] {
            data.extend(count.to_be_bytes());
        }
        data.extend(1000i32.to_be_bytes());
        data.push(1);
        data.extend(0i32.to_be_bytes());
        data.extend([0, 0]);
        data.extend(3600i32.to_be_bytes());
        data.extend([0, 4]);
        data.extend(b"UTC\0XST\0");
        data
    }

    #[test]
    fn a_zone_file_gives_the_offset_and_name_in_force() {
        let data = tzif();
        assert_eq!(tzif_zone(&data, 999), Some((0, "UTC".into())));
        assert_eq!(tzif_zone(&data, 1000), Some((3600, "XST".into())));
        assert_eq!(tzif_zone(b"not a zone", 0), None);
    }
}
