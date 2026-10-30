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
    // #331 D5: no login is found to register, so `af provider detect --apply` could not clear
    // the check: no fix, and the hint names the setup a person runs at a private terminal.
    assert!(
        !shown.contains("fix: af provider detect --apply"),
        "{shown}"
    );
    assert!(
        shown.contains("hint: no login was found to register; log in and register one"),
        "{shown}"
    );
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
    let providers = check(&document, "providers");
    assert_eq!(providers["fix"], serde_json::Value::Null, "{document}");
    assert!(
        providers["hint"].as_str().unwrap().ends_with(
            "at a private terminal: af provider setup claude-main --kind claude --login"
        ),
        "{document}"
    );
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
    // #331 D4: `af onboard` only previews and writes no `.af/`, so it is no fix: the hint gives
    // the preview and then the `--apply` that writes it.
    assert_eq!(project["fix"], serde_json::Value::Null, "{document}");
    let onboard = format!(
        "af onboard --repo {}",
        shell_words::quote(&canonical(&repo))
    );
    assert_eq!(
        project["hint"],
        format!(
            "preview the .af/ it would write, token-free, writing nothing: {onboard}; then \
             write it, to review and commit: {onboard} --apply"
        ),
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
    assert_eq!(error["schema"], "af/error@2");
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
        "schema = \"af/self-check@4\"\nchecked_at_epoch = 1\nattempted_at_epoch = 86400\n\
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
/// each fix command stays whole on its line or is broken between its arguments. The PATH is the
/// test's own, so both forms of the binary's install fix are proven whatever the host's PATH
/// holds (#331, Linux CI): with an `af` on PATH it runs that `af`; with none, this binary by its
/// path, its printed lines the command the JSON carries.
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
    // A PATH with git and no `af`, and one with an `af` link to this binary before it.
    let tools = machine.path("tools-path");
    std::fs::create_dir_all(&tools).unwrap();
    std::os::unix::fs::symlink(on_path("git"), tools.join("git")).unwrap();
    let linked = machine.path("af-path");
    std::fs::create_dir_all(&linked).unwrap();
    std::os::unix::fs::symlink(crate::common::af_binary(), linked.join("af")).unwrap();
    let with_af = std::env::join_paths([&linked, &tools]).unwrap();
    let version = env!("CARGO_PKG_VERSION");
    for (columns, af_on_path) in [(100, false), (100, true), (80, false), (80, true)] {
        let path = if af_on_path {
            with_af.as_os_str()
        } else {
            tools.as_os_str()
        };
        let wide = columns.to_string();
        let envs = [("COLUMNS", wide.as_str())];
        let output = machine.doctor_at(&home, &work, &[], Some(path), &envs);
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
        // Without $SHELL no line is written for an unknown shell: the hint says to install first,
        // which links the default, and names the directory (#331 D7).
        assert!(
            flat.contains(&squeeze("first on PATH in your shell's startup file"))
                && flat.contains(&squeeze(&format!("put {} first on PATH", bin.display()))),
            "{columns}\n{text}"
        );
        assert!(
            flat.contains("hint:installfirst,whichlinks"),
            "{columns}\n{text}"
        );
        // The receipt path is printed canonical; its tail below the machine root is the same.
        assert!(
            flat.contains(&format!(
                "/home/a-long-gate-like-directory-name-for-path-length-tests/\
                 and-one-more-level-of-it/.local/share/af/versions/{version}/receipt.toml(asource"
            )),
            "{columns}\n{text}"
        );
        // Each af fix command is whole on its line, or broken between its arguments: the
        // binary's install runs the `af` on PATH, or this binary by its path when none is.
        if af_on_path {
            assert!(
                text.lines()
                    .any(|line| line.ends_with(&format!("fix: af self install {version}"))),
                "{columns}\n{text}"
            );
        } else {
            let json = document(&machine.doctor_at(&home, &work, &["--json"], Some(path), &envs));
            let fix = check(&json, "binary")["fix"].as_str().unwrap().to_owned();
            let program = crate::common::af_binary();
            let program = shell_words::quote(program.to_str().unwrap()).into_owned();
            assert_eq!(fix, format!("{program} self install {version}"), "{json}");
            let printed = printed_fix(&text, "binary");
            assert_eq!(
                shell_words::split(&printed.replace("\\\n", "")).unwrap(),
                shell_words::split(&fix).unwrap(),
                "{columns}\n{text}"
            );
        }
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

        // With zsh the shell row names the long completion and man page paths, and the PATH
        // fix, which installs first because the default link is absent (#331 D7), is held to
        // the width, its directory cut where the shell joins it again (D6): the printed lines
        // are the command the JSON carries.
        let envs = [("COLUMNS", wide.as_str()), ("SHELL", "/bin/zsh")];
        let zsh = machine.doctor_at(&home, &work, &[], Some(path), &envs);
        let text = out(&zsh);
        for line in text.lines() {
            assert!(
                line.chars().count() <= columns,
                "{columns}: `{line}`\n{text}"
            );
        }
        let json = document(&machine.doctor_at(&home, &work, &["--json"], Some(path), &envs));
        let fix = check(&json, "path")["fix"].as_str().unwrap().to_owned();
        assert!(
            fix.ends_with(&format!(
                " self install {version} && export PATH='{}':\"$PATH\"",
                bin.display()
            )),
            "{fix}"
        );
        let printed = printed_fix(&text, "path");
        assert_eq!(
            shell_words::split(&printed.replace("\\\n", "")).unwrap(),
            shell_words::split(&fix).unwrap(),
            "{columns}\n{text}"
        );
        assert!(
            squeeze(&text).contains(&squeeze(&format!(
                "no zsh completions at {}",
                home.join(".local/share/zsh/site-functions").display()
            ))) || squeeze(&text).contains("nozshcompletionsat"),
            "{columns}\n{text}"
        );
        assert!(
            squeeze(&text).contains("addthePATHlineto~/.zshrc,thenopenanewshell"),
            "{columns}\n{text}"
        );
    }
}

