//! The lock records the `af` release that wrote it; older locks stay readable and unpinned.

use review_config::lock::Lockfile;

const ZERO_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[test]
fn a_lock_without_an_af_version_parses_and_stays_unpinned() {
    let text = format!(
        "version = 1\n\n[reviewers.x]\nversion = \"1.0.0\"\ndigest = \"sha256:{ZERO_DIGEST}\"\n"
    );
    let lock = Lockfile::from_toml(&text).unwrap();
    assert_eq!(lock.af_version, None);
    assert!(!lock.to_toml().contains("af_version"));
}

#[test]
fn the_af_version_round_trips_right_after_the_format_version() {
    let mut lock = Lockfile::empty();
    lock.af_version = Some("0.7.0".to_string());
    let text = lock.to_toml();
    assert!(
        text.starts_with("version = 1\naf_version = \"0.7.0\"\n"),
        "{text}"
    );
    assert_eq!(Lockfile::from_toml(&text).unwrap(), lock);
}
