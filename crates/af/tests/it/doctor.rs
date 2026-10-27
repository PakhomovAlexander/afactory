//! `af doctor`: a token-free check of the machine and the repository it runs in (ADR-0210). Each
//! case runs in a private home that a snapshot proves the command leaves exactly as it found it,
//! and every `--json` document is checked against `schemas/doctor-v3.json`.

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
    schemas::valid(&schemas::validator("doctor-v3.json"), &value);
    assert_eq!(value["schema"], "af/doctor@3");
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
            "harness",
            "sandbox",
            "network",
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
    // $SHELL is unset: the fix names the SHELL placeholder, the hint its choices.
    assert_eq!(
        check(&document, "shell")["fix"],
        "af self setup-shell --shell SHELL --write",
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
    // No package manager on this PATH either: no command installs them, the hint says how.
    assert_eq!(tools["fix"], serde_json::Value::Null, "{document}");
    assert!(
        tools["hint"]
            .as_str()
            .unwrap()
            .contains("install git with your package manager"),
        "{document}"
    );

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
    assert_eq!(document["checks"].as_array().unwrap().len(), 11);
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
    assert_eq!(update["fix"], "af self status --refresh");
    assert_eq!(update["hint"], "it runs the release check again now");

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
    // The zsh PATH fix is one line, printed whole even where it is wider than one.
    let export = format!("export PATH='{}':\"$PATH\"", bin.display());
    for columns in [100, 80] {
        let wide = columns.to_string();
        let output = machine.doctor_at(&home, &work, &[], None, &[("COLUMNS", &wide)]);
        assert_eq!(output.status.code(), Some(0), "{}", err(&output));
        let text = out(&output);
        for line in text.lines().filter(|line| !line.contains(&export)) {
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
        // Without $SHELL no line is written for an unknown shell: the hint names the directory.
        assert!(
            flat.contains(&squeeze(&format!(
                "hint: put {} first on PATH",
                bin.display()
            ))),
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
            .take_while(|line| {
                !line.starts_with("warn ")
                    && !line.starts_with("ok ")
                    && !line.trim_start().starts_with("hint: ")
            })
            .flat_map(|line| line.split_whitespace().map(str::to_string))
            .filter(|word| word != "\\" && word != "fix:")
            .map(|word| word.trim_matches('\'').to_string())
            .collect();
        assert_eq!(
            setup,
            ["af", "self", "setup-shell", "--shell", "SHELL", "--write"],
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
            squeeze(&text).contains("hint:addthatlineto~/.zshrc,thenopenanewshell"),
            "{columns}\n{text}"
        );
    }
}

/// The `--json` document of `af doctor` under `envs`, with `$SHELL` set when `shell` names one.
fn shell_document(machine: &Machine, shell: Option<&str>) -> (serde_json::Value, String) {
    let work = machine.path("work");
    let envs: Vec<(&str, &str)> = shell.map(|shell| ("SHELL", shell)).into_iter().collect();
    let home = machine.path("home");
    let json = machine.doctor_at(&home, &work, &["--json"], None, &envs);
    let mut text_envs = envs.clone();
    text_envs.push(("COLUMNS", "100"));
    let text = machine.doctor_at(&home, &work, &[], None, &text_envs);
    (document(&json), out(&text))
}

/// `af ARGS --help` exits 0: clap accepts every argument of `ARGS` as typed.
fn parses(args: &[String]) -> Result<(), String> {
    let output = crate::common::af()
        .args(args)
        .arg("--help")
        .env("AF_SELF_OFFLINE", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("af {args:?} --help: {}", err(&output)))
    }
}

/// GA second fix item 3: the shell row's fix is one command a shell runs. With $SHELL naming a
/// supported shell it names that shell; without one it names the SHELL placeholder, and the hint
/// lists the choices, in the JSON document and in the text.
#[test]
fn the_shell_fix_is_one_command_for_the_users_shell() {
    let machine = Machine::new();
    for (shell, name) in [("/bin/zsh", "zsh"), ("/usr/bin/fish", "fish")] {
        let (document, text) = shell_document(&machine, Some(shell));
        let row = check(&document, "shell");
        assert_eq!(row["status"], "warn", "{document}");
        let fix = format!("af self setup-shell --shell {name} --write");
        assert_eq!(row["fix"], fix.as_str(), "{document}");
        assert_eq!(row["hint"], serde_json::Value::Null, "{document}");
        assert!(
            text.lines()
                .any(|line| line.trim_start() == format!("fix: {fix}")),
            "{text}"
        );
        // The fix parses with the CLI as printed.
        let words = shell_words::split(row["fix"].as_str().unwrap()).unwrap();
        assert_eq!(words[0], "af");
        parses(&words[1..]).unwrap();
    }

    let (document, text) = shell_document(&machine, None);
    let row = check(&document, "shell");
    assert_eq!(row["fix"], "af self setup-shell --shell SHELL --write");
    let choices = "SHELL: the shell you use, one of bash, zsh, fish, elvish, powershell";
    assert_eq!(row["hint"], choices, "{document}");
    assert!(
        text.contains(&format!(
            "fix: af self setup-shell --shell SHELL --write\n                 hint: {choices}\n"
        )),
        "{text}"
    );
    // Each choice the hint lists makes the fix a command the CLI accepts.
    for choice in ["bash", "zsh", "fish", "elvish", "powershell"] {
        let words =
            shell_words::split(&row["fix"].as_str().unwrap().replace("SHELL", choice)).unwrap();
        parses(&words[1..]).unwrap();
    }
}

/// A program on this machine's PATH, as the test process sees it.
fn on_path(program: &str) -> PathBuf {
    let path = std::env::var_os("PATH").unwrap();
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("no {program} on PATH"))
}

