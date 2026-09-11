use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;
#[path = "support/task_cli.rs"]
mod task_cli;

fn commit(repo: &Path, message: &str) {
    for args in [vec!["add", "-A"], vec!["commit", "-qm", message]] {
        assert!(
            Command::new("git")
                .current_dir(repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
}
fn af(repo: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap()
}
fn success(output: std::process::Output) -> Value {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn shared(repo: &Path) {
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(repo.join(".af/task-catalog.toml")).unwrap())
            .unwrap();
    let kind = repo.join(".af/task-packages/team/feature-kind");
    std::fs::create_dir_all(&kind).unwrap();
    std::fs::write(kind.join("kind.toml"), "schema = 'af.task-kind/1'\nname = 'team/feature-kind'\nversion = '1.0.0'\nkind = 'implement'\nprofile = 'reviewed_implementation'\n").unwrap();
    catalog["packages"].as_table_mut().unwrap().insert("team/feature-kind".into(), toml::Value::try_from(json!({"version":"1.0.0","path":".af/task-packages/team/feature-kind","digest":review_config::lock::package_digest("team/feature-kind",&kind).unwrap()})).unwrap());
    let packages = catalog["packages"].as_table().unwrap();
    let mut roots = toml::map::Map::new();
    let mut workers = toml::map::Map::new();
    for (name, pin) in packages {
        if name == "fixture/implementation" || name == "fixture/review" {
            roots.insert(name.clone(), pin.clone());
        } else {
            workers.insert(name.clone(), pin.clone());
        }
    }
    std::fs::create_dir(repo.join("catalogs")).unwrap();
    std::fs::write(repo.join("catalog.toml"), toml::to_string(&json!({"schema":"af.shared-task-catalog/1","packages":roots,"imports":["catalogs/workers.toml"]})).unwrap()).unwrap();
    std::fs::write(
        repo.join("catalogs/workers.toml"),
        toml::to_string(&json!({"schema":"af.shared-task-catalog/1","packages":workers})).unwrap(),
    )
    .unwrap();
    commit(repo, "shared transitive catalog");
}

#[test]
fn git_catalog_sync_is_exact_transitive_absent_only_and_runs_offline_after_capture() {
    let temp = tempfile::tempdir().unwrap();
    let producer = temp.path().join("producer");
    let consumer = temp.path().join("consumer");
    std::fs::create_dir(&producer).unwrap();
    std::fs::create_dir(&consumer).unwrap();
    let (source, _) = task_cli::fixture_named(&producer, "embedded-review");
    let (repo, state) = task_cli::fixture_named(&consumer, "embedded-review");
    shared(&source);
    let imported = success(af(
        &repo,
        &[
            "catalog",
            "sync",
            "--source",
            source.to_str().unwrap(),
            "--revision",
            "main",
            "--destination",
            ".af/vendor/team",
            "--json",
        ],
    ));
    assert_eq!(imported["packages"].as_object().unwrap().len(), 6);
    let head = Command::new("git")
        .current_dir(&source)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert_eq!(
        imported["source"]["commit"],
        String::from_utf8_lossy(&head.stdout).trim()
    );
    let lock = repo.join(".af/vendor/team/catalog.lock.json");
    let before = std::fs::read(&lock).unwrap();
    assert!(!String::from_utf8_lossy(&before).contains(temp.path().to_str().unwrap()));
    assert!(
        !af(
            &repo,
            &[
                "catalog",
                "sync",
                "--source",
                source.to_str().unwrap(),
                "--destination",
                ".af/vendor/team",
                "--json"
            ]
        )
        .status
        .success()
    );
    assert_eq!(std::fs::read(&lock).unwrap(), before);
    let catalog_path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
    catalog["packages"] = toml::Value::Table(toml::map::Map::new());
    catalog.as_table_mut().unwrap().insert(
        "imports".into(),
        toml::Value::try_from(vec![".af/vendor/team/catalog.lock.json"]).unwrap(),
    );
    catalog.as_table_mut().unwrap().insert(
        "kinds".into(),
        toml::Value::try_from(json!({"implement":"team/feature-kind"})).unwrap(),
    );
    std::fs::write(&catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    std::fs::remove_dir_all(repo.join(".af/task-packages")).unwrap();
    commit(&repo, "activate exact imported catalog");
    std::fs::rename(&source, producer.join("source-moved-away")).unwrap();
    let planned = success(af(
        &repo,
        &[
            "task",
            "plan",
            "--file",
            "ticket.json",
            "--state",
            state.to_str().unwrap(),
            "--json",
        ],
    ));
    assert_eq!(planned["attempts"], 0);
    assert!(planned["plan"]["dependencies"]["team/feature-kind"].is_object());
    std::fs::write(
        repo.join(".af/vendor/team")
            .join(
                imported["packages"]["fixture/implementer"]["path"]
                    .as_str()
                    .unwrap(),
            )
            .join("worker.py"),
        "raise Exception('mutated import')",
    )
    .unwrap();
    let result = success(af(
        &repo,
        &[
            "task",
            "run",
            "pagination-cli",
            "--state",
            state.to_str().unwrap(),
            "--json",
        ],
    ));
    assert_eq!(result["result"]["acceptance"], "satisfied");
    assert_eq!(result["attempts"], 4);
    commit(&repo, "unreviewed package mutation");
    let mut task: Value =
        serde_json::from_slice(&std::fs::read(repo.join("ticket.json")).unwrap()).unwrap();
    task["task_id"] = json!("mutated-import");
    std::fs::write(repo.join("ticket.json"), serde_json::to_vec(&task).unwrap()).unwrap();
    let rejected = af(
        &repo,
        &[
            "task",
            "plan",
            "--file",
            "ticket.json",
            "--state",
            state.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("changed since it was locked"));
}

#[test]
fn shared_catalog_cycles_collisions_missing_dependencies_and_symlinks_are_refused() {
    for case in ["cycle", "collision", "missing", "symlink"] {
        let temp = tempfile::tempdir().unwrap();
        let (source, _) = task_cli::fixture_named(temp.path(), "embedded-review");
        shared(&source);
        let path = source.join("catalogs/workers.toml");
        let mut catalog: toml::Value =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        match case {
            "cycle" => {
                catalog.as_table_mut().unwrap().insert(
                    "imports".into(),
                    toml::Value::try_from(vec!["catalog.toml"]).unwrap(),
                );
            }
            "collision" => {
                let root: toml::Value =
                    toml::from_str(&std::fs::read_to_string(source.join("catalog.toml")).unwrap())
                        .unwrap();
                catalog["packages"].as_table_mut().unwrap().insert(
                    "fixture/review".into(),
                    root["packages"]["fixture/review"].clone(),
                );
            }
            "missing" => {
                catalog["packages"]
                    .as_table_mut()
                    .unwrap()
                    .remove("fixture/implementer");
            }
            "symlink" => {
                #[cfg(unix)]
                std::os::unix::fs::symlink(
                    "/etc/passwd",
                    source.join(".af/task-packages/fixture/bugs/escape"),
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        std::fs::write(path, toml::to_string(&catalog).unwrap()).unwrap();
        commit(&source, case);
        let output = af(
            temp.path(),
            &[
                "catalog",
                "sync",
                "--source",
                source.to_str().unwrap(),
                "--destination",
                "import",
                "--json",
            ],
        );
        assert!(!output.status.success(), "{case}");
        assert!(
            !temp.path().join("import").exists(),
            "{case} published files"
        );
    }
}
