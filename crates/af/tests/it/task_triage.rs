//! The issue-triage Task of `docs/design/issue-triage.md` §6.1, staged under
//! `fixtures/issue-triage/` because a Task Worker may not edit `.af/` (ADR-0133). On a copy of
//! this repository's `.af/` the test performs the install steps of that directory's README, runs
//! the staged catalog's contract test and plans the example Task for issue #432 with zero
//! Attempts: the business kind `triage` runs on the installed report profile with the two staged
//! model Workers. Isolated fake Providers serve only catalog and identity probes. A second test
//! feeds the report renderer's own bytes to `scripts/backlog-check-triage.py`.

use af_store::Cas;
use serde_json::{Value, json};
use std::path::Path;

use crate::task_cli;
use crate::task_report::{af, git, text, workspace};

const STAGED: &str = "fixtures/issue-triage";

/// The staged packages and the Provider label each model Worker is installed with.
const PACKAGES: [(&str, Option<&str>); 4] = [
    ("backlog/triage-kind", None),
    ("backlog/triage", None),
    ("backlog/triage-author", Some("codex-main")),
    ("backlog/triage-verifier", Some("codex-main")),
];

/// Perform the install steps of `fixtures/issue-triage/README.md` on `repo`'s `.af/`, or, where
/// a human has already taken a step there, verify that what they installed is what is staged.
/// Every staged pin is the digest of the staged bytes.
fn install_issue_triage(repo: &Path) {
    let staged: toml::Value =
        toml::from_str(&std::fs::read_to_string(repo.join(STAGED).join("catalog.toml")).unwrap())
            .unwrap();
    let catalog_path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
    // The triage Task runs on the report policy `fixtures/kernel-report` installed.
    assert_eq!(
        catalog["report_policy"].as_str(),
        Some(".af/report-policy.toml")
    );
    for (name, provider) in PACKAGES {
        let mut pin = staged["packages"][name].clone();
        assert_eq!(
            pin["digest"].as_str().unwrap(),
            af_pipeline_config::lock::package_digest(
                name,
                &repo.join(pin["path"].as_str().unwrap())
            )
            .unwrap(),
            "{name}: the staged pin is the staged bytes"
        );
        let path = format!(".af/task-packages/{name}");
        if !repo.join(&path).is_dir() {
            task_cli::copy_tree(&repo.join(pin["path"].as_str().unwrap()), &repo.join(&path));
        }
        pin["path"] = toml::Value::String(path);
        let packages = catalog["packages"].as_table_mut().unwrap();
        match packages.get(name) {
            Some(installed) => assert_eq!(
                installed["digest"], pin["digest"],
                "{name}: the committed pin is the staged package"
            ),
            None => {
                packages.insert(name.into(), pin);
            }
        }
        if let Some(provider) = provider {
            catalog["providers"]
                .as_table_mut()
                .unwrap()
                .entry(name)
                .or_insert_with(|| toml::Value::String(provider.into()));
        }
    }
    catalog["kinds"]
        .as_table_mut()
        .unwrap()
        .entry("triage")
        .or_insert_with(|| toml::Value::String("backlog/triage-kind".into()));
    std::fs::write(&catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
}

#[test]
fn the_staged_triage_packages_install_pass_their_catalog_test_and_plan_the_example_task() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let repo = root.join("triage");
    task_cli::copy_tree(&workspace().join(".af"), &repo.join(".af"));
    // The release pin would dispatch to (and install) another `af`; this test runs this one.
    std::fs::remove_file(repo.join(".af/af.lock")).unwrap();
    task_cli::copy_tree(&workspace().join(STAGED), &repo.join(STAGED));
    std::fs::write(repo.join("README.md"), "# Triage\n\nA fixture copy.\n").unwrap();
    install_issue_triage(&repo);
    let catalog = std::fs::read_to_string(repo.join(".af/task-catalog.toml")).unwrap();
    install_issue_triage(&repo);
    assert_eq!(
        std::fs::read_to_string(repo.join(".af/task-catalog.toml")).unwrap(),
        catalog,
        "installing again over an installed `.af/` changes nothing"
    );
    // The committed packages name the receipt this release no longer reads; the copy takes
    // ADR-0255's migration.
    task_cli::migrate_check_receipt(&repo, &[".af"], &[".af/task-catalog.toml"]);
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "issue triage"]);
    for (schema, path) in [
        ("task-file-v1.json", "example/task.json"),
        ("document-sources-v1.json", "example/sources.json"),
        ("catalog-contract-fixtures-v1.json", "contracts.json"),
    ] {
        let value: Value =
            serde_json::from_slice(&std::fs::read(repo.join(STAGED).join(path)).unwrap()).unwrap();
        crate::schemas::valid(&crate::schemas::validator(schema), &value);
    }

    let tested = af(
        &repo,
        &[
            "catalog",
            "test",
            "--source",
            ".",
            "--manifest",
            "fixtures/issue-triage/catalog.toml",
            "--json",
        ],
    );
    assert!(tested.status.success(), "{}", text(&tested));
    let tested: Value = serde_json::from_slice(&tested.stdout).unwrap();
    assert_eq!(tested["contract_fixtures"], "passed");
    assert_eq!(tested["attempts"], 0);
    assert_eq!(tested["pipelines"], json!(["backlog/triage"]));
    assert_eq!(
        tested["workers"],
        json!(["backlog/triage-author", "backlog/triage-verifier"])
    );

    // The staged Workers themselves, bound to isolated fake Providers by package name.
    let providers = crate::planning_models::Providers::new(&root);
    let bindings = root.join("bindings.toml");
    std::fs::write(
        &bindings,
        "schema = \"af.task-bindings/1\"\n\n[providers]\n\
         \"backlog/triage-author\" = \"fixture-codex\"\n\
         \"backlog/triage-verifier\" = \"fixture-codex\"\n",
    )
    .unwrap();
    let state = root.join("state");
    let planned = providers
        .command(&repo)
        .args([
            "task",
            "plan",
            "--file",
            "fixtures/issue-triage/example/task.json",
            "--bindings",
            bindings.to_str().unwrap(),
            "--state",
            state.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    providers.assert_no_dispatch();
    assert_eq!(planned.status.code(), Some(0), "{}", text(&planned));
    let planned: Value = serde_json::from_slice(&planned.stdout).unwrap();
    assert_eq!(planned["attempts"], 0);
    assert_eq!(
        planned["selection"]["assessment"]["decision"],
        json!({"kind": "selected", "pipeline": "backlog/triage"})
    );

    // The report shape: each Provider admission precedes its Worker; author, seal, check,
    // verify and accept retain their order.
    let graph = &planned["graph"];
    let operators: Vec<&Value> = graph["order"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| &graph["nodes"][node.as_str().unwrap()]["operator"])
        .collect();
    assert_eq!(
        operators,
        [
            &json!({"kind": "root_inputs"}),
            &json!({"kind": "provider_admission", "bindings": ["root.slots.author"]}),
            &json!({"kind": "primitive", "operator": {"op": "worker", "slot": "root.slots.author"},
                    "signature": "worker/backlog/triage-author"}),
            &json!({"kind": "primitive", "operator": {"op": "report_seal"},
                    "signature": "operator/report-seal"}),
            &json!({"kind": "primitive", "operator": {"op": "report_check"},
                    "signature": "operator/report-check"}),
            &json!({"kind": "provider_admission", "bindings": ["root.slots.verifier"]}),
            &json!({"kind": "primitive", "operator": {"op": "verify", "slot": "root.slots.verifier"},
                    "signature": "worker/backlog/triage-verifier"}),
            &json!({"kind": "primitive", "operator": {"op": "report_accept"},
                    "signature": "operator/report-accept"}),
        ]
    );
    assert_eq!(
        graph["slots"]["root.slots.verifier"]["independent_from"],
        json!(["root.slots.author"])
    );
    // Each slot is bound to the staged Worker itself, its pinned bytes and its model identity.
    let staged: toml::Value =
        toml::from_str(&std::fs::read_to_string(repo.join(STAGED).join("catalog.toml")).unwrap())
            .unwrap();
    for (slot, worker, provider_kind, model) in [
        ("author", "backlog/triage-author", "codex", "gpt-6.1-sol"),
        (
            "verifier",
            "backlog/triage-verifier",
            "codex",
            "gpt-6.1-terra",
        ),
    ] {
        let slot = format!("root.slots.{slot}");
        assert_eq!(graph["slots"][&slot]["worker"], worker);
        let bound = &planned["plan"]["bindings"][&slot];
        assert_eq!(
            bound["package_digest"].as_str(),
            staged["packages"][worker]["digest"].as_str(),
            "{slot}"
        );
        assert_eq!(bound["execution"]["provider_kind"], provider_kind);
        assert_eq!(bound["execution"]["model"], model);
        assert_eq!(bound["execution"]["effort"], "medium");
        let manifest: toml::Value = toml::from_str(
            &std::fs::read_to_string(
                repo.join(".af/task-packages")
                    .join(worker)
                    .join("worker.toml"),
            )
            .unwrap(),
        )
        .unwrap();
        // Neither triager writes source or runs checks: the author has no shell.
        assert_eq!(
            manifest["signature"]["effects"],
            toml::Value::try_from(["read-source"]).unwrap(),
            "{worker}"
        );
    }
    // Two Attempts of each Worker, two Provider admissions and the report check fit §6.1's
    // budget: 2 × 40,000 + 2 × 32,768 tokens, 2 × 500,000 + 90,000 + 65,000 ms.
    for node in ["root.nodes.author", "root.nodes.verify"] {
        let allowance = &graph["allowances"][node];
        assert_eq!(allowance["tokens_per_attempt"], 40_000, "{node}");
        assert_eq!(allowance["wall_ms_per_attempt"], 500_000, "{node}");
        assert_eq!(allowance["max_attempts"], 2, "{node}");
    }

    // The Task's business kind is `triage`, its caps are §6.1's unraised, and it captured the
    // six entries of §6.2.
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let revision = cas
        .get_json(planned["revision_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(revision["payload"]["kind"], "triage");
    // The revision keeps the wall as its deadline; the Task file the planner admitted names it.
    let limits = &revision["payload"]["limits"];
    assert_eq!(limits["tokens"], 150_000, "{limits}");
    assert_eq!(limits["max_attempts"], 8, "{limits}");
    let file: Value = serde_json::from_slice(
        &std::fs::read(repo.join(STAGED).join("example/task.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        file["limits"],
        json!({"tokens": 150_000, "max_attempts": 8, "wall_ms": 1_200_000,
               "verification": {"tokens": 72_768, "attempts": 3, "wall_ms": 605_000}})
    );
    let sources = &planned["plan"]["inputs"]["sources"];
    assert_eq!(sources["artifact_type"], "af/ReportSources@1");
    let captured = cas
        .get_json(sources["artifact_ids"][0].as_str().unwrap())
        .unwrap();
    let names: Vec<&String> = captured["payload"]["sources"]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert_eq!(
        names,
        [
            "citations",
            "excerpts",
            "issue",
            "neighbours",
            "pulls",
            "rubric"
        ]
    );
}

/// The author's `Triage` block survives the report renderer: the canonical bytes
/// (`af task output --port report --format canonical-markdown`, the text the checks and the
/// verifier judged) escape every backtick of the fence, and `--document` reads the block back
/// exactly, through the real renderer and the real script.
#[test]
fn the_check_reads_the_triage_block_from_the_report_renderers_bytes() {
    use af_contracts::task::document::DocumentDraftV2;
    use af_runtime::task::report_domain::{ReportDraft, render_report};

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let triage = json!({
        "schema": "af/issue-triage@1", "issue": 432, "body_sha256": "ab".repeat(32),
        "class": "aix", "size": "M", "pipeline": "default", "adr": null, "confidence": 0.86,
        "rationale": "1. `TaskAttentionV1::of` puts *every* waiting Task under ```approval```; \
                      a_b _c_ [x] <y> ~z~ | $ & &amp; # \\ \"q\" www.example.com \
                      https://example.com/a?b=c fixture@example.invalid `a@b`.",
        "resolution": {"state": "open", "evidence": [], "addressed": [], "remaining": []},
        "duplicates": [], "needs_info": [],
        "acceptance": "- af task list --attention shows it under its own group.",
    });
    let block = format!(
        "```json\n{}\n```",
        serde_json::to_string_pretty(&triage).unwrap()
    );
    let draft: DocumentDraftV2 = serde_json::from_value(json!({
        "schema": "af.document-draft/2",
        "title": "Triage of issue #432",
        "sections": [
            {"heading": "Triage", "body": block},
            {"heading": "Rationale", "body": "The approval group is named for a plan to confirm."},
            {"heading": "Findings", "body": "state.rs:483 puts every waiting Task in one group."},
            {"heading": "Recommendation", "body": "Take it on the default pipeline."},
        ],
        "citations": ["issue"],
        "repository_citations": [{"path": "crates/af-contracts/src/task/state.rs", "line": 483}],
    }))
    .unwrap();
    let sources = serde_json::from_slice(
        &std::fs::read(workspace().join(STAGED).join("example/sources.json")).unwrap(),
    )
    .unwrap();
    let rendered = render_report(&ReportDraft::V2(draft), &sources).unwrap();
    assert!(
        rendered.contains("\n\\`\\`\\`json\n") && !rendered.contains("\n```"),
        "the renderer escapes the authored fence: {rendered}"
    );
    std::fs::write(root.join("report.md"), &rendered).unwrap();

    let document_id = format!("sha256:{}", "1".repeat(64));
    let evaluation = json!({
        "document_id": document_id, "sources_id": format!("sha256:{}", "2".repeat(64)),
        "requirements_id": format!("sha256:{}", "3".repeat(64)),
        "check_receipt_id": format!("sha256:{}", "4".repeat(64)),
        "source_snapshot_id": format!("sha256:{}", "5".repeat(64)),
        "outcome": "passed",
        "summary": format!("state.rs:483 says what the report claims.\n\n{block}"),
    });
    std::fs::write(root.join("evaluation.json"), evaluation.to_string()).unwrap();
    let repo = root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "main"]);

    let checked = std::process::Command::new("python3")
        .arg("-I")
        .arg(workspace().join("scripts/backlog-check-triage.py"))
        .arg("--document")
        .arg(root.join("report.md"))
        .arg("--evaluation")
        .arg(root.join("evaluation.json"))
        .arg("--repo")
        .arg(&repo)
        .args(["--main", "main", "--task-id", "triage-432", "--document-id"])
        .arg(&document_id)
        .arg("--evaluation-id")
        .arg(format!("sha256:{}", "6".repeat(64)))
        .args(["--bundle-sha256", &"7".repeat(64)])
        .args([
            "--policy-version",
            "triage-runner/1",
            "--event-id",
            "opened-1",
        ])
        .env("HOME", &root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert_eq!(checked.status.code(), Some(0), "{}", text(&checked));
    let record: Value = serde_json::from_slice(&checked.stdout).unwrap();
    assert_eq!(record["author"], triage);
    assert_eq!(record["verdict"], "agreed");
    assert_eq!(
        record["labels"],
        json!(["class/aix", "size/M", "pipeline/default"])
    );
}
