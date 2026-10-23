use super::*;

#[test]
fn expiry_diagnostics_use_exact_utc_calendar_dates_and_enable_action() {
    for (seconds, nanoseconds, expected) in [
        (0, 0, "1970-01-01T00:00:00Z"),
        (951_782_400, 1, "2000-02-29T00:00:00.000000001Z"),
        (1_790_599_874, 123456789, "2026-09-28T12:51:14.123456789Z"),
        (4_102_444_799, 999999999, "2099-12-31T23:59:59.999999999Z"),
    ] {
        let expiry = Expiry {
            unix_seconds: seconds,
            nanoseconds,
        };
        assert_eq!(
            expiry.diagnostic("example"),
            format!(
                "Provider `example` is disabled until {expected} (UTC); run `af provider enable example` to enable immediately"
            )
        );
        assert_eq!(Expiry::from_time(expiry.time().unwrap()).unwrap(), expiry);
    }
}

#[test]
fn aggregate_byte_limit_bounds_long_ids_before_atomic_publication() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.json");
    let now = UNIX_EPOCH + Duration::from_secs(100);
    let id = "p".repeat(1024);
    let expiry = update(&path, &id, Some(Duration::from_secs(60)), || now)
        .unwrap()
        .unwrap();
    assert_eq!(until_at(&path, &id, now).unwrap(), Some(expiry));
    let original = fs::read(&path).unwrap();
    let oversized = "p".repeat(MAX_REGISTRY_BYTES as usize);
    assert!(update(&path, &oversized, Some(Duration::from_secs(60)), || now).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    let mut oversized_read = original;
    oversized_read.resize(MAX_REGISTRY_BYTES as usize + 1, b' ');
    assert!(State::parse(&oversized_read).is_err());
    fs::write(&path, &oversized_read).unwrap();
    assert!(until_at(&path, &id, now).is_err());
}

#[test]
fn duration_is_positive_bounded_and_checked() {
    for (value, seconds) in [
        ("1s", 1),
        ("2m", 120),
        ("3h", 10800),
        ("2d", 172800),
        ("30d", MAX_SECONDS),
    ] {
        assert_eq!(duration(value).unwrap().as_secs(), seconds);
    }
    for value in [
        "",
        "s",
        "0s",
        "-1h",
        "+1d",
        "1.5h",
        "1w",
        "1H",
        " 1s",
        "1s ",
        "31d",
        "2592001s",
        "18446744073709551615d",
        "18446744073709551616s",
        "é",
    ] {
        assert!(duration(value).is_err(), "{value}");
    }
}

#[test]
fn expiry_is_exact_across_reads_and_enable_is_immediate() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.json");
    let now = UNIX_EPOCH + Duration::new(100, 123456789);
    let expiry = update(&path, "example", Some(Duration::from_secs(172800)), || now)
        .unwrap()
        .unwrap();
    let at = expiry.time().unwrap();
    assert_eq!(at, now + Duration::from_secs(172800));
    assert_eq!(
        until_at(&path, "example", at - Duration::from_nanos(1)).unwrap(),
        Some(expiry)
    );
    assert_eq!(until_at(&path, "example", at).unwrap(), None);
    assert_eq!(
        until_at(&path, "example", at + Duration::from_nanos(1)).unwrap(),
        None
    );
    update(&path, "example", None, || now).unwrap();
    assert_eq!(until_at(&path, "example", now).unwrap(), None);
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
}

#[test]
fn concurrent_updates_preserve_each_other_and_readers_see_whole_files() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("private/state.json");
    let now = UNIX_EPOCH + Duration::from_secs(100);
    thread::scope(|scope| {
        for index in 0..16 {
            let path = &path;
            scope.spawn(move || {
                update(
                    path,
                    &format!("p{index}"),
                    Some(Duration::from_secs(60)),
                    || now,
                )
                .unwrap();
            });
        }
        let path = &path;
        scope.spawn(move || {
            for _ in 0..100 {
                if let Err(error) = until_at(path, "p0", now) {
                    assert!(
                        error.contains("changed") || error.contains("singly linked"),
                        "{error}"
                    );
                }
            }
        });
    });
    let directory = bind_directory(path.parent().unwrap(), LABEL).unwrap();
    assert_eq!(read(&path, &directory).unwrap().providers.len(), 16);
    assert_eq!(
        fs::metadata(path.parent().unwrap()).unwrap().mode() & 0o777,
        0o700
    );
}

#[test]
fn concurrent_orphan_cleanup_preserves_a_new_disable() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.json");
    let now = UNIX_EPOCH + Duration::from_secs(100);
    for i in 0..16 {
        update(
            &path,
            &format!("orphan-{i}"),
            Some(Duration::from_secs(60)),
            || now,
        )
        .unwrap();
    }
    thread::scope(|scope| {
        for i in 0..16 {
            let path = &path;
            scope.spawn(move || update(path, &format!("orphan-{i}"), None, || now).unwrap());
        }
        let path = &path;
        scope.spawn(move || update(path, "new", Some(Duration::from_secs(60)), || now).unwrap());
    });
    let state = load_at(&path).unwrap();
    assert_eq!(state.providers.len(), 1);
    assert_eq!(state.providers[0].id, "new");
    assert!(state.disabled_until("new", now).unwrap().is_some());
}