/// Every fix in `document` is a command a shell runs as given: it splits as shell words, each
/// command of it (between `&&`, `|` or `;`) starts with a program, an `af` command parses with
/// the CLI once each upper-case placeholder is replaced by a value its hint documents, and a
/// check without a fix says what to do in its hint.
fn assert_fixes_are_commands(document: &serde_json::Value) {
    const PROGRAMS: &[&str] = &[
        "af",
        "apk",
        "brew",
        "cargo",
        "curl",
        "export",
        "fish_add_path",
        "gh",
        "rm",
        "sh",
        "sudo",
        "xcode-select",
    ];
    for check in document["checks"].as_array().unwrap() {
        if check["status"] == "ok" {
            continue;
        }
        let hint = check["hint"].as_str().unwrap_or_default();
        let Some(fix) = check["fix"].as_str() else {
            assert!(
                !hint.is_empty(),
                "a problem without a fix says what to do: {check}"
            );
            continue;
        };
        assert!(!fix.contains('#') && !fix.contains('('), "prose in {check}");
        let words = shell_words::split(fix).unwrap_or_else(|e| panic!("{e}: {check}"));
        for command in words.split(|word| ["&&", "|", ";"].contains(&word.as_str())) {
            assert!(
                PROGRAMS.contains(&command[0].as_str()),
                "`{}` is no command: {check}",
                command[0]
            );
            if command[0] != "af" {
                continue;
            }
            let args: Vec<String> = command[1..]
                .iter()
                .map(|word| {
                    let placeholder = !word.is_empty()
                        && word.chars().all(|c| c.is_ascii_uppercase() || c == '_');
                    if !placeholder {
                        return word.clone();
                    }
                    assert!(
                        hint.contains(&format!("{word}: ")),
                        "the placeholder {word} is documented in the hint: {check}"
                    );
                    match word.as_str() {
                        "SHELL" => "zsh".to_owned(),
                        _ => "/tmp".to_owned(),
                    }
                })
                .collect();
            parses(&args).unwrap_or_else(|e| panic!("{e}\n{check}"));
        }
    }
}

