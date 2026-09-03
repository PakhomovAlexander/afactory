//! The CLI surface: scoped help, topics, completions, the configuration ladder, dispatch to a
//! pinned version, and `af self` against a directory release source.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const AF: &str = env!("CARGO_BIN_EXE_af");
const VERSION: &str = env!("CARGO_PKG_VERSION");
const TARGET: &str = env!("AF_TARGET");

fn af(args: &[&str]) -> Output {
    Command::new(AF)
        .args(args)
        .env("AF_SELF_OFFLINE", "1")
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn namespace_help_is_scoped_to_the_namespace() {
    let help = af(&["provider", "--help"]);
    assert!(help.status.success());
    let text = out(&help);
    assert!(text.contains("status"), "{text}");
    assert!(text.contains("doctor"), "{text}");
    assert!(
        !text.contains("ledger"),
        "provider help leaked review commands: {text}"
    );
    assert!(!text.contains("onboard"), "{text}");

    let bare = af(&["provider"]);
    assert_eq!(bare.status.code(), Some(2));
    assert!(err(&bare).contains("Usage: af provider"), "{}", err(&bare));

    let root = af(&[]);
    assert_eq!(root.status.code(), Some(2));
    assert!(err(&root).contains("review"), "{}", err(&root));
}

#[test]
fn usage_errors_name_only_the_failing_command() {
    let bad = af(&["review", "ledger", "--campaign", "x", "--nope"]);
    assert_eq!(bad.status.code(), Some(2));
    let text = err(&bad);
    assert!(text.contains("--nope"), "{text}");
    assert!(text.contains("Usage: af review ledger"), "{text}");
    assert!(!text.contains("attest-change"), "{text}");
}

#[test]
fn help_topics_and_command_paths() {
    let layers = af(&["help", "layers"]);
    assert!(layers.status.success());
    assert!(out(&layers).contains("directory"), "{}", out(&layers));

    let ledger = af(&["help", "review", "ledger"]);
    assert!(ledger.status.success());
    assert!(out(&ledger).contains("--campaign"), "{}", out(&ledger));

    let unknown = af(&["help", "nonsense"]);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(err(&unknown).contains("topics:"), "{}", err(&unknown));
}

#[test]
fn version_is_a_line_or_a_document() {
    let line = af(&["--version"]);
    assert_eq!(out(&line).trim(), format!("af {VERSION}"));
    let json: serde_json::Value =
        serde_json::from_slice(&af(&["--version", "--json"]).stdout).unwrap();
    assert_eq!(json["version"], VERSION);
    assert_eq!(json["target"], TARGET);
    assert!(json["commit"].is_string());
}

#[test]
fn completion_scripts_delegate_to_the_binary() {
    for shell in ["bash", "zsh", "fish", "elvish", "powershell"] {
        let script = af(&["completions", shell]);
        assert!(script.status.success(), "{shell}: {}", err(&script));
        assert!(
            out(&script).contains("COMPLETE"),
            "{shell}: {}",
            out(&script)
        );
    }
    let dynamic = Command::new(AF)
        .env("COMPLETE", "fish")
        .env("AF_SELF_OFFLINE", "1")
        .args(["--", "af", "provider", ""])
        .output()
        .unwrap();
    assert!(dynamic.status.success(), "{}", err(&dynamic));
    let text = out(&dynamic);
    assert!(text.contains("status"), "{text}");
    assert!(text.contains("doctor"), "{text}");
    assert!(!text.contains("ledger"), "{text}");
}

#[test]
fn man_pages_render_for_every_command_and_topic() {
    let dir = tempfile::tempdir().unwrap();
    let man = af(&["self", "man", dir.path().to_str().unwrap()]);
    assert!(man.status.success(), "{}", err(&man));
    for page in [
        "af.1",
        "af-review.1",
        "af-review-run.1",
        "af-self.1",
        "af-layers.7",
    ] {
        assert!(dir.path().join(page).is_file(), "{page} missing");
    }
    let root = std::fs::read_to_string(dir.path().join("af.1")).unwrap();
    assert!(root.contains("review"));
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn the_configuration_ladder_merges_in_order_and_names_origins() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let config = root.path().join("config");
    write(
        &config.join("af/config.toml"),
        "[self]\nauto_update = \"never\"\nchannel = \"stable\"\n",
    );
    write(
        &config.join("af/conf.d/10-extra.toml"),
        "[self]\nkeep_versions = 9\n",
    );
    let work = root.path().join("work");
    write(
        &work.join(".af/af.toml"),
        "[ui]\ncolor = \"never\"\n[defaults]\npipeline = \"shared\"\n",
    );
    let project = work.join("proj");
    std::fs::create_dir_all(project.join(".git")).unwrap();
    write(
        &project.join(".af/af.toml"),
        "version = 1\n[defaults]\npipeline = \"review\"\n[self]\nsource = \"attacker/repo\"\n",
    );
    write(
        &project.join(".af/af.local.toml"),
        "[ui]\npager = \"less\"\n",
    );
    // Nothing below the toplevel is a layer.
    write(
        &project.join("sub/.af/af.toml"),
        "[ui]\ncolor = \"never-read\"\n",
    );

    let show = Command::new(AF)
        .args(["config", "show", "--origin", "--repo"])
        .arg(project.join("sub"))
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &config)
        .env("AF_SELF__CHECK_EVERY", "\"1h\"")
        .env("AF_SELF_OFFLINE", "1")
        .output()
        .unwrap();
    assert!(show.status.success(), "{}", err(&show));
    let text = out(&show);
    let line = |key: &str| {
        text.lines()
            .find(|line| line.starts_with(&format!("{key} = ")))
            .unwrap_or_else(|| panic!("{key} missing in\n{text}"))
            .to_string()
    };
    let check = |key: &str, value: &str, origin: &str| {
        let found = line(key);
        assert!(
            found.contains(value) && found.contains(origin),
            "{key}: expected {value} from {origin}, got `{found}` in\n{text}"
        );
    };
    check("self.auto_update", "\"never\"", "# user");
    check("self.keep_versions", "= 9", "# user");
    check("ui.color", "\"never\"", "# directory");
    check("defaults.pipeline", "\"review\"", "# project");
    check("ui.pager", "\"less\"", "# local");
    check("self.check_every", "\"1h\"", "# environment");
    check("self.install_pins", "true", "# built-in");
    check("self.source", "PakhomovAlexander", "# built-in");
    assert!(
        text.contains("# ignored: [self] in") && text.contains("proj/.af/af.toml (project layer)"),
        "{text}"
    );
    assert!(!text.contains("never-read"), "{text}");
    assert!(line("ui.color").contains("work/.af/af.toml:2"), "{text}");

    let json: serde_json::Value = serde_json::from_slice(
        &Command::new(AF)
            .args(["config", "show", "--json", "--repo"])
            .arg(&project)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &config)
            .env("AF_SELF_OFFLINE", "1")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(json["effective"]["self"]["keep_versions"], 9);
    assert_eq!(
        json["effective"]["self"]["source"],
        "PakhomovAlexander/afactory"
    );
    assert_eq!(json["origins"]["ui.color"]["layer"], "directory");
    assert_eq!(json["ignored"].as_array().unwrap().len(), 1);
    let layers: Vec<&str> = json["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["layer"].as_str().unwrap())
        .collect();
    assert!(
        layers.starts_with(&["system", "user", "user"]),
        "{layers:?}"
    );
    assert!(
        layers.ends_with(&["directory", "project", "local"]),
        "{layers:?}"
    );
}

struct Sandbox {
    root: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        for dir in [
            "home", "config", "data", "state", "cache", "bin", "releases",
        ] {
            std::fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        Self { root }
    }
    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }
    fn command(&self, binary: &Path) -> Command {
        let mut command = Command::new(binary);
        command
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("XDG_CACHE_HOME", self.path("cache"))
            .env("XDG_BIN_HOME", self.path("bin"))
            .env("AF_RELEASE_SOURCE", self.path("releases"))
            .env("NO_COLOR", "1")
            .env_remove("AF_SELF_OFFLINE")
            .env_remove("CI");
        command
    }
    fn versions(&self) -> PathBuf {
        self.path("data/af/versions")
    }
    fn default_target(&self) -> Option<String> {
        std::fs::read_link(self.path("bin/af"))
            .ok()
            .and_then(|link| {
                link.parent()
                    .and_then(|dir| dir.file_name())
                    .map(|name| name.to_string_lossy().into_owned())
            })
    }
    /// A release whose `af` is a script reporting `version`.
    fn publish(&self, version: &str, tamper: bool) {
        let tag_dir = self.path(&format!("releases/v{version}"));
        std::fs::create_dir_all(&tag_dir).unwrap();
        let stage = self.path(&format!("stage-{version}"));
        std::fs::create_dir_all(&stage).unwrap();
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo \"af {version}\"; exit 0; fi\necho \"fake af {version} $*\"\necho \"from=${{AF_DISPATCHED_FROM:-none}}\"\n"
        );
        write(&stage.join("af"), &script);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(stage.join("af"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let asset = format!("af-v{version}-{TARGET}.tar.gz");
        let status = Command::new("tar")
            .arg("-czf")
            .arg(tag_dir.join(&asset))
            .arg("-C")
            .arg(&stage)
            .arg("af")
            .status()
            .unwrap();
        assert!(status.success());
        let bytes = std::fs::read(tag_dir.join(&asset)).unwrap();
        let mut digest = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(&bytes));
        if tamper {
            digest = digest.chars().rev().collect();
        }
        write(&tag_dir.join("SHA256SUMS"), &format!("{digest}  {asset}\n"));
    }
    /// Adopt the real binary under test as an installed, receipted version.
    fn adopt_real_binary(&self) -> PathBuf {
        let dir = self.versions().join(VERSION);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(AF, dir.join("af")).unwrap();
        write(
            &dir.join("receipt.toml"),
            &format!(
                "version = \"{VERSION}\"\ntarget = \"{TARGET}\"\nsource = \"test\"\nasset = \"none\"\nsha256 = \"none\"\nverified_by = \"test\"\ninstalled_at = \"2026-09-03T00:00:00Z\"\n"
            ),
        );
        dir.join("af")
    }
}

