//! `af self` and dispatch against a directory release source: installs verified by the release's
//! signed checksums, pins that bind bytes, the oldest supported release, and the versions `remove`
//! and `prune` must keep.

use std::path::Path;

mod common;
use common::{AF, Sandbox, Signer, TARGET, VERSION, err, out, write};

#[test]
fn self_install_update_rollback_and_remove_against_a_directory_source() {
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    sandbox.publish("0.8.1", false);
    sandbox.publish("0.9.0", false);
    sandbox.publish("0.8.5", true);
    // Signed as their release job would sign them; 0.8.5's signature is over the tampered sums,
    // so it is the checksum that has to catch it.
    sandbox.sign("0.8.1", &signer, None);
    sandbox.sign("0.9.0", &signer, None);
    sandbox.sign("0.8.5", &signer, None);

    let install = sandbox
        .command(Path::new(AF))
        .args(["self", "install", "0.8.1"])
        .output()
        .unwrap();
    assert!(install.status.success(), "{}", err(&install));
    assert!(sandbox.versions().join("0.8.1/af").is_file());
    let receipt = std::fs::read_to_string(sandbox.versions().join("0.8.1/receipt.toml")).unwrap();
    assert!(receipt.contains("verified_by = \"minisign\""), "{receipt}");
    assert_eq!(
        sandbox.default_target().as_deref(),
        Some("0.8.1"),
        "first install becomes the default"
    );

    let tampered = sandbox
        .command(Path::new(AF))
        .args(["self", "install", "0.8.5"])
        .output()
        .unwrap();
    assert!(!tampered.status.success());
    assert!(
        err(&tampered).contains("checksum mismatch")
            && err(&tampered).contains("release checksums"),
        "{}",
        err(&tampered)
    );
    assert!(!sandbox.versions().join("0.8.5").exists());

    // Update and rollback need a receipt: this binary has none.
    let foreign = sandbox
        .command(Path::new(AF))
        .args(["self", "update"])
        .output()
        .unwrap();
    assert!(!foreign.status.success());
    assert!(
        err(&foreign).contains("not installed by `af self`"),
        "{}",
        err(&foreign)
    );

    let real = sandbox.adopt_real_binary();
    let check = sandbox
        .command(&real)
        .args(["self", "update", "--check"])
        .output()
        .unwrap();
    assert_eq!(check.status.code(), Some(10), "{}", err(&check));
    assert!(out(&check).contains("0.9.0"), "{}", out(&check));

    let update = sandbox
        .command(&real)
        .args(["self", "update"])
        .output()
        .unwrap();
    assert!(update.status.success(), "{}", err(&update));
    assert_eq!(sandbox.default_target().as_deref(), Some("0.9.0"));

    let status: serde_json::Value = serde_json::from_slice(
        &sandbox
            .command(&real)
            .args(["self", "status", "--json"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(status["default"], "0.9.0");
    assert_eq!(
        status["installed"],
        serde_json::json!(["0.8.1", VERSION, "0.9.0"])
    );
    assert!(status["receipt"].is_object());
    assert_eq!(status["release_key"], "environment");

    let rollback = sandbox
        .command(&real)
        .args(["self", "rollback"])
        .output()
        .unwrap();
    assert!(rollback.status.success(), "{}", err(&rollback));
    assert_eq!(sandbox.default_target().as_deref(), Some("0.8.1"));

    let remove_default = sandbox
        .command(&real)
        .args(["self", "remove", "0.8.1"])
        .output()
        .unwrap();
    assert!(!remove_default.status.success());
    assert!(
        err(&remove_default).contains("is the default"),
        "{}",
        err(&remove_default)
    );
    let remove = sandbox
        .command(&real)
        .args(["self", "remove", "0.9.0"])
        .output()
        .unwrap();
    assert!(remove.status.success(), "{}", err(&remove));
    assert!(!sandbox.versions().join("0.9.0").exists());
}

#[test]
fn a_source_installed_binary_reports_and_adopts_its_default_path() {
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    sandbox.publish("0.8.1", false);
    sandbox.sign("0.8.1", &signer, None);
    let source_install = sandbox.path("bin/af");
    std::fs::copy(AF, &source_install).unwrap();

    let status = sandbox
        .command(&source_install)
        .args(["self", "status"])
        .output()
        .unwrap();
    assert!(status.status.success(), "{}", err(&status));
    assert!(
        out(&status).contains("unmanaged running binary")
            && out(&status).contains("af self install"),
        "{}",
        out(&status)
    );
    let json: serde_json::Value = serde_json::from_slice(
        &sandbox
            .command(&source_install)
            .args(["self", "status", "--json"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(json["default"].is_null());
    assert_eq!(json["default_status"], "unmanaged-file");

    let install = sandbox
        .command(&source_install)
        .args(["self", "install", "0.8.1"])
        .output()
        .unwrap();
    assert!(install.status.success(), "{}", err(&install));
    assert_eq!(sandbox.default_target().as_deref(), Some("0.8.1"));
    assert!(
        std::fs::symlink_metadata(&source_install)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(sandbox.versions().join("0.8.1/receipt.toml").is_file());
}

#[test]
fn self_install_does_not_replace_an_unrelated_default_file() {
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    sandbox.publish("0.8.1", false);
    sandbox.sign("0.8.1", &signer, None);
    let unrelated = sandbox.path("bin/af");
    write(&unrelated, "leave me alone\n");

    let install = sandbox
        .command(Path::new(AF))
        .args(["self", "install", "0.8.1"])
        .output()
        .unwrap();
    assert!(!install.status.success());
    assert!(err(&install).contains("move it aside"), "{}", err(&install));
    assert_eq!(
        std::fs::read_to_string(unrelated).unwrap(),
        "leave me alone\n"
    );
}

#[test]
fn nothing_older_than_the_oldest_supported_release_is_activated_or_dispatched_to() {
    let sandbox = Sandbox::new();
    sandbox.publish("0.7.1", false);
    for args in [
        &["self", "install", "0.7.1"][..],
        &["self", "update", "--version", "0.7.1"],
    ] {
        let refused = sandbox.command(Path::new(AF)).args(args).output().unwrap();
        assert!(!refused.status.success(), "{args:?}");
        assert!(
            err(&refused).contains("older than the oldest supported release"),
            "{args:?}: {}",
            err(&refused)
        );
        assert!(!sandbox.versions().join("0.7.1").exists());
    }

    // A lock below the floor is an older pin this binary cannot honour: it runs and says so.
    let repo = sandbox.pinned_repo("repo", &Sandbox::lock("0.7.1", None));
    let run = sandbox
        .command(Path::new(AF))
        .args(["review", "campaigns"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", err(&run));
    assert!(
        err(&run).contains("older than the oldest supported release")
            && err(&run).contains("--af <version>"),
        "{}",
        err(&run)
    );
    assert!(!out(&run).contains("fake af"), "{}", out(&run));

    // An explicit request below the floor is refused outright.
    let explicit = sandbox
        .command(Path::new(AF))
        .args(["review", "campaigns"])
        .env("AF_VERSION", "0.7.1")
        .output()
        .unwrap();
    assert_eq!(explicit.status.code(), Some(1));
    assert!(
        err(&explicit).contains("older than the oldest supported release"),
        "{}",
        err(&explicit)
    );
}

#[test]
fn dispatch_runs_the_version_a_project_pins_when_the_bytes_match() {
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    let digest = sandbox.publish("0.8.1", false);
    let repo = sandbox.pinned_repo("repo", &Sandbox::lock("0.8.1", Some(&digest)));

    // Not installed, offline: the running binary continues and names the fix.
    let offline = sandbox
        .command(Path::new(AF))
        .args(["review", "plan", "--repo"])
        .arg(&repo)
        .env("AF_SELF_OFFLINE", "1")
        .output()
        .unwrap();
    assert!(
        err(&offline).contains("af self install 0.8.1"),
        "{}",
        err(&offline)
    );
    assert!(!out(&offline).contains("fake af"), "{}", out(&offline));

    // Online: installed on demand against the lock's digest, then exec'd with the same argv.
    let dispatched = sandbox
        .command(Path::new(AF))
        .args(["review", "plan", "--repo"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(dispatched.status.success(), "{}", err(&dispatched));
    let text = out(&dispatched);
    assert!(text.contains("fake af 0.8.1 review plan --repo"), "{text}");
    assert!(text.contains(&format!("from={VERSION}")), "{text}");
    let receipt = std::fs::read_to_string(sandbox.versions().join("0.8.1/receipt.toml")).unwrap();
    assert!(receipt.contains("verified_by = \"lock\""), "{receipt}");

    // The dispatch is remembered, so the pinned version survives remove and prune elsewhere.
    let real = sandbox.adopt_real_binary();
    let update = sandbox
        .command(&real)
        .args(["self", "update", "--version", VERSION])
        .output()
        .unwrap();
    assert!(update.status.success(), "{}", err(&update));
    let remove = sandbox
        .command(&real)
        .args(["self", "remove", "0.8.1"])
        .current_dir(sandbox.path("home"))
        .output()
        .unwrap();
    assert!(!remove.status.success());
    assert!(
        err(&remove).contains("pinned by a project this machine has seen"),
        "{}",
        err(&remove)
    );
    let status: serde_json::Value = serde_json::from_slice(
        &sandbox
            .command(&real)
            .args(["self", "status", "--json"])
            .current_dir(sandbox.path("home"))
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(status["pinned_projects"][0]["version"], "0.8.1");
    write(
        &sandbox.path("config/af/config.toml"),
        "[self]\nkeep_versions = 1\n",
    );
    let prune = sandbox
        .command(&real)
        .args(["self", "prune"])
        .current_dir(sandbox.path("home"))
        .output()
        .unwrap();
    assert!(prune.status.success(), "{}", err(&prune));
    assert!(
        sandbox.versions().join("0.8.1/af").is_file(),
        "prune kept the pin"
    );
    // Once the project is gone, the pin is forgotten and the version can go.
    std::fs::remove_dir_all(&repo).unwrap();
    let remove = sandbox
        .command(&real)
        .args(["self", "remove", "0.8.1"])
        .current_dir(sandbox.path("home"))
        .output()
        .unwrap();
    assert!(remove.status.success(), "{}", err(&remove));

    // Self commands, help, and version never dispatch.
    sandbox.publish("0.8.1", false);
    sandbox.sign("0.8.1", &signer, None);
    let repo = sandbox.pinned_repo("repo", &Sandbox::lock("0.8.1", Some(&digest)));
    for args in [&["self", "status"][..], &["--version"], &["help", "self"]] {
        let output = sandbox
            .command(Path::new(AF))
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(
            !out(&output).contains("fake af"),
            "{args:?} dispatched: {}",
            out(&output)
        );
    }

    // AF_VERSION overrides the pin; so does `af onboard --af`.
    let forced = sandbox
        .command(Path::new(AF))
        .args(["review", "plan"])
        .env("AF_VERSION", "0.8.1")
        .current_dir(sandbox.path("home"))
        .output()
        .unwrap();
    assert!(
        out(&forced).contains("fake af 0.8.1 review plan"),
        "{}",
        out(&forced)
    );
    let onboard = sandbox
        .command(Path::new(AF))
        .args(["onboard", "--refresh-lock", "--af", "0.8.1"])
        .current_dir(sandbox.path("home"))
        .output()
        .unwrap();
    assert!(
        out(&onboard).contains("fake af 0.8.1 onboard --refresh-lock --af 0.8.1"),
        "{}",
        out(&onboard)
    );

    // A pin matching the running version runs in place.
    write(&repo.join(".af/af.lock"), &Sandbox::lock(VERSION, None));
    let same = sandbox
        .command(Path::new(AF))
        .args(["config", "paths", "--repo"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(!out(&same).contains("fake af"));
}

#[test]
fn a_pin_binds_bytes_not_just_a_version() {
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    let digest = sandbox.publish("0.8.1", false);
    sandbox.sign("0.8.1", &signer, None);

    // No digest for this target: never installed on demand; an explicit install still works.
    let repo = sandbox.pinned_repo("repo", &Sandbox::lock("0.8.1", None));
    let older = sandbox
        .command(Path::new(AF))
        .args(["review", "campaigns"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(older.status.success(), "{}", err(&older));
    assert!(
        err(&older).contains("no digest for")
            && err(&older).contains("--refresh-lock --af 0.8.1")
            && err(&older).contains("af self install 0.8.1"),
        "{}",
        err(&older)
    );
    assert!(!sandbox.versions().join("0.8.1").exists());
    let newer = sandbox.pinned_repo("newer", &Sandbox::lock("9.9.9", None));
    let refused = sandbox
        .command(Path::new(AF))
        .args(["review", "campaigns"])
        .current_dir(&newer)
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(1), "{}", err(&refused));
    assert!(err(&refused).contains("no digest for"), "{}", err(&refused));

    // The wrong digest in the lock: the download is refused, whatever the versions.
    let wrong: String = digest.chars().rev().collect();
    let mismatched = sandbox.pinned_repo("mismatch", &Sandbox::lock("0.8.1", Some(&wrong)));
    let refused = sandbox
        .command(Path::new(AF))
        .args(["review", "campaigns"])
        .current_dir(&mismatched)
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(1), "{}", err(&refused));
    assert!(
        err(&refused).contains("checksum mismatch") && err(&refused).contains("project lock"),
        "{}",
        err(&refused)
    );
    assert!(!sandbox.versions().join("0.8.1").exists());

    // Installed explicitly (release checksums), the digest-less pin dispatches …
    let install = sandbox
        .command(Path::new(AF))
        .args(["self", "install", "0.8.1"])
        .output()
        .unwrap();
    assert!(install.status.success(), "{}", err(&install));
    let dispatched = sandbox
        .command(Path::new(AF))
        .args(["review", "campaigns"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        out(&dispatched).contains("fake af 0.8.1 review campaigns"),
        "{}",
        out(&dispatched)
    );
    // … but a lock whose digest disagrees with the installed receipt is refused.
    let refused = sandbox
        .command(Path::new(AF))
        .args(["review", "campaigns"])
        .current_dir(&mismatched)
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(1), "{}", err(&refused));
    assert!(
        err(&refused).contains("is not the release the lock recorded"),
        "{}",
        err(&refused)
    );
}

#[test]
fn every_release_must_carry_a_valid_signature_when_the_build_has_a_key() {
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let other = Signer::new(&keys.path().join("other"));
    let sandbox = Sandbox::new().with_key(&signer);
    sandbox.publish("0.8.0", false);
    sandbox.publish("0.8.1", false);
    sandbox.publish("0.8.2", false);

    let unsigned = sandbox
        .command(Path::new(AF))
        .args(["self", "install", "0.8.0"])
        .output()
        .unwrap();
    assert!(!unsigned.status.success());
    assert!(
        err(&unsigned).contains("must be signed"),
        "{}",
        err(&unsigned)
    );

    sandbox.sign("0.8.1", &other, None);
    let forged = sandbox
        .command(Path::new(AF))
        .args(["self", "install", "0.8.1"])
        .output()
        .unwrap();
    assert!(!forged.status.success());
    assert!(err(&forged).contains("does not verify"), "{}", err(&forged));

    sandbox.sign("0.8.2", &signer, Some(b"not the sums"));
    let tampered = sandbox
        .command(Path::new(AF))
        .args(["self", "install", "0.8.2"])
        .output()
        .unwrap();
    assert!(!tampered.status.success());
    assert!(
        err(&tampered).contains("does not verify"),
        "{}",
        err(&tampered)
    );

    sandbox.sign("0.8.0", &signer, None);
    let signed = sandbox
        .command(Path::new(AF))
        .args(["self", "install", "0.8.0"])
        .output()
        .unwrap();
    assert!(signed.status.success(), "{}", err(&signed));
    let receipt = std::fs::read_to_string(sandbox.versions().join("0.8.0/receipt.toml")).unwrap();
    assert!(receipt.contains("verified_by = \"minisign\""), "{receipt}");

    let status: serde_json::Value = serde_json::from_slice(
        &sandbox
            .command(Path::new(AF))
            .args(["self", "status", "--json"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(status["release_key"], "environment");
}

#[test]
fn refresh_check_caches_the_latest_and_applies_always() {
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    sandbox.publish("0.9.0", false);
    sandbox.sign("0.9.0", &signer, None);
    let real = sandbox.adopt_real_binary();
    let refresh = sandbox
        .command(&real)
        .args(["self", "refresh-check"])
        .output()
        .unwrap();
    assert!(refresh.status.success(), "{}", err(&refresh));
    let cache = std::fs::read_to_string(sandbox.path("cache/af/self/latest.toml")).unwrap();
    assert!(cache.contains("latest = \"0.9.0\""), "{cache}");
    assert!(sandbox.default_target().is_none(), "notify never installs");

    write(
        &sandbox.path("config/af/config.toml"),
        "[self]\nauto_update = \"always\"\n",
    );
    let always = sandbox
        .command(&real)
        .args(["self", "refresh-check"])
        .output()
        .unwrap();
    assert!(always.status.success(), "{}", err(&always));
    assert_eq!(sandbox.default_target().as_deref(), Some("0.9.0"));
    let cache = std::fs::read_to_string(sandbox.path("cache/af/self/latest.toml")).unwrap();
    assert!(cache.contains("auto_updated_to = \"0.9.0\""), "{cache}");

    // Offline, the check is a no-op and never fails a command.
    let offline = sandbox
        .command(&real)
        .args(["self", "refresh-check"])
        .env("AF_SELF_OFFLINE", "1")
        .output()
        .unwrap();
    assert!(offline.status.success(), "{}", err(&offline));
}

#[test]
fn project_config_cannot_steer_dispatch_and_unavailable_newer_pins_fail_closed() {
    let sandbox = Sandbox::new();
    let digest = sandbox.publish("0.8.1", false);
    write(
        &sandbox.path("config/af/config.toml"),
        "[self]\ninstall_pins = false\n",
    );
    let repo = sandbox.pinned_repo("repo", &Sandbox::lock("0.8.1", Some(&digest)));
    // The repository asks for installs from elsewhere; the user said no installs. The user wins.
    write(
        &repo.join(".af/af.toml"),
        "[self]\nsource = \"attacker/repo\"\ninstall_pins = true\n",
    );
    let older = sandbox
        .command(Path::new(AF))
        .args(["review", "campaigns"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        !sandbox.versions().join("0.8.1").exists(),
        "a repository must not trigger an install"
    );
    assert!(
        err(&older).contains("af self install 0.8.1"),
        "{}",
        err(&older)
    );
    assert!(
        older.status.success(),
        "an older pin under a newer binary proceeds: {}",
        err(&older)
    );

    // A newer pin that cannot be executed is a refusal, never a fallback to this binary.
    write(
        &repo.join(".af/af.lock"),
        &Sandbox::lock("9.9.9", Some(&digest)),
    );
    let newer = sandbox
        .command(Path::new(AF))
        .args(["review", "campaigns"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert_eq!(newer.status.code(), Some(1), "{}", err(&newer));
    assert!(
        err(&newer).contains("9.9.9") && err(&newer).contains("af self install 9.9.9"),
        "{}",
        err(&newer)
    );
    assert!(out(&newer).is_empty(), "{}", out(&newer));

    // An explicit AF_VERSION that is not installed is a refusal too.
    let explicit = sandbox
        .command(Path::new(AF))
        .args(["review", "campaigns"])
        .env("AF_VERSION", "5.5.5")
        .env("AF_SELF_OFFLINE", "1")
        .current_dir(sandbox.path("home"))
        .output()
        .unwrap();
    assert_eq!(explicit.status.code(), Some(1), "{}", err(&explicit));
    assert!(err(&explicit).contains("5.5.5"), "{}", err(&explicit));
}

#[test]
fn a_bare_binary_without_a_receipt_is_not_installed() {
    let sandbox = Sandbox::new();
    let digest = sandbox.publish("0.8.1", false);
    let dir = sandbox.versions().join("0.8.1");
    write(&dir.join("af"), "#!/bin/sh\necho planted\n");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.join("af"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let status: serde_json::Value = serde_json::from_slice(
        &sandbox
            .command(Path::new(AF))
            .args(["self", "status", "--json"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(status["installed"], serde_json::json!([]));

    // Dispatch does not exec it; install replaces it with a verified, receipted binary.
    let repo = sandbox.pinned_repo("repo", &Sandbox::lock("0.8.1", Some(&digest)));
    let dispatched = sandbox
        .command(Path::new(AF))
        .args(["review", "campaigns"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        !out(&dispatched).contains("planted"),
        "{}",
        out(&dispatched)
    );
    assert!(
        out(&dispatched).contains("fake af 0.8.1 review campaigns"),
        "{}\n{}",
        out(&dispatched),
        err(&dispatched)
    );
    let receipt = std::fs::read_to_string(dir.join("receipt.toml")).unwrap();
    assert!(receipt.contains("version = \"0.8.1\""), "{receipt}");
    assert!(receipt.contains(TARGET), "{receipt}");
}

#[test]
fn remove_takes_a_version_never_a_path_and_a_same_version_pin_still_binds_bytes() {
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    sandbox.publish("0.8.1", false);
    sandbox.sign("0.8.1", &signer, None);
    let real = sandbox.adopt_real_binary();
    for bad in ["..", "/", "../..", "0.8.1/../.."] {
        let refused = sandbox
            .command(&real)
            .args(["self", "remove", bad])
            .output()
            .unwrap();
        assert!(!refused.status.success(), "{bad} was accepted");
        assert!(
            sandbox.versions().join(VERSION).is_dir(),
            "{bad} removed the layout"
        );
    }
    let absent = sandbox
        .command(&real)
        .args(["self", "remove", "0.6.9"])
        .output()
        .unwrap();
    assert!(err(&absent).contains("not installed"), "{}", err(&absent));

    // A lock pinning the running version with other bytes: refused, with the fix.
    let wrong = "e".repeat(64);
    let repo = sandbox.pinned_repo("repo", &Sandbox::lock(VERSION, Some(&wrong)));
    let refused = sandbox
        .command(&real)
        .args(["review", "campaigns"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(1), "{}", err(&refused));
    assert!(
        err(&refused).contains("is not the release the lock recorded")
            && err(&refused).contains("AF_VERSION="),
        "{}",
        err(&refused)
    );
    // With the receipt's own digest, or no digest, it runs in place.
    let own = common::sha256_hex(&std::fs::read(AF).unwrap());
    for lock in [
        Sandbox::lock(VERSION, Some(&own)),
        Sandbox::lock(VERSION, None),
    ] {
        write(&repo.join(".af/af.lock"), &lock);
        let run = sandbox
            .command(&real)
            .args(["review", "campaigns"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(run.status.success(), "{}", err(&run));
    }

    // AF_VERSION accepts a leading `v`.
    let forced = sandbox
        .command(&real)
        .args(["review", "plan"])
        .env("AF_VERSION", "v0.8.1")
        .current_dir(sandbox.path("home"))
        .output()
        .unwrap();
    assert!(
        out(&forced).contains("fake af 0.8.1 review plan"),
        "{}\n{}",
        out(&forced),
        err(&forced)
    );
}
