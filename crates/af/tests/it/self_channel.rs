//! `af self channel` against a directory release source holding a release, a release candidate
//! and a nightly (ADR-0267): the switch installs the channel's newest, writes `[self] channel`
//! into the user layer, keeping the file's other keys and comments, and makes that newest the
//! default, moving back to an older release when the channel says so; `--no-update` only records.
//! The nightly is verified against the nightly key alone: a build without one refuses the channel
//! with one sentence, and a nightly signed with the release key is refused. A switch that fails,
//! even once the default was moved, leaves the user layer and the default as they were. Running
//! the nightly it activated, the binary the source published, the
//! check tells a newer date and the status names the nightly; a lock that pins a nightly and a
//! request for another version of an installed nightly's date are refused. Nothing reaches the
//! network.

use std::path::{Path, PathBuf};
use std::process::Output;

use crate::common;
use common::{NIGHTLY_COMMIT, Sandbox, Signer, VERSION, err, out, write};

const NIGHTLY_TAG: &str = "nightly-2026.10.29";
const NIGHTLY: &str = "0.14.0-nightly.20261029";

/// A user layer with keys and comments of its own, which a switch must keep.
const USER_LAYER: &str = "# my machine\n[self]\nkeep_versions = 5   # room for a few nightlies\n\n[storage]\nkeep_days = 7\n";

fn user_layer(sandbox: &Sandbox) -> PathBuf {
    sandbox.path("config/af/config.toml")
}

/// The user layer as af names it: its configuration root is resolved (macOS `/var` is a link).
fn user_layer_named(sandbox: &Sandbox) -> String {
    std::fs::canonicalize(user_layer(sandbox))
        .unwrap()
        .display()
        .to_string()
}

/// The release source of the goal: `v0.13.0`, `v0.14.0-rc.3` signed with the release key, and
/// `nightly-2026.10.29` signed with `nightly_signer`.
fn sources(sandbox: &Sandbox, release: &Signer, nightly_signer: &Signer) {
    for version in ["0.13.0", "0.14.0-rc.3"] {
        sandbox.publish(version, false);
        sandbox.sign(version, release, None);
    }
    sandbox.publish_nightly(NIGHTLY_TAG, NIGHTLY);
    sandbox.sign_tag(NIGHTLY_TAG, nightly_signer, None);
}

/// `af self <args>` from the adopted binary under test, verifying nightlies with `nightly_key`.
fn af_self(sandbox: &Sandbox, real: &Path, nightly_key: Option<&Path>, args: &[&str]) -> Output {
    let mut command = sandbox.command(real);
    command.arg("self").args(args);
    if let Some(key) = nightly_key {
        command.env("AF_NIGHTLY_KEY", key);
    }
    command.output().unwrap()
}

