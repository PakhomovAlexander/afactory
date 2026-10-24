//! `af doctor`: a token-free check of the machine and the repository it runs in (ADR-0201). Each
//! case runs in a private home that a snapshot proves the command leaves exactly as it found it,
//! and every `--json` document is checked against `schemas/doctor-v1.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::schemas;

const FIRST_LINE: &str = "token-free; the charged end-to-end Provider check is af provider doctor";

/// A machine of its own: a fresh home with every XDG directory below it.
struct Machine {
    root: tempfile::TempDir,
}

impl Machine {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("home")).unwrap();
        std::fs::create_dir_all(root.path().join("work")).unwrap();
        Machine { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn doctor(&self, dir: &Path, args: &[&str], path: Option<&std::ffi::OsStr>) -> Output {
        self.doctor_at(&self.path("home"), dir, args, path, &[])
    }

    /// `af doctor` with `home` as its home and `envs` set on top.
    fn doctor_at(
        &self,
        home: &Path,
        dir: &Path,
        args: &[&str],
        path: Option<&std::ffi::OsStr>,
        envs: &[(&str, &str)],
    ) -> Output {
        let home = home.to_path_buf();
        let mut command = crate::common::af();
        command
            .arg("doctor")
            .args(args)
            .current_dir(dir)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_BIN_HOME", home.join(".local/bin"))
            .env_remove("SHELL")
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .env_remove("AF_PROVIDERS_FILE")
            // Plenty of free disk, whatever the machine running the test has.
            .env("AF_TEST_FREE_BYTES", (1_u64 << 40).to_string());
        if let Some(path) = path {
            command.env("PATH", path);
        }
        command.envs(envs.iter().copied());
        command.output().unwrap()
    }

    /// Every file and directory under the machine's root, with each file's bytes.
    fn snapshot(&self) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        fn walk(dir: &Path, root: &Path, found: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                if path.is_dir() {
                    found.insert(relative, None);
                    walk(&path, root, found);
                } else {
                    found.insert(relative, Some(std::fs::read(&path).unwrap()));
                }
            }
        }
        let mut found = BTreeMap::new();
        walk(self.root.path(), self.root.path(), &mut found);
        found
    }
}

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The `--json` document, valid against the published schema, with the exit code it states.
fn document(output: &Output) -> serde_json::Value {
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{error}: {}\n{}", out(output), err(output)));
    schemas::valid(&schemas::validator("doctor-v1.json"), &value);
    assert_eq!(value["schema"], "af/doctor@1");
    assert_eq!(value["token_free"], true);
    assert_eq!(
        value["exit_code"].as_i64(),
        output.status.code().map(i64::from)
    );
    value
}

fn check<'a>(document: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    document["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == id)
        .unwrap_or_else(|| panic!("no {id} check: {document}"))
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