/// GA second fix item 4: a machine where every check is a problem — no install receipt, the
/// default link off PATH, no $SHELL, an unreadable config, missing tools, an unreadable Provider
/// registry and a repository whose `.af/` does not validate — and every fix in `--json` is a
/// command a shell runs as given, every `af` one parsing with the CLI; the hint carries the rest.
/// With no package manager on PATH the tools row has no fix, and its hint says what to do.
#[test]
fn every_fix_of_a_machine_where_every_check_fails_is_a_command() {
    let machine = Machine::new();
    let repo = machine.path("work/repo");
    std::fs::create_dir_all(repo.join(".af")).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".af/af.toml"), "version = [\n").unwrap();
    let config = machine.path("home/.config/af/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "this is = not [valid\n").unwrap();
    let registry = machine.path("providers.toml");
    std::fs::write(&registry, "garbage = [\n").unwrap();
    // A PATH with git and a package manager, and nothing else af looks for.
    let bin = machine.path("tools-path");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(on_path("git"), bin.join("git")).unwrap();
    std::fs::write(bin.join("brew"), "#!/bin/sh\nexit 0\n").unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(bin.join("brew"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let registry_env = registry.to_str().unwrap().to_owned();
    let envs = [("AF_PROVIDERS_FILE", registry_env.as_str())];
    let home = machine.path("home");
    let empty = machine.path("empty-path");
    std::fs::create_dir_all(&empty).unwrap();
    let before = machine.snapshot();

    let output = machine.doctor_at(&home, &repo, &["--json"], Some(bin.as_os_str()), &envs);
    assert_eq!(output.status.code(), Some(1), "{}", err(&output));
    let failing = document(&output);
    let statuses: Vec<(&str, &str)> = failing["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|check| {
            (
                check["id"].as_str().unwrap(),
                check["status"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(statuses.len(), 11, "{failing}");
    assert!(
        statuses
            .iter()
            .filter(|(id, _)| !["harness", "sandbox", "network"].contains(id))
            .all(|(_, status)| *status != "ok"),
        "every check is a problem: {statuses:?}"
    );
    assert_eq!(
        check(&failing, "tools")["fix"],
        "brew install curl && brew install minisign && brew install gh",
        "{failing}"
    );
    // GA third fix item 3: `af onboard` would stop on the same parse error, so the project check
    // has no fix; its hint names the file, the line and the command that checks again.
    let project = check(&failing, "project");
    assert_eq!(project["status"], "fail", "{failing}");
    assert_eq!(project["fix"], serde_json::Value::Null, "{failing}");
    let hint = project["hint"].as_str().unwrap();
    let quoted = shell_words::quote(&canonical(&repo)).into_owned();
    assert!(
        hint.starts_with(&format!(
            "correct {}/.af/af.toml line 1 by hand (",
            canonical(&repo)
        )),
        "{hint}"
    );
    assert!(
        hint.ends_with(&format!("then recheck: af doctor --repo {quoted}")),
        "{hint}"
    );
    // The unreadable config and registry: no command clears them, and every fix that would run
    // `af self` waits for the config.
    for id in ["update", "storage", "providers", "binary"] {
        assert_eq!(
            check(&failing, id)["fix"],
            serde_json::Value::Null,
            "{id}: {failing}"
        );
    }
    assert!(
        check(&failing, "storage")["hint"]
            .as_str()
            .unwrap()
            .contains("/home/.config/af/config.toml line 1 by hand"),
        "{failing}"
    );
    assert!(
        check(&failing, "providers")["hint"]
            .as_str()
            .unwrap()
            .starts_with(&format!("correct {} line 1 by hand", registry.display())),
        "{failing}"
    );
    assert!(
        check(&failing, "binary")["hint"]
            .as_str()
            .unwrap()
            .contains("which stops this fix; then run: af self install "),
        "{failing}"
    );
    assert_fixes_are_commands(&failing);

    // Without a package manager no command installs a tool: the hint says what to do.
    let work = machine.path("work");
    let output = machine.doctor_at(
        &home,
        &work,
        &["--repo", "..", "--json"],
        Some(empty.as_os_str()),
        &envs,
    );
    let bare = document(&output);
    let tools = check(&bare, "tools");
    assert_eq!(tools["fix"], serde_json::Value::Null, "{bare}");
    assert!(
        tools["hint"]
            .as_str()
            .unwrap()
            .contains("install git with your package manager"),
        "{bare}"
    );
    assert_eq!(check(&bare, "project")["fix"], "af doctor --repo DIR");
    assert_fixes_are_commands(&bare);
    assert_eq!(machine.snapshot(), before);

    // Each fix still offered runs, in this fixture, without the error its check reported or any
    // other the unreadable files hold.
    let broken = [repo.join(".af/af.toml"), config, registry];
    let fixed = run_fixes(&machine, &failing, &repo, &bin, &envs, &broken);
    assert!(fixed >= 2, "the shell and tools fixes ran: {failing}");
}

/// Run every fix `document` still offers, in `dir` on `machine` with `path` and `envs`, and
/// assert that none fails with the error its check reported or names one of `broken`, the
/// fixture's unreadable files: a fix is offered only when running it can clear its check. An
/// `af` command runs through the private-home helper; a `brew` or `apt-get` line runs only when
/// `path` holds the fixture's own fake one. Returns how many commands ran.
fn run_fixes(
    machine: &Machine,
    document: &serde_json::Value,
    dir: &Path,
    path: &Path,
    envs: &[(&str, &str)],
    broken: &[PathBuf],
) -> usize {
    let home = machine.path("home");
    let mut ran = 0;
    for check in document["checks"].as_array().unwrap() {
        let Some(fix) = check["fix"].as_str() else {
            continue;
        };
        let reported = check["detail"].as_str().unwrap();
        let words = shell_words::split(fix).unwrap();
        for command in words.split(|word| word == "&&") {
            let command: Vec<String> = command
                .iter()
                .map(|word| {
                    if word == "SHELL" {
                        "zsh".into()
                    } else {
                        word.clone()
                    }
                })
                .collect();
            let output = if command[0] == "af" {
                crate::common::af()
                    .args(&command[1..])
                    .current_dir(dir)
                    .env("HOME", &home)
                    .env("XDG_CONFIG_HOME", home.join(".config"))
                    .env("XDG_STATE_HOME", home.join(".local/state"))
                    .env("XDG_CACHE_HOME", home.join(".cache"))
                    .env("XDG_DATA_HOME", home.join(".local/share"))
                    .env("XDG_BIN_HOME", home.join(".local/bin"))
                    .env("PATH", path)
                    .env_remove("SHELL")
                    .env_remove("CLAUDE_CONFIG_DIR")
                    .env_remove("CODEX_HOME")
                    .envs(envs.iter().copied())
                    .stdin(std::process::Stdio::null())
                    .output()
                    .unwrap()
            } else if path.join(&command[0]).is_file() {
                Command::new(path.join(&command[0]))
                    .args(&command[1..])
                    .env("PATH", path)
                    .output()
                    .unwrap()
            } else {
                continue;
            };
            ran += 1;
            let said = format!("{}{}", out(&output), err(&output));
            if !output.status.success() {
                let core = reported.rsplit(": ").next().unwrap_or(reported);
                assert!(
                    !said.contains(core),
                    "`{fix}` fails as {check} did:\n{said}"
                );
            }
            for file in broken {
                for spelled in [file.display().to_string(), canonical_or(file)] {
                    assert!(
                        !said.contains(&spelled),
                        "`{fix}` stops on {spelled}:\n{said}"
                    );
                }
            }
        }
    }
    ran
}

/// `path` canonical when it exists, else as given.
fn canonical_or(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

/// A repository whose `.af/` validates, onboarded in `machine`'s private home, with a Worker
/// that needs a claude Provider.
fn onboarded(machine: &Machine) -> PathBuf {
    let repo = machine.path("work/onboarded");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    let home = machine.path("home");
    let output = crate::common::af()
        .args([
            "onboard",
            "--apply",
            "--gate",
            "check=git --version",
            "--runner",
            "claude",
        ])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", err(&output));
    repo
}

/// GA third fix item 3, one check kind at a time: a fix is offered only when running it can
/// clear its check, never when the error the check reported would stop it. Each case breaks one
/// file, reads the check's hint (the file, the parse error's line, the command that checks
/// again), proves the command it no longer offers stops on that error, and runs every fix it
/// still offers without it.
#[test]
fn no_fix_is_offered_that_the_reported_error_would_stop() {
    let machine = Machine::new();
    let repo = onboarded(&machine);
    let home = machine.path("home");
    // git for the repository, a fake brew so no tools fix installs anything.
    let bin = machine.path("tools-path");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(on_path("git"), bin.join("git")).unwrap();
    std::fs::write(bin.join("brew"), "#!/bin/sh\nexit 0\n").unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(bin.join("brew"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let registry = machine.path("providers.toml");
    let registry_env = registry.to_str().unwrap().to_owned();
    let envs = [("AF_PROVIDERS_FILE", registry_env.as_str())];
    let doctor =
        || document(&machine.doctor_at(&home, &repo, &["--json"], Some(bin.as_os_str()), &envs));
    let quoted = shell_words::quote(&canonical(&repo)).into_owned();
    let run = |args: &[&str]| -> String {
        let output = crate::common::af()
            .args(args)
            .current_dir(&repo)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .envs(envs.iter().copied())
            .output()
            .unwrap();
        assert!(!output.status.success(), "af {args:?} ran");
        err(&output)
    };

    // The project: each `.af/` file `af onboard` reads, broken in turn.
    for file in [".af/af.toml", ".af/af.lock", ".af/pipelines/review.toml"] {
        let path = repo.join(file);
        let kept = std::fs::read(&path).unwrap();
        std::fs::write(&path, [kept.as_slice(), b"\nbroken = [\n"].concat()).unwrap();
        let line = String::from_utf8_lossy(&kept).lines().count() + 2;
        let document = doctor();
        let project = check(&document, "project");
        assert_eq!(project["status"], "fail", "{file}: {document}");
        assert_eq!(
            project["fix"],
            serde_json::Value::Null,
            "{file}: {document}"
        );
        let hint = project["hint"].as_str().unwrap();
        assert!(
            hint.starts_with(&format!(
                "correct {}/{file} line {line} by hand (",
                canonical(&repo)
            )),
            "{file}: {hint}"
        );
        assert!(
            hint.ends_with(&format!("then recheck: af doctor --repo {quoted}")),
            "{hint}"
        );
        // The command the check used to offer stops on that very error.
        let said = run(&["onboard", "--repo", &canonical(&repo)]);
        assert!(said.contains("TOML parse error"), "{file}: {said}");
        run_fixes(&machine, &document, &repo, &bin, &envs, &[path.clone()]);
        std::fs::write(&path, kept).unwrap();
    }

    // The machine configuration: the update and storage checks name it; the binary's
    // `af self install` waits for it.
    let config = home.join(".config/af/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "[storage]\nmax_bytes = [\n").unwrap();
    let document = doctor();
    for id in ["update", "storage", "binary"] {
        let row = check(&document, id);
        assert_eq!(row["fix"], serde_json::Value::Null, "{id}: {document}");
        let hint = row["hint"].as_str().unwrap();
        assert!(
            hint.contains(&format!("{} line 2 by hand (", canonical(&config))),
            "{hint}"
        );
    }
    assert!(
        check(&document, "storage")["hint"]
            .as_str()
            .unwrap()
            .ends_with("then recheck: af doctor"),
        "{document}"
    );
    for args in [
        &["config", "show", "--origin"][..],
        &["self", "install", "0.1.0"],
    ] {
        let said = run(args);
        assert!(said.contains(&canonical(&config)), "af {args:?}: {said}");
    }
    run_fixes(&machine, &document, &repo, &bin, &envs, &[config.clone()]);
    std::fs::remove_file(&config).unwrap();

    // The Provider registry: the providers check names it; the project's `af provider detect
    // --apply`, which the reviewer's missing Provider would offer, waits for it.
    std::fs::write(&registry, "version = 1\n[[providers]\n").unwrap();
    let document = doctor();
    let providers = check(&document, "providers");
    assert_eq!(providers["fix"], serde_json::Value::Null, "{document}");
    assert!(
        providers["hint"]
            .as_str()
            .unwrap()
            .starts_with(&format!("correct {} line 2 by hand (", registry.display())),
        "{document}"
    );
    let project = check(&document, "project");
    assert_eq!(project["fix"], serde_json::Value::Null, "{document}");
    assert!(
        project["hint"]
            .as_str()
            .unwrap()
            .contains("which stops this fix; then run: af provider detect --apply"),
        "{document}"
    );
    let said = run(&["provider", "detect", "--apply"]);
    assert!(said.contains(&registry_env), "{said}");
    run_fixes(&machine, &document, &repo, &bin, &envs, &[registry.clone()]);

    // `af doctor --help`, the command's contract, states the rule.
    let help = crate::common::af()
        .args(["doctor", "--help"])
        .output()
        .unwrap();
    let flat = out(&help).split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains("A fix is offered only when running it can clear the check"),
        "{flat}"
    );

    // With the registry gone the same project check offers the command again: it can clear it.
    std::fs::remove_file(&registry).unwrap();
    let document = doctor();
    assert_eq!(
        check(&document, "project")["fix"],
        "af provider detect --apply",
        "{document}"
    );
    run_fixes(&machine, &document, &repo, &bin, &envs, &[]);
}

/// `path` as git names its top level: canonical, without a trailing `/`.
fn canonical(path: &Path) -> String {
    std::fs::canonicalize(path).unwrap().display().to_string()
}