#[test]
fn a_channel_switch_writes_the_user_layer_and_moves_between_nightly_and_stable() {
    let keys = tempfile::tempdir().unwrap();
    let release = Signer::new(keys.path());
    let nightly_keys = keys.path().join("nightly");
    let nightly = Signer::new(&nightly_keys);
    let sandbox = Sandbox::new().with_key(&release);
    sources(&sandbox, &release, &nightly);
    write(&user_layer(&sandbox), USER_LAYER);
    let real = sandbox.adopt_real_binary();
    let key = Some(nightly.public_key_file.as_path());

    // Before any switch: the built-in default.
    let shown = af_self(&sandbox, &real, key, &["channel"]);
    assert!(shown.status.success(), "{}", err(&shown));
    assert_eq!(out(&shown).trim(), "stable (built-in)");

    // Nightly: written into the user layer, its other keys and comments kept, and the newest
    // nightly installed and made the default under the version its binary reports.
    let switched = af_self(&sandbox, &real, key, &["channel", "nightly"]);
    assert!(switched.status.success(), "{}", err(&switched));
    let layer = std::fs::read_to_string(user_layer(&sandbox)).unwrap();
    assert_eq!(
        layer,
        USER_LAYER.replace(
            "keep_versions = 5   # room for a few nightlies\n",
            "keep_versions = 5   # room for a few nightlies\nchannel = \"nightly\"\n"
        ),
        "{layer}"
    );
    assert!(
        out(&switched).contains(&format!(
            "channel: nightly (written to {})",
            user_layer_named(&sandbox)
        )) && out(&switched).contains(&format!("af {NIGHTLY} ({NIGHTLY_TAG}) is now the default")),
        "{}",
        out(&switched)
    );
    assert_eq!(sandbox.default_target().as_deref(), Some(NIGHTLY));
    let receipt: toml::Value = toml::from_str(
        &std::fs::read_to_string(sandbox.versions().join(NIGHTLY).join("receipt.toml")).unwrap(),
    )
    .unwrap();
    assert_eq!(receipt["version"].as_str(), Some(NIGHTLY));
    assert_eq!(receipt["tag"].as_str(), Some(NIGHTLY_TAG));
    assert_eq!(receipt["channel"].as_str(), Some("nightly"));
    assert_eq!(receipt["commit"].as_str(), Some(NIGHTLY_COMMIT));
    assert_eq!(receipt["verified_by"].as_str(), Some("minisign"));
    assert_eq!(
        receipt["asset"].as_str(),
        Some(format!("af-{NIGHTLY_TAG}-{}.tar.gz", common::TARGET).as_str())
    );
    let shown = af_self(&sandbox, &real, key, &["channel"]);
    assert!(
        out(&shown).starts_with(&format!("nightly (user {}:", user_layer_named(&sandbox))),
        "{}",
        out(&shown)
    );

    // On the nightly channel the check asks for nightlies: this af runs none, so the newest is
    // news, named by its tag with the switch that installs it.
    let check = af_self(&sandbox, &real, key, &["update", "--check"]);
    assert_eq!(check.status.code(), Some(10), "{}", err(&check));
    assert_eq!(
        out(&check).trim(),
        format!("af {NIGHTLY_TAG} is available (running {VERSION}) — af self channel nightly")
    );
    // Status names the channel and the newest nightly the check found, and keeps its shape.
    let status = af_self(&sandbox, &real, key, &["status", "--json"]);
    assert!(status.status.success(), "{}", err(&status));
    let document: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    crate::schemas::valid(&crate::schemas::validator("self-status-v2.json"), &document);
    assert_eq!(document["policy"]["channel"], "nightly");
    assert_eq!(document["latest"], NIGHTLY_TAG);
    let status = af_self(&sandbox, &real, key, &["status"]);
    assert!(
        out(&status).contains(&format!(
            "nightly:    running af {VERSION}, no nightly · newest {NIGHTLY_TAG} (af self channel nightly)"
        )) && out(&status).contains("channel=nightly"),
        "{}",
        out(&status)
    );

    // Stable: the newest release, older than the nightly it replaces, and said so. A nightly
    // never stands for the stable channel.
    let back = af_self(&sandbox, &real, key, &["channel", "stable"]);
    assert!(back.status.success(), "{}", err(&back));
    assert_eq!(sandbox.default_target().as_deref(), Some("0.13.0"));
    assert!(
        out(&back).contains("af 0.13.0 is now the default")
            && out(&back).contains(&format!(
                "moved back from af {NIGHTLY} to af 0.13.0: the stable channel's newest is older"
            )),
        "{}",
        out(&back)
    );
    assert!(
        std::fs::read_to_string(user_layer(&sandbox))
            .unwrap()
            .contains("channel = \"stable\""),
    );

    // --no-update records the channel and installs nothing.
    let before = installed(&sandbox);
    let recorded = af_self(&sandbox, &real, key, &["channel", "rc", "--no-update"]);
    assert!(recorded.status.success(), "{}", err(&recorded));
    assert!(
        out(&recorded).contains(
            "nothing installed: `af self channel rc` without --no-update installs the channel's newest and makes it the default"
        ),
        "{}",
        out(&recorded)
    );
    let layer = std::fs::read_to_string(user_layer(&sandbox)).unwrap();
    assert!(
        layer.contains("channel = \"rc\"") && layer.contains("# room for a few nightlies"),
        "{layer}"
    );
    assert_eq!(sandbox.default_target().as_deref(), Some("0.13.0"));
    assert_eq!(installed(&sandbox), before);
}

