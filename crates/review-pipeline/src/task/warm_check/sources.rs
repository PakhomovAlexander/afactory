//! The content rule of a warm layer (ADR-0207, issue #266): a Cargo target is reused only for
//! source groups whose content the layer last built.
//!
//! A Snapshot's files fall into groups: each Cargo package directory (a directory holding a
//! `Cargo.toml`) owns the files below it that no deeper package owns, and the files no package
//! directory holds form the root group `""`. Each group has a digest of its paths, kinds and
//! content IDs. The key's `af.warm-sources/1` record names, per group, the digest the key last
//! built and the modification time the kernel gave that group's files then.
//!
//! Before a check, under the key's lock: a group whose digest matches the record gets its
//! recorded time back, which is older than every artifact built from that content, so Cargo
//! keeps them; every other group — changed, new, or any group of a key with no record — is
//! stamped now, newer than everything in the target, so Cargo rebuilds exactly those and what
//! depends on them. The record is removed before any file is stamped and written again, with the
//! new stamps, before the check starts: whatever the check then builds into the key is built
//! from the recorded content after the recorded time, whether the check passes, fails or is
//! killed. A record that cannot be read or written leaves the key with no record, which only
//! costs a rebuild.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use review_core::task::check_layer::{
    MAX_WARM_SOURCE_GROUPS, TaskLayerSourcesV1, WarmSourceGroupV1, WarmSourcesV1,
};
use review_source_git::{EntryKind, Manifest};

/// The source groups of `manifest`: by group path, its digest.
pub(crate) fn source_groups(manifest: &Manifest) -> BTreeMap<String, String> {
    let packages: std::collections::BTreeSet<&str> = manifest
        .entries
        .iter()
        .filter_map(|entry| match entry.path.rsplit_once('/') {
            Some((directory, "Cargo.toml")) => Some(directory),
            None if entry.path == "Cargo.toml" => Some(""),
            _ => None,
        })
        .collect();
    let mut members: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for entry in &manifest.entries {
        let group = group_of(&entry.path, &packages);
        let fields = members.entry(group.to_string()).or_default();
        for field in [
            entry.path.as_bytes(),
            match entry.kind {
                EntryKind::File => b"file".as_slice(),
                EntryKind::Executable => b"executable",
                EntryKind::Symlink => b"symlink",
            },
            entry.content.as_bytes(),
        ] {
            fields.extend_from_slice(&(field.len() as u64).to_be_bytes());
            fields.extend_from_slice(field);
        }
    }
    members
        .into_iter()
        .map(|(group, fields)| {
            let digest = super::key_digest("af.warm-sources.group/1", &[group.as_bytes(), &fields]);
            (group, digest)
        })
        .collect()
}

/// The deepest package directory holding `path`, or `""`.
pub(crate) fn group_of<'a>(path: &str, packages: &std::collections::BTreeSet<&'a str>) -> &'a str {
    let mut directory = path;
    while let Some((parent, _)) = directory.rsplit_once('/') {
        if let Some(found) = packages.get(parent) {
            return found;
        }
        directory = parent;
    }
    packages.get("").copied().unwrap_or("")
}

/// What the content rule decided for one check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourcePlan {
    /// The current groups and their digests.
    pub(crate) groups: BTreeMap<String, String>,
    /// The groups whose content the key built, with the time their files get back.
    pub(crate) kept: BTreeMap<String, SystemTime>,
}

impl SourcePlan {
    /// Decide which groups keep their recorded time: those whose digest equals the record's and
    /// whose time is not after `now`. A record that does not parse or validate keeps nothing.
    pub(crate) fn decide(manifest: &Manifest, record: Option<&[u8]>, now: SystemTime) -> Self {
        let groups = source_groups(manifest);
        let recorded = record
            .and_then(|bytes| serde_json::from_slice::<WarmSourcesV1>(bytes).ok())
            .filter(|record| record.validate().is_ok());
        let kept = groups
            .iter()
            .filter_map(|(group, digest)| {
                let entry = recorded.as_ref()?.groups.get(group)?;
                let time = UNIX_EPOCH.checked_add(nanos(entry.stamp()?)?)?;
                (entry.digest == *digest && time <= now).then(|| (group.clone(), time))
            })
            .collect();
        Self { groups, kept }
    }

    /// The time a Manifest path keeps, when its group keeps one.
    pub(crate) fn kept_time(&self, packages: &PackageIndex, path: &str) -> Option<SystemTime> {
        self.kept.get(packages.group(path)).copied()
    }

    pub(crate) fn counts(&self) -> TaskLayerSourcesV1 {
        let fresh = self.kept.len();
        TaskLayerSourcesV1 {
            stale: u32::try_from(self.groups.len() - fresh).unwrap_or(u32::MAX),
            fresh: u32::try_from(fresh).unwrap_or(u32::MAX),
        }
    }

    /// Every group stale: a layer with no usable record, or a private copy.
    pub(crate) fn all_stale(manifest: &Manifest) -> Self {
        Self {
            groups: source_groups(manifest),
            kept: BTreeMap::new(),
        }
    }

    /// The record to write once the files are stamped: kept groups with their old times, every
    /// other group with `stamp`. `None` when the Snapshot has more groups than a record holds.
    pub(crate) fn record(&self, stamp: SystemTime) -> Option<WarmSourcesV1> {
        if self.groups.len() > MAX_WARM_SOURCE_GROUPS {
            return None;
        }
        let groups = self
            .groups
            .iter()
            .map(|(group, digest)| {
                let time = self.kept.get(group).copied().unwrap_or(stamp);
                let ns = time.duration_since(UNIX_EPOCH).ok()?.as_nanos();
                (ns > 0).then(|| {
                    (
                        group.clone(),
                        WarmSourceGroupV1 {
                            digest: digest.clone(),
                            stamp_unix_ns: ns.to_string(),
                        },
                    )
                })
            })
            .collect::<Option<BTreeMap<_, _>>>()?;
        let record = WarmSourcesV1::new(groups);
        record.validate().ok()?;
        Some(record)
    }
}