/// The fix of row `id` as the text form printed it: from past `fix: ` to the line before its
/// hint or the next row, its lines as printed.
fn printed_fix(text: &str, id: &str) -> String {
    let mut lines = text
        .lines()
        .skip_while(|line| !line.starts_with(&format!("warn  {id} ")))
        .skip_while(|line| !line.contains(" fix: "));
    let first = lines
        .next()
        .unwrap_or_else(|| panic!("no {id} fix\n{text}"));
    let mut printed = first[first.find(" fix: ").unwrap() + 6..].to_owned();
    for line in lines.take_while(|line| {
        !line.trim_start().starts_with("hint: ")
            && !line.starts_with("warn ")
            && !line.starts_with("ok ")
            && !line.starts_with("fail ")
            && !line.starts_with("nothing failed")
    }) {
        printed.push('\n');
        printed.push_str(line);
    }
    printed
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
    // With no `af` on PATH, the install runs this binary by its path (#331 D7).
    let binary = check(&failing, "binary")["hint"].as_str().unwrap();
    assert!(
        binary.contains(&format!(
            "which stops this fix; then run: {} self install ",
            crate::common::af_binary().display()
        )),
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
            // An `af` named by its path runs as the binary under test, in the private home.
            let output = if command[0] == "af" || command[0].ends_with("/af") {
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
    onboarded_with(machine, "claude")
}

/// [`onboarded`] with Workers that need a `runner` Provider.
fn onboarded_with(machine: &Machine, runner: &str) -> PathBuf {
    let repo = machine.path(&format!("work/onboarded-{runner}"));
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
            runner,
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
    // --apply`, which the reviewer's missing Provider would offer for the login found here,
    // waits for it.
    let found = home.join(".claude");
    std::fs::create_dir_all(&found).unwrap();
    std::fs::set_permissions(&found, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(found.join(".credentials.json"), "{}").unwrap();
    // A login its CLI on PATH reports, which `af provider detect --apply` would register.
    std::fs::write(found.join("logged-in"), "").unwrap();
    std::fs::write(
        bin.join("claude"),
        crate::provider_cli::with_help("claude", CLAUDE),
    )
    .unwrap();
    std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o755)).unwrap();
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

    // With the registry gone and no login found, `af provider detect --apply` would register
    // nothing, so it is no fix (#331 D5): the hint names the setup a person runs.
    std::fs::remove_file(&registry).unwrap();
    std::fs::remove_dir_all(&found).unwrap();
    let document = doctor();
    let project = check(&document, "project");
    assert_eq!(project["fix"], serde_json::Value::Null, "{document}");
    assert!(
        project["hint"].as_str().unwrap().contains(
            "no claude login was found to register; log in and register one yourself at a \
             private terminal: af provider setup claude-main --kind claude --login"
        ),
        "{document}"
    );
    run_fixes(&machine, &document, &repo, &bin, &envs, &[]);

    // An empty auth directory holds no login: `af provider detect --apply` probes none there
    // and registers nothing, so it is no fix either (#331 D5); the hint says why, naming the
    // directory `~/.claude` below HOME whether or not HOME is reached through a symbolic link
    // (macOS's `/var`).
    std::fs::create_dir_all(&found).unwrap();
    std::fs::set_permissions(&found, std::fs::Permissions::from_mode(0o700)).unwrap();
    let document = doctor();
    for (id, login) in [("project", "claude login"), ("providers", "login")] {
        let row = check(&document, id);
        assert_eq!(row["fix"], serde_json::Value::Null, "{id}: {document}");
        assert!(
            row["hint"].as_str().unwrap().contains(&format!(
                "no {login} was found to register: no CLI on PATH reports a login in \
                 ~/.claude; log in and register one yourself at a private terminal"
            )),
            "{id}: {document}"
        );
    }
    let detected = crate::common::af()
        .args(["provider", "detect", "--apply", "--json"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .envs(envs.iter().copied())
        .output()
        .unwrap();
    assert!(!registry.exists(), "{}", err(&detected));

    // A marker alone is no login: with the Claude CLI on PATH reporting the context logged
    // out, and then with no Claude CLI on PATH, `af provider detect --apply` registers nothing,
    // so it is no fix (#331 UX close-out).
    std::fs::write(found.join("settings.json"), "{}").unwrap();
    let no_fix = |document: &serde_json::Value| {
        for id in ["project", "providers"] {
            assert_eq!(
                check(document, id)["fix"],
                serde_json::Value::Null,
                "{id}: {document}"
            );
        }
    };
    no_fix(&doctor());
    std::fs::rename(bin.join("claude"), machine.path("claude-off-path")).unwrap();
    no_fix(&doctor());
    std::fs::rename(machine.path("claude-off-path"), bin.join("claude")).unwrap();
    // Logged in, its CLI on PATH: the command registers it, and the check clears.
    std::fs::write(found.join("logged-in"), "").unwrap();
    let document = doctor();
    for id in ["project", "providers"] {
        assert_eq!(
            check(&document, id)["fix"],
            "af provider detect --apply",
            "{id}: {document}"
        );
    }
    let detected = crate::common::af()
        .args(["provider", "detect", "--apply", "--json"])
        .current_dir(&repo)
        .env("HOME", &home)
        .env("PATH", &bin)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .envs(envs.iter().copied())
        .output()
        .unwrap();
    assert!(registry.exists(), "{}", err(&detected));
    let document = doctor();
    for id in ["project", "providers"] {
        assert_eq!(
            check(&document, id)["fix"],
            serde_json::Value::Null,
            "{id}: {document}"
        );
    }
    std::fs::remove_dir_all(&found).unwrap();
}

/// #331 final review: a project whose Workers need a codex Provider, on a machine whose one
/// login is a claude login its CLI reports authenticated, which `af provider detect --apply`
/// registers: the project hint names the missing codex login and never denies the claude one.
#[test]
fn a_missing_codex_login_never_denies_an_available_claude_login() {
    let machine = Machine::new();
    let repo = onboarded_with(&machine, "codex");
    let home = machine.path("home");
    let bin = machine.path("tools-path");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(on_path("git"), bin.join("git")).unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    let found = home.join(".claude");
    std::fs::create_dir_all(&found).unwrap();
    std::fs::set_permissions(&found, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(found.join(".credentials.json"), "{}").unwrap();
    std::fs::write(found.join("logged-in"), "").unwrap();
    std::fs::write(
        bin.join("claude"),
        crate::provider_cli::with_help("claude", CLAUDE),
    )
    .unwrap();
    std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let document =
        document(&machine.doctor_at(&home, &repo, &["--json"], Some(bin.as_os_str()), &[]));
    assert_eq!(
        check(&document, "providers")["fix"],
        "af provider detect --apply",
        "{document}"
    );
    let hint = check(&document, "project")["hint"].as_str().unwrap();
    assert!(
        hint.contains(
            "needs a codex Provider and no codex login was found to register; af provider \
             detect --apply registers only the claude login it finds; log in and register one \
             yourself at a private terminal: af provider setup codex-main --kind codex --login"
        ),
        "{hint}"
    );
    assert!(!hint.contains("no CLI on PATH reports a login"), "{hint}");
}

/// A Claude CLI whose context is `$CLAUDE_CONFIG_DIR`, or `~/.claude` without it: logged in
/// where that directory holds `logged-in`; installed with the help page af's capability check
/// reads.
const CLAUDE: &str = r#"#!/bin/sh
if [ "$1" = --version ]; then
  printf '%s\n' '2.1.299 (Claude Code)'
  exit 0
fi
if [ "$1" = auth ] && [ "$2" = status ] && [ "$3" = --json ]; then
  if [ -f "${CLAUDE_CONFIG_DIR:-$HOME/.claude}/logged-in" ]; then
    printf '%s\n' '{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","subscriptionType":"max"}'
    exit 0
  fi
  printf '%s\n' '{"loggedIn":false,"authMethod":"none","apiProvider":"firstParty"}'
  exit 1
fi
exit 64
"#;

/// #331 D2 and D3: the `.af/` file the project row names is the one onboarding stopped at, in
/// its reader's order (the selected pipeline before the lock), and a typed error is located by
/// its file and line like a syntax error; the hint's file is the one `af onboard` names.
#[test]
fn the_project_row_names_the_file_onboarding_stopped_at() {
    let machine = Machine::new();
    let repo = onboarded(&machine);
    let home = machine.path("home");
    let quoted = shell_words::quote(&canonical(&repo)).into_owned();
    let project_hint = || {
        let document = document(&machine.doctor_at(&home, &repo, &["--json"], None, &[]));
        let project = check(&document, "project").clone();
        assert_eq!(project["status"], "fail", "{document}");
        assert_eq!(project["fix"], serde_json::Value::Null, "{document}");
        project["hint"].as_str().unwrap().to_owned()
    };
    let onboard_says = || {
        let output = crate::common::af()
            .args(["onboard", "--repo", &canonical(&repo)])
            .env("HOME", &home)
            .output()
            .unwrap();
        assert!(!output.status.success());
        err(&output)
    };
    let lock = repo.join(".af/af.lock");
    let pipeline = repo.join(".af/pipelines/review.toml");
    let (kept_lock, kept_pipeline) = (
        std::fs::read(&lock).unwrap(),
        std::fs::read(&pipeline).unwrap(),
    );
    std::fs::write(&lock, [kept_lock.as_slice(), b"\nbroken = [\n"].concat()).unwrap();
    std::fs::write(
        &pipeline,
        [kept_pipeline.as_slice(), b"\nbroken = [\n"].concat(),
    )
    .unwrap();
    let line = String::from_utf8_lossy(&kept_pipeline).lines().count() + 2;
    let hint = project_hint();
    assert!(
        hint.starts_with(&format!(
            "correct {}/.af/pipelines/review.toml line {line} by hand (",
            canonical(&repo)
        )),
        "{hint}"
    );
    assert!(
        hint.ends_with(&format!("then recheck: af doctor --repo {quoted}")),
        "{hint}"
    );
    // Onboarding itself stops at that pipeline, never at the lock.
    let said = onboard_says();
    assert!(said.contains(&format!("line {line}")), "{said}");
    assert!(!said.contains("af.lock"), "{said}");
    std::fs::write(&lock, &kept_lock).unwrap();
    std::fs::write(&pipeline, &kept_pipeline).unwrap();

    // A value of the wrong type parses as TOML, and is still located.
    let project = repo.join(".af/af.toml");
    let kept = std::fs::read_to_string(&project).unwrap();
    let at = kept
        .lines()
        .position(|line| line.starts_with("version = "))
        .unwrap()
        + 1;
    std::fs::write(
        &project,
        kept.replacen("version = 1", "version = \"one\"", 1),
    )
    .unwrap();
    let hint = project_hint();
    assert!(
        hint.starts_with(&format!(
            "correct {}/.af/af.toml line {at} by hand (invalid type",
            canonical(&repo)
        )),
        "{hint}"
    );
    std::fs::write(&project, kept).unwrap();
}

/// #331 D2 and D3, the Worker packages: the hint names the file onboarding stops at because it
/// takes onboarding's own steps. A package's pin is checked before its manifest is parsed, so a
/// changed and malformed `reviewer.toml` gets its line and the re-pin; a manifest is parsed as
/// its typed shape and no other file of the package as TOML; a symlinked manifest stops the
/// reader before any later route; and a package named by a path outside `.af/workers` names
/// the pipeline, never a file outside `.af/`.
#[test]
fn the_authority_hint_follows_the_package_reader_and_stays_in_af() {
    let machine = Machine::new();
    let repo = onboarded(&machine);
    let home = machine.path("home");
    let af = repo.join(".af");
    let root = canonical(&repo);
    let quoted = shell_words::quote(&root).into_owned();
    let project_hint = || {
        let document = document(&machine.doctor_at(&home, &repo, &["--json"], None, &[]));
        let project = check(&document, "project").clone();
        assert_eq!(project["status"], "fail", "{document}");
        assert_eq!(project["fix"], serde_json::Value::Null, "{document}");
        project["hint"].as_str().unwrap().to_owned()
    };
    let onboard_says = || {
        let output = crate::common::af()
            .args(["onboard", "--repo", &root])
            .env("HOME", &home)
            .output()
            .unwrap();
        assert!(!output.status.success());
        err(&output)
    };
    let manifest = af.join("workers/correctness/reviewer.toml");
    let pipeline = af.join("pipelines/review.toml");
    let lock = af.join("af.lock");
    let kept = |path: &Path| std::fs::read_to_string(path).unwrap();
    let (kept_manifest, kept_pipeline, kept_lock, kept_project) = (
        kept(&manifest),
        kept(&pipeline),
        kept(&lock),
        kept(&af.join("af.toml")),
    );
    // A later route whose pipeline is malformed: the reader reaches it only when every file of
    // the selected pipeline reads, so no hint below may name it.
    std::fs::write(
        af.join("af.toml"),
        format!(
            "{kept_project}\n[[routes]]\nname = \"docs\"\npaths = [\"docs/**\"]\npipeline = \"docs\"\n"
        ),
    )
    .unwrap();
    std::fs::write(af.join("pipelines/docs.toml"), "version = \"one\"\n").unwrap();
    let restore = || {
        std::fs::write(&manifest, &kept_manifest).unwrap();
        std::fs::write(&pipeline, &kept_pipeline).unwrap();
        std::fs::write(&lock, &kept_lock).unwrap();
    };
    // Re-pins one Worker package, or the selected pipeline, as `--refresh-lock` would.
    let repin = |section: &str, digest: String| {
        let text = kept(&lock);
        let at = text.find(&format!("[{section}]")).unwrap();
        let start = at + text[at..].find("digest = \"").unwrap() + "digest = \"".len();
        let end = start + text[start..].find('"').unwrap();
        std::fs::write(&lock, format!("{}{digest}{}", &text[..start], &text[end..])).unwrap();
    };
    let package_digest = || {
        review_config::lock::package_digest("correctness", &af.join("workers/correctness")).unwrap()
    };

    // Changed and malformed, the lock untouched: onboarding stops at the package's pin, and
    // the hint names its manifest's line and the re-pin, never the route.
    std::fs::write(&manifest, format!("{kept_manifest}\nbroken = [\n")).unwrap();
    let line = kept_manifest.lines().count() + 2;
    let hint = project_hint();
    assert!(
        hint.starts_with(&format!(
            "correct {root}/.af/workers/correctness/reviewer.toml line {line} by hand ("
        )),
        "{hint}"
    );
    assert!(
        hint.contains(&format!(
            "then re-pin it, as its package changed since .af/af.lock pinned it: af onboard \
             --repo {quoted} --refresh-lock; then recheck: af doctor --repo {quoted}"
        )),
        "{hint}"
    );
    assert!(!hint.contains("docs.toml"), "{hint}");
    let said = onboard_says();
    assert!(
        said.contains("reviewer `correctness`") && said.contains("does not match its pin"),
        "{said}"
    );

    // Its pin matching, a manifest of the wrong shape (`subjects` a number) is refused by the
    // typed reader and located; a malformed file beside it is no manifest and never named.
    std::fs::write(
        &manifest,
        kept_manifest.replacen("subjects = [\"diff\"]", "subjects = 1", 1),
    )
    .unwrap();
    std::fs::write(af.join("workers/correctness/a-notes.toml"), "broken = [\n").unwrap();
    repin("workers.correctness", package_digest());
    let at = kept_manifest
        .lines()
        .position(|line| line.starts_with("subjects = "))
        .unwrap()
        + 1;
    let hint = project_hint();
    assert!(
        hint.starts_with(&format!(
            "correct {root}/.af/workers/correctness/reviewer.toml line {at} by hand (invalid type"
        )),
        "{hint}"
    );
    assert!(
        hint.ends_with(&format!("then recheck: af doctor --repo {quoted}")),
        "{hint}"
    );
    let said = onboard_says();
    assert!(said.contains("reviewer.toml for `correctness`"), "{said}");
    std::fs::remove_file(af.join("workers/correctness/a-notes.toml")).unwrap();
    restore();

    // A symlinked manifest stops the package reader, before the route is read.
    let outside = machine.path("outside.toml");
    std::fs::write(&outside, &kept_manifest).unwrap();
    std::fs::remove_file(&manifest).unwrap();
    std::os::unix::fs::symlink(&outside, &manifest).unwrap();
    let hint = project_hint();
    assert!(
        hint.starts_with(&format!(
            "correct {root}/.af/workers/correctness/reviewer.toml by hand (reviewer \
             `correctness` contains a symlink"
        )),
        "{hint}"
    );
    assert!(onboard_says().contains("contains a symlink"));
    std::fs::remove_file(&manifest).unwrap();
    restore();

    // A package named by a path outside `.af/workers`, its pipeline re-pinned: the reader
    // refuses the name, and the hint names the pipeline, never the directory's malformed file.
    let elsewhere = machine.path("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("bad.toml"), "broken = [\n").unwrap();
    std::fs::write(elsewhere.join("reviewer.toml"), "broken = [\n").unwrap();
    for package in [
        elsewhere.to_str().unwrap().to_owned(),
        "../../../elsewhere".to_owned(),
    ] {
        let changed = kept_pipeline.replacen(
            "package = \"correctness\"",
            &format!("package = \"{package}\""),
            1,
        );
        std::fs::write(&pipeline, &changed).unwrap();
        repin(
            "pipelines.review",
            review_store::canonical::blob_content_id(changed.as_bytes()),
        );
        let hint = project_hint();
        assert!(
            hint.starts_with(&format!(
                "correct {root}/.af/pipelines/review.toml by hand ("
            )),
            "{package}: {hint}"
        );
        assert!(
            !hint.contains("bad.toml") && !hint.contains("line "),
            "{hint}"
        );
        assert!(
            onboard_says().contains("is not in the lockfile"),
            "{package}"
        );
        restore();
    }

    // With every selected file whole, the reader goes on to the route, and so does the hint.
    let hint = project_hint();
    assert!(
        hint.starts_with(&format!("{root}/.af/pipelines/docs.toml no longer matches")),
        "{hint}"
    );
}

/// `program doctor` on a terminal `columns` wide, from `dir`, in `machine`'s private home with
/// `path` as PATH and `envs` on top: what the terminal shows.
fn doctor_in_terminal(
    machine: &Machine,
    program: &Path,
    dir: &Path,
    columns: u16,
    path: &std::ffi::OsStr,
    envs: &[(&str, &str)],
) -> String {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::Read as _;
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 60,
            cols: columns,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let home = machine.path("home");
    let mut command = CommandBuilder::new(program);
    command.arg("doctor");
    command.cwd(dir);
    command.env_clear();
    for (name, value) in [
        ("HOME", home.clone()),
        ("XDG_CONFIG_HOME", home.join(".config")),
        ("XDG_STATE_HOME", home.join(".local/state")),
        ("XDG_CACHE_HOME", home.join(".cache")),
        ("XDG_DATA_HOME", home.join(".local/share")),
        ("XDG_BIN_HOME", home.join(".local/bin")),
    ] {
        command.env(name, value);
    }
    command.env("PATH", path);
    command.env("AF_SELF_OFFLINE", "1");
    // Doctor probes the Provider CLIs it finds: never the developer's own install directories.
    command.env(crate::common::DETECT_SYSTEM_DIRS_SETTING, "");
    command.env("AF_TEST_FREE_BYTES", (1_u64 << 40).to_string());
    if let Some((name, value)) = crate::common::inherited_tmpdir() {
        command.env(name, value);
    }
    for (name, value) in envs {
        command.env(name, value);
    }
    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let drained = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = reader.read_to_end(&mut bytes);
        bytes
    });
    child.wait().unwrap();
    drop(pair.master);
    String::from_utf8_lossy(&drained.join().unwrap()).replace("\r\n", "\n")
}

/// The lines of the `fix:` (or `hint:`, by `label`) under the row `id` in doctor's text, from
/// the label on, with a shell's backslash-newline removed: the text a person pastes.
fn printed(text: &str, id: &str, label: &str) -> String {
    let row = |line: &str| {
        ["ok    ", "warn  ", "fail  "]
            .iter()
            .any(|status| line.starts_with(status))
            || line.starts_with("nothing failed")
            || line.contains("check(s) failed")
    };
    let mut lines = text
        .lines()
        .skip_while(|line| {
            !(row(line)
                && line
                    .get(6..)
                    .is_some_and(|rest| rest.starts_with(&format!("{id} "))))
        })
        .skip(1)
        .take_while(|line| !row(line));
    let mut block = String::new();
    let mut inside = false;
    for line in lines.by_ref() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("fix: ") || trimmed.starts_with("hint: ") {
            inside = trimmed.starts_with(label);
            if inside {
                block.push_str(&trimmed[label.len()..]);
                continue;
            }
        }
        if inside {
            block.push('\n');
            block.push_str(line);
        }
    }
    block.replace("\\\n", "")
}

/// #331 D4, D6 and D7 on a terminal 80 and 100 columns wide: no line doctor prints is wider. The
/// onboarding preview in the project hint is the command to the repository, its `;` the
/// sentence's; the install-first PATH fix run by this binary's own long path is cut into pieces
/// a shell joins and installs first; and the elvish PATH line, its directory past the width,
/// fits too and spells the directory.
#[test]
fn every_doctor_line_fits_the_terminal_and_its_commands_run_as_printed() {
    let machine = Machine::new();
    let repo = machine.path("work/repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    // This binary at a path wider than either terminal, its name `af`.
    let long = machine.path(&format!(
        "work/{}bin",
        "a-long-download-directory/".repeat(5)
    ));
    std::fs::create_dir_all(&long).unwrap();
    let program = long.join("af");
    if std::fs::hard_link(crate::common::af_binary(), &program).is_err() {
        std::fs::copy(crate::common::af_binary(), &program).unwrap();
    }
    let program_text = program.to_str().unwrap().to_owned();
    assert!(program_text.chars().count() > 100);
    let tools = machine.path("tools-path");
    std::fs::create_dir_all(&tools).unwrap();
    std::os::unix::fs::symlink(on_path("git"), tools.join("git")).unwrap();
    // The default link's directory below HOME, a long fixed directory in it so that the elvish
    // line is wider than 100 columns whatever the temporary root is (`/tmp/.tmpXXXXXX` on
    // Linux, longer on macOS): the cut form is proved on every platform.
    let bin =
        machine.path("home/.local/a-fixed-directory-long-enough-that-the-elvish-line-is-cut/bin");
    let bin_env = bin.to_str().unwrap().to_owned();
    let version = crate::common::VERSION;
    for columns in [80_u16, 100] {
        let fits = |text: &str| {
            for line in text.lines() {
                assert!(
                    line.chars().count() <= usize::from(columns),
                    "{columns}: {line:?}\n{text}"
                );
            }
        };
        let text = doctor_in_terminal(
            &machine,
            &program,
            &repo,
            columns,
            tools.as_os_str(),
            &[("SHELL", "/bin/zsh"), ("XDG_BIN_HOME", &bin_env)],
        );
        fits(&text);
        // The preview, from `af onboard` to the sentence's `;`: the repository, as it is.
        let hint = printed(&text, "project", "hint: ");
        let start = hint
            .find("af onboard --repo")
            .unwrap_or_else(|| panic!("{text}"));
        let end = start + hint[start..].find("then").unwrap();
        let preview = hint[start..end].trim_end();
        let preview = preview
            .strip_suffix(';')
            .unwrap_or_else(|| panic!("{columns}: {preview:?}\n{text}"));
        assert_eq!(
            shell_words::split(preview).unwrap(),
            ["af", "onboard", "--repo", canonical(&repo).as_str()],
            "{columns}\n{text}"
        );
        // The install-first fix, by this binary's path: pieces a shell joins.
        let fix = printed(&text, "path", "fix: ");
        let words = shell_words::split(&fix).unwrap_or_else(|_| panic!("{text}"));
        assert_eq!(
            words[..5],
            [program_text.as_str(), "self", "install", version, "&&"],
            "{columns}\n{text}"
        );
        assert_eq!(
            words[5..],
            ["export", format!("PATH={}:$PATH", bin.display()).as_str()][..],
            "{columns}\n{text}"
        );
        // As printed, line breaks included, sh parses it: `&&` ends a line, never starts one.
        let parses = |command: &str| {
            let parsed = Command::new("/bin/sh")
                .args(["-n", "-c", command])
                .output()
                .unwrap();
            assert!(
                parsed.status.success(),
                "{columns}: {command:?}: {}\n{text}",
                String::from_utf8_lossy(&parsed.stderr)
            );
        };
        parses(&fix);
        // The binary row's install alone, by the same path: one command too, never prose.
        let install = printed(&text, "binary", "fix: ");
        assert_eq!(
            shell_words::split(&install).unwrap_or_else(|_| panic!("{text}")),
            [program_text.as_str(), "self", "install", version],
            "{columns}\n{text}"
        );
        assert!(!install.contains('\n'), "{columns}: {install:?}\n{text}");
        parses(&install);

        // Elvish, the default link present and its directory off PATH.
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(&program, bin.join("af")).unwrap();
        let text = doctor_in_terminal(
            &machine,
            &program,
            &repo,
            columns,
            tools.as_os_str(),
            &[("SHELL", "/usr/bin/elvish"), ("XDG_BIN_HOME", &bin_env)],
        );
        fits(&text);
        let fix = printed(&text, "path", "fix: ");
        let lines: Vec<&str> = fix.lines().collect();
        assert_eq!(lines[0], "use str; set paths = [(str:join '' [", "{text}");
        assert_eq!(lines.last().copied(), Some("]) $@paths]"), "{text}");
        let dir: String = lines[1..lines.len() - 1]
            .iter()
            .map(|piece| {
                let piece = piece.trim();
                piece[1..piece.len() - 1].replace("''", "'")
            })
            .collect();
        assert_eq!(dir, bin.to_str().unwrap(), "{text}");
        std::fs::remove_file(bin.join("af")).unwrap();
    }
}

/// #331 D7, the dispatch: the default link's directory is on PATH but holds no link yet, and
/// another `af` is found later on PATH. Putting the empty directory first finds no `af` there,
/// so the fix installs first and then puts it first; run as printed, it does both in order.
#[test]
fn the_path_fix_installs_first_when_the_directory_is_on_path_but_empty() {
    let machine = Machine::new();
    let bin = machine.path("home/.local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let other = machine.path("other-bin");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("af"), "#!/bin/sh\necho \"$*\" > \"$0.args\"\n").unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(other.join("af"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths([
        bin.clone(),
        other.clone(),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ])
    .unwrap();
    let output = machine.doctor_at(
        &machine.path("home"),
        &machine.path("work"),
        &["--json"],
        Some(path.as_os_str()),
        &[("SHELL", "/bin/zsh")],
    );
    let document = document(&output);
    let row = check(&document, "path");
    assert_eq!(row["status"], "warn", "{document}");
    assert!(
        row["detail"]
            .as_str()
            .unwrap()
            .contains("does not exist yet"),
        "{document}"
    );
    let fix = row["fix"].as_str().unwrap();
    let version = crate::common::VERSION;
    assert_eq!(
        fix,
        format!(
            "af self install {version} && export PATH='{}':\"$PATH\"",
            bin.display()
        ),
        "{document}"
    );
    let ran = Command::new("/bin/sh")
        .args(["-c", &format!("{fix}\nprintf %s \"$PATH\"")])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(ran.status.success(), "{ran:?}");
    assert_eq!(
        std::fs::read_to_string(other.join("af.args")).unwrap(),
        format!("self install {version}\n")
    );
    assert!(
        String::from_utf8(ran.stdout)
            .unwrap()
            .starts_with(&format!("{}:", bin.display()))
    );
}

/// #331 D1: an `AF_RELEASE_SOURCE` that is a regular file stops `af self update --check` with
/// the same error the update row reports, so the row offers no fix; its hint says what to change.
#[test]
fn a_release_source_that_is_a_file_offers_no_update_fix() {
    let machine = Machine::new();
    let work = machine.path("work");
    let source = machine.path("releases.txt");
    std::fs::write(&source, "not a directory\n").unwrap();
    let source_env = source.to_str().unwrap().to_owned();
    let envs = [("AF_RELEASE_SOURCE", source_env.as_str())];
    let home = machine.path("home");
    let document = document(&machine.doctor_at(&home, &work, &["--json"], None, &envs));
    let update = check(&document, "update");
    assert_eq!(update["status"], "warn", "{document}");
    assert_eq!(update["fix"], serde_json::Value::Null, "{document}");
    let detail = update["detail"].as_str().unwrap();
    assert!(
        detail.starts_with(&format!(
            "AF_RELEASE_SOURCE {source_env} is not a directory"
        )),
        "{detail}"
    );
    assert!(
        update["hint"]
            .as_str()
            .unwrap()
            .starts_with("point AF_RELEASE_SOURCE at the directory that holds the releases"),
        "{document}"
    );
    // The command the row used to offer fails on that very source.
    for args in [
        &["self", "update", "--check"][..],
        &["self", "status", "--refresh"],
    ] {
        let output = crate::common::af()
            .args(args)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env_remove("AF_SELF_OFFLINE")
            .envs(envs)
            .output()
            .unwrap();
        let said = format!("{}{}", out(&output), err(&output));
        assert!(said.contains(&source_env), "af {args:?}: {said}");
    }
}

/// `path` as git names its top level: canonical, without a trailing `/`.
fn canonical(path: &Path) -> String {
    std::fs::canonicalize(path).unwrap().display().to_string()
}