/// The installed versions' directories, sorted.
fn installed(sandbox: &Sandbox) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(sandbox.versions())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_failed_switch_leaves_the_user_layer_and_the_default_as_they_were() {
    let keys = tempfile::tempdir().unwrap();
    let release = Signer::new(keys.path());
    let nightly = Signer::new(&keys.path().join("nightly"));
    let sandbox = Sandbox::new().with_key(&release);
    // The nightly's sums signed with the release key, as a release job that took the wrong key
    // would sign them.
    sources(&sandbox, &release, &release);
    let real = sandbox.adopt_real_binary();

    // A build that embeds no nightly key refuses the channel with its one sentence and writes
    // nothing, --no-update included.
    for args in [
        &["channel", "nightly"][..],
        &["channel", "nightly", "--no-update"],
    ] {
        let refused = sandbox
            .command(&real)
            .arg("self")
            .args(args)
            .env("AF_TEST_EMBEDDED_NIGHTLY_KEY", "")
            .output()
            .unwrap();
        assert_eq!(refused.status.code(), Some(1), "{}", out(&refused));
        assert!(
            err(&refused).contains("this af carries no nightly key"),
            "{}",
            err(&refused)
        );
        assert!(!user_layer(&sandbox).exists());
    }

    // A first switch, to stable: the layer and the default the failed switches below keep.
    let key = Some(nightly.public_key_file.as_path());
    let stable = af_self(&sandbox, &real, key, &["channel", "stable"]);
    assert!(stable.status.success(), "{}", err(&stable));
    let layer = std::fs::read_to_string(user_layer(&sandbox)).unwrap();
    assert_eq!(layer, "[self]\nchannel = \"stable\"\n");
    assert_eq!(sandbox.default_target().as_deref(), Some("0.13.0"));
    let unchanged = |refused: &Output, cause: &str| {
        assert_eq!(refused.status.code(), Some(1), "{}", out(refused));
        assert!(
            err(refused).contains(cause)
                && err(refused).contains(
                    "the switch to the nightly channel changed nothing: [self] channel and the default are as they were — fix: `af self channel nightly` tries again"
                )
                && !err(refused).contains("af self update"),
            "{}",
            err(refused)
        );
        assert!(
            !out(refused).contains("channel: nightly"),
            "{}",
            out(refused)
        );
        assert_eq!(
            std::fs::read_to_string(user_layer(&sandbox)).unwrap(),
            layer
        );
        assert_eq!(sandbox.default_target().as_deref(), Some("0.13.0"));
        assert!(!sandbox.versions().join(NIGHTLY).exists());
    };

    // The nightly key verifies a nightly's sums, and the release key's signature is not it.
    let refused = af_self(&sandbox, &real, key, &["channel", "nightly"]);
    unchanged(
        &refused,
        &format!("SHA256SUMS of {NIGHTLY_TAG} does not verify against the nightly key"),
    );

    // A release source that is not there: the listing fails before anything is written.
    let missing = sandbox.path("no-such-releases");
    let refused = sandbox
        .command(&real)
        .args(["self", "channel", "nightly"])
        .env("AF_NIGHTLY_KEY", &nightly.public_key_file)
        .env("AF_RELEASE_SOURCE", &missing)
        .output()
        .unwrap();
    unchanged(&refused, &format!("reading {}", missing.display()));

    // The nightly verifies and installs, but the activation cannot be recorded: a directory
    // where the state file goes. The link was moved and is put back, the layer too; the nightly
    // stays installed for the retry the message names.
    sandbox.sign_tag(NIGHTLY_TAG, &nightly, None);
    let state = sandbox.path("state/af/self.toml");
    let was = std::fs::read(&state).unwrap();
    std::fs::remove_file(&state).unwrap();
    std::fs::create_dir(&state).unwrap();
    let refused = af_self(&sandbox, &real, key, &["channel", "nightly"]);
    assert_eq!(refused.status.code(), Some(1), "{}", out(&refused));
    assert!(
        err(&refused).contains(&format!(
            "recording the activation of af {NIGHTLY}: renaming into {}",
            std::fs::canonicalize(&state).unwrap().display()
        )) && err(&refused).contains("the default is as it was")
            && err(&refused).contains(
                "the switch to the nightly channel changed nothing: [self] channel and the default are as they were — fix: `af self channel nightly` tries again"
            ),
        "{}",
        err(&refused)
    );
    assert_eq!(
        std::fs::read_to_string(user_layer(&sandbox)).unwrap(),
        layer
    );
    assert_eq!(sandbox.default_target().as_deref(), Some("0.13.0"));
    assert!(sandbox.versions().join(NIGHTLY).join("af").is_file());
    std::fs::remove_dir(&state).unwrap();
    std::fs::write(&state, was).unwrap();
    let switched = af_self(&sandbox, &real, key, &["channel", "nightly"]);
    assert!(switched.status.success(), "{}", err(&switched));
    assert_eq!(sandbox.default_target().as_deref(), Some(NIGHTLY));
}