/// The package directories of a Manifest, to find each path's group while stamping.
pub(crate) struct PackageIndex<'a> {
    packages: std::collections::BTreeSet<&'a str>,
}

impl<'a> PackageIndex<'a> {
    pub(crate) fn new(manifest: &'a Manifest) -> Self {
        Self {
            packages: manifest
                .entries
                .iter()
                .filter_map(|entry| match entry.path.rsplit_once('/') {
                    Some((directory, "Cargo.toml")) => Some(directory),
                    None if entry.path == "Cargo.toml" => Some(""),
                    _ => None,
                })
                .collect(),
        }
    }

    pub(crate) fn group(&self, path: &str) -> &'a str {
        group_of(path, &self.packages)
    }
}

fn nanos(value: u128) -> Option<Duration> {
    let seconds = u64::try_from(value / 1_000_000_000).ok()?;
    Some(Duration::new(seconds, (value % 1_000_000_000) as u32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_source_git::Entry;

    fn manifest(files: &[(&str, &str)]) -> Manifest {
        Manifest::new(
            files
                .iter()
                .map(|(path, bytes)| Entry {
                    path: (*path).into(),
                    kind: EntryKind::File,
                    content: review_source_git::digest_bytes(bytes.as_bytes()),
                    size: bytes.len() as u64,
                })
                .collect(),
        )
        .unwrap()
    }

    fn workspace(lib: &str) -> Manifest {
        manifest(&[
            ("Cargo.lock", "lock"),
            ("Cargo.toml", "[workspace]"),
            ("crates/x/Cargo.toml", "[package] x"),
            ("crates/x/src/lib.rs", lib),
            ("crates/x/tests/fixture/Cargo.toml", "[package] fixture"),
            ("crates/x/tests/fixture/src/lib.rs", "fixture"),
            ("crates/y/Cargo.toml", "[package] y"),
            ("crates/y/src/lib.rs", "use x::a;"),
            ("schemas/s.json", "{}"),
        ])
    }

    #[test]
    fn files_belong_to_their_deepest_package_and_the_rest_to_the_root_group() {
        let groups = source_groups(&workspace("pub fn a() {}"));
        assert_eq!(
            groups.keys().map(String::as_str).collect::<Vec<_>>(),
            ["", "crates/x", "crates/x/tests/fixture", "crates/y"]
        );
        let changed = source_groups(&workspace("pub fn b() {}"));
        for group in ["", "crates/x/tests/fixture", "crates/y"] {
            assert_eq!(groups[group], changed[group], "{group} kept its digest");
        }
        assert_ne!(groups["crates/x"], changed["crates/x"]);
        let index_of = workspace("x");
        let index = PackageIndex::new(&index_of);
        assert_eq!(index.group("crates/x/src/lib.rs"), "crates/x");
        assert_eq!(
            index.group("crates/x/tests/fixture/src/lib.rs"),
            "crates/x/tests/fixture"
        );
        assert_eq!(index.group("schemas/s.json"), "");
        assert_eq!(index.group("Cargo.lock"), "");
    }

    #[test]
    fn the_record_keeps_exactly_the_groups_whose_content_it_built() {
        let now = SystemTime::now();
        let built = UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_789);
        let a = workspace("pub fn a() {}");
        let cold = SourcePlan::decide(&a, None, now);
        assert!(
            cold.kept.is_empty(),
            "a layer with no record is cold for every group"
        );
        assert_eq!(cold.counts(), TaskLayerSourcesV1 { stale: 4, fresh: 0 });
        let record = cold.record(built).unwrap();
        let bytes = serde_json::to_vec(&record).unwrap();

        let same = SourcePlan::decide(&a, Some(&bytes), now);
        assert_eq!(same.kept.len(), 4);
        assert!(
            same.kept.values().all(|time| *time == built),
            "the exact stamp comes back"
        );

        let b = workspace("pub fn b() {}");
        let changed = SourcePlan::decide(&b, Some(&bytes), now);
        assert_eq!(changed.counts(), TaskLayerSourcesV1 { stale: 1, fresh: 3 });
        assert!(!changed.kept.contains_key("crates/x"));
        let index = PackageIndex::new(&b);
        assert_eq!(changed.kept_time(&index, "crates/x/src/lib.rs"), None);
        assert_eq!(
            changed.kept_time(&index, "crates/y/src/lib.rs"),
            Some(built)
        );

        let later = UNIX_EPOCH + Duration::new(1_800_000_000, 0);
        let rewritten = changed.record(later).unwrap();
        assert_eq!(
            rewritten.groups["crates/x"].stamp_unix_ns,
            "1800000000000000000"
        );
        assert_eq!(
            rewritten.groups["crates/y"].stamp_unix_ns,
            "1700000000123456789"
        );

        // A stamp after now, or a record that does not validate, keeps nothing.
        assert!(
            SourcePlan::decide(&a, Some(&bytes), built - Duration::from_secs(1))
                .kept
                .is_empty()
        );
        assert!(
            SourcePlan::decide(&a, Some(b"{\"schema\":\"x\"}"), now)
                .kept
                .is_empty()
        );
    }
}
