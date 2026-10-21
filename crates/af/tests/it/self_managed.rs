//! `af self` and dispatch against a directory release source: installs verified by the release's
//! signed checksums, pins that bind bytes, the oldest supported release, and the versions `remove`
//! and `prune` must keep.

use crate::common;
use common::{
    FAKE_CURL, FAKE_GH, Sandbox, Signer, TARGET, VERSION, err, executable, out, tool_dir, write,
};
use std::path::Path;

// Update fixtures must remain newer than the binary under test as releases advance.
fn next_release_version() -> String {
    let current = semver::Version::parse(VERSION).unwrap();
    semver::Version::new(current.major, current.minor + 1, 0).to_string()
}

#[test]
fn self_install_update_rollback_and_remove_against_a_directory_source() {
    let newer = next_release_version();
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    sandbox.publish("0.8.1", false);
    sandbox.publish(&newer, false);
    sandbox.publish("0.8.5", true);
    // Signed as their release job would sign them; 0.8.5's signature is over the tampered sums,
    // so it is the checksum that has to catch it.
    sandbox.sign("0.8.1", &signer, None);
    sandbox.sign(&newer, &signer, None);
    sandbox.sign("0.8.5", &signer, None);

    let install = sandbox
        .command(common::af_binary())
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
        .command(common::af_binary())
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
        .command(common::af_binary())
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
    assert!(out(&check).contains(&newer), "{}", out(&check));

    let update = sandbox
        .command(&real)
        .args(["self", "update"])
        .output()
        .unwrap();
    assert!(update.status.success(), "{}", err(&update));
    assert_eq!(sandbox.default_target().as_deref(), Some(newer.as_str()));

    let status: serde_json::Value = serde_json::from_slice(
        &sandbox
            .command(&real)
            .args(["self", "status", "--json"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(status["default"], newer.as_str());
    assert_eq!(
        status["installed"],
        serde_json::json!(["0.8.1", VERSION, &newer])
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
        .args(["self", "remove", &newer])
        .output()
        .unwrap();
    assert!(remove.status.success(), "{}", err(&remove));
    assert!(!sandbox.versions().join(&newer).exists());
}

#[test]
fn a_source_installed_binary_reports_and_adopts_its_default_path() {
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    sandbox.publish("0.8.1", false);
    sandbox.sign("0.8.1", &signer, None);
    let source_install = sandbox.path("bin/af");
    std::fs::copy(common::af_binary(), &source_install).unwrap();

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
        .command(common::af_binary())
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
        let refused = sandbox
            .command(common::af_binary())
            .args(args)
            .output()
            .unwrap();
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
        .command(common::af_binary())
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
        .command(common::af_binary())
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
    sandbox.sign("0.8.1", &signer, None);
    sandbox.install_pins("\"always\"");
    let repo = sandbox.pinned_repo("repo", &Sandbox::lock("0.8.1", Some(&digest)));

    // Not installed, offline: the running binary continues and names the fix.
    let offline = sandbox
        .command(common::af_binary())
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

    // `af review plan` is token-free and stateless: online and under "always" it still installs
    // nothing, and the running binary plans.
    let plan = sandbox
        .command(common::af_binary())
        .args(["review", "plan", "--repo"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(
        err(&plan).contains("`af review plan` never installs a release")
            && err(&plan).contains("af self install 0.8.1"),
        "{}",
        err(&plan)
    );
    assert!(!out(&plan).contains("fake af"), "{}", out(&plan));
    assert!(!sandbox.versions().join("0.8.1").exists());

    // Online: installed on demand against the lock's digest and the signed checksums, then
    // exec'd with the same argv.
    let dispatched = sandbox
        .command(common::af_binary())
        .args(["review", "campaigns", "--repo"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(dispatched.status.success(), "{}", err(&dispatched));
    let text = out(&dispatched);
    assert!(
        text.contains("fake af 0.8.1 review campaigns --repo"),
        "{text}"
    );
    assert!(text.contains(&format!("from={VERSION}")), "{text}");
    let receipt = std::fs::read_to_string(sandbox.versions().join("0.8.1/receipt.toml")).unwrap();
    assert!(
        receipt.contains("verified_by = \"lock+signature\""),
        "{receipt}"
    );
    // Installed, the pin dispatches `af review plan` too.
    let plan = sandbox
        .command(common::af_binary())
        .args(["review", "plan", "--repo"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(
        out(&plan).contains("fake af 0.8.1 review plan --repo"),
        "{}",
        out(&plan)
    );

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
            .command(common::af_binary())
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
        .command(common::af_binary())
        .args(["review", "campaigns"])
        .env("AF_VERSION", "0.8.1")
        .current_dir(sandbox.path("home"))
        .output()
        .unwrap();
    assert!(
        out(&forced).contains("fake af 0.8.1 review campaigns"),
        "{}",
        out(&forced)
    );
    let onboard = sandbox
        .command(common::af_binary())
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
        .command(common::af_binary())
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
    sandbox.install_pins("\"always\"");

    // No digest for this target: never installed on demand; an explicit install still works.
    let repo = sandbox.pinned_repo("repo", &Sandbox::lock("0.8.1", None));
    let older = sandbox
        .command(common::af_binary())
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
        .command(common::af_binary())
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
        .command(common::af_binary())
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
        .command(common::af_binary())
        .args(["self", "install", "0.8.1"])
        .output()
        .unwrap();
    assert!(install.status.success(), "{}", err(&install));
    let dispatched = sandbox
        .command(common::af_binary())
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
        .command(common::af_binary())
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
        .command(common::af_binary())
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
        .command(common::af_binary())
        .args(["self", "install", "0.8.1"])
        .output()
        .unwrap();
    assert!(!forged.status.success());
    assert!(err(&forged).contains("does not verify"), "{}", err(&forged));

    sandbox.sign("0.8.2", &signer, Some(b"not the sums"));
    let tampered = sandbox
        .command(common::af_binary())
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
        .command(common::af_binary())
        .args(["self", "install", "0.8.0"])
        .output()
        .unwrap();
    assert!(signed.status.success(), "{}", err(&signed));
    let receipt = std::fs::read_to_string(sandbox.versions().join("0.8.0/receipt.toml")).unwrap();
    assert!(receipt.contains("verified_by = \"minisign\""), "{receipt}");

    let status: serde_json::Value = serde_json::from_slice(
        &sandbox
            .command(common::af_binary())
            .args(["self", "status", "--json"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(status["release_key"], "environment");
}

/// Rewrite the `SHA256SUMS.minisig` that `Sandbox::sign` produced for `version` with `damage`.
fn damage_signature(sandbox: &Sandbox, version: &str, damage: &str) {
    let path = sandbox.release_dir(version).join("SHA256SUMS.minisig");
    let text = std::fs::read_to_string(&path).unwrap();
    // untrusted comment, signature, trusted comment, global signature
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 4, "{text}");
    assert!(lines[2].starts_with("trusted comment: "), "{text}");
    let edited_comment = "trusted comment: timestamp:0\tfile:SHA256SUMS\tedited";
    assert_ne!(lines[2], edited_comment);
    let damaged = match damage {
        "not base64" => format!(
            "{}\nnot base64 at all!\n{}\n{}\n",
            lines[0], lines[2], lines[3]
        ),
        // Valid base64 that decodes to fewer bytes than a signature has.
        "wrong length" => format!(
            "{}\n{}\n{}\n{}\n",
            lines[0],
            &lines[1][..lines[1].len() - 4],
            lines[2],
            lines[3]
        ),
        "cut after the untrusted comment" => format!("{}\n", lines[0]),
        "cut mid-signature" => format!("{}\n{}", lines[0], &lines[1][..lines[1].len() / 2]),
        // Still parses, but the global signature covers the original comment.
        "trusted comment edited" => format!(
            "{}\n{}\n{edited_comment}\n{}\n",
            lines[0], lines[1], lines[3]
        ),
        other => panic!("unknown damage {other}"),
    };
    write(&path, &damaged);
}

#[test]
fn a_malformed_truncated_or_recommented_signature_is_refused_and_installs_nothing() {
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    let undecodable = ["SHA256SUMS.minisig of", "Invalid encoding in minisign data"];
    let unverified = [
        "does not verify against the release key",
        "signature verification failed",
    ];
    for (version, damage, reason) in [
        ("0.8.1", "not base64", undecodable),
        ("0.8.2", "wrong length", undecodable),
        ("0.8.3", "cut after the untrusted comment", undecodable),
        ("0.8.4", "cut mid-signature", undecodable),
        ("0.8.5", "trusted comment edited", unverified),
    ] {
        // Signed by the trusted key over its own sums; only the signature file is damaged.
        sandbox.publish(version, false);
        sandbox.sign(version, &signer, None);
        damage_signature(&sandbox, version, damage);
        let refused = sandbox
            .command(common::af_binary())
            .args(["self", "install", version])
            .output()
            .unwrap();
        assert!(!refused.status.success(), "{damage}: installed");
        assert!(
            reason.iter().all(|part| err(&refused).contains(part)),
            "{damage}: {}",
            err(&refused)
        );
        assert!(
            !sandbox.versions().join(version).exists(),
            "{damage}: left an install or receipt behind"
        );
        assert!(sandbox.default_target().is_none(), "{damage}: activated");
    }

    // The same fixture, undamaged, installs: the refusals above are the damage, not the setup.
    sandbox.publish("0.8.6", false);
    sandbox.sign("0.8.6", &signer, None);
    let signed = sandbox
        .command(common::af_binary())
        .args(["self", "install", "0.8.6"])
        .output()
        .unwrap();
    assert!(signed.status.success(), "{}", err(&signed));
    let receipt = std::fs::read_to_string(sandbox.versions().join("0.8.6/receipt.toml")).unwrap();
    assert!(receipt.contains("verified_by = \"minisign\""), "{receipt}");
    assert_eq!(sandbox.default_target().as_deref(), Some("0.8.6"));
}

#[test]
fn refresh_check_caches_the_latest_and_applies_always() {
    let newer = next_release_version();
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    sandbox.publish(&newer, false);
    sandbox.sign(&newer, &signer, None);
    let real = sandbox.adopt_real_binary();
    let refresh = sandbox.refresh_check(&real).output().unwrap();
    assert!(refresh.status.success(), "{}", err(&refresh));
    let cache = std::fs::read_to_string(sandbox.path("cache/af/self/latest.toml")).unwrap();
    assert!(cache.contains(&format!("latest = \"{newer}\"")), "{cache}");
    assert!(sandbox.default_target().is_none(), "notify never installs");

    // The check above answered for `check_every`; a day later the next one is due.
    let aged: String = cache
        .lines()
        .map(|line| {
            if line.starts_with("checked_at_epoch = ") {
                "checked_at_epoch = 1\n".to_string()
            } else {
                format!("{line}\n")
            }
        })
        .collect();
    write(&sandbox.path("cache/af/self/latest.toml"), &aged);
    write(
        &sandbox.path("config/af/config.toml"),
        "[self]\nauto_update = \"always\"\n",
    );
    let always = sandbox.refresh_check(&real).output().unwrap();
    assert!(always.status.success(), "{}", err(&always));
    assert_eq!(sandbox.default_target().as_deref(), Some(newer.as_str()));
    let cache = std::fs::read_to_string(sandbox.path("cache/af/self/latest.toml")).unwrap();
    assert!(
        cache.contains("[auto_updated]") && cache.contains(&format!("to = \"{newer}\"")),
        "{cache}"
    );

    // Offline, the check is a no-op and never fails a command.
    let offline = sandbox
        .refresh_check(&real)
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
        .command(common::af_binary())
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
        .command(common::af_binary())
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
        .command(common::af_binary())
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
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    let digest = sandbox.publish("0.8.1", false);
    sandbox.sign("0.8.1", &signer, None);
    sandbox.install_pins("\"always\"");
    let dir = sandbox.versions().join("0.8.1");
    write(&dir.join("af"), "#!/bin/sh\necho planted\n");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.join("af"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let status: serde_json::Value = serde_json::from_slice(
        &sandbox
            .command(common::af_binary())
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
        .command(common::af_binary())
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
    let own = common::sha256_hex(&std::fs::read(common::af_binary()).unwrap());
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
        .args(["review", "campaigns"])
        .env("AF_VERSION", "v0.8.1")
        .current_dir(sandbox.path("home"))
        .output()
        .unwrap();
    assert!(
        out(&forced).contains("fake af 0.8.1 review campaigns"),
        "{}\n{}",
        out(&forced),
        err(&forced)
    );
}

#[test]
fn an_install_from_a_lock_needs_the_signed_checksums_to_agree_with_the_lock() {
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    sandbox.install_pins("\"always\"");
    let run = |repo: &Path| {
        sandbox
            .command(common::af_binary())
            .args(["review", "campaigns"])
            .current_dir(repo)
            .output()
            .unwrap()
    };

    // A release source holding only the archive: the lock's digest alone installs nothing, and
    // this newer af runs the older pin's command instead.
    let digest = sandbox.publish("0.8.1", false);
    std::fs::remove_file(sandbox.release_dir("0.8.1").join("SHA256SUMS")).unwrap();
    let repo = sandbox.pinned_repo("bare", &Sandbox::lock("0.8.1", Some(&digest)));
    let bare = run(&repo);
    assert!(bare.status.success(), "{}", err(&bare));
    assert!(
        err(&bare).contains("has no SHA256SUMS") && err(&bare).contains("af self install 0.8.1"),
        "{}",
        err(&bare)
    );
    assert!(!out(&bare).contains("fake af"), "{}", out(&bare));
    assert!(!sandbox.versions().join("0.8.1").exists());

    // Checksums without a signature: refused as outside a lock.
    let digest = sandbox.publish("0.8.2", false);
    let repo = sandbox.pinned_repo("unsigned", &Sandbox::lock("0.8.2", Some(&digest)));
    let unsigned = run(&repo);
    assert!(
        err(&unsigned).contains("must be signed"),
        "{}",
        err(&unsigned)
    );
    assert!(!sandbox.versions().join("0.8.2").exists());

    // Signed checksums that disagree with the lock and the archive: a refusal, never a fallback.
    let digest = sandbox.publish("0.8.3", true);
    sandbox.sign("0.8.3", &signer, None);
    let repo = sandbox.pinned_repo("disagree", &Sandbox::lock("0.8.3", Some(&digest)));
    let disagree = run(&repo);
    assert_eq!(disagree.status.code(), Some(1), "{}", err(&disagree));
    assert!(
        err(&disagree).contains("checksum mismatch")
            && err(&disagree).contains("disagree with the project lock"),
        "{}",
        err(&disagree)
    );
    assert!(!out(&disagree).contains("fake af"), "{}", out(&disagree));
    assert!(!sandbox.versions().join("0.8.3").exists());

    // Both agree: installed, and the receipt names both checks.
    let digest = sandbox.publish("0.8.4", false);
    sandbox.sign("0.8.4", &signer, None);
    let repo = sandbox.pinned_repo("agree", &Sandbox::lock("0.8.4", Some(&digest)));
    let agree = run(&repo);
    assert!(
        out(&agree).contains("fake af 0.8.4 review campaigns"),
        "{}",
        err(&agree)
    );
    let receipt = std::fs::read_to_string(sandbox.versions().join("0.8.4/receipt.toml")).unwrap();
    assert!(
        receipt.contains("verified_by = \"lock+signature\""),
        "{receipt}"
    );
}

#[test]
fn install_pins_asks_by_default_and_without_a_terminal_runs_this_af() {
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    let mut repos = Vec::new();
    for version in ["0.8.1", "0.8.2", "0.8.3"] {
        let digest = sandbox.publish(version, false);
        sandbox.sign(version, &signer, None);
        repos.push(sandbox.pinned_repo(
            &format!("repo-{version}"),
            &Sandbox::lock(version, Some(&digest)),
        ));
    }
    let run = |repo: &Path, args: &[&str]| {
        sandbox
            .command(common::af_binary())
            .args(args)
            .current_dir(repo)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    };

    // The default asks, and with no terminal to ask on installs nothing: this af runs, after one
    // line naming the step and the setting.
    let asked = run(&repos[0], &["review", "campaigns"]);
    assert!(asked.status.success(), "{}", err(&asked));
    let stderr = err(&asked);
    let notice: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains("pins af 0.8.1"))
        .collect();
    assert_eq!(notice.len(), 1, "{}", err(&asked));
    assert!(
        notice[0].contains("install_pins = \"ask\"")
            && notice[0].contains("no terminal to ask on")
            && notice[0].contains("af self install 0.8.1"),
        "{}",
        notice[0]
    );
    assert!(!out(&asked).contains("fake af"), "{}", out(&asked));
    assert!(!sandbox.versions().join("0.8.1").exists());

    // `false` is "never": nothing installed either.
    sandbox.install_pins("false");
    let never = run(&repos[0], &["review", "campaigns"]);
    assert!(never.status.success(), "{}", err(&never));
    assert!(
        err(&never).contains("install_pins = \"never\""),
        "{}",
        err(&never)
    );
    assert!(!sandbox.versions().join("0.8.1").exists());

    // Under every setting, `af review plan` installs nothing.
    for setting in ["\"ask\"", "\"always\"", "true", "\"never\""] {
        sandbox.install_pins(setting);
        let plan = run(&repos[2], &["review", "plan", "--policy-rev", "HEAD"]);
        assert!(!out(&plan).contains("fake af"), "{setting}: {}", out(&plan));
        assert!(
            !sandbox.versions().join("0.8.3").exists(),
            "{setting}: review plan installed"
        );
    }

    // `true` is "always", and so is "always": installed and run.
    for (setting, index, version) in [("true", 0, "0.8.1"), ("\"always\"", 1, "0.8.2")] {
        sandbox.install_pins(setting);
        let always = run(&repos[index], &["review", "campaigns"]);
        assert!(
            out(&always).contains(&format!("fake af {version} review campaigns")),
            "{setting}: {}",
            err(&always)
        );
    }

    // Installed, a pin dispatches under "ask" without asking.
    sandbox.install_pins("\"ask\"");
    let installed = run(&repos[1], &["review", "campaigns"]);
    assert!(
        out(&installed).contains("fake af 0.8.2 review campaigns"),
        "{}",
        err(&installed)
    );
}

#[test]
fn update_without_a_version_never_moves_the_default_backwards() {
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    sandbox.publish("0.8.1", false);
    sandbox.sign("0.8.1", &signer, None);
    let real = sandbox.adopt_real_binary();
    let update = |args: &[&str]| {
        sandbox
            .command(&real)
            .args(["self", "update"])
            .args(args)
            .output()
            .unwrap()
    };

    // The newest release listed is older than this one: refused, naming both and the way out.
    let refused = update(&[]);
    assert!(!refused.status.success(), "{}", out(&refused));
    assert!(
        err(&refused).contains("af 0.8.1")
            && err(&refused).contains(&format!("older than the running af {VERSION}"))
            && err(&refused).contains("--version 0.8.1"),
        "{}",
        err(&refused)
    );
    assert!(!sandbox.versions().join("0.8.1").exists());
    assert!(sandbox.default_target().is_none());
    let check = update(&["--check"]);
    assert!(check.status.success(), "{}", err(&check));
    assert!(out(&check).contains("is up to date"), "{}", out(&check));

    // Named explicitly, it is the operator's choice.
    let explicit = update(&["--version", "0.8.1"]);
    assert!(explicit.status.success(), "{}", err(&explicit));
    assert_eq!(sandbox.default_target().as_deref(), Some("0.8.1"));
}

#[test]
fn updates_reach_a_public_repository_anonymously_and_use_gh_only_as_a_fallback() {
    let newer = next_release_version();
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    sandbox.publish(&newer, false);
    sandbox.sign(&newer, &signer, None);
    sandbox.publish("0.8.1", false);
    sandbox.sign("0.8.1", &signer, None);
    let real = sandbox.adopt_real_binary();
    write(
        &sandbox.path("config/af/config.toml"),
        "[self]\nsource = \"owner/repo\"\n",
    );
    let tools = tool_dir(
        &sandbox.path("fakes"),
        &["sh", "tar", "gzip", "cp", "basename"],
    );
    executable(&tools.join("curl"), FAKE_CURL);
    let gh_dir = sandbox.path("fakes/gh");
    executable(&gh_dir.join("gh"), FAKE_GH);
    let (curl_log, gh_log) = (sandbox.path("curl.log"), sandbox.path("gh.log"));
    let with_gh = std::env::join_paths([&gh_dir, &tools]).unwrap();
    let run = |path: &std::ffi::OsStr, args: &[&str], status: Option<&str>| {
        let mut command = sandbox.command(&real);
        command
            .args(args)
            .env_remove("AF_RELEASE_SOURCE")
            .env("PATH", path)
            .env("FAKE_RELEASES", sandbox.path("releases"))
            .env("FAKE_REPO", "owner/repo")
            .env("FAKE_CURL_LOG", &curl_log)
            .env("FAKE_GH_LOG", &gh_log);
        if let Some(status) = status {
            command.env("FAKE_CURL_STATUS", status);
        }
        command.output().unwrap()
    };

    // No gh on PATH: the check and an install go over anonymous HTTPS.
    let check = run(tools.as_os_str(), &["self", "update", "--check"], None);
    assert_eq!(check.status.code(), Some(10), "{}", err(&check));
    assert!(out(&check).contains(&newer), "{}", out(&check));
    let install = run(tools.as_os_str(), &["self", "install", "0.8.1"], None);
    assert!(install.status.success(), "{}", err(&install));
    let receipt = std::fs::read_to_string(sandbox.versions().join("0.8.1/receipt.toml")).unwrap();
    assert!(
        receipt.contains("source = \"github:owner/repo\"")
            && receipt.contains("verified_by = \"minisign\""),
        "{receipt}"
    );
    let calls = std::fs::read_to_string(&curl_log).unwrap();
    assert!(
        calls.contains("--proto =https --proto-redir =https --tlsv1.2"),
        "{calls}"
    );
    assert!(
        calls.contains("https://github.com/owner/repo/releases/download/v0.8.1/SHA256SUMS.minisig"),
        "{calls}"
    );

    // With gh on PATH and the anonymous route open, gh is never asked.
    let check = run(&with_gh, &["self", "update", "--check"], None);
    assert_eq!(check.status.code(), Some(10), "{}", err(&check));
    assert!(!gh_log.exists(), "gh was asked");

    // A repository that is not public answers 404 anonymously: gh answers instead, and without
    // it the failure says what is missing.
    let check = run(&with_gh, &["self", "update", "--check"], Some("404"));
    assert_eq!(check.status.code(), Some(10), "{}", err(&check));
    assert!(
        std::fs::read_to_string(&gh_log)
            .unwrap()
            .contains("release list --repo owner/repo"),
        "gh was not asked"
    );
    let install = run(&with_gh, &["self", "install", &newer], Some("404"));
    assert!(install.status.success(), "{}", err(&install));
    let without = run(
        tools.as_os_str(),
        &["self", "update", "--check"],
        Some("404"),
    );
    assert!(!without.status.success(), "{}", out(&without));
    assert!(
        err(&without).contains("GitHub CLI is not installed"),
        "{}",
        err(&without)
    );
}

#[test]
fn uninstall_leaves_a_default_path_that_is_not_afs_link() {
    let sandbox = Sandbox::new();
    let real = sandbox.adopt_real_binary();
    let uninstall = || {
        sandbox
            .command(&real)
            .args(["self", "uninstall"])
            .output()
            .unwrap()
    };

    // A binary the user built and put there is theirs.
    let user_built = sandbox.path("bin/af");
    write(&user_built, "#!/bin/sh\necho mine\n");
    let kept = uninstall();
    assert!(kept.status.success(), "{}", err(&kept));
    assert!(
        out(&kept).contains(&format!("left {} in place", user_built.display())),
        "{}",
        out(&kept)
    );
    assert_eq!(
        std::fs::read_to_string(&user_built).unwrap(),
        "#!/bin/sh\necho mine\n"
    );
    assert!(!sandbox.versions().exists());

    // So is a link to something outside af's versions.
    std::fs::remove_file(&user_built).unwrap();
    let elsewhere = sandbox.path("elsewhere/af");
    write(&elsewhere, "#!/bin/sh\n");
    std::os::unix::fs::symlink(&elsewhere, &user_built).unwrap();
    let real = sandbox.adopt_real_binary();
    let kept = sandbox
        .command(&real)
        .args(["self", "uninstall"])
        .output()
        .unwrap();
    assert!(kept.status.success(), "{}", err(&kept));
    assert!(std::fs::symlink_metadata(&user_built).is_ok());
    assert!(elsewhere.is_file());

    // af's own link goes.
    std::fs::remove_file(&user_built).unwrap();
    let real = sandbox.adopt_real_binary();
    std::os::unix::fs::symlink(&real, &user_built).unwrap();
    let removed = sandbox
        .command(&real)
        .args(["self", "uninstall"])
        .output()
        .unwrap();
    assert!(removed.status.success(), "{}", err(&removed));
    assert!(
        out(&removed).contains(&format!("removed {}", user_built.display())),
        "{}",
        out(&removed)
    );
    assert!(std::fs::symlink_metadata(&user_built).is_err());
}