#[test]
fn a_running_nightly_is_told_a_newer_date_and_no_pin_or_other_version_reaches_a_nightly() {
    let keys = tempfile::tempdir().unwrap();
    let release = Signer::new(keys.path());
    let nightly = Signer::new(&keys.path().join("nightly"));
    let sandbox = Sandbox::new().with_key(&release);
    sources(&sandbox, &release, &nightly);
    // The nightly of the goal is the binary under test, which reports NIGHTLY as its version
    // while `AF_TEST_RUNNING_VERSION` says so: the switch installs it from the source, and it is
    // what then runs as the nightly below. Its commit is the build's.
    sandbox.publish_nightly_binary(NIGHTLY_TAG);
    sandbox.sign_tag(NIGHTLY_TAG, &nightly, None);
    let older = "nightly-2026.10.28";
    sandbox.publish_nightly(older, "0.14.0-nightly.20261028");
    sandbox.sign_tag(older, &nightly, None);
    let real = sandbox.adopt_real_binary();
    let key = Some(nightly.public_key_file.as_path());
    let switched = sandbox
        .command(&real)
        .args(["self", "channel", "nightly"])
        .env("AF_NIGHTLY_KEY", &nightly.public_key_file)
        .env("AF_TEST_RUNNING_VERSION", NIGHTLY)
        .output()
        .unwrap();
    assert!(switched.status.success(), "{}", err(&switched));
    assert_eq!(sandbox.default_target().as_deref(), Some(NIGHTLY));
    let running = sandbox.versions().join(NIGHTLY).join("af");
    assert_eq!(
        std::fs::read(&running).unwrap(),
        std::fs::read(common::private_af()).unwrap(),
        "the installed nightly is the binary the source published"
    );

    // A lock that pins the installed nightly pins nothing: one sentence, and this af runs. A
    // lock that pins a release still dispatches to it.
    let digest = common::sha256_hex(
        &std::fs::read(sandbox.path(&format!(
            "releases/{NIGHTLY_TAG}/af-{NIGHTLY_TAG}-{}.tar.gz",
            common::TARGET
        )))
        .unwrap(),
    );
    let repo = sandbox.pinned_repo("nightly-pin", &Sandbox::lock(NIGHTLY, Some(&digest)));
    let run = sandbox
        .command(&real)
        .args(["review", "campaigns"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(run.status.success(), "{}", err(&run));
    let lock = std::fs::canonicalize(repo.join(".af/af.lock")).unwrap();
    let refusal = format!(
        "af: {} pins af {NIGHTLY}, a nightly, and a nightly is never pinned, so it pins nothing — fix: af onboard --refresh-lock --af <version>",
        lock.display()
    );
    assert_eq!(
        err(&run).lines().filter(|line| *line == refusal).count(),
        1,
        "{}",
        err(&run)
    );
    assert!(!out(&run).contains("fake af"), "{}", out(&run));
    let release_digest = common::sha256_hex(
        &std::fs::read(sandbox.release_dir("0.13.0").join(Sandbox::asset("0.13.0"))).unwrap(),
    );
    let repo = sandbox.pinned_repo(
        "release-pin",
        &Sandbox::lock("0.13.0", Some(&release_digest)),
    );
    let run = sandbox
        .command(&real)
        .args(["review", "campaigns"])
        .current_dir(&repo)
        .env("AF_SELF__INSTALL_PINS", "\"always\"")
        .output()
        .unwrap();
    assert!(
        out(&run).contains("fake af 0.13.0 review campaigns"),
        "{}\n{}",
        out(&run),
        err(&run)
    );

    // Another core version of the installed nightly's date is never the installed binary.
    let other = "0.15.0-nightly.20261029";
    let refused = af_self(&sandbox, &real, key, &["install", other]);
    assert_eq!(refused.status.code(), Some(1), "{}", out(&refused));
    assert!(
        err(&refused).contains(&format!(
            "the nightly {NIGHTLY_TAG} is af {NIGHTLY}, not the af {other} asked for"
        )) && !out(&refused).contains("installed at"),
        "{}\n{}",
        out(&refused),
        err(&refused)
    );
    assert!(!sandbox.versions().join(other).exists());
    assert_eq!(sandbox.default_target().as_deref(), Some(NIGHTLY));

    // Running the activated nightly, installed from the source: the newest nightly is its own
    // date, so it is up to date, and so it is when the listing holds only an older nightly.
    let as_nightly = |args: &[&str]| {
        sandbox
            .command(&running)
            .arg("self")
            .args(args)
            .env("AF_TEST_RUNNING_VERSION", NIGHTLY)
            .output()
            .unwrap()
    };
    let version = String::from_utf8(
        sandbox
            .command(&running)
            .args(["--version", "--json"])
            .env("AF_TEST_RUNNING_VERSION", NIGHTLY)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let reported: serde_json::Value = serde_json::from_str(&version).unwrap();
    assert_eq!(reported["version"], NIGHTLY, "{version}");
    let commit = reported["commit"].as_str().unwrap().to_string();
    let check = as_nightly(&["update", "--check"]);
    assert_eq!(check.status.code(), Some(0), "{}", err(&check));
    assert_eq!(
        out(&check).trim(),
        format!("af {NIGHTLY} is up to date (the newest nightly is {NIGHTLY_TAG})")
    );
    let published = sandbox.path(&format!("releases/{NIGHTLY_TAG}"));
    let aside = sandbox.path(&format!("aside-{NIGHTLY_TAG}"));
    std::fs::rename(&published, &aside).unwrap();
    let check = as_nightly(&["update", "--check"]);
    assert_eq!(check.status.code(), Some(0), "{}", err(&check));
    assert_eq!(
        out(&check).trim(),
        format!("af {NIGHTLY} is up to date (the newest nightly is {older})")
    );
    std::fs::rename(&aside, &published).unwrap();

    // A nightly of a later date: news, exit 10, and the status names the running nightly's tag
    // and commit and the newest the check found.
    let newer = "nightly-2026.10.30";
    sandbox.publish_nightly(newer, "0.14.0-nightly.20261030");
    sandbox.sign_tag(newer, &nightly, None);
    let check = as_nightly(&["update", "--check"]);
    assert_eq!(check.status.code(), Some(10), "{}", err(&check));
    assert_eq!(
        out(&check).trim(),
        format!("af {newer} is available (running {NIGHTLY}) — af self update")
    );
    let status = as_nightly(&["status"]);
    assert!(status.status.success(), "{}", err(&status));
    assert!(
        out(&status).starts_with(&format!("af {NIGHTLY} ("))
            && out(&status).contains(&format!(
                "nightly:    running {NIGHTLY_TAG} (commit {commit}) · newest {newer} (af self update)"
            )),
        "{}",
        out(&status)
    );
}
