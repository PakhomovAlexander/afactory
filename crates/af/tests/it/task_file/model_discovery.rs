//! Plan-time model validation, end to end (issue #258, ADR-0148): `af task plan` and `af review
//! plan --file` refuse an alias and an effort level the Provider does not accept before any
//! Provider process starts, naming the exact ID or the levels and `af provider models <id>`; a
//! binding the catalog lists is `ok`, a custom ID that passes the spelling rule is `unverified
//! against the catalog`, and both plans continue. The fake CLIs serve the recorded catalogs.
use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;

use crate::fake_models;
use crate::task_cli::fixture_named;

/// The review fixture with both business Workers bound to one model and effort level on one
/// Provider of `kind`, `claude-personal` or `codex-main`, served by its fake CLI.
struct Fixture {
    _directory: tempfile::TempDir,
    repo: PathBuf,
    state: PathBuf,
    home: PathBuf,
    auth: PathBuf,
    path: std::ffi::OsString,
    registry: PathBuf,
}

impl Fixture {
    fn new(kind: &str, model: &str, effort: &str) -> Self {
        use review_config::task::catalog::{TaskWorkerManifest, TaskWorkerRunner};
        let directory = tempfile::tempdir().unwrap();
        let (repo, state) = fixture_named(directory.path(), "review");
        let home = directory.path().join("home");
        let auth = directory.path().join(format!("{kind}-auth"));
        let bin = directory.path().join("bin");
        std::fs::create_dir_all(&home).unwrap();
        fake_models::install(kind, &bin, &auth);
        let provider = match kind {
            "claude" => "claude-personal",
            _ => "codex-main",
        };
        let registry = home.join("providers.toml");
        std::fs::write(
            &registry,
            toml::to_string(&serde_json::json!({"version":1,"providers":[{"id":provider,"kind":kind,"auth_dir":auth}]}))
                .unwrap(),
        )
        .unwrap();
        let catalog_path = repo.join(".af/task-catalog.toml");
        let mut catalog: toml::Value =
            toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
        let mut providers = toml::map::Map::new();
        for name in ["bugs", "correctness"] {
            let package = format!("fixture/{name}");
            let path = repo.join(format!(".af/task-packages/{package}"));
            let mut worker: TaskWorkerManifest =
                toml::from_str(&std::fs::read_to_string(path.join("worker.toml")).unwrap())
                    .unwrap();
            worker.runner = TaskWorkerRunner::Model {
                provider_kind: kind.into(),
                model: model.into(),
                effort: effort.into(),
            };
            let attempt = worker.signature.attempt.as_mut().unwrap();
            attempt.tokens = 1000;
            attempt.wall_ms = super::NATIVE_REVIEWER_WALL_MS;
            std::fs::write(path.join("worker.toml"), toml::to_string(&worker).unwrap()).unwrap();
            catalog["packages"][&package]["digest"] =
                toml::Value::String(review_config::lock::package_digest(&package, &path).unwrap());
            providers.insert(package, toml::Value::String(provider.into()));
        }
        // Room for verification and the mandatory Provider admission, as the fallback fixture.
        let pipeline_dir = repo.join(".af/task-packages/fixture/review");
        let mut pipeline: review_core::task::pipeline::PipelineDefinitionV1 =
            toml::from_str(&std::fs::read_to_string(pipeline_dir.join("pipeline.toml")).unwrap())
                .unwrap();
        pipeline.max_attempts = 4;
        std::fs::write(
            pipeline_dir.join("pipeline.toml"),
            toml::to_string(&pipeline).unwrap(),
        )
        .unwrap();
        catalog["packages"]["fixture/review"]["digest"] = toml::Value::String(
            review_config::lock::package_digest("fixture/review", &pipeline_dir).unwrap(),
        );
        catalog
            .as_table_mut()
            .unwrap()
            .insert("providers".into(), toml::Value::Table(providers));
        std::fs::write(catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
        let file_path = repo.join("review.json");
        let mut file: Value = serde_json::from_slice(&std::fs::read(&file_path).unwrap()).unwrap();
        file["limits"] = super::native_model_limits();
        std::fs::write(file_path, serde_json::to_vec(&file).unwrap()).unwrap();
        for args in [vec!["add", "-A"], vec!["commit", "-qm", "model bindings"]] {
            assert!(
                Command::new("git")
                    .current_dir(&repo)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let path = std::env::join_paths(
            std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        Self {
            _directory: directory,
            repo,
            state,
            home,
            auth,
            path,
            registry,
        }
    }

    fn plan(&self, namespace: &str) -> Output {
        crate::common::af()
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("USER", "fixture")
            .env("PATH", &self.path)
            .env("AF_PROVIDERS_FILE", &self.registry)
            .args([
                namespace,
                "plan",
                "--file",
                "review.json",
                "--json",
                "--state",
            ])
            .arg(&self.state)
            .output()
            .unwrap()
    }

    fn model_lines(output: &Output) -> Vec<String> {
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .filter(|line| line.starts_with("model "))
            .map(str::to_owned)
            .collect()
    }
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn an_alias_is_refused_before_any_provider_probe_with_its_exact_id() {
    for namespace in ["task", "review"] {
        let fixture = Fixture::new("claude", "fable", "xhigh");
        let planned = fixture.plan(namespace);
        assert_ne!(
            planned.status.code(),
            Some(0),
            "{namespace}: {}",
            err(&planned)
        );
        let stderr = err(&planned);
        for part in [
            "af refused the binding of model fable (effort xhigh) for Provider claude-personal (claude) (model_not_accepted)",
            "the exact ID claude-fable-5-1",
            "af binds exact IDs",
            "`af provider models claude-personal` (token-free)",
        ] {
            assert!(stderr.contains(part), "{namespace}: {part}:\n{stderr}");
        }
        assert!(
            Fixture::model_lines(&planned)
                .iter()
                .any(|line| line.starts_with("model fixture/bugs fable xhigh  refused: ")),
            "{stderr}"
        );
        assert_eq!(
            fake_models::started(&fixture.auth),
            "",
            "{namespace}: a Provider process started"
        );
    }
}

#[test]
fn an_effort_level_claude_does_not_accept_is_refused_with_the_five_levels() {
    for namespace in ["task", "review"] {
        let fixture = Fixture::new("claude", "claude-fable-5-1", "ultra");
        let planned = fixture.plan(namespace);
        assert_ne!(
            planned.status.code(),
            Some(0),
            "{namespace}: {}",
            err(&planned)
        );
        let stderr = err(&planned);
        assert!(
            stderr.contains(
                "the effort level `ultra` is not one Claude Code accepts: low, medium, high, xhigh, max"
            ),
            "{namespace}:\n{stderr}"
        );
        assert!(
            stderr.contains("`af provider models claude-personal`"),
            "{stderr}"
        );
        assert_eq!(fake_models::started(&fixture.auth), "", "{namespace}");
    }
}

#[test]
fn a_listed_binding_is_ok_and_a_custom_id_is_unverified_and_both_plans_continue() {
    for (model, verdict) in [
        ("claude-fable-5-1", "ok"),
        ("claude-opus-4-8", "unverified against the catalog"),
    ] {
        for namespace in ["task", "review"] {
            let fixture = Fixture::new("claude", model, "xhigh");
            let planned = fixture.plan(namespace);
            assert_eq!(
                planned.status.code(),
                Some(0),
                "{namespace} {model}: {}",
                err(&planned)
            );
            let lines = Fixture::model_lines(&planned);
            for worker in ["fixture/bugs", "fixture/correctness"] {
                let line = format!("model {worker} {model} xhigh  {verdict}");
                assert!(lines.contains(&line), "{namespace}: {line}: {lines:?}");
            }
            // The catalog was read once, token-free, before the identity probe; no turn.
            let started = fake_models::started(&fixture.auth);
            let sessions = started
                .lines()
                .filter(|line| line.starts_with("-p --input-format stream-json"))
                .count();
            assert_eq!(sessions, 1, "{namespace}: {started}");
            assert_eq!(fake_models::turns(&fixture.auth), "");
            let plan: Value = serde_json::from_slice(&planned.stdout).unwrap();
            assert!(
                plan.is_object(),
                "{namespace}: the plan document is unchanged"
            );
        }
    }
}

#[test]
fn the_codex_level_ultra_passes_on_codex() {
    let fixture = Fixture::new("codex", "gpt-6-sol", "ultra");
    let planned = fixture.plan("task");
    assert_eq!(planned.status.code(), Some(0), "{}", err(&planned));
    assert!(
        Fixture::model_lines(&planned).contains(&"model fixture/bugs gpt-6-sol ultra  ok".into()),
        "{}",
        err(&planned)
    );
    let fixture = Fixture::new("codex", "gpt-6-luna", "ultra");
    let planned = fixture.plan("task");
    assert_ne!(planned.status.code(), Some(0));
    assert!(
        err(&planned).contains(
            "the catalog lists the effort levels low, medium, high, xhigh, max for gpt-6-luna, and `ultra` is not one of them"
        ),
        "{}",
        err(&planned)
    );
}
