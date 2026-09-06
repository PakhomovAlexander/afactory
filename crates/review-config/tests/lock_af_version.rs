//! The lock records the `af` release that wrote it and the bytes that release has; older locks
//! stay readable, and the 0.7.1 shape (`af_version` alone) is read as a pin without digests.

use review_config::lock::{AfPin, Lockfile, pinned_af};

const ZERO_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[test]
fn a_lock_without_an_af_pin_parses_and_stays_unpinned() {
    let text = format!(
        "version = 1\n\n[reviewers.x]\nversion = \"1.0.0\"\ndigest = \"sha256:{ZERO_DIGEST}\"\n"
    );
    let lock = Lockfile::from_toml(&text).unwrap();
    assert_eq!(lock.af, None);
    assert_eq!(lock.af_version(), None);
    assert!(!lock.to_toml().contains("af"));
    assert_eq!(pinned_af(&text), None);
}

#[test]
fn the_af_pin_round_trips_as_a_table_right_after_the_format_version() {
    let mut lock = Lockfile::empty();
    let mut pin = AfPin::version_only("0.8.0");
    pin.digests.insert(
        "aarch64-apple-darwin".into(),
        format!("sha256:{ZERO_DIGEST}"),
    );
    lock.af = Some(pin.clone());
    let text = lock.to_toml();
    assert!(
        text.starts_with(&format!(
            "version = 1\n\n[af]\nversion = \"0.8.0\"\n\n[af.digests]\naarch64-apple-darwin = \"sha256:{ZERO_DIGEST}\"\n"
        )),
        "{text}"
    );
    assert!(!text.contains("af_version"), "{text}");
    assert_eq!(Lockfile::from_toml(&text).unwrap(), lock);
    assert_eq!(pinned_af(&text), Some(pin));
}

#[test]
fn the_0_7_1_shape_is_read_as_a_pin_without_digests_and_rewritten_as_a_table() {
    let text = "version = 1\naf_version = \"0.7.1\"\n\n[reviewers]\n";
    let lock = Lockfile::from_toml(text).unwrap();
    assert_eq!(lock.af, Some(AfPin::version_only("0.7.1")));
    assert_eq!(lock.af_version(), Some("0.7.1"));
    assert!(
        lock.to_toml()
            .starts_with("version = 1\n\n[af]\nversion = \"0.7.1\"\n")
    );
    assert_eq!(pinned_af(text), Some(AfPin::version_only("0.7.1")));
    let both = "version = 1\naf_version = \"0.7.1\"\n\n[af]\nversion = \"0.8.0\"\n";
    assert!(Lockfile::from_toml(both).is_err());
}

#[test]
fn a_malformed_pin_is_refused_at_parse_time() {
    for text in [
        "version = 1\n\n[af]\nversion = \"latest\"\n",
        "version = 1\n\n[af]\nversion = \"0.8.0\"\n\n[af.digests]\nx = \"sha256:short\"\n",
        "version = 1\n\n[af]\nversion = \"0.8.0\"\n\n[af.digests]\n\"a/b\" = \"sha256:0000000000000000000000000000000000000000000000000000000000000000\"\n",
    ] {
        assert!(Lockfile::from_toml(text).is_err(), "{text}");
    }
}

#[test]
fn the_lenient_reader_ignores_fields_a_newer_release_may_add() {
    let text = "version = 7\nfuture = true\n\n[af]\nversion = \"9.0.0\"\nextra = 1\n\n[af.digests]\nt = \"sha256:abc\"\n";
    assert!(Lockfile::from_toml(text).is_err());
    let pin = pinned_af(text).unwrap();
    assert_eq!(pin.version, "9.0.0");
    assert_eq!(pin.digest_for("t"), Some("sha256:abc"));
}