#[test]
fn self_install_update_rollback_and_remove_against_a_directory_source() {
    let sandbox = Sandbox::new();
    sandbox.publish("0.6.0", false);
    sandbox.publish("0.9.0", false);
    sandbox.publish("0.8.0", true);

    let install = sandbox
        .command(Path::new(AF))
        .args(["self", "install", "0.6.0"])
        .output()
        .unwrap();
    assert!(install.status.success(), "{}", err(&install));
    assert!(sandbox.versions().join("0.6.0/af").is_file());
    let receipt = std::fs::read_to_string(sandbox.versions().join("0.6.0/receipt.toml")).unwrap();
    assert!(
        receipt.contains("verified_by = \"sha256sums\""),
        "{receipt}"
    );
    assert_eq!(
        sandbox.default_target().as_deref(),
        Some("0.6.0"),
        "first install becomes the default"
    );

    let tampered = sandbox
        .command(Path::new(AF))
        .args(["self", "install", "0.8.0"])
        .output()
        .unwrap();
    assert!(!tampered.status.success());
    assert!(
        err(&tampered).contains("checksum mismatch"),
        "{}",
        err(&tampered)
    );
    assert!(!sandbox.versions().join("0.8.0").exists());

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
        serde_json::json!(["0.6.0", VERSION, "0.9.0"])
    );
    assert!(status["receipt"].is_object());

    let rollback = sandbox
        .command(&real)
        .args(["self", "rollback"])
        .output()
        .unwrap();
    assert!(rollback.status.success(), "{}", err(&rollback));
    assert_eq!(sandbox.default_target().as_deref(), Some("0.6.0"));

    let remove_default = sandbox
        .command(&real)
        .args(["self", "remove", "0.6.0"])
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
fn dispatch_runs_the_version_a_project_pins() {
    let sandbox = Sandbox::new();
    sandbox.publish("0.6.0", false);
    let repo = sandbox.path("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    write(
        &repo.join(".af/af.lock"),
        "version = 1\naf_version = \"0.6.0\"\n\n[reviewers]\n",
    );

    // Not installed, offline: the running binary continues and names the fix.
    let offline = sandbox
        .command(Path::new(AF))
        .args(["review", "plan", "--repo"])
        .arg(&repo)
        .env("AF_SELF_OFFLINE", "1")
        .output()
        .unwrap();
    assert!(
        err(&offline).contains("af self install 0.6.0"),
        "{}",
        err(&offline)
    );
    assert!(!out(&offline).contains("fake af"), "{}", out(&offline));

    // Online (a directory source): installed on demand, then exec'd with the same argv.
    let dispatched = sandbox
        .command(Path::new(AF))
        .args(["review", "plan", "--repo"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(dispatched.status.success(), "{}", err(&dispatched));
    let text = out(&dispatched);
    assert!(text.contains("fake af 0.6.0 review plan --repo"), "{text}");
    assert!(text.contains(&format!("from={VERSION}")), "{text}");
    assert!(sandbox.versions().join("0.6.0/receipt.toml").is_file());

    // Self commands, help, and version never dispatch.
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

    // AF_VERSION overrides the pin.
    let forced = sandbox
        .command(Path::new(AF))
        .args(["review", "plan"])
        .env("AF_VERSION", "0.6.0")
        .current_dir(sandbox.path("home"))
        .output()
        .unwrap();
    assert!(
        out(&forced).contains("fake af 0.6.0 review plan"),
        "{}",
        out(&forced)
    );

    // A pin matching the running version runs in place.
    write(
        &repo.join(".af/af.lock"),
        &format!("version = 1\naf_version = \"{VERSION}\"\n\n[reviewers]\n"),
    );
    let same = sandbox
        .command(Path::new(AF))
        .args(["config", "paths", "--repo"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(!out(&same).contains("fake af"));
}

#[test]
fn refresh_check_caches_the_latest_and_applies_always() {
    let sandbox = Sandbox::new();
    sandbox.publish("0.9.0", false);
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
    sandbox.publish("0.6.0", false);
    write(
        &sandbox.path("config/af/config.toml"),
        "[self]\ninstall_pins = false\n",
    );
    let repo = sandbox.path("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    // The repository asks for installs from elsewhere; the user said no installs. The user wins.
    write(
        &repo.join(".af/af.toml"),
        "[self]\nsource = \"attacker/repo\"\ninstall_pins = true\n",
    );
    write(
        &repo.join(".af/af.lock"),
        "version = 1\naf_version = \"0.6.0\"\n\n[reviewers]\n",
    );
    let older = sandbox
        .command(Path::new(AF))
        .args(["review", "campaigns"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        !sandbox.versions().join("0.6.0").exists(),
        "a repository must not trigger an install"
    );
    assert!(
        err(&older).contains("af self install 0.6.0"),
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
        "version = 1\naf_version = \"9.9.9\"\n\n[reviewers]\n",
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
    sandbox.publish("0.6.0", false);
    let dir = sandbox.versions().join("0.6.0");
    write(&dir.join("af"), "#!/bin/sh\necho planted\n");
    #[cfg(unix)]
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
    let repo = sandbox.path("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    write(
        &repo.join(".af/af.lock"),
        "version = 1\naf_version = \"0.6.0\"\n\n[reviewers]\n",
    );
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
        out(&dispatched).contains("fake af 0.6.0 review campaigns"),
        "{}\n{}",
        out(&dispatched),
        err(&dispatched)
    );
    let receipt = std::fs::read_to_string(dir.join("receipt.toml")).unwrap();
    assert!(receipt.contains("version = \"0.6.0\""), "{receipt}");
}

#[test]
fn json_errors_are_one_document_on_stdout() {
    let failed = Command::new(AF)
        .args(["config", "show", "--json"])
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env("AF_SELF_OFFLINE", "1")
        .output()
        .unwrap();
    assert_eq!(failed.status.code(), Some(1));
    let document: serde_json::Value = serde_json::from_slice(&failed.stdout)
        .unwrap_or_else(|_| panic!("not JSON: {}", out(&failed)));
    assert_eq!(document["schema"], "af/error@1");
    assert!(
        document["error"].as_str().unwrap().contains("HOME"),
        "{document}"
    );
    assert_eq!(document["exit_code"], 1);
}
