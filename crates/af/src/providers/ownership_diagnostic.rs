//! Explain unmapped ownership without granting it any trust.

#[cfg(target_os = "linux")]
pub(super) fn unmapped_owner(owner: u32) -> Option<String> {
    use std::io::Read;

    fn read_bounded(path: &str, limit: u64) -> Option<String> {
        let mut text = String::new();
        std::fs::File::open(path)
            .ok()?
            .take(limit + 1)
            .read_to_string(&mut text)
            .ok()?;
        (text.len() as u64 <= limit).then_some(text)
    }

    let overflow = read_bounded("/proc/sys/kernel/overflowuid", 32)?;
    let map = read_bounded("/proc/self/uid_map", 16 * 1024)?;
    owner_is_unmapped(owner, &overflow, &map).then(|| {
        format!(
            "Linux reports uid {owner}, its overflow UID, and that UID is absent from this process's user namespace mapping; the real owner is not visible to af"
        )
    })
}

#[cfg(not(target_os = "linux"))]
pub(super) fn unmapped_owner(_owner: u32) -> Option<String> {
    None
}

#[cfg(any(target_os = "linux", test))]
fn owner_is_unmapped(owner: u32, overflow: &str, map: &str) -> bool {
    if overflow.trim().parse::<u32>().ok() != Some(owner) {
        return false;
    }
    let mut rows = 0;
    let mut mapped = false;
    for line in map.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 3 {
            return false;
        }
        let Ok(values) = fields
            .iter()
            .map(|v| v.parse::<u32>())
            .collect::<Result<Vec<_>, _>>()
        else {
            return false;
        };
        let [inside, outside, count] = [values[0], values[1], values[2]];
        if count == 0
            || u64::from(inside) + u64::from(count) > u64::from(u32::MAX)
            || u64::from(outside) + u64::from(count) > u64::from(u32::MAX)
        {
            return false;
        }
        mapped |= owner >= inside && u64::from(owner) < u64::from(inside) + u64::from(count);
        rows += 1;
    }
    rows > 0 && !mapped
}

#[cfg(test)]
mod tests {
    use super::owner_is_unmapped;

    #[test]
    fn namespace_overflow_is_explained_but_an_actual_mapped_owner_is_not() {
        assert!(owner_is_unmapped(65534, "65534\n", "1000 0 1\n"));
        assert!(!owner_is_unmapped(1001, "65534\n", "1000 0 1\n"));
        assert!(!owner_is_unmapped(65534, "65534\n", "0 0 4294967295\n"));
        assert!(!owner_is_unmapped(
            65534,
            "65534\n",
            "1000 0 1\n65534 1001 1\n"
        ));
        assert!(owner_is_unmapped(12345, "12345\n", "1000 0 1\n"));
    }

    #[test]
    fn missing_or_malformed_namespace_evidence_keeps_the_ordinary_refusal() {
        for map in [
            "",
            "bad",
            "1000 0",
            "1000 0 1 extra",
            "1000 0 0",
            "4294967295 0 2",
        ] {
            assert!(!owner_is_unmapped(65534, "65534", map), "{map}");
        }
        assert!(!owner_is_unmapped(65534, "unknown", "1000 0 1"));
    }
}
