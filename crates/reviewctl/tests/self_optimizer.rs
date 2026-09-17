use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;
use std::process::Command;
#[path = "support/task_cli.rs"]
mod task_cli;

fn af(repo: &Path, state: &Path, extra: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(repo)
        .args([
            "self",
            "optimize",
            "--state",
            state.to_str().unwrap(),
            "--json",
        ])
        .args(extra)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn install_optimizer_catalog(repo: &Path) {
    let workspace = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let source = workspace.join("fixtures/self-optimizer/catalog");
    let destination = repo.join(".af/optimization");
    task_cli::copy_tree(&source, &destination);
    let catalog_path = repo.join(".af/task-catalog.toml");
    let parsed: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
    let mut catalog = serde_json::to_value(parsed).unwrap();
    for (name, directory) in [
        ("builtin/optimization-economics", "optimization"),
        ("builtin/optimization-analysis-kind", "optimization-kind"),
    ] {
        catalog["packages"][name] = json!({
            "version":"1.0.0",
            "path":format!(".af/optimization/{directory}"),
            "digest":review_config::lock::package_digest(name,&destination.join(directory)).unwrap()
        });
    }
    if !catalog["kinds"].is_object() {
        catalog["kinds"] = json!({});
    }
    catalog["kinds"]["optimize"] = json!("builtin/optimization-analysis-kind");
    std::fs::write(catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
}

fn install_candidate_optimizer_catalog(repo: &Path) -> minisign::KeyPair {
    let workspace = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let source = workspace.join("fixtures/self-optimizer/catalog");
    let destination = repo.join(".af/optimization");
    task_cli::copy_tree(&source, &destination);
    let catalog_path = repo.join(".af/task-catalog.toml");
    let parsed: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
    let mut catalog = serde_json::to_value(parsed).unwrap();
    for (name, directory) in [
        ("builtin/optimization-experiment", "optimization-experiment"),
        ("builtin/optimization-controlled", "optimization-controlled"),
        ("builtin/optimization-light", "optimization-light"),
        (
            "builtin/optimization-light-diagnose",
            "optimization-light-diagnose",
        ),
        (
            "builtin/optimization-light-propose",
            "optimization-light-propose",
        ),
        (
            "builtin/optimization-check-baseline",
            "optimization-check-baseline",
        ),
        (
            "builtin/optimization-check-candidate",
            "optimization-check-candidate",
        ),
        (
            "builtin/optimization-check-evaluator",
            "optimization-check-evaluator",
        ),
        (
            "builtin/optimization-candidate-kind",
            "optimization-candidate-kind",
        ),
        ("builtin/optimization-baseline", "optimization-baseline"),
        ("builtin/optimization-candidate", "optimization-candidate"),
    ] {
        catalog["packages"][name] = json!({
            "version":"1.0.0",
            "path":format!(".af/optimization/{directory}"),
            "digest":review_config::lock::package_digest(name,&destination.join(directory)).unwrap()
        });
    }
    catalog["kinds"]["optimize"] = json!("builtin/optimization-candidate-kind");
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    catalog["developers"] = json!({
        "schema":"af.task-developers/1",
        "keys":{"owner":key.pk.to_box().unwrap().into_string()}
    });
    std::fs::write(catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    key
}

fn command_json(repo: &Path, state: &Path, args: &[&str]) -> Value {
    command_json_with_env(repo, state, args, &[])
}

fn command_json_with_env(
    repo: &Path,
    state: &Path,
    args: &[&str],
    environment: &[(&str, &Path)],
) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_af"));
    command
        .current_dir(repo)
        .args(args)
        .args(["--json", "--state"])
        .arg(state);
    for (name, value) in environment {
        command.env(name, value);
    }
    let output = command.output().unwrap();
    let expected_inconclusive =
        args.starts_with(&["task", "run"]) || args.starts_with(&["self", "optimize"]);
    assert!(
        output.status.success() || expected_inconclusive,
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn controlled_candidate_repairs_real_harness_and_delivers_verified_snapshot() {
    controlled_candidate(
        "harness.py",
        "import sys\nprint('correct:' + sys.argv[1])\n",
        true,
    );
}

#[test]
fn controlled_candidate_rejects_ineffective_harness() {
    controlled_candidate("harness.py", "print('still broken')\n", false);
}

#[test]
fn controlled_candidate_must_fix_every_distinct_case() {
    controlled_candidate("harness.py", "print('correct:a')\n", false);
}

#[test]
fn controlled_candidate_cannot_rewrite_protected_oracle() {
    controlled_candidate("oracle.py", "# erased oracle\n", false);
}

#[test]
fn controlled_candidate_cannot_count_relabelled_inputs_as_distinct_families() {
    controlled_candidate_with_inputs("harness.py", "print('correct')\n", false, true);
}

#[test]
fn light_strategy_generates_one_candidate_without_exposing_source_to_author_workers() {
    let temp = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(temp.path(), "pagination");
    let key = install_candidate_optimizer_catalog(&repo);
    let project = format!("sha256:{}", "3".repeat(64));
    std::fs::write(
        repo.join(".af/optimization-sources.toml"),
        toml::to_string(&json!({
            "schema":"af.optimization-sources/1", "project_id":project,
            "sources":[{"adapter":"af","path":"history.jsonl","source_id":"fixture","execution_id":"session"}]
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        repo.join(".af/history.jsonl"),
        json!({"observed_unix_ms":"1","attribution":{"project_id":project,"case_family":"development_prior","execution_id":"prior"},"tokens":{"cumulative_key":"prior-context","usage":{"input_tokens":"100","output_tokens":"10","chargeable_tokens":"110"},"status":"exact","context_tokens":"80","retrieval_tokens":"10","repeated_context_tokens":"20","outer_session":false},"outcome":{"outcome":"verified","retries":0,"repairs":0,"later_defects":0}}).to_string()+"\n",
    )
    .unwrap();
    std::fs::write(
        repo.join(".af/optimization-policy.json"),
        json!({
            "schema":"af.optimization-configuration-policy/1", "writable_paths":[".af/optimization/optimization-check-candidate/instructions.md"],
            "checks":{"oracle":"oracle.py"},
            "light_economics":{"comparable_future_runs":10,"recurring_tokens_per_run":0,
                "recurring_time_ms_per_run":0,"maximum_one_off_tokens":1000,
                "maximum_one_off_time_ms":60000,"objective_exception":"correctness"},
            "experiment":{"recipe":"deterministic_correction","uncertainty_rule":"deterministic","repetitions":1,
                "minimum_families":1,"token_increase_ceiling_bps":0,
                "cases":[{"family":"protected_holdout","membership":"holdout","input_path":"case.json"}]}
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        repo.join("case.json"),
        r#"{"argument":"a","expected":"correct:a"}"#,
    )
    .unwrap();
    std::fs::write(repo.join("oracle.py"), "import json, sys\nfrom pathlib import Path\njson.loads(Path(sys.argv[1]).read_text())\ntext = Path('.af/optimization/optimization-check-candidate/instructions.md').read_text()\nassert text.count('Keep the required acceptance oracle unchanged.') == 1\n").unwrap();
    for args in [
        vec!["add", "-A"],
        vec!["commit", "-qm", "light optimizer fixture"],
    ] {
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let waiting = af(&repo, &state, &["--strategy", "light", "--execute"]);
    assert_eq!(
        waiting["phase"]["reason"], "needs_plan_review",
        "{waiting:#}"
    );
    assert_eq!(
        waiting["attempts"], 2,
        "diagnose and propose run exactly once"
    );
    let task = waiting["task_id"].as_str().unwrap();
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let store = review_store::EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    let projection = store.task_projection(&cas, task).unwrap().unwrap();
    let execution = projection.execution.as_ref().unwrap();
    for node in ["root.nodes.diagnose", "root.nodes.propose"] {
        let invocation = &execution.invocations.get(node).unwrap().1;
        assert!(
            !invocation.inputs.contains_key("source"),
            "author Worker received protected source: {node}"
        );
    }
    let payload = temp.path().join("light.payload");
    let signature = temp.path().join("light.minisig");
    command_json(
        &repo,
        &state,
        &[
            "task",
            "decision-payload",
            task,
            "--developer",
            "owner",
            "--decision",
            "approved",
            "--reason",
            "one generated light candidate",
            "--output",
            payload.to_str().unwrap(),
        ],
    );
    let bytes = std::fs::read(&payload).unwrap();
    std::fs::write(
        &signature,
        minisign::sign(
            Some(&key.pk),
            &key.sk,
            bytes.as_slice(),
            Some("light candidate"),
            None,
        )
        .unwrap()
        .into_string(),
    )
    .unwrap();
    command_json(
        &repo,
        &state,
        &[
            "task",
            "approve",
            task,
            "--payload",
            payload.to_str().unwrap(),
            "--signature",
            signature.to_str().unwrap(),
        ],
    );
    let completed = command_json(&repo, &state, &["task", "run", task, "--execute"]);
    assert_eq!(
        completed["result"]["acceptance"], "satisfied",
        "{completed:#}"
    );
    assert_eq!(
        completed["attempts"], 5,
        "two author, two arm, one evaluator Attempts"
    );
    let completed_projection =
        review_store::EventStore::open_read_only(state.join("events.sqlite"))
            .unwrap()
            .task_projection(&cas, task)
            .unwrap()
            .unwrap();
    let first_configuration_id = completed_projection
        .execution
        .as_ref()
        .unwrap()
        .outputs
        .get("root.nodes.prepare")
        .unwrap()
        .1
        .outputs["configuration"]
        .artifact_ids[0]
        .clone();
    let first_configuration = cas.get_artifact(&first_configuration_id).unwrap();
    assert_eq!(
        first_configuration.payload["light_recipe_id"],
        "context_retrieval_dedup"
    );
    let candidate_execution_id =
        first_configuration.payload["candidate_execution_configuration_id"]
            .as_str()
            .unwrap();
    assert!(
        first_configuration.payload["baseline_execution_configuration_id"].is_null(),
        "instructions are no longer transported as baseline arm data"
    );
    let candidate_execution = cas.get_artifact(candidate_execution_id).unwrap();
    assert_ne!(
        candidate_execution.payload["original_package_digest"],
        candidate_execution.payload["package_digest"],
        "the approved candidate must identify changed package bytes"
    );
    for suffix in ["_baseline", "_candidate"] {
        let (_, invocation) = completed_projection
            .execution
            .as_ref()
            .unwrap()
            .invocations
            .iter()
            .find(|(node, _)| node.ends_with(suffix))
            .map(|(_, value)| value)
            .unwrap();
        assert!(
            !invocation.inputs.contains_key("execution_configuration"),
            "package instructions must not be duplicated as arm business data"
        );
    }
    let first_invalidation = first_configuration.payload["light_invalidation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let proposal_id = completed["result"]["outputs"]["proposal"]["artifact_ids"][0]
        .as_str()
        .unwrap();
    let proposal = cas.get_artifact(proposal_id).unwrap();
    assert_eq!(proposal.artifact_type, "af/OptimizationProposal@1");
    assert_eq!(proposal.payload["recipe_id"], "context_retrieval_dedup");
    let replay = command_json(&repo, &state, &["task", "run", task, "--execute"]);
    assert_eq!(
        replay["attempts"], 5,
        "replay made no fresh author or trial call"
    );

    let delivered = temp.path().join("light-delivery");
    let delivery = command_json(
        &repo,
        &state,
        &[
            "task",
            "deliver",
            task,
            "--repo",
            repo.to_str().unwrap(),
            "--branch",
            "af/light-adoption",
            "--worktree",
            delivered.to_str().unwrap(),
            "--confirm",
            task,
        ],
    );
    assert_eq!(delivery["outcome"]["kind"], "delivered", "{delivery:#}");
    assert_eq!(
        std::fs::read_to_string(
            delivered.join(".af/optimization/optimization-check-candidate/instructions.md")
        )
        .unwrap(),
        candidate_execution.payload["instructions"]
            .as_str()
            .unwrap(),
        "delivered configuration must equal the exact measured candidate bytes"
    );
    let projection = review_store::EventStore::open_read_only(state.join("events.sqlite"))
        .unwrap()
        .task_projection(&cas, task)
        .unwrap()
        .unwrap();
    let terminal_record_id = &projection.deliveries.last().unwrap().0;
    let terminal_record = cas.get_artifact(terminal_record_id).unwrap();
    assert!(terminal_record.input_artifacts.iter().any(|id| {
        cas.get_artifact(id)
            .is_ok_and(|artifact| artifact.artifact_type == "af/OptimizationAdoptionReceipt@1")
    }));

    assert!(
        Command::new("git")
            .current_dir(&delivered)
            .args(["add", "-A"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&delivered)
            .args(["commit", "-qm", "adopt exact light optimization"])
            .status()
            .unwrap()
            .success()
    );
    let exact_adoption_commit = String::from_utf8(
        Command::new("git")
            .current_dir(&delivered)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    let equivalent = command_json(
        &delivered,
        &state,
        &[
            "task",
            "observe-adoption",
            task,
            "--commit",
            "HEAD",
            "--workload",
            "fixture-v1",
            "--model",
            "command-workers-v1",
            "--environment",
            "fixture-environment-v1",
            "--repo",
            delivered.to_str().unwrap(),
        ],
    );
    assert_eq!(equivalent["observation"]["equivalence"], "equivalent");
    let equivalent_replay = command_json(
        &delivered,
        &state,
        &[
            "task",
            "observe-adoption",
            task,
            "--commit",
            "HEAD",
            "--workload",
            "fixture-v1",
            "--model",
            "command-workers-v1",
            "--environment",
            "fixture-environment-v1",
            "--repo",
            delivered.to_str().unwrap(),
        ],
    );
    assert_eq!(
        equivalent_replay["observation_id"], equivalent["observation_id"],
        "replaying unchanged adoption evidence must not create another Store event"
    );

    std::fs::write(
        delivered.join(".af/optimization/optimization-check-candidate/instructions.md"),
        "edited after delivery\n",
    )
    .unwrap();
    assert!(
        Command::new("git")
            .current_dir(&delivered)
            .args(["add", "-A"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&delivered)
            .args(["commit", "-qm", "edit adopted optimization"])
            .status()
            .unwrap()
            .success()
    );
    let edited = command_json(
        &delivered,
        &state,
        &[
            "task",
            "observe-adoption",
            task,
            "--commit",
            "HEAD",
            "--workload",
            "fixture-v2",
            "--model",
            "command-workers-v1",
            "--environment",
            "fixture-environment-v1",
            "--repo",
            delivered.to_str().unwrap(),
        ],
    );
    assert_eq!(edited["observation"]["equivalence"], "edited");

    std::fs::write(
        repo.join("source-invalidation.txt"),
        "changed source identity\n",
    )
    .unwrap();
    assert!(
        Command::new("git")
            .current_dir(&repo)
            .args(["add", "-A"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&repo)
            .args(["commit", "-qm", "change recipe invalidation source"])
            .status()
            .unwrap()
            .success()
    );
    let invalidated = af(&repo, &state, &["--strategy", "light", "--execute"]);
    assert_eq!(invalidated["phase"]["reason"], "needs_plan_review");
    let invalidated_task = invalidated["task_id"].as_str().unwrap();
    let invalidated_projection =
        review_store::EventStore::open_read_only(state.join("events.sqlite"))
            .unwrap()
            .task_projection(&cas, invalidated_task)
            .unwrap()
            .unwrap();
    let invalidated_configuration_id = &invalidated_projection.execution.as_ref().unwrap().outputs
        ["root.nodes.prepare"]
        .1
        .outputs["configuration"]
        .artifact_ids[0];
    let invalidated_configuration = cas.get_artifact(invalidated_configuration_id).unwrap();
    assert_ne!(
        invalidated_configuration.payload["light_invalidation_id"], first_invalidation,
        "a changed source must invalidate the installed recipe identity"
    );

    let propose_root = repo.join(".af/optimization/optimization-light-propose");
    let propose_worker = propose_root.join("worker.py");
    let policy_path = repo.join(".af/optimization-policy.json");
    let mut policy: Value = serde_json::from_slice(&std::fs::read(&policy_path).unwrap()).unwrap();
    policy["light_economics"]["objective_exception"] = Value::Null;
    std::fs::write(&policy_path, serde_json::to_vec(&policy).unwrap()).unwrap();
    let catalog_path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value = std::fs::read_to_string(&catalog_path)
        .unwrap()
        .parse()
        .unwrap();
    catalog["packages"]["builtin/optimization-light-propose"]["digest"] = toml::Value::String(
        review_config::lock::package_digest("builtin/optimization-light-propose", &propose_root)
            .unwrap(),
    );
    std::fs::write(&catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    assert!(
        Command::new("git")
            .current_dir(&repo)
            .args(["add", "-A"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&repo)
            .args(["commit", "-qm", "remove objective exception"])
            .status()
            .unwrap()
            .success()
    );
    let uneconomic = command_json(
        &repo,
        &state,
        &["self", "optimize", "--strategy", "light", "--execute"],
    );
    assert_eq!(uneconomic["phase"]["reason"], "needs_plan_review");
    let uneconomic_task = uneconomic["task_id"].as_str().unwrap();
    let payload = temp.path().join("uneconomic.payload");
    let signature = temp.path().join("uneconomic.minisig");
    command_json(
        &repo,
        &state,
        &[
            "task",
            "decision-payload",
            uneconomic_task,
            "--developer",
            "owner",
            "--decision",
            "approved",
            "--reason",
            "measure economics before adoption",
            "--output",
            payload.to_str().unwrap(),
        ],
    );
    let bytes = std::fs::read(&payload).unwrap();
    std::fs::write(
        &signature,
        minisign::sign(
            Some(&key.pk),
            &key.sk,
            bytes.as_slice(),
            Some("economics"),
            None,
        )
        .unwrap()
        .into_string(),
    )
    .unwrap();
    command_json(
        &repo,
        &state,
        &[
            "task",
            "approve",
            uneconomic_task,
            "--payload",
            payload.to_str().unwrap(),
            "--signature",
            signature.to_str().unwrap(),
        ],
    );
    let withheld = command_json(
        &repo,
        &state,
        &["task", "run", uneconomic_task, "--execute"],
    );
    assert_eq!(withheld["result"]["acceptance"], "inconclusive");
    let withheld_id = withheld["result"]["outputs"]["result"]["artifact_ids"][0]
        .as_str()
        .unwrap();
    let withheld_result = cas.get_artifact(withheld_id).unwrap();
    assert_eq!(withheld_result.payload["adoption_offered"], false);
    assert_eq!(withheld_result.payload["conclusion"], "recommendation_only");

    let original_worker = std::fs::read_to_string(&propose_worker).unwrap();
    let binding_worker = original_worker.replace(
        "    'recipe_id': selected,\n",
        "    'recipe_id': selected,\n    'candidate_binding': {'package':'project/worker','provider_kind':'codex','model':'gpt-test','effort':'high'},\n",
    );
    assert_ne!(binding_worker, original_worker);
    std::fs::write(&propose_worker, binding_worker).unwrap();
    let mut catalog: toml::Value = std::fs::read_to_string(&catalog_path)
        .unwrap()
        .parse()
        .unwrap();
    catalog["packages"]["builtin/optimization-light-propose"]["digest"] = toml::Value::String(
        review_config::lock::package_digest("builtin/optimization-light-propose", &propose_root)
            .unwrap(),
    );
    std::fs::write(&catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    assert!(
        Command::new("git")
            .current_dir(&repo)
            .args(["add", "-A"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&repo)
            .args(["commit", "-qm", "refuse unexecuted candidate binding"])
            .status()
            .unwrap()
            .success()
    );
    let unexecuted_binding = command_json(
        &repo,
        &state,
        &["self", "optimize", "--strategy", "light", "--execute"],
    );
    assert_eq!(
        unexecuted_binding["phase"]["kind"], "finished",
        "{unexecuted_binding:#}"
    );
    assert_eq!(unexecuted_binding["result"]["acceptance"], "inconclusive");
    assert_eq!(
        unexecuted_binding["attempts"], 2,
        "a binding that cannot be installed is refused before either protected arm"
    );
    std::fs::write(&propose_worker, original_worker).unwrap();

    let cache_root = temp.path().join("approved-cargo-cache");
    let cached_crate = cache_root.join("registry/cache/index/example.crate");
    std::fs::create_dir_all(cached_crate.parent().unwrap()).unwrap();
    std::fs::write(&cached_crate, b"bounded cached crate bytes").unwrap();
    let cache_policy = temp.path().join("cache-policy.toml");
    std::fs::write(
        &cache_policy,
        format!(
            "version = 1\n[cache.cargo]\nsource = {cache_root:?}\nmax_bytes = 1048576\nmax_files = 100\nmax_copy_bytes = 1048576\n"
        ),
    )
    .unwrap();
    std::fs::write(
        repo.join(".af/cache-history.jsonl"),
        json!({"observed_unix_ms":"2","attribution":{"project_id":project,"case_family":"development_cache","execution_id":"prior-cache"},"tokens":{"cumulative_key":"prior-cache","usage":{"input_tokens":"10","output_tokens":"1","chargeable_tokens":"11"},"status":"exact","outer_session":false},"caches":[{"kind":"cargo","eligible":true,"result":"hit","temperature":"warm","invalidation_id":"Cargo.lock:fixture","bytes_reused":"4096","tokens_reused":"0","lookup_ms":"2","warmup_ms":"1"}],"outcome":{"outcome":"verified","retries":0,"repairs":0,"later_defects":0}}).to_string()+"\n",
    )
    .unwrap();
    std::fs::write(
        repo.join(".af/optimization-sources.toml"),
        toml::to_string(&json!({
            "schema":"af.optimization-sources/1", "project_id":project,
            "sources":[{"adapter":"external","path":"cache-history.jsonl","source_id":"cache-fixture","execution_id":"cache-session"}]
        }))
        .unwrap(),
    )
    .unwrap();
    let mut policy: Value = serde_json::from_slice(&std::fs::read(&policy_path).unwrap()).unwrap();
    policy["writable_paths"] = json!([".af/cache/cargo.json"]);
    policy["light_economics"]["objective_exception"] = json!("correctness");
    policy["experiment"]["recipe"] = json!("latency");
    policy["experiment"]["uncertainty_rule"] = json!("repetition_dispersion");
    policy["experiment"]["repetitions"] = json!(2);
    policy["experiment"]["minimum_families"] = json!(2);
    policy["experiment"]["cases"] = json!([
        {"family":"cache_holdout_a","membership":"holdout","input_path":"case.json"},
        {"family":"cache_holdout_b","membership":"holdout","input_path":"case-cache-b.json"}
    ]);
    std::fs::write(&policy_path, serde_json::to_vec(&policy).unwrap()).unwrap();
    std::fs::write(
        repo.join("case-cache-b.json"),
        r#"{"argument":"b","expected":"correct:b"}"#,
    )
    .unwrap();
    std::fs::write(
        repo.join("oracle.py"),
        "import json, sys\nfrom pathlib import Path\njson.loads(Path(sys.argv[1]).read_text())\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("rust-toolchain.toml"),
        std::fs::read_to_string(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rust-toolchain.toml"),
        )
        .unwrap(),
    )
    .unwrap();
    let diagnose_root = repo.join(".af/optimization/optimization-light-diagnose");
    let diagnose_worker = diagnose_root.join("worker.py");
    let diagnose_source = std::fs::read_to_string(&diagnose_worker).unwrap();
    let cache_diagnose = diagnose_source.replace(
        "selected = 'sandbox_dependency_cache' if 'sandbox_dependency_cache' in eligible else eligible[0]",
        "selected = 'sandbox_dependency_cache'",
    );
    assert_ne!(diagnose_source, cache_diagnose);
    std::fs::write(&diagnose_worker, cache_diagnose).unwrap();
    let baseline_root = repo.join(".af/optimization/optimization-check-baseline");
    let baseline_worker = baseline_root.join("worker.py");
    let baseline_source = std::fs::read_to_string(&baseline_worker).unwrap();
    std::fs::write(
        &baseline_worker,
        baseline_source.replace(
            "request = json.load(sys.stdin)",
            "request = json.load(sys.stdin)\nimport time; time.sleep(0.2)",
        ),
    )
    .unwrap();
    let ordinary_evaluator_root = repo.join(".af/task-packages/fixture/evaluator");
    let ordinary_evaluator = ordinary_evaluator_root.join("worker.py");
    let ordinary_source = std::fs::read_to_string(&ordinary_evaluator).unwrap();
    std::fs::write(
        &ordinary_evaluator,
        ordinary_source.replace(
            "request=json.load(sys.stdin)",
            "request=json.load(sys.stdin)\nimport os\nfrom pathlib import Path\nassert os.environ['CARGO_NET_OFFLINE'] == 'true'\nassert Path(os.environ['CARGO_HOME']).parts[-2:] == ('.af-cache', 'cargo')\nassert (Path(os.environ['CARGO_HOME']) / 'registry/cache/index/example.crate').read_bytes() == b'bounded cached crate bytes'",
        ),
    )
    .unwrap();
    let mut catalog: toml::Value = std::fs::read_to_string(&catalog_path)
        .unwrap()
        .parse()
        .unwrap();
    catalog["packages"]["builtin/optimization-light-propose"]["digest"] = toml::Value::String(
        review_config::lock::package_digest("builtin/optimization-light-propose", &propose_root)
            .unwrap(),
    );
    catalog["packages"]["builtin/optimization-light-diagnose"]["digest"] = toml::Value::String(
        review_config::lock::package_digest("builtin/optimization-light-diagnose", &diagnose_root)
            .unwrap(),
    );
    catalog["packages"]["builtin/optimization-check-baseline"]["digest"] = toml::Value::String(
        review_config::lock::package_digest("builtin/optimization-check-baseline", &baseline_root)
            .unwrap(),
    );
    catalog["packages"]["fixture/evaluator"]["digest"] = toml::Value::String(
        review_config::lock::package_digest("fixture/evaluator", &ordinary_evaluator_root).unwrap(),
    );
    std::fs::write(&catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    for args in [
        vec!["add", "-A"],
        vec!["commit", "-qm", "exercise admitted sandbox cache"],
    ] {
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let cache_waiting = command_json(
        &repo,
        &state,
        &["self", "optimize", "--strategy", "light", "--execute"],
    );
    assert_eq!(
        cache_waiting["phase"]["reason"], "needs_plan_review",
        "{cache_waiting:#}"
    );
    let cache_task = cache_waiting["task_id"].as_str().unwrap();
    let payload = temp.path().join("cache.payload");
    let signature = temp.path().join("cache.minisig");
    command_json(
        &repo,
        &state,
        &[
            "task",
            "decision-payload",
            cache_task,
            "--developer",
            "owner",
            "--decision",
            "approved",
            "--reason",
            "measure admitted cache preparation",
            "--output",
            payload.to_str().unwrap(),
        ],
    );
    let bytes = std::fs::read(&payload).unwrap();
    std::fs::write(
        &signature,
        minisign::sign(
            Some(&key.pk),
            &key.sk,
            bytes.as_slice(),
            Some("cache candidate"),
            None,
        )
        .unwrap()
        .into_string(),
    )
    .unwrap();
    command_json(
        &repo,
        &state,
        &[
            "task",
            "approve",
            cache_task,
            "--payload",
            payload.to_str().unwrap(),
            "--signature",
            signature.to_str().unwrap(),
        ],
    );
    let cache_completed = command_json_with_env(
        &repo,
        &state,
        &["task", "run", cache_task, "--execute"],
        &[
            ("AF_CACHE_POLICY_FILE", &cache_policy),
            ("AF_TEST_CLOCK_QUANTUM_MS", Path::new("4")),
        ],
    );
    assert_eq!(
        cache_completed["result"]["execution"], "completed",
        "{cache_completed:#}"
    );
    let cache_result_id = cache_completed["result"]["outputs"]["result"]["artifact_ids"][0]
        .as_str()
        .unwrap();
    let cache_result = cas.get_artifact(cache_result_id).unwrap();
    let cache_comparison_id = cache_completed["result"]["outputs"]["comparison"]["artifact_ids"][0]
        .as_str()
        .unwrap();
    let cache_comparison = cas.get_artifact(cache_comparison_id).unwrap();
    assert_eq!(
        cache_result.payload["experiment_conclusion"], "accepted",
        "comparison={:#}",
        cache_comparison.payload
    );
    assert_eq!(cache_result.payload["adoption_offered"], true);
    assert_eq!(
        cache_result.payload["economics"]["accounting_complete"],
        true
    );
    assert!(
        cache_result.payload["economics"]
            .get("missing_measurements")
            .is_none(),
        "quantized observed timestamps must produce exact matched economics"
    );
    assert_eq!(cache_comparison.payload["conclusion"], "accepted");
    assert_eq!(
        cache_comparison.payload["trials"].as_array().unwrap().len(),
        8,
        "repeated cold control and prepared-cache candidate remain in the comparison"
    );
    let cache_projection = review_store::EventStore::open_read_only(state.join("events.sqlite"))
        .unwrap()
        .task_projection(&cas, cache_task)
        .unwrap()
        .unwrap();
    let cache_accounting = cache_projection
        .execution
        .as_ref()
        .unwrap()
        .attempt_accounting();
    let candidate_accounting = cache_accounting
        .iter()
        .find(|attempt| attempt.reservation.node.ends_with("_candidate"))
        .unwrap();
    let runtime_id = candidate_accounting
        .raw_artifact_ids
        .iter()
        .find(|id| {
            cas.get_optional_artifact(id)
                .unwrap()
                .is_some_and(|artifact| {
                    artifact.artifact_type == "af/TaskRuntimeEvidence@1"
                        && artifact.payload["caches"][0]["bytes_available"]
                            .as_u64()
                            .is_some_and(|bytes| bytes > 0)
                })
        })
        .unwrap_or_else(|| {
            let diagnostic = match candidate_accounting.result.as_ref().unwrap() {
                review_core::task::execution::TaskAttemptResultV1::Failed {
                    diagnostic_id,
                    ..
                }
                | review_core::task::execution::TaskAttemptResultV1::Abandoned {
                    diagnostic_id,
                } => cas.get_json(diagnostic_id).unwrap(),
                review_core::task::execution::TaskAttemptResultV1::Succeeded { .. } => {
                    json!(null)
                }
            };
            panic!(
                "candidate cache Attempt retained no populated runtime evidence: {candidate_accounting:#?}; diagnostic={diagnostic:#}"
            )
        });
    let runtime = cas.get_artifact(runtime_id).unwrap();
    assert_eq!(runtime.payload["caches"][0]["kind"], "cargo");
    assert_eq!(runtime.payload["caches"][0]["result"], "prepared");
    assert!(
        runtime.payload["caches"][0]["bytes_available"]
            .as_u64()
            .unwrap()
            > 0
    );
    let cache_replay = command_json_with_env(
        &repo,
        &state,
        &["task", "run", cache_task, "--execute"],
        &[
            ("AF_CACHE_POLICY_FILE", &cache_policy),
            ("AF_TEST_CLOCK_QUANTUM_MS", Path::new("4")),
        ],
    );
    assert_eq!(cache_replay["attempts"], cache_completed["attempts"]);

    let cache_delivery = temp.path().join("cache-delivery");
    let delivered_cache = command_json(
        &repo,
        &state,
        &[
            "task",
            "deliver",
            cache_task,
            "--repo",
            repo.to_str().unwrap(),
            "--branch",
            "af/cache-adoption",
            "--worktree",
            cache_delivery.to_str().unwrap(),
            "--confirm",
            cache_task,
        ],
    );
    assert_eq!(delivered_cache["outcome"]["kind"], "delivered");
    assert_eq!(
        std::fs::read_to_string(cache_delivery.join(".af/cache/cargo.json"))
            .unwrap()
            .trim(),
        r#"{"schema":"af.sandbox-cache-selection/1","kind":"cargo"}"#
    );
    for args in [
        vec!["add", "-A"],
        vec!["commit", "-qm", "adopt exact cache selection"],
    ] {
        assert!(
            Command::new("git")
                .current_dir(&cache_delivery)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let cache_adoption = command_json(
        &cache_delivery,
        &state,
        &[
            "task",
            "observe-adoption",
            cache_task,
            "--commit",
            "HEAD",
            "--workload",
            "ordinary-cache-fixture",
            "--model",
            "command-workers-v1",
            "--environment",
            "offline-cargo-fixture-v1",
            "--repo",
            cache_delivery.to_str().unwrap(),
        ],
    );
    assert_eq!(cache_adoption["observation"]["equivalence"], "equivalent");
    let ordinary = command_json_with_env(
        &cache_delivery,
        &state,
        &["task", "start", "--execute", "--file", "ticket.json"],
        &[("AF_CACHE_POLICY_FILE", &cache_policy)],
    );
    assert_eq!(
        ordinary["result"]["acceptance"], "satisfied",
        "{ordinary:#}"
    );
    let ordinary_snapshot_id = ordinary["result"]["outputs"]["snapshot"]["snapshot_id"]
        .as_str()
        .unwrap();
    let (_, ordinary_manifest) =
        review_source_git::task::read_snapshot(&cas, ordinary_snapshot_id).unwrap();
    assert!(
        ordinary_manifest.entries.iter().all(|entry| {
            !review_source_git::decode_path(&entry.path).starts_with(b".af-cache/")
        })
    );
    assert!(!cache_delivery.join(".af-cache").exists());

    std::fs::remove_file(repo.join("rust-toolchain.toml")).unwrap();
    for args in [
        vec!["add", "-A"],
        vec!["commit", "-qm", "exercise unknown cache toolchain"],
    ] {
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let unknown_waiting = command_json(
        &repo,
        &state,
        &["self", "optimize", "--strategy", "light", "--execute"],
    );
    assert_eq!(unknown_waiting["phase"]["reason"], "needs_plan_review");
    let unknown_task = unknown_waiting["task_id"].as_str().unwrap();
    let payload = temp.path().join("unknown-toolchain.payload");
    let signature = temp.path().join("unknown-toolchain.minisig");
    command_json(
        &repo,
        &state,
        &[
            "task",
            "decision-payload",
            unknown_task,
            "--developer",
            "owner",
            "--decision",
            "approved",
            "--reason",
            "prove unknown toolchain withholds cache adoption",
            "--output",
            payload.to_str().unwrap(),
        ],
    );
    let bytes = std::fs::read(&payload).unwrap();
    std::fs::write(
        &signature,
        minisign::sign(
            Some(&key.pk),
            &key.sk,
            bytes.as_slice(),
            Some("unknown toolchain"),
            None,
        )
        .unwrap()
        .into_string(),
    )
    .unwrap();
    command_json(
        &repo,
        &state,
        &[
            "task",
            "approve",
            unknown_task,
            "--payload",
            payload.to_str().unwrap(),
            "--signature",
            signature.to_str().unwrap(),
        ],
    );
    let unknown = command_json_with_env(
        &repo,
        &state,
        &["task", "run", unknown_task, "--execute"],
        &[
            ("AF_CACHE_POLICY_FILE", &cache_policy),
            ("AF_TEST_CLOCK_QUANTUM_MS", Path::new("4")),
        ],
    );
    assert_eq!(
        unknown["result"]["acceptance"], "inconclusive",
        "{unknown:#}"
    );
    let unknown_comparison_id = unknown["result"]["outputs"]["comparison"]["artifact_ids"][0]
        .as_str()
        .unwrap_or_else(|| panic!("unknown-toolchain run omitted comparison: {unknown:#}"));
    let unknown_comparison = cas.get_artifact(unknown_comparison_id).unwrap();
    assert_eq!(unknown_comparison.payload["conclusion"], "inconclusive");
    assert_eq!(
        unknown_comparison.payload["reason"],
        "unsupported_measurements"
    );
    assert!(
        unknown_comparison.payload["trials"]
            .as_array()
            .unwrap()
            .iter()
            .all(|trial| trial["missing_measurements"]
                .as_array()
                .unwrap()
                .iter()
                .any(|name| name == "cache_toolchain_identity")),
        "unknown toolchain was not retained on every measured arm: {unknown_comparison:#?}"
    );
    let refused_cache_delivery = temp.path().join("unknown-cache-delivery");
    let output = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(&repo)
        .args([
            "task",
            "deliver",
            unknown_task,
            "--repo",
            repo.to_str().unwrap(),
            "--branch",
            "af/unknown-cache",
            "--worktree",
            refused_cache_delivery.to_str().unwrap(),
            "--confirm",
            unknown_task,
            "--json",
            "--state",
            state.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!refused_cache_delivery.exists());

    let observed_task = command_json(
        &delivered,
        &state,
        &[
            "task",
            "observe-adoption",
            task,
            "--commit",
            &exact_adoption_commit,
            "--workload",
            "fixture-with-task-evidence",
            "--model",
            "attested-command-workers-v1",
            "--environment",
            "attested-fixture-environment-v1",
            "--evidence-task",
            cache_task,
            "--repo",
            delivered.to_str().unwrap(),
        ],
    );
    assert!(
        observed_task["task_evidence_id"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    let adoption_projection = command_json(
        &delivered,
        &state,
        &["task", "show", task, "--repo", delivered.to_str().unwrap()],
    );
    assert_eq!(adoption_projection["schema"], "af/task-inspection@11");
    let projected = adoption_projection["adoption_observations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| !entry["task_evidence"].as_array().unwrap().is_empty())
        .unwrap();
    assert_eq!(
        projected["task_evidence"][0]["record"]["observed_task_id"],
        cache_task
    );
    assert_eq!(
        projected["task_evidence"][0]["record"]["causal_claim"],
        false
    );

    let worker = std::fs::read_to_string(&propose_worker).unwrap().replace(
        "'recipe_id': selected",
        "'recipe_id': 'deterministic_artifact_reuse'",
    );
    std::fs::write(&propose_worker, worker).unwrap();
    let mut catalog: toml::Value = std::fs::read_to_string(&catalog_path)
        .unwrap()
        .parse()
        .unwrap();
    catalog["packages"]["builtin/optimization-light-propose"]["digest"] = toml::Value::String(
        review_config::lock::package_digest("builtin/optimization-light-propose", &propose_root)
            .unwrap(),
    );
    std::fs::write(&catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    assert!(
        Command::new("git")
            .current_dir(&repo)
            .args(["add", "-A"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&repo)
            .args(["commit", "-qm", "propose unsupported recipe"])
            .status()
            .unwrap()
            .success()
    );
    let unsupported = command_json(
        &repo,
        &state,
        &["self", "optimize", "--strategy", "light", "--execute"],
    );
    assert_eq!(unsupported["phase"]["kind"], "finished", "{unsupported:#}");
    assert_eq!(
        unsupported["result"]["acceptance"], "inconclusive",
        "{unsupported:#}"
    );
    assert_eq!(
        unsupported["attempts"], 2,
        "an unsupported recipe is refused before any protected child is prepared"
    );
}

fn controlled_candidate(edit_path: &str, edit_text: &str, accepted: bool) {
    controlled_candidate_with_inputs(edit_path, edit_text, accepted, false);
}

fn controlled_candidate_with_inputs(
    edit_path: &str,
    edit_text: &str,
    accepted: bool,
    duplicate_inputs: bool,
) {
    let temp = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(temp.path(), "pagination");
    let key = install_candidate_optimizer_catalog(&repo);
    let project = format!("sha256:{}", "3".repeat(64));
    std::fs::write(repo.join(".af/optimization-sources.toml"), toml::to_string(&json!({
        "schema":"af.optimization-sources/1", "project_id":project,
        "sources":[{"adapter":"af","path":"history.jsonl","source_id":"fixture","execution_id":"session"}]
    })).unwrap()).unwrap();
    std::fs::write(repo.join(".af/history.jsonl"), json!({"observed_unix_ms":"1","attribution":{"project_id":project,"case_family":"prior","execution_id":"prior"},"outcome":{"outcome":"verified","retries":0,"repairs":0,"later_defects":0}}).to_string()+"\n").unwrap();
    std::fs::write(
        repo.join(".af/optimization-policy.json"),
        json!({
            "schema":"af.optimization-configuration-policy/1", "writable_paths":["harness.py"],
            "checks":{"oracle":"oracle.py"}, "harness_path":"harness.py",
            "experiment":{"recipe":"deterministic_correction","uncertainty_rule":"deterministic","repetitions":2,
                "minimum_families":2,"token_increase_ceiling_bps":0,
                "cases":[
                    {"family":"protected_correction_a","membership":"holdout","input_path":"case-a.json"},
                    {"family":"protected_correction_b","membership":"holdout","input_path":"case-b.json"}
                ]}
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        repo.join("case-a.json"),
        r#"{"argument":"a","expected":"correct:a"}"#,
    )
    .unwrap();
    std::fs::write(
        repo.join("case-b.json"),
        r#"{"argument":"b","expected":"correct:b"}"#,
    )
    .unwrap();
    if duplicate_inputs {
        std::fs::write(
            repo.join("case-b.json"),
            r#"{ "expected": "correct:a", "argument": "a" }"#,
        )
        .unwrap();
    }
    std::fs::write(repo.join("harness.py"), "print('broken')\n").unwrap();
    std::fs::write(repo.join("oracle.py"), "import json, subprocess, sys\nfrom pathlib import Path\ncase = json.loads(Path(sys.argv[1]).read_text())\nr = subprocess.run([sys.executable, '-B', 'harness.py', case['argument']], capture_output=True, text=True)\nassert r.returncode == 0 and r.stdout.strip() == case['expected']\n").unwrap();
    for args in [
        vec!["add", "-A"],
        vec!["commit", "-qm", "protected optimization fixture"],
    ] {
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let proposal = temp.path().join("candidate.json");
    std::fs::write(&proposal, json!({"schema":"af.optimization-candidate/1", "edits":{(edit_path):{"text":edit_text,"executable":false}}}).to_string()).unwrap();
    let waiting = command_json(
        &repo,
        &state,
        &[
            "self",
            "optimize",
            "--candidate",
            proposal.to_str().unwrap(),
            "--execute",
        ],
    );
    assert_eq!(waiting["attempts"], 0);
    let task = waiting["task_id"].as_str().unwrap();
    let result = if edit_path == "oracle.py" || duplicate_inputs {
        assert_eq!(waiting["phase"]["kind"], "finished", "{waiting:#}");
        waiting.clone()
    } else {
        assert_eq!(
            waiting["phase"]["reason"], "needs_plan_review",
            "{waiting:#}"
        );
        let payload = temp.path().join("approve.payload");
        let signature = temp.path().join("approve.minisig");
        command_json(
            &repo,
            &state,
            &[
                "task",
                "decision-payload",
                task,
                "--developer",
                "owner",
                "--decision",
                "approved",
                "--reason",
                "exact controlled candidate",
                "--output",
                payload.to_str().unwrap(),
            ],
        );
        let bytes = std::fs::read(&payload).unwrap();
        std::fs::write(
            &signature,
            minisign::sign(
                Some(&key.pk),
                &key.sk,
                bytes.as_slice(),
                Some("exact closure"),
                None,
            )
            .unwrap()
            .into_string(),
        )
        .unwrap();
        command_json(
            &repo,
            &state,
            &[
                "task",
                "approve",
                task,
                "--payload",
                payload.to_str().unwrap(),
                "--signature",
                signature.to_str().unwrap(),
            ],
        );
        command_json(&repo, &state, &["task", "run", task, "--execute"])
    };
    if !accepted {
        assert_ne!(result["result"]["acceptance"], "satisfied", "{result:#}");
        let delivery = temp.path().join("refused-delivery");
        let output = Command::new(env!("CARGO_BIN_EXE_af"))
            .current_dir(&repo)
            .args([
                "task",
                "deliver",
                task,
                "--branch",
                "fixture/refused",
                "--worktree",
            ])
            .arg(&delivery)
            .args(["--confirm", task, "--state"])
            .arg(&state)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!delivery.exists());
        assert_eq!(
            std::fs::read_to_string(repo.join("harness.py")).unwrap(),
            "print('broken')\n"
        );
        if edit_path == "oracle.py" || duplicate_inputs {
            assert_eq!(result["attempts"], 0, "{result:#}");
        } else {
            assert_eq!(result["attempts"], 9, "{result:#}");
        }
        return;
    }
    if result["result"]["acceptance"] != "satisfied" {
        let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
        for output in result["result"]["outputs"]
            .as_object()
            .into_iter()
            .flat_map(|v| v.values())
        {
            for id in output["artifact_ids"].as_array().into_iter().flatten() {
                eprintln!(
                    "output evidence {}",
                    cas.get_artifact(id.as_str().unwrap()).unwrap().payload
                );
            }
        }
        for record in result["execution_records"].as_array().into_iter().flatten() {
            if record["record"]["kind"] == "settled" {
                eprintln!("settlement {}", record["record"]);
            }
        }
    }
    assert_eq!(result["result"]["acceptance"], "satisfied", "{result:#}");
    assert_eq!(result["attempts"], 9);
    let replay = command_json(&repo, &state, &["task", "run", task, "--execute"]);
    assert_eq!(replay["attempts"], 9);
    let delivery = temp.path().join("delivered");
    command_json(
        &repo,
        &state,
        &[
            "task",
            "deliver",
            task,
            "--branch",
            "fixture/optimized",
            "--worktree",
            delivery.to_str().unwrap(),
            "--confirm",
            task,
        ],
    );
    assert_eq!(
        std::fs::read_to_string(delivery.join("harness.py")).unwrap(),
        "import sys\nprint('correct:' + sys.argv[1])\n"
    );
    assert_eq!(
        std::fs::read(repo.join("oracle.py")).unwrap(),
        std::fs::read(delivery.join("oracle.py")).unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("harness.py")).unwrap(),
        "print('broken')\n"
    );

    // Round-trip the actual accepted @10 receipt through M1. The four baseline children are
    // real failed Attempts; all nine charges remain attributable once and replay added none.
    std::fs::write(
        repo.join(".af/controlled-task.jsonl"),
        serde_json::to_string(&result).unwrap() + "\n",
    )
    .unwrap();
    install_optimizer_catalog(&repo);
    let sources = json!({"schema":"af.optimization-sources/1","project_id":project,
        "sources":[{"adapter":"af","path":"controlled-task.jsonl","source_id":"actual-controlled-experiment","execution_id":task,"attest_project":true}]});
    std::fs::write(
        repo.join(".af/optimization-sources.toml"),
        toml::to_string(&sources).unwrap(),
    )
    .unwrap();
    for args in [
        vec!["add", "-A"],
        vec!["commit", "-qm", "import controlled experiment"],
    ] {
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let analysis = af(&repo, &state, &["--execute"]);
    assert_eq!(analysis["result"]["acceptance"], "satisfied");
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let economics_id = analysis["result"]["outputs"]["economics"]["artifact_ids"][0]
        .as_str()
        .unwrap();
    let economics = cas.get_json(economics_id).unwrap();
    assert_eq!(
        economics["payload"]["failed_or_incomplete"], 4,
        "{economics:#}"
    );
    assert_eq!(economics["payload"]["repeated_failures"], 0);
    assert_eq!(economics["payload"]["verified"], 2);
    assert_eq!(economics["payload"]["af_usage"]["chargeable_tokens"], "0");
}

#[test]
fn approved_experiment_that_exceeds_parent_resources_becomes_explicit_non_success() {
    let temp = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(temp.path(), "pagination");
    let key = install_candidate_optimizer_catalog(&repo);
    let project = format!("sha256:{}", "3".repeat(64));
    std::fs::write(
        repo.join(".af/optimization-sources.toml"),
        toml::to_string(&json!({
            "schema":"af.optimization-sources/1", "project_id":project,
            "sources":[{"adapter":"af","path":"history.jsonl","source_id":"fixture","execution_id":"session"}]
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        repo.join(".af/history.jsonl"),
        json!({"observed_unix_ms":"1","attribution":{"project_id":project,"case_family":"prior","execution_id":"prior"},"outcome":{"outcome":"verified","retries":0,"repairs":0,"later_defects":0}}).to_string()+"\n",
    )
    .unwrap();
    let cases = (0..42)
        .map(|index| {
            let input_path = format!("case-{index}.json");
            std::fs::write(repo.join(&input_path), json!({"argument":index.to_string(),"expected":format!("correct:{index}")}).to_string()).unwrap();
            json!({"family":format!("family_{index}"),"membership":"holdout","input_path":input_path})
        })
        .collect::<Vec<_>>();
    std::fs::write(
        repo.join(".af/optimization-policy.json"),
        json!({
            "schema":"af.optimization-configuration-policy/1", "writable_paths":["harness.py"],
            "checks":{"oracle":"oracle.py"}, "harness_path":"harness.py",
            "experiment":{"recipe":"deterministic_correction","uncertainty_rule":"deterministic","repetitions":1,
                "minimum_families":1,"token_increase_ceiling_bps":0,"cases":cases}
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(repo.join("harness.py"), "print('broken')\n").unwrap();
    std::fs::write(repo.join("oracle.py"), "import json, subprocess, sys\nfrom pathlib import Path\ncase = json.loads(Path(sys.argv[1]).read_text())\nr = subprocess.run([sys.executable, '-B', 'harness.py', case['argument']], capture_output=True, text=True)\nassert r.returncode == 0 and r.stdout.strip() == case['expected']\n").unwrap();
    for args in [
        vec!["add", "-A"],
        vec!["commit", "-qm", "resource bounded optimization fixture"],
    ] {
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let proposal = temp.path().join("candidate.json");
    std::fs::write(
        &proposal,
        json!({"schema":"af.optimization-candidate/1", "edits":{"harness.py":{"text":"print('correct')\n","executable":false}}}).to_string(),
    )
    .unwrap();
    let waiting = command_json(
        &repo,
        &state,
        &[
            "self",
            "optimize",
            "--candidate",
            proposal.to_str().unwrap(),
            "--execute",
        ],
    );
    assert_eq!(waiting["attempts"], 0);
    assert_eq!(
        waiting["phase"]["reason"], "needs_plan_review",
        "{waiting:#}"
    );
    let task = waiting["task_id"].as_str().unwrap();
    let payload = temp.path().join("resource.payload");
    let signature = temp.path().join("resource.minisig");
    command_json(
        &repo,
        &state,
        &[
            "task",
            "decision-payload",
            task,
            "--developer",
            "owner",
            "--decision",
            "approved",
            "--reason",
            "resource boundary fixture",
            "--output",
            payload.to_str().unwrap(),
        ],
    );
    let bytes = std::fs::read(&payload).unwrap();
    std::fs::write(
        &signature,
        minisign::sign(
            Some(&key.pk),
            &key.sk,
            bytes.as_slice(),
            Some("resource boundary"),
            None,
        )
        .unwrap()
        .into_string(),
    )
    .unwrap();
    let refused = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(&repo)
        .args([
            "task",
            "approve",
            task,
            "--payload",
            payload.to_str().unwrap(),
            "--signature",
            signature.to_str().unwrap(),
            "--json",
            "--state",
            state.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let store = review_store::EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    let projection = store.task_projection(&cas, task).unwrap().unwrap();
    assert_eq!(
        projection.phase,
        review_core::task::TaskPhaseV1::Waiting {
            reason: review_core::task::TaskWaitingReasonV1::NeedsHuman
        }
    );
    let execution = projection.execution.unwrap();
    assert_eq!(execution.budget.begun_attempts(), 0);
}

#[test]
fn experimental_cli_signs_registers_executes_and_imports_actual_child_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(temp.path(), "pagination");
    let key = install_candidate_optimizer_catalog(&repo);
    let project = format!("sha256:{}", "3".repeat(64));
    let config = json!({"schema":"af.optimization-sources/1","project_id":project,
        "sources":[{"adapter":"af","path":"history.jsonl","source_id":"fixture","execution_id":"session"}]});
    std::fs::write(
        repo.join(".af/optimization-sources.toml"),
        toml::to_string(&config).unwrap(),
    )
    .unwrap();
    std::fs::write(
        repo.join(".af/history.jsonl"),
        json!({"observed_unix_ms":"1","attribution":{"project_id":project,"case_family":"prior","execution_id":"prior"},"outcome":{"outcome":"verified","retries":0,"repairs":0,"later_defects":0}}).to_string()+"\n",
    )
    .unwrap();
    for args in [
        vec!["add", "-A"],
        vec!["commit", "-qm", "candidate optimizer"],
    ] {
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }

    let waiting = af(&repo, &state, &["--experiment", "--execute"]);
    assert_eq!(waiting["schema"], "af/task-inspection@10");
    assert_eq!(waiting["attempts"], 0);
    assert_eq!(waiting["phase"]["reason"], "needs_plan_review");
    let task_id = waiting["task_id"].as_str().unwrap();

    let payload = temp.path().join("experiment.payload");
    let signature = temp.path().join("experiment.minisig");
    let authorization = command_json(
        &repo,
        &state,
        &[
            "task",
            "decision-payload",
            task_id,
            "--developer",
            "owner",
            "--decision",
            "approved",
            "--reason",
            "Reviewed bounded command arms",
            "--output",
            payload.to_str().unwrap(),
        ],
    );
    for case in [
        "stale_preparation",
        "wrong_slot",
        "changed_dependency",
        "expired",
        "revoked_key",
    ] {
        let mut request = authorization["payload"].clone();
        match case {
            "stale_preparation" => {
                request["prepared_id"] = json!(format!("sha256:{}", "0".repeat(64)))
            }
            "wrong_slot" => request["slot_id"] = json!(format!("sha256:{}", "0".repeat(64))),
            "changed_dependency" => {
                request["compiled_child_plan_id"] = json!(format!("sha256:{}", "0".repeat(64)))
            }
            "expired" => request["expires_unix_ms"] = json!(1),
            "revoked_key" => {}
            _ => unreachable!(),
        }
        let mut message = b"af/experiment-plan-authorization/1\n".to_vec();
        message.extend(review_store::canonicalize(&request).unwrap());
        let case_payload = temp.path().join(format!("{case}.payload"));
        let case_signature = temp.path().join(format!("{case}.minisig"));
        std::fs::write(&case_payload, &message).unwrap();
        let signer = if case == "revoked_key" {
            minisign::KeyPair::generate_unencrypted_keypair().unwrap()
        } else {
            minisign::KeyPair {
                pk: key.pk.clone(),
                sk: key.sk.clone(),
            }
        };
        std::fs::write(
            &case_signature,
            minisign::sign(
                Some(&signer.pk),
                &signer.sk,
                message.as_slice(),
                Some("negative exact closure"),
                None,
            )
            .unwrap()
            .into_string(),
        )
        .unwrap();
        let refused = Command::new(env!("CARGO_BIN_EXE_af"))
            .current_dir(&repo)
            .args([
                "task",
                "approve",
                task_id,
                "--payload",
                case_payload.to_str().unwrap(),
                "--signature",
                case_signature.to_str().unwrap(),
                "--json",
                "--state",
                state.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(!refused.status.success(), "{case}");
        let check_cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
        let check_store =
            review_store::EventStore::open_read_only(state.join("events.sqlite")).unwrap();
        let untouched = check_store
            .task_projection(&check_cas, task_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            untouched.execution.unwrap().budget.begun_attempts(),
            0,
            "{case}"
        );
    }
    let forged = temp.path().join("forged.minisig");
    std::fs::write(&forged, "not a minisign signature").unwrap();
    let refused = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(&repo)
        .args([
            "task",
            "approve",
            task_id,
            "--payload",
            payload.to_str().unwrap(),
            "--signature",
            forged.to_str().unwrap(),
            "--json",
            "--state",
            state.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let check_cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let check_store =
        review_store::EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    let after_forgery = check_store
        .task_projection(&check_cas, task_id)
        .unwrap()
        .unwrap();
    assert_eq!(after_forgery.execution.unwrap().budget.begun_attempts(), 0);
    let payload_bytes = std::fs::read(&payload).unwrap();
    std::fs::write(
        &signature,
        minisign::sign(
            Some(&key.pk),
            &key.sk,
            payload_bytes.as_slice(),
            Some("exact experimental closure"),
            None,
        )
        .unwrap()
        .into_string(),
    )
    .unwrap();
    command_json(
        &repo,
        &state,
        &[
            "task",
            "approve",
            task_id,
            "--payload",
            payload.to_str().unwrap(),
            "--signature",
            signature.to_str().unwrap(),
        ],
    );
    let completed = command_json(&repo, &state, &["task", "run", task_id, "--execute"]);
    assert_eq!(completed["attempts"], 2);
    assert_eq!(completed["result"]["acceptance"], "inconclusive");
    assert_eq!(
        completed["result"]["domain_conclusion"],
        "comparison_ready_finalization_missing"
    );
    assert_eq!(
        completed["experiments"][0]["record"]["children"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let registered = completed["experiments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["kind"] == "registered")
        .unwrap();
    for child in registered["record"]["children"]
        .as_object()
        .unwrap()
        .values()
    {
        assert_eq!(child["definition"]["operator"]["operator"]["op"], "verify");
        assert_eq!(child["allowance"]["verification_attempts"], 1);
    }
    let replay = command_json(&repo, &state, &["task", "run", task_id, "--execute"]);
    assert_eq!(
        replay["attempts"], 2,
        "fresh-process replay reran a settled arm"
    );

    // A separately signed rejection is factual evidence only. It leaves the Task paused and
    // cannot register or dispatch either child.
    std::fs::OpenOptions::new()
        .append(true)
        .open(repo.join(".af/history.jsonl"))
        .unwrap()
        .write_all(
            (json!({"observed_unix_ms":"2","attribution":{"project_id":project,"case_family":"prior","execution_id":"later"},"outcome":{"outcome":"verified","retries":0,"repairs":0,"later_defects":0}}).to_string()+"\n").as_bytes(),
        )
        .unwrap();
    for args in [
        vec!["add", "-A"],
        vec!["commit", "-qm", "second experiment"],
    ] {
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let rejected_waiting = af(&repo, &state, &["--experiment", "--execute"]);
    let rejected_task = rejected_waiting["task_id"].as_str().unwrap();
    let reject_payload = temp.path().join("reject.payload");
    let reject_signature = temp.path().join("reject.minisig");
    command_json(
        &repo,
        &state,
        &[
            "task",
            "decision-payload",
            rejected_task,
            "--developer",
            "owner",
            "--decision",
            "rejected",
            "--reason",
            "Candidate authority rejected",
            "--output",
            reject_payload.to_str().unwrap(),
        ],
    );
    let reject_bytes = std::fs::read(&reject_payload).unwrap();
    std::fs::write(
        &reject_signature,
        minisign::sign(
            Some(&key.pk),
            &key.sk,
            reject_bytes.as_slice(),
            Some("rejected closure"),
            None,
        )
        .unwrap()
        .into_string(),
    )
    .unwrap();
    let rejected = command_json(
        &repo,
        &state,
        &[
            "task",
            "reject",
            rejected_task,
            "--payload",
            reject_payload.to_str().unwrap(),
            "--signature",
            reject_signature.to_str().unwrap(),
        ],
    );
    assert_eq!(rejected["attempts"], 0);
    assert_eq!(rejected["phase"]["reason"], "needs_human");
    assert!(
        rejected["experiments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| {
                entry["kind"] == "decision" && entry["record"]["decision"] == "rejected"
            })
    );

    // Feed the exact native receipt back through M1. This proves child Attempts and their host
    // time survive the real inspection/export adapter rather than a hand-authored @10 fixture.
    std::fs::write(
        repo.join(".af/experimental-task.jsonl"),
        serde_json::to_string(&completed).unwrap() + "\n",
    )
    .unwrap();
    install_optimizer_catalog(&repo);
    let sources = json!({"schema":"af.optimization-sources/1","project_id":project,
        "sources":[{"adapter":"af","path":"experimental-task.jsonl","source_id":"actual-experiment","execution_id":task_id,"attest_project":true}]});
    std::fs::write(
        repo.join(".af/optimization-sources.toml"),
        toml::to_string(&sources).unwrap(),
    )
    .unwrap();
    for args in [
        vec!["add", "-A"],
        vec!["commit", "-qm", "import experiment"],
    ] {
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let analysis = af(&repo, &state, &["--execute"]);
    assert_eq!(analysis["result"]["acceptance"], "satisfied");
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let economics_id = analysis["result"]["outputs"]["economics"]["artifact_ids"][0]
        .as_str()
        .unwrap();
    let economics = cas.get_json(economics_id).unwrap();
    assert_eq!(economics["payload"]["af_usage"]["chargeable_tokens"], "0");
    assert!(economics["payload"]["summed_work_ms"].as_str().is_some());
}

#[test]
fn self_optimize_compiles_executes_and_replays_without_model_calls() {
    let temp = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(temp.path(), "pagination");
    install_optimizer_catalog(&repo);
    let project = format!("sha256:{}", "1".repeat(64));
    let config = json!({"schema":"af.optimization-sources/1","project_id":project,
        "sources":[{"adapter":"af","path":"history.jsonl","source_id":"fixture","execution_id":"session"}]});
    std::fs::write(
        repo.join(".af/optimization-sources.toml"),
        toml::to_string(&config).unwrap(),
    )
    .unwrap();
    let record = |time: &str, charge: &str| {
        json!({"observed_unix_ms":time,
        "attribution":{"project_id":project,"case_family":"fixture","execution_id":"execution"},
        "tokens":{"cumulative_key":"invocation","usage":{"chargeable_tokens":charge},"status":"exact","outer_session":false}}).to_string()+"\n"
    };
    std::fs::write(
        repo.join(".af/history.jsonl"),
        record("1", "10") + &record("2", "20"),
    )
    .unwrap();
    for args in [
        vec!["add", "-A"],
        vec!["commit", "-qm", "optimizer fixture"],
    ] {
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let preview = af(&repo, &state, &[]);
    assert_eq!(preview["attempts"], 0);
    assert!(
        preview
            .to_string()
            .contains("operator/optimization-project")
    );
    let executed = af(&repo, &state, &["--execute"]);
    assert_eq!(executed["chargeable_tokens"], "0");
    assert_eq!(executed["result"]["acceptance"], "satisfied");
    let report_id = executed["result"]["outputs"]["report"]["artifact_ids"][0]
        .as_str()
        .unwrap();
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let report = cas.get_json(report_id).unwrap();
    assert_eq!(report["type"], "af/OptimizationReport@1");
    assert_eq!(report["payload"]["live_demonstrations"], "pending");
    let output = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(&repo)
        .args([
            "task",
            "run",
            executed["task_id"].as_str().unwrap(),
            "--state",
            state.to_str().unwrap(),
            "--execute",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let replay: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(replay["attempts"], executed["attempts"]);
    assert_eq!(replay["chargeable_tokens"], "0");
}

#[test]
fn actual_code_task_runtime_evidence_round_trips_through_native_af_capture() {
    let temp = tempfile::tempdir().unwrap();
    let (repo, state) = task_cli::fixture_named(temp.path(), "pagination");
    let task = Command::new(env!("CARGO_BIN_EXE_af"))
        .current_dir(&repo)
        .args([
            "task",
            "start",
            "--execute",
            "--file",
            "ticket.json",
            "--json",
            "--state",
            state.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        task.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&task.stdout),
        String::from_utf8_lossy(&task.stderr)
    );
    let receipt: Value = serde_json::from_slice(&task.stdout).unwrap();
    assert_eq!(receipt["schema"], "af/task-inspection@9");
    let runtime = receipt["runtime_observations"].as_array().unwrap();
    assert!(runtime.iter().any(|entry| {
        entry["record"]["spans"]
            .as_array()
            .is_some_and(|spans| spans.iter().any(|span| span["kind"] == "check"))
    }));
    let wall = receipt["attempt_walls"].as_array().unwrap();
    assert!(wall.iter().any(|entry| {
        entry["started_unix_ms"]
            .as_u64()
            .is_some_and(|time| time > 0)
            && entry["elapsed_ms"].as_u64().is_some()
    }));

    std::fs::write(
        repo.join(".af/native-task.jsonl"),
        serde_json::to_string(&receipt).unwrap() + "\n",
    )
    .unwrap();
    install_optimizer_catalog(&repo);
    let project = format!("sha256:{}", "2".repeat(64));
    let sources = json!({
        "schema":"af.optimization-sources/1",
        "project_id":project,
        "sources":[{
            "adapter":"af",
            "path":"native-task.jsonl",
            "source_id":"actual-task-inspection",
            "execution_id":"pagination-cli",
            "attest_project":true
        }]
    });
    std::fs::write(
        repo.join(".af/optimization-sources.toml"),
        toml::to_string(&sources).unwrap(),
    )
    .unwrap();
    for args in [vec!["add", "-A"], vec!["commit", "-qm", "runtime evidence"]] {
        assert!(
            Command::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let optimized = af(&repo, &state, &["--execute"]);
    assert_eq!(optimized["result"]["acceptance"], "satisfied");
    let report_id = optimized["result"]["outputs"]["report"]["artifact_ids"][0]
        .as_str()
        .unwrap();
    let cas = review_store::Cas::open_existing(state.join("cas")).unwrap();
    let report = cas.get_json(report_id).unwrap();
    let economics_id = report["payload"]["economics_id"].as_str().unwrap();
    let economics = cas.get_json(economics_id).unwrap();
    assert!(economics["payload"]["summed_work_ms"].as_str().is_some());
    assert!(
        !economics["payload"]["missing_fields"]
            .as_array()
            .is_some_and(|fields| fields.iter().any(|field| field == "elapsed_time")),
        "measured Attempt/check time was downgraded to missing"
    );
    assert_eq!(report["payload"]["live_demonstrations"], "pending");
}