#[test]
fn malformed_unsafe_and_unknown_state_fail_closed_without_replacement() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.json");
    let now = UNIX_EPOCH + Duration::from_secs(100);
    for text in [
        "{}",
        "{",
        r#"{"schema":"af.provider-disables/0","providers":[]}"#,
        r#"{"schema":"af.provider-disables/1","providers":[],"extra":true}"#,
        r#"{"schema":"af.provider-disables/1","providers":[{"id":"x","expires_at":{"unix_seconds":100,"nanoseconds":1000000000}}]}"#,
        r#"{"schema":"af.provider-disables/1","providers":[{"id":"../x","expires_at":{"unix_seconds":100,"nanoseconds":0}}]}"#,
        r#"{"schema":"af.provider-disables/1","providers":[{"id":"x","expires_at":{"unix_seconds":100,"nanoseconds":0}},{"id":"x","expires_at":{"unix_seconds":101,"nanoseconds":0}}]}"#,
    ] {
        fs::write(&path, text).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(until_at(&path, "x", now).is_err(), "{text}");
        assert!(update(&path, "x", None, || now).is_err(), "{text}");
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
    }
    fs::remove_file(&path).unwrap();
    let other = root.path().join("other");
    fs::write(&other, "untouched").unwrap();
    std::os::unix::fs::symlink(&other, &path).unwrap();
    assert!(update(&path, "x", None, || now).is_err());
    assert_eq!(fs::read_to_string(&other).unwrap(), "untouched");
    fs::remove_file(&path).unwrap();
    fs::hard_link(&other, &path).unwrap();
    assert!(update(&path, "x", None, || now).is_err());
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(until_at(&path, "x", now).is_err());
}

#[test]
fn published_state_fixture_round_trips_the_closed_rust_shape() {
    let bytes = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/provider-disables/v1/state.json"
    ));
    let state = State::parse(bytes).unwrap();
    let value: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    assert_eq!(serde_json::to_value(&state).unwrap(), value);
    let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../schemas/provider-disables-v1.json"
    )))
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&value));
    let mut long_id = value.clone();
    long_id["providers"][0]["id"] = serde_json::json!("p".repeat(1024));
    assert!(validator.is_valid(&long_id));
    let parsed = State::parse(&serde_json::to_vec(&long_id).unwrap()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), long_id);
    for invalid in [
        serde_json::json!({"schema":"af.provider-disables/0", "providers":[]}),
        serde_json::json!({"schema":"af.provider-disables/1", "providers":[], "extra":true}),
        serde_json::json!({"schema":"af.provider-disables/1", "providers":[{"id":"x", "expires_at":{"unix_seconds":1,"nanoseconds":1000000000}}]}),
    ] {
        assert!(!validator.is_valid(&invalid));
        assert!(State::parse(&serde_json::to_vec(&invalid).unwrap()).is_err());
    }
}

#[test]
fn unsafe_lock_directory_and_timestamp_overflow_refuse_without_publication() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.json");
    let lock = root.path().join("state.json.lock");
    let other = root.path().join("other");
    fs::write(&other, "untouched").unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(100);
    std::os::unix::fs::symlink(&other, &lock).unwrap();
    assert!(update(&path, "x", None, || now).is_err());
    assert!(!path.exists());
    fs::remove_file(&lock).unwrap();
    fs::hard_link(&other, &lock).unwrap();
    let permissions = fs::metadata(&other).unwrap().mode();
    assert!(update(&path, "x", None, || now).is_err());
    assert_eq!(fs::metadata(&other).unwrap().mode(), permissions);
    assert_eq!(fs::read_to_string(&other).unwrap(), "untouched");
    fs::remove_file(&lock).unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o777)).unwrap();
    assert!(update(&path, "x", None, || now).is_err());
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!path.exists());
    assert!(
        Expiry {
            unix_seconds: u64::MAX,
            nanoseconds: 0
        }
        .time()
        .is_err()
    );
    let maximum = UNIX_EPOCH
        .checked_add(Duration::from_secs(i64::MAX as u64))
        .unwrap();
    assert!(update(&path, "x", Some(Duration::from_secs(1)), || maximum).is_err());
    assert!(!path.exists());
}

#[test]
fn replaced_directory_never_turns_an_absent_read_into_permission_to_call() {
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().join("private");
    create_private_directory_tree(&parent, LABEL, true).unwrap();
    let directory = bind_directory(&parent, LABEL).unwrap();
    fs::rename(&parent, root.path().join("previous")).unwrap();
    fs::create_dir(&parent).unwrap();
    assert!(read(&parent.join("state.json"), &directory).is_err());
}
