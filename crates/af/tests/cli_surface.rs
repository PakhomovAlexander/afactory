//! The CLI surface: scoped help, topics, completions, and the configuration ladder. Dispatch and
//! `af self` live in `self_managed.rs`.

use std::path::Path;
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