#[test]
fn a_clean_host_only_warns_says_what_to_do_and_writes_nothing() {
    let machine = Machine::new();
    let work = machine.path("work");
    let before = machine.snapshot();

    let text = machine.doctor(&work, &[], None);
    assert_eq!(
        text.status.code(),
        Some(0),
        "{}\n{}",
        out(&text),
        err(&text)
    );
    let shown = out(&text);
    assert_eq!(shown.lines().next(), Some(FIRST_LINE), "{shown}");
    assert!(
        shown.contains("warn  providers  no Provider registered"),
        "{shown}"
    );
    assert!(shown.contains("fix: af provider detect --apply"), "{shown}");
    assert!(shown.contains("fix: af self setup-shell --"), "{shown}");
    assert!(
        shown.lines().last().unwrap().starts_with("nothing failed"),
        "{shown}"
    );

    let json = machine.doctor(&work, &["--json"], None);
    assert_eq!(json.status.code(), Some(0), "{}", err(&json));
    let document = document(&json);
    let ids: Vec<&str> = document["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|check| check["id"].as_str().unwrap())
        .collect();
    // Outside a repository there is no project check.
    assert_eq!(
        ids,
        [
            "binary",
            "path",
            "shell",
            "update",
            "tools",
            "providers",
            "storage"
        ]
    );
    for check in document["checks"].as_array().unwrap() {
        assert_ne!(check["status"], "fail", "{check}");
    }
    // A test binary carries no receipt; the fix installs a release.
    assert_eq!(check(&document, "binary")["status"], "warn");
    assert!(
        check(&document, "binary")["detail"]
            .as_str()
            .unwrap()
            .contains("no install receipt"),
        "{document}"
    );
    // The default link's directory is not on this PATH.
    assert_eq!(check(&document, "path")["status"], "warn");
    assert!(
        check(&document, "shell")["fix"]
            .as_str()
            .unwrap()
            .contains("bash|zsh|fish|elvish|powershell"),
        "{document}"
    );
    assert!(
        check(&document, "update")["detail"]
            .as_str()
            .unwrap()
            .contains("AF_SELF_OFFLINE"),
        "{document}"
    );
    assert_eq!(check(&document, "storage")["status"], "ok", "{document}");

    // Neither run wrote a file or a directory: no state, no cache, no update check.
    assert_eq!(machine.snapshot(), before);
}

#[test]
fn a_missing_tool_fails_with_the_install_line_and_exit_1() {
    let machine = Machine::new();
    let work = machine.path("work");
    // A PATH holding nothing: no git, curl or minisign.
    let empty = machine.path("empty-path");
    std::fs::create_dir_all(&empty).unwrap();
    let before = machine.snapshot();

    let json = machine.doctor(&work, &["--json"], Some(empty.as_os_str()));
    assert_eq!(json.status.code(), Some(1), "{}", err(&json));
    let document = document(&json);
    let tools = check(&document, "tools");
    assert_eq!(tools["status"], "fail", "{document}");
    let detail = tools["detail"].as_str().unwrap();
    assert!(detail.contains("git is not installed"), "{detail}");
    assert!(detail.contains("minisign is not installed"), "{detail}");
    assert!(tools["fix"].as_str().unwrap().contains("git"), "{document}");

    let text = machine.doctor(&work, &[], Some(empty.as_os_str()));
    assert_eq!(text.status.code(), Some(1));
    assert!(out(&text).contains("fail  tools"), "{}", out(&text));
    assert!(
        out(&text)
            .lines()
            .last()
            .unwrap()
            .contains("1 check(s) failed")
    );
    assert_eq!(machine.snapshot(), before);
}

#[test]
fn a_repository_without_af_is_told_to_onboard() {
    let machine = Machine::new();
    let repo = machine.path("work/repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    let before = machine.snapshot();

    let json = machine.doctor(
        &machine.path("work"),
        &["--repo", repo.to_str().unwrap(), "--json"],
        None,
    );
    assert_eq!(json.status.code(), Some(0), "{}", err(&json));
    let document = document(&json);
    assert_eq!(document["checks"].as_array().unwrap().len(), 8);
    let project = check(&document, "project");
    assert_eq!(project["status"], "warn", "{document}");
    assert!(
        project["detail"].as_str().unwrap().contains("has no .af/"),
        "{document}"
    );
    assert!(
        project["fix"]
            .as_str()
            .unwrap()
            .starts_with("af onboard --repo "),
        "{document}"
    );
    // The same from inside the repository, without --repo.
    let inside = machine.doctor(&repo, &[], None);
    assert!(out(&inside).contains("warn  project"), "{}", out(&inside));

    // A --repo that is no repository, or nothing at all, is named as typed.
    let plain = machine.path("work");
    let outside = machine.doctor(&plain, &["--repo", ".."], None);
    let named = out(&outside);
    assert!(
        named.contains("warn  project    --repo .. is not inside a git repository"),
        "{named}"
    );
    let missing = machine.doctor(&plain, &["--repo", "absent", "--json"], None);
    assert_eq!(missing.status.code(), Some(1));
    let error: serde_json::Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert_eq!(error["schema"], "af/error@1");
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .starts_with("--repo absent: "),
        "{error}"
    );
    assert!(
        err(&missing).contains("--repo absent: "),
        "{}",
        err(&missing)
    );
    assert_eq!(machine.snapshot(), before);
}

/// C3 item 22: a release check whose last attempt failed, or one stalled on its lock, makes
/// the update row a warning naming the cause and the fix, as `af self status` does.
#[test]
fn the_update_row_warns_of_a_failed_or_stalled_release_check() {
    let machine = Machine::new();
    let work = machine.path("work");
    let cache = machine.path("home/.cache/af/self/latest.toml");
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    std::fs::write(
        &cache,
        "schema = \"af/self-check@3\"\nchecked_at_epoch = 1\nattempted_at_epoch = 86400\n\
         last_error = \"reading /releases: No such file or directory\"\nchannel = \"stable\"\n",
    )
    .unwrap();
    let failed = document(&machine.doctor(&work, &["--json"], None));
    let update = check(&failed, "update");
    assert_eq!(update["status"], "warn", "{update}");
    assert!(
        update["detail"].as_str().unwrap().starts_with(
            "last attempt 1970-01-02T00:00:00Z failed: reading /releases: No such file or \
             directory"
        ),
        "{update}"
    );
    assert_eq!(
        update["fix"],
        "af self status --refresh (runs the release check again now)"
    );

    // A check that has held the lock for two hours: the row says so, and how to clear it.
    let lock = cache.with_extension("lock");
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 2 * 3600;
    std::fs::write(&lock, format!("started_at_epoch = {since}\npid = 1\n")).unwrap();
    let held = std::fs::File::open(&lock).unwrap();
    rustix::fs::flock(&held, rustix::fs::FlockOperation::LockExclusive).unwrap();
    let stalled = document(&machine.doctor(&work, &["--json"], None));
    let update = check(&stalled, "update");
    assert_eq!(update["status"], "warn", "{update}");
    assert!(
        update["detail"]
            .as_str()
            .unwrap()
            .starts_with("a release check has been running since "),
        "{update}"
    );
    let fix = update["fix"].as_str().unwrap();
    assert!(
        fix.starts_with("rm ") && fix.ends_with("af/self/latest.lock"),
        "{update}"
    );
    drop(held);
}

/// GA final fix item 7: the human rows fit 100 columns, or a narrower `COLUMNS` (80 here), with
/// a long install path: a detail or path continues on indented lines and is never cut short, and
/// each fix command stays whole on its line or is broken between its arguments.
#[test]
fn rows_fit_100_and_80_columns_with_a_long_install_path() {
    let machine = Machine::new();
    let work = machine.path("work");
    let home = machine.path(
        "home/a-long-gate-like-directory-name-for-path-length-tests/and-one-more-level-of-it",
    );
    std::fs::create_dir_all(&home).unwrap();
    let bin = home.join(".local/bin");
    let squeeze = |text: &str| text.split_whitespace().collect::<String>();
    for columns in [100, 80] {
        let wide = columns.to_string();
        let output = machine.doctor_at(&home, &work, &[], None, &[("COLUMNS", &wide)]);
        assert_eq!(output.status.code(), Some(0), "{}", err(&output));
        let text = out(&output);
        for line in text.lines() {
            assert!(
                line.chars().count() <= columns,
                "{columns}: `{line}`\n{text}"
            );
        }
        for row in ["warn  binary", "warn  path", "warn  shell"] {
            assert!(text.contains(row), "{columns}: {row}\n{text}");
        }
        // The paths are all there, character for character, across their continuation lines.
        let flat = squeeze(&text);
        assert!(
            flat.contains(&format!("{}isnotonPATH", bin.display())),
            "{columns}\n{text}"
        );
        assert!(
            flat.contains(&format!("add{}toPATH", bin.display())),
            "{columns}\n{text}"
        );
        // The receipt path is printed canonical; its tail below the machine root is the same.
        let version = env!("CARGO_PKG_VERSION");
        assert!(
            flat.contains(&format!(
                "/home/a-long-gate-like-directory-name-for-path-length-tests/\
                 and-one-more-level-of-it/.local/share/af/versions/{version}/receipt.toml(asource"
            )),
            "{columns}\n{text}"
        );
        // Each af fix command is whole on its line, or broken between its arguments.
        assert!(
            text.lines()
                .any(|line| line.ends_with(&format!("fix: af self install {version}"))),
            "{columns}\n{text}"
        );
        let setup: Vec<String> = text
            .lines()
            .skip_while(|line| !line.contains("fix: af self setup-shell"))
            .take_while(|line| !line.starts_with("warn ") && !line.starts_with("ok "))
            .flat_map(|line| line.split_whitespace().map(str::to_string))
            .filter(|word| word != "\\" && word != "fix:")
            .map(|word| word.trim_matches('\'').to_string())
            .collect();
        assert_eq!(
            setup,
            [
                "af",
                "self",
                "setup-shell",
                "--shell",
                "bash|zsh|fish|elvish|powershell",
                "--write"
            ],
            "{columns}\n{text}"
        );

        // With zsh the shell row names the long completion and man page paths, and the PATH fix
        // is one shell word, printed whole on its own line even where it is wider than one.
        let zsh = machine.doctor_at(
            &home,
            &work,
            &[],
            None,
            &[("COLUMNS", &wide), ("SHELL", "/bin/zsh")],
        );
        let text = out(&zsh);
        let export = format!("export PATH=\"{}:$PATH\"", bin.display());
        assert!(
            text.lines()
                .any(
                    |line| line.trim_start().strip_prefix("fix: ") == Some(export.as_str())
                        || line.trim_start() == export
                ),
            "{columns}: `{export}` whole\n{text}"
        );
        for line in text.lines().filter(|line| !line.contains(&export)) {
            assert!(
                line.chars().count() <= columns,
                "{columns}: `{line}`\n{text}"
            );
        }
        assert!(
            squeeze(&text).contains(&squeeze(&format!(
                "no zsh completions at {}",
                home.join(".local/share/zsh/site-functions").display()
            ))) || squeeze(&text).contains("nozshcompletionsat"),
            "{columns}\n{text}"
        );
        assert!(
            text.contains("# in ~/.zshrc, then open a new shell"),
            "{columns}\n{text}"
        );
    }
}
