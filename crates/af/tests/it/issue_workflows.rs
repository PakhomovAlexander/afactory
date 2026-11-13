//! Real native compilation and token-free planning of the staged operator catalog.
use crate::{common, fake_models, task_cli};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn workspace() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}
fn git(repo: &Path, args: &[&str]) {
    let o = std::process::Command::new("git")
        .current_dir(repo)
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}
fn copy_private_catalog(source: &Path, destination: &Path) {
    // Copy bytes rather than modes: sealed source fixtures are read-only, but this private
    // catalog is deliberately mutated by the missing-comparison negative test below.
    std::fs::write(destination, std::fs::read(source).unwrap()).unwrap();
}

pub(crate) fn install(repo: &Path) {
    let root = workspace();
    let staged = root.join("fixtures/issue-workflows");
    task_cli::copy_tree(&staged, &repo.join("fixtures/issue-workflows"));
    task_cli::copy_tree(
        &staged.join("task-packages"),
        &repo.join(".af/task-packages"),
    );
    for policy in ["code-policy.toml", "report-policy.toml"] {
        copy_private_catalog(
            &staged.join(format!("approved-{policy}")),
            &repo.join(".af").join(policy),
        );
    }
    copy_private_catalog(
        &staged.join("replacement-catalog.toml"),
        &repo.join(".af/task-catalog.toml"),
    );
    copy_private_catalog(
        &staged.join("replacement-af.toml"),
        &repo.join(".af/af.toml"),
    );
    if !repo.join("README.md").exists() {
        std::fs::write(repo.join("README.md"), "# Fixture\n").unwrap();
    }
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", "staged catalog"]);
}
fn text(o: &std::process::Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn assert_primitive_model_policy_refusal(output: &std::process::Output) {
    assert!(!output.status.success(), "normal model policy must refuse");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema"], "af/task-selection@3", "{value}");
    assert_eq!(value["selection"]["decision"]["kind"], "invalid", "{value}");
    let candidates = value["selection"]["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 1, "{value}");
    assert_eq!(candidates[0]["pipeline"], "bugfix/primitive-codex");
    assert_eq!(
        candidates[0]["state"],
        json!({"kind": "invalid", "reason": "Trusted policy requires distinct models"}),
        "{value}"
    );
    assert_eq!(value["attempts"], 0);
    assert_eq!(value["chargeable_tokens"], 0);
    assert!(value.get("plan_id").is_none(), "{value}");
}

#[test]
fn private_catalog_copy_is_writable_and_preserves_read_only_source() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("replacement-catalog.toml");
    let source_bytes =
        std::fs::read(workspace().join("fixtures/issue-workflows/replacement-catalog.toml"))
            .unwrap();
    std::fs::write(&source, &source_bytes).unwrap();
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o444)).unwrap();
    let source_mode = std::fs::metadata(&source).unwrap().permissions().mode();
    assert_eq!(source_mode & 0o222, 0, "source must start read-only");

    let private = temp.path().join("task-catalog.toml");
    copy_private_catalog(&source, &private);

    assert_eq!(std::fs::read(&private).unwrap(), source_bytes);
    assert_ne!(
        std::fs::metadata(&private).unwrap().permissions().mode() & 0o200,
        0,
        "the private catalog must have its owner-write bit"
    );
    let mut mutated = source_bytes.clone();
    mutated.extend_from_slice(b"\n# private missing-comparison mutation\n");
    std::fs::write(&private, mutated).unwrap();

    assert_eq!(std::fs::read(&source).unwrap(), source_bytes);
    assert_eq!(
        std::fs::metadata(&source).unwrap().permissions().mode(),
        source_mode,
        "copying and mutating the private catalog must not change the source mode"
    );
}

#[test]
fn catalog_contracts_and_all_nine_native_plans_preserve_roles_gates_and_stage_dataflow() {
    let temp = tempfile::tempdir().unwrap();
    let (repo, state) = crate::task_report::starter(temp.path());
    let original = std::process::Command::new("git")
        .current_dir(&repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let original = String::from_utf8(original.stdout).unwrap();
    install(&repo);
    // A real, independently accepted command-fixture Report Task on this exact source.
    // Its old authority chooses only hermetic command Workers; no production policy is weakened.
    let report = common::af()
        .current_dir(&repo)
        .args([
            "task",
            "start",
            "--execute",
            "--file",
            "report.json",
            "--authority",
            original.trim(),
            "--json",
        ])
        .arg("--state")
        .arg(&state)
        .output()
        .unwrap();
    assert!(report.status.success(), "{}", text(&report));
    let report: Value = serde_json::from_slice(&report.stdout).unwrap();
    assert_eq!(report["result"]["acceptance"], "satisfied");
    let report_task = report["task_id"].as_str().unwrap();
    let o = common::af()
        .current_dir(&repo)
        .args([
            "catalog",
            "test",
            "--source",
            ".",
            "--manifest",
            "fixtures/issue-workflows/catalog.toml",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", text(&o));
    let tested: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(tested["attempts"], 0);
    assert_eq!(tested["contract_fixtures"], "passed");
    let home = temp.path().join("home");
    let (sdk, path, _) = fake_models::install_pi(temp.path(), &home, "kimi-document");
    let sdk_text = std::fs::read_to_string(&sdk)
        .unwrap()
        .replace("id: 'k3'", "id: 'k3.1-pro'");
    std::fs::write(&sdk, sdk_text).unwrap();
    let bin = PathBuf::from(&path);
    let auth = home.join("codex");
    fake_models::install("codex", &bin, &auth);
    for (name, source) in [("env", "/usr/bin/env"), ("bash", "/bin/bash")] {
        std::os::unix::fs::symlink(source, bin.join(name)).unwrap();
    }
    std::fs::write(bin.join("npx"), "#!/bin/sh\nexit 99\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin.join("npx"), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(
        home.join("providers.toml"),
        toml::to_string(&json!({"version":4,"providers":[
      {"id":"pi-personal","kind":"pi","backend":"kimi-coding","auth_dir":home,"sdk_entrypoint":sdk},
      {"id":"codex-personal","kind":"codex","auth_dir":auth}]}))
        .unwrap(),
    )
    .unwrap();
    let selections: Value = serde_json::from_slice(
        &std::fs::read(repo.join("fixtures/issue-workflows/selections.json")).unwrap(),
    )
    .unwrap();
    let example = |file: &str| -> Value {
        serde_json::from_slice(
            &std::fs::read(repo.join("fixtures/issue-workflows").join(file)).unwrap(),
        )
        .unwrap()
    };
    let plan_document = |mut task: Value, id: &str, evidence: bool| {
        let file = temp.path().join(format!("{id}.json"));
        task["task_id"] = json!(id);
        if evidence {
            task["inputs"] = json!({"research_report":{"task":report_task,"port":"report"}});
        }
        std::fs::write(&file, serde_json::to_vec(&task).unwrap()).unwrap();
        common::af()
            .current_dir(&repo)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_CACHE_HOME", home.join("cache"))
            .env("AF_PI_HOST_MODULE", &sdk)
            .env("AF_PROVIDERS_FILE", home.join("providers.toml"))
            .env("PATH", &path)
            .args(["task", "plan", "--file"])
            .arg(file)
            .arg("--state")
            .arg(&state)
            .arg("--json")
            .output()
            .unwrap()
    };
    let default = plan_document(
        json!({"task_id": "default", "kind": "implement", "goal": "A bounded known fix"}),
        "three-field-default",
        false,
    );
    assert!(default.status.success(), "{}", text(&default));
    let default: Value = serde_json::from_slice(&default.stdout).unwrap();
    assert_eq!(
        default["selection"]["assessment"]["decision"]["pipeline"],
        "bugfix/light-kimi-terra-sol"
    );
    let mut unlinked = example("known-fix.json");
    unlinked["requirements"]
        .as_object_mut()
        .unwrap()
        .remove("issue_url");
    let unlinked = plan_document(unlinked, "unlinked-local", false);
    assert!(unlinked.status.success(), "{}", text(&unlinked));
    // Exercise the shipped documents with only Task identity and accepted evidence replaced.
    for (i, file) in [
        "known-fix.json",
        "light-implementation.json",
        "performance-kimi.json",
        "research-kimi.json",
        "research-fix.json",
        "diagnose.json",
        "design.json",
        "verify.json",
    ]
    .into_iter()
    .enumerate()
    {
        let task = example(file);
        let evidence = matches!(file, "research-fix.json" | "performance-kimi.json");
        let o = plan_document(task.clone(), &format!("example-{i}"), evidence);
        assert!(o.status.success(), "{file}: {}", text(&o));
        let v: Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(v["attempts"], 0);
        if matches!(
            file,
            "research-fix.json" | "performance-kimi.json" | "verify.json"
        ) {
            let mut missing = task.clone();
            missing.as_object_mut().unwrap().remove("facts");
            let o = plan_document(missing, &format!("example-missing-facts-{i}"), evidence);
            assert!(!o.status.success(), "{file}: missing facts must refuse");
            assert!(text(&o).contains("needs facts: standard"), "{}", text(&o));
        }
        if evidence {
            let o = plan_document(task, "example-missing-research", false);
            assert!(
                !o.status.success(),
                "{file}: missing accepted Document must refuse"
            );
            assert!(text(&o).contains("research_report"), "{}", text(&o));
        }
    }
    // Reuse those documents for the other catalog selections without synthesizing facts,
    // strategies, limits or verification settings that could conceal example omissions.
    let plan = |name: &str, id: &str, evidence: bool| {
        let file = match name {
            "issue-internal/report" => "diagnose.json",
            "design/astra-kimi" => "design.json",
            "issue-internal/kimi-report" => "diagnose.json",
            "issue-internal/verification" => "verify.json",
            _ if name.contains("research") || name.contains("design") => "research-fix.json",
            _ => "known-fix.json",
        };
        let mut task = example(file);
        task["pipeline"]["name"] = json!(name);
        plan_document(task, id, evidence && file == "research-fix.json")
    };
    for (i, (name, _)) in selections["selections"]
        .as_object()
        .unwrap()
        .iter()
        .enumerate()
    {
        if name.starts_with("design/") {
            continue; // Compiled with report stages below; no implementation or code gates.
        }
        let o = plan(name, &format!("selection-{i}"), true);
        if name == "bugfix/primitive-codex" {
            assert_primitive_model_policy_refusal(&o);
            continue;
        }
        assert!(o.status.success(), "{name}: {}", text(&o));
        let v: Value = serde_json::from_slice(&o.stdout).unwrap();
        let bindings = v["plan"]["bindings"].as_object().unwrap();
        assert!(!bindings.is_empty());
        let light = name.contains("/light-");
        for (slot, b) in bindings {
            let role = slot.rsplit('.').next().unwrap();
            let (model, effort) = match role {
                "implementer" if light && name.contains("kimi") => ("k3.1-pro", "high"),
                "implementer" => ("gpt-6.1-sol", "medium"),
                "bugs" | "correctness" => ("gpt-6.1-terra", "high"),
                "evaluator" if light && name.contains("kimi") => ("gpt-6.1-sol", "medium"),
                "evaluator" | "measurement_evaluator" => ("gpt-6-astra", "medium"),
                "author" if name.contains("design") => ("gpt-6-astra", "medium"),
                "author" => ("gpt-6.1-sol", "high"),
                "verifier" => ("gpt-6.1-terra", "high"),
                _ => panic!("unexpected role {slot}"),
            };
            assert_eq!(b["execution"]["model"], model, "{name} {slot}");
            assert_eq!(b["execution"]["effort"], effort);
            assert_eq!(
                b["execution"]["provider"],
                if model == "k3.1-pro" {
                    "pi-personal"
                } else {
                    "codex-personal"
                }
            );
        }
        let graph = &v["graph"];
        if !light {
            for role in ["bugs", "correctness"] {
                let slot = format!("root.slots.{role}");
                assert_eq!(
                    graph["slots"][&slot]["worker"],
                    format!("issue-internal/{role}")
                );
                assert_ne!(
                    bindings[&slot]["execution"]["model"],
                    bindings["root.slots.implementer"]["execution"]["model"]
                );
            }
        }
        let rendered = graph.to_string();
        assert!(rendered.contains("markdownlint") && rendered.contains("kernel"));
        assert!(
            !rendered.contains("publish-gate")
                && !rendered.contains("codex-work")
                && !rendered.contains("uix")
        );
        if name.contains("research") || name.contains("design") {
            assert_eq!(
                graph["nodes"]["root.nodes.implement"]["inputs"]["research_report"]["port"],
                "research_report"
            );
            let implement = &graph["nodes"]["root.nodes.implement"];
            assert!(
                implement.to_string().contains("research_report"),
                "{implement}"
            );
        }
        if name.starts_with("perf/") {
            assert!(
                rendered.contains("\"op\":\"measure\"") && rendered.contains("\"op\":\"compare\"")
            );
        }
        if name.starts_with("perf/") {
            let stages = selections["selections"][name]["stages"].as_array().unwrap();
            let internal = [
                ("baseline", "root.nodes.baseline"),
                ("implementation", "root.nodes.implement"),
                ("code review", "root.nodes.accept"),
                ("measurement", "root.nodes.trial.nodes.measure"),
                ("comparison", "root.nodes.trial.nodes.compare"),
                ("measurement acceptance", "root.nodes.accept_measurement"),
            ];
            assert_eq!(stages.first().unwrap(), "research");
            assert_eq!(stages.last().unwrap(), "verification");
            let order = graph["order"].as_array().unwrap();
            let positions: Vec<_> = internal
                .iter()
                .enumerate()
                .map(|(i, (stage, node))| {
                    assert_eq!(stages[i + 1], *stage);
                    order
                        .iter()
                        .position(|id| id == node)
                        .unwrap_or_else(|| panic!("{node}: {order:?}"))
                })
                .collect();
            assert!(
                positions.windows(2).all(|pair| pair[0] < pair[1]),
                "{positions:?}"
            );
        }
        // Follow real checked inspection IDs into the native Store. This proves the script
        // reads actual revision, Requirements, Snapshot and origin shapes, not invented fields.
        let native_inspection = temp.path().join(format!("native-{i}.json"));
        std::fs::write(&native_inspection, serde_json::to_vec(&v).unwrap()).unwrap();
        let isolated = common::af();
        let mut probe = std::process::Command::new("python3");
        for (key, value) in isolated.get_envs() {
            if let Some(value) = value {
                probe.env(key, value);
            }
        }
        let checked = probe
            .args([
                "-I",
                "-c",
                r#"
import importlib.util, json, pathlib, subprocess, sys
spec = importlib.util.spec_from_file_location('workflow', sys.argv[1])
w = importlib.util.module_from_spec(spec)
spec.loader.exec_module(w)
state, repo, af, file, name = sys.argv[2:]
v = w.read(file)
# Assert parity of staged TOML normalization with the kernel's typed captured policy.
w.validate_policy(v, state)
import copy
from unittest import mock
real_recorded = w.recorded
authority_id = v['plan']['authority']['policy_id']
authority = real_recorded(state, authority_id)
for key in ('code_policy_id', 'review_policy_id'):
    altered = copy.deepcopy(authority)
    if key == 'code_policy_id':
        altered[key] = w.approved_policy_id('report')
        target = authority_id
    else:
        target = authority[key]
        altered = copy.deepcopy(real_recorded(state, target))
        altered['check_policy_id'] = w.approved_policy_id('report')
    def recorded(state, identity, kind=None):
        return altered if identity == target else real_recorded(state, identity, kind)
    with mock.patch.object(w, 'recorded', side_effect=recorded):
        try:
            w.validate_policy(v, state)
        except ValueError:
            pass
        else:
            raise AssertionError('accepted forged effective ' + key)
remote = copy.deepcopy(v)
for node in remote['graph']['nodes'].values():
    operator = node['operator'].get('operator', {})
    if operator.get('op') == 'check':
        operator['remote_checks'] = ['kernel']
        break
else:
    raise AssertionError('missing native check operator')
try:
    w.validate_policy(remote, state)
except ValueError:
    pass
else:
    raise AssertionError('accepted remote check substitution')
r = w.recorded(state, v['revision_id'], 'af/TaskRevision@1')
assert r['inputs'] == v['plan']['inputs']
document = w.read(pathlib.Path(repo) / 'fixtures/issue-workflows/known-fix.json')
contract = w.original_issue(document['goal'], document['requirements'])
assert w.native_original_issue(state, r) == contract
snapshot, origin = w.source_origin(state, r['inputs']['source']['snapshot_id'])
assert origin['kind'] == 'captured'
commit = subprocess.check_output(['git', '-C', repo, 'rev-parse', 'HEAD']).decode().strip()
assert origin['source_revision'] == commit
if not w.selection(name)['research_pipeline']:
    m = {'schema': 'af.issue-workflow-pilot/1', 'issue': document['requirements']['issue_url'],
         'pull_request': None, 'source_commit': commit, 'selection': name, 'tasks': [v['task_id']]}
    result = w.reconcile(af, repo, state, m)
    assert result['chain_chargeable_tokens'] == v['chargeable_tokens']
    assert result['tasks'][0]['source_snapshot_id'] == r['inputs']['source']['snapshot_id']
    assert result['pull_request_association'] == 'absent'
    assert result['remote_lookup_performed'] is False
    linked = dict(m, pull_request='https://github.com/owner/repo/pull/2')
    linked_result = w.reconcile(af, repo, state, linked)
    assert linked_result['pull_request_association'] == 'unverified'
    assert linked_result['remote_lookup_performed'] is False
    unlinked = dict(m, tasks=['unlinked-local'])
    try:
        w.reconcile(af, repo, state, unlinked)
    except ValueError as error:
        assert 'original native requirements.issue_url' in str(error), str(error)
    else:
        raise AssertionError('accepted unlinked local Task as a pilot')
    for field, wrong in [('source_commit', '0' * 40), ('selection', 'impl/design-codex'),
                         ('issue', 'https://github.com/OWNER/REPO/issues/2'),
                         ('issue', 'https://github.com/other/repo/issues/1'),
                         ('pull_request', 'https://github.com/OWNER/elsewhere/pull/2'),
                         ('pull_request', 'https://github.com/other/REPO/pull/2')]:
        bad = dict(m); bad[field] = wrong
        try:
            w.reconcile(af, repo, state, bad)
        except ValueError:
            pass
        else:
            raise AssertionError('accepted wrong ' + field)
else:
    binding = v['input_bindings']['bindings']['research_report']
    report = w.invoke(af, repo, state, 'explain', binding['task']['task_id'])
    w.check_output(binding, report, r['inputs']['research_report'], 'report', 'af/Document@1')
    # The hermetic starter Report has different authority/Pipeline; acceptance alone
    # cannot substitute it for this route's required report/design producer.
    try:
        w.check_stage(report, w.selection(name)['research_pipeline'], state)
    except ValueError:
        pass
    else:
        raise AssertionError('accepted unrelated report Pipeline')
"#,
            ])
            .arg(workspace().join("scripts/issue-workflows.py"))
            .arg(&state)
            .arg(&repo)
            .arg(isolated.get_program())
            .arg(&native_inspection)
            .arg(name)
            .output()
            .unwrap();
        assert!(checked.status.success(), "{name}: {}", text(&checked));
        let inspection = temp.path().join("inspection.json");
        std::fs::write(&inspection, serde_json::to_vec(&v).unwrap()).unwrap();
        let checked = std::process::Command::new("python3")
            .arg("-I")
            .arg(workspace().join("scripts/issue-workflows.py"))
            .arg("--state")
            .arg(&state)
            .args(["check-plan", name])
            .arg(&inspection)
            .output()
            .unwrap();
        assert!(checked.status.success(), "{name}: {}", text(&checked));
        if name == "bugfix/default-codex" {
            for swap in [false, true] {
                let mut altered = v.clone();
                let bugs = "root.slots.bugs";
                let correctness = "root.slots.correctness";
                altered["graph"]["slots"][bugs] = v["graph"]["slots"][correctness].clone();
                altered["plan"]["bindings"][bugs] = v["plan"]["bindings"][correctness].clone();
                if swap {
                    altered["graph"]["slots"][correctness] = v["graph"]["slots"][bugs].clone();
                    altered["plan"]["bindings"][correctness] = v["plan"]["bindings"][bugs].clone();
                }
                std::fs::write(&inspection, serde_json::to_vec(&altered).unwrap()).unwrap();
                let checked = std::process::Command::new("python3")
                    .arg("-I")
                    .arg(workspace().join("scripts/issue-workflows.py"))
                    .arg("--state")
                    .arg(&state)
                    .args(["check-plan", name])
                    .arg(&inspection)
                    .output()
                    .unwrap();
                assert!(
                    !checked.status.success(),
                    "approved role substitution must refuse"
                );
                assert!(
                    text(&checked).contains("native stored"),
                    "{}",
                    text(&checked)
                );
            }
        }
        if i == 0 {
            let gate = v["graph"]["nodes"]
                .as_object()
                .unwrap()
                .iter()
                .find(|(_, node)| node["operator"]["operator"]["op"] == "check")
                .map(|(id, _)| id.clone())
                .expect("real local check node");
            for case in [
                "empty",
                "removed-check",
                "mutated-check",
                "order",
                "dataflow",
                "plan-metadata",
                "graph-id",
                "plan-id",
                "revision",
                "task",
                "json-type",
            ] {
                let mut altered = v.clone();
                match case {
                    "empty" => altered["graph"]["nodes"] = json!({}),
                    "removed-check" => {
                        altered["graph"]["nodes"]
                            .as_object_mut()
                            .unwrap()
                            .remove(&gate);
                    }
                    "mutated-check" => {
                        altered["graph"]["nodes"][&gate]["operator"]["operator"]["checks"] =
                            json!([])
                    }
                    "order" => altered["graph"]["order"].as_array_mut().unwrap().reverse(),
                    "dataflow" => altered["graph"]["nodes"][&gate]["inputs"] = json!({}),
                    "plan-metadata" => altered["plan"]["limits"]["tokens"] = json!(1),
                    "graph-id" => altered["plan"]["compiled_graph_id"] = v["plan_id"].clone(),
                    "plan-id" => altered["plan_id"] = v["revision_id"].clone(),
                    "revision" => altered["revision_id"] = report["revision_id"].clone(),
                    "task" => altered["task_id"] = json!("another-task"),
                    "json-type" => altered["graph"]["max_parallel"] = json!(true),
                    _ => unreachable!(),
                }
                assert_ne!(altered, v, "{case} must change the native projection");
                std::fs::write(&inspection, serde_json::to_vec(&altered).unwrap()).unwrap();
                let checked = std::process::Command::new("python3")
                    .arg("-I")
                    .arg(workspace().join("scripts/issue-workflows.py"))
                    .arg("--state")
                    .arg(&state)
                    .args(["check-plan", name])
                    .arg(&inspection)
                    .output()
                    .unwrap();
                assert!(
                    !checked.status.success(),
                    "{case}: accepted edited native inspection"
                );
            }
            for field in ["model", "effort", "provider"] {
                let mut altered = v.clone();
                altered["plan"]["bindings"]["root.slots.implementer"]["execution"][field] =
                    json!(if field == "provider" {
                        "codex-work"
                    } else {
                        "wrong"
                    });
                std::fs::write(&inspection, serde_json::to_vec(&altered).unwrap()).unwrap();
                let checked = std::process::Command::new("python3")
                    .arg("-I")
                    .arg(workspace().join("scripts/issue-workflows.py"))
                    .arg("--state")
                    .arg(&state)
                    .args(["check-plan", name])
                    .arg(&inspection)
                    .output()
                    .unwrap();
                assert!(!checked.status.success(), "changed {field} must refuse");
            }
            let mut altered = v.clone();
            altered["plan"]["bindings"]
                .as_object_mut()
                .unwrap()
                .remove("root.slots.evaluator");
            std::fs::write(&inspection, serde_json::to_vec(&altered).unwrap()).unwrap();
            let checked = std::process::Command::new("python3")
                .arg("-I")
                .arg(workspace().join("scripts/issue-workflows.py"))
                .arg("--state")
                .arg(&state)
                .args(["check-plan", name])
                .arg(&inspection)
                .output()
                .unwrap();
            assert!(!checked.status.success(), "missing evaluator must refuse");
        }
        assert_eq!(v["attempts"], 0);
    }
    for (i, old) in [
        "default/implementation",
        "default-light/implementation",
        "pi-light/implementation",
        "warm/implementation-reviewed",
        "kernel/implementation-reviewed",
        "owner-default/implementation",
        "impl/design-codex",
    ]
    .into_iter()
    .enumerate()
    {
        let o = plan(old, &format!("old-{i}"), false);
        assert!(!o.status.success(), "{old} unexpectedly selectable");
    }
    for (i, name) in [
        "issue-internal/report",
        "design/astra-kimi",
        "issue-internal/verification",
        "issue-internal/kimi-report",
    ]
    .into_iter()
    .enumerate()
    {
        let o = plan(name, &format!("stage-{i}"), false);
        assert!(o.status.success(), "{name}: {}", text(&o));
        let v: Value = serde_json::from_slice(&o.stdout).unwrap();
        let file = temp.path().join(format!("stage-plan-{i}.json"));
        std::fs::write(&file, serde_json::to_vec(&v).unwrap()).unwrap();
        let checked = std::process::Command::new("python3")
            .arg("-I")
            .arg(workspace().join("scripts/issue-workflows.py"))
            .arg("--state")
            .arg(&state)
            .args(["check-plan", name])
            .arg(file)
            .output()
            .unwrap();
        assert!(checked.status.success(), "{name}: {}", text(&checked));
        if name == "design/astra-kimi" {
            let nodes = &v["graph"]["nodes"];
            let final_inputs = &nodes["root.nodes.verify"]["inputs"];
            assert_eq!(final_inputs["document"]["node"], "root.nodes.seal");
            assert_eq!(
                final_inputs["terra_challenge"]["node"],
                "root.nodes.terra_challenger"
            );
            assert_eq!(
                final_inputs["kimi_challenge"]["node"],
                "root.nodes.kimi_challenger"
            );
            assert_eq!(
                nodes["root.nodes.terra_challenger"]["inputs"]["document"],
                final_inputs["document"]
            );
            assert_eq!(
                nodes["root.nodes.kimi_challenger"]["inputs"]["document"],
                final_inputs["document"]
            );
            for (role, model, effort) in [
                ("author", "gpt-6-astra", "medium"),
                ("terra_challenger", "gpt-6.1-terra", "high"),
                ("kimi_challenger", "k3.1-pro", "high"),
                ("verifier", "gpt-6.1-sol", "medium"),
            ] {
                let execution = &v["plan"]["bindings"][format!("root.slots.{role}")]["execution"];
                assert_eq!(execution["model"], model);
                assert_eq!(execution["effort"], effort);
            }
            assert!(!v["graph"]["outputs"].to_string().contains("snapshot"));
        }
        if name.ends_with("verification") {
            for (role, model, effort) in [
                ("bugs", "gpt-6.1-terra", "high"),
                ("correctness", "gpt-6.1-terra", "high"),
                ("evaluator", "gpt-6-astra", "medium"),
            ] {
                let slot = format!("root.slots.{role}");
                assert_eq!(
                    v["graph"]["slots"][&slot]["worker"],
                    format!("issue-internal/{role}")
                );
                let execution = &v["plan"]["bindings"][&slot]["execution"];
                assert_eq!(execution["model"], model);
                assert_eq!(execution["effort"], effort);
            }
            assert!(!v["graph"]["slots"].to_string().contains("implementer"));
        } else {
            let graph = v["graph"].to_string();
            assert!(graph.contains("report_accept") && graph.contains("report_check"));
            for node in ["author", "verify"] {
                assert_eq!(
                    v["graph"]["nodes"][format!("root.nodes.{node}")]["inputs"]["requirements"]["port"],
                    "requirements"
                );
            }
            // Inspect actual native revisions, including negative documents which native
            // Report planning legitimately admits but the issue-workflow relationship refuses.
            let mut inspections = vec![v.clone()];
            for case in ["missing", "goal", "acceptance", "empty", "issue-url"] {
                let mut task = example(if name.starts_with("design/") {
                    "design.json"
                } else {
                    "diagnose.json"
                });
                match case {
                    "missing" => {
                        task["requirements"]
                            .as_object_mut()
                            .unwrap()
                            .remove("original_issue");
                    }
                    "goal" => {
                        task["requirements"]["original_issue"]["goal"] = json!("another issue")
                    }
                    "acceptance" => {
                        task["requirements"]["original_issue"]["requirements"]["acceptance"] =
                            json!(["another acceptance"])
                    }
                    "empty" => task["requirements"]["original_issue"]["requirements"] = json!({}),
                    "issue-url" => {
                        task["requirements"]["original_issue"]["requirements"]["issue_url"] =
                            json!("https://github.com/OWNER/REPO/issues/2")
                    }
                    _ => unreachable!(),
                }
                let o = plan_document(task, &format!("stage-{i}-{case}"), false);
                assert!(o.status.success(), "{name} {case}: {}", text(&o));
                inspections.push(serde_json::from_slice(&o.stdout).unwrap());
            }
            // Run the hermetic starter Report on this exact already-recorded Snapshot.
            // Separate captures can have distinct Snapshot identities; bind, do not infer.
            let mut unrelated: Value =
                serde_json::from_slice(&std::fs::read(repo.join("report.json")).unwrap()).unwrap();
            unrelated["task_id"] = json!(format!("unrelated-report-{i}"));
            unrelated["inputs"]["source"] =
                json!({"artifact": v["plan"]["inputs"]["source"]["artifact_ids"][0]});
            unrelated["requirements"] = json!({"original_issue": {
                "goal": "another issue on the exact same source",
                "requirements": {"acceptance": ["unrelated acceptance"]}, "acceptance": null
            }});
            let file = temp.path().join(format!("unrelated-report-{i}.json"));
            std::fs::write(&file, serde_json::to_vec(&unrelated).unwrap()).unwrap();
            let accepted = common::af()
                .current_dir(&repo)
                .args(["task", "start", "--execute", "--file"])
                .arg(file)
                .args(["--authority", original.trim(), "--json"])
                .arg("--state")
                .arg(&state)
                .output()
                .unwrap();
            assert!(accepted.status.success(), "{}", text(&accepted));
            inspections.push(serde_json::from_slice(&accepted.stdout).unwrap());
            let file = temp.path().join(format!("stage-records-{i}.json"));
            std::fs::write(&file, serde_json::to_vec(&inspections).unwrap()).unwrap();
            let checked = std::process::Command::new("python3")
                .args([
                    "-I",
                    "-c",
                    r#"
import importlib.util, sys
spec = importlib.util.spec_from_file_location('workflow', sys.argv[1])
w = importlib.util.module_from_spec(spec); spec.loader.exec_module(w)
state, file, document = sys.argv[2:]
d = w.read(document)
contract = w.original_issue(d['goal'], d['requirements'])
values = w.read(file)
for i, v in enumerate(values):
    r = w.recorded(state, v['revision_id'], 'af/TaskRevision@1')
    assert r['inputs']['source']['snapshot_id'] == values[0]['plan']['inputs']['source']['snapshot_id'], (i, r['inputs']['source'], values[0]['plan']['inputs']['source'])
    try:
        w.check_native_research_issue(state, r, contract)
    except ValueError:
        assert i > 0, 'valid diagnosis/design contract refused'
    else:
        assert i == 0, 'unrelated or missing native issue contract accepted'
assert values[-1]['result']['acceptance'] == 'satisfied'
"#,
                ])
                .arg(workspace().join("scripts/issue-workflows.py"))
                .arg(&state)
                .arg(&file)
                .arg(repo.join("fixtures/issue-workflows/known-fix.json"))
                .output()
                .unwrap();
            assert!(checked.status.success(), "{name}: {}", text(&checked));
        }
    }
    for (i, name) in [
        "bugfix/research-fix-codex",
        "perf/research-kimi-codex",
        "perf/research-experiment-codex",
    ]
    .into_iter()
    .enumerate()
    {
        let o = plan(name, &format!("missing-{i}"), false);
        assert!(
            !o.status.success(),
            "missing accepted Document must refuse: {name}"
        );
    }
    // Capture real template pins, compose every public route, and ask the native Scenario
    // validator to check the full typed dataflow and authority. No Scenario is executed.
    let commit = std::process::Command::new("git")
        .current_dir(&repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let commit = String::from_utf8(commit.stdout).unwrap();
    for (i, (name, choice)) in selections["selections"]
        .as_object()
        .unwrap()
        .iter()
        .enumerate()
    {
        if name == "bugfix/primitive-codex" {
            continue; // Its exact separate authority is tested below.
        }
        let mut stages = Vec::new();
        if name == "impl/default-codex" {
            stages.push("stage-1.json".to_owned()); // Native accepted design reuse.
        }
        if !choice["research_pipeline"].is_null() {
            stages.push(if name == "perf/research-kimi-codex" {
                "stage-3.json".to_owned()
            } else if name.contains("design") {
                "stage-1.json".to_owned()
            } else {
                "stage-0.json".to_owned()
            });
        }
        if name.starts_with("design/") {
            stages.push("stage-1.json".to_owned());
        } else {
            stages.push(format!("selection-{i}.json"));
            stages.push("stage-2.json".to_owned());
        }
        let mut pins = Vec::new();
        let mut ids = Vec::new();
        for (j, file) in stages.iter().enumerate() {
            let mut document: Value =
                serde_json::from_slice(&std::fs::read(temp.path().join(file)).unwrap()).unwrap();
            document.as_object_mut().unwrap().remove("inputs");
            document["base"] = json!({"commit":commit.trim()});
            let file = temp.path().join(format!("template-{i}-{j}.json"));
            std::fs::write(&file, serde_json::to_vec(&document).unwrap()).unwrap();
            let pinned = common::af()
                .current_dir(&repo)
                .env("HOME", &home)
                .env("AF_PROVIDERS_FILE", home.join("providers.toml"))
                .env("PATH", &path)
                .args(["scenario", "template-pin", "--file"])
                .arg(file)
                .arg("--repo")
                .arg(&repo)
                .arg("--task-state")
                .arg(&state)
                .args(["--authority-commit", commit.trim(), "--json"])
                .output()
                .unwrap();
            assert!(pinned.status.success(), "{name}: {}", text(&pinned));
            let value: Value = serde_json::from_slice(&pinned.stdout).unwrap();
            ids.push(value["template_id"].clone());
            let pin = temp.path().join(format!("pin-{i}-{j}.json"));
            std::fs::write(&pin, pinned.stdout).unwrap();
            pins.push(pin);
        }
        let output = std::process::Command::new("python3")
            .env("HOME", &home)
            .env("AF_PROVIDERS_FILE", home.join("providers.toml"))
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .arg("-I")
            .arg(workspace().join("scripts/issue-workflows.py"))
            .arg("--af")
            .arg(common::af().get_program())
            .args(["compose", name])
            .args(&pins)
            .arg("--routing")
            .arg(workspace().join("fixtures/issue-workflows/routing-known.json"))
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", text(&output));
        if !choice["research_pipeline"].is_null() {
            // Even genuine native template pins on the same source cannot substitute a
            // different issue contract. Re-pin each negative through the native CLI.
            let valid: Value = serde_json::from_slice(&std::fs::read(&pins[0]).unwrap()).unwrap();
            for case in ["missing", "goal", "acceptance"] {
                let mut document = valid["template"]["document"].clone();
                match case {
                    "missing" => {
                        document["requirements"]
                            .as_object_mut()
                            .unwrap()
                            .remove("original_issue");
                    }
                    "goal" => {
                        document["requirements"]["original_issue"]["goal"] =
                            json!("unrelated issue")
                    }
                    "acceptance" => {
                        document["requirements"]["original_issue"]["requirements"]["acceptance"] =
                            json!(["unrelated acceptance"])
                    }
                    _ => unreachable!(),
                }
                let file = temp.path().join(format!("bad-template-{i}-{case}.json"));
                std::fs::write(&file, serde_json::to_vec(&document).unwrap()).unwrap();
                let pinned = common::af()
                    .current_dir(&repo)
                    .env("HOME", &home)
                    .env("AF_PROVIDERS_FILE", home.join("providers.toml"))
                    .env("PATH", &path)
                    .args(["scenario", "template-pin", "--file"])
                    .arg(file)
                    .arg("--repo")
                    .arg(&repo)
                    .arg("--task-state")
                    .arg(&state)
                    .args(["--authority-commit", commit.trim(), "--json"])
                    .output()
                    .unwrap();
                assert!(pinned.status.success(), "{name} {case}: {}", text(&pinned));
                let bad_pin = temp.path().join(format!("bad-pin-{i}-{case}.json"));
                std::fs::write(&bad_pin, pinned.stdout).unwrap();
                let refused = std::process::Command::new("python3")
                    .arg("-I")
                    .arg(workspace().join("scripts/issue-workflows.py"))
                    .args(["compose", name])
                    .arg(bad_pin)
                    .args(&pins[1..])
                    .arg("--routing")
                    .arg(workspace().join("fixtures/issue-workflows/routing-known.json"))
                    .output()
                    .unwrap();
                assert!(
                    !refused.status.success(),
                    "{name} {case}: accepted unrelated issue"
                );
                assert!(
                    text(&refused).contains("original issue contract"),
                    "{}",
                    text(&refused)
                );
            }
        }
        let bundle = temp.path().join(format!("bundle-{i}.json"));
        std::fs::write(&bundle, output.stdout).unwrap();
        let policy = temp.path().join(format!("policy-{i}.json"));
        std::fs::write(&policy,serde_json::to_vec(&json!({"schema":"af/scenario-policy@5","adapters":{},"task_plans":[],"task_templates":ids,"file_roots":[],"decision_keys":{}})).unwrap()).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&policy, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let validated = common::af()
            .args(["scenario", "validate", "--file"])
            .arg(bundle)
            .arg("--authority")
            .arg(policy)
            .arg("--json")
            .output()
            .unwrap();
        assert!(validated.status.success(), "{name}: {}", text(&validated));
    }
    // Exact artifacts cannot impersonate an accepted native report predecessor.
    let mut raw: Value =
        serde_json::from_slice(&std::fs::read(temp.path().join("missing-0.json")).unwrap())
            .unwrap();
    raw["task_id"] = json!("raw-research");
    raw["inputs"] = json!({"research_report":{"artifact":report["result"]["outputs"]["report"]["artifact_ids"][0]}});
    let file = temp.path().join("raw.json");
    std::fs::write(&file, serde_json::to_vec(&raw).unwrap()).unwrap();
    let refused = common::af()
        .current_dir(&repo)
        .args(["task", "plan", "--file"])
        .arg(file)
        .arg("--state")
        .arg(&state)
        .arg("--json")
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(text(&refused).contains("only a recorded Task"));
    // Change only the native Review gate, keeping every package and code check byte.
    // Both inspection admission and reconciliation must reject the actual captured Task.
    let catalog_file = repo.join(".af/task-catalog.toml");
    let catalog_bytes = std::fs::read(&catalog_file).unwrap();
    let code_bytes = std::fs::read(repo.join(".af/code-policy.toml")).unwrap();
    let weakened = String::from_utf8(catalog_bytes.clone())
        .unwrap()
        .replace("\"gate\" = \"major\"", "\"gate\" = \"blocker\"");
    assert_ne!(weakened.as_bytes(), catalog_bytes);
    std::fs::write(&catalog_file, weakened).unwrap();
    git(&repo, &["add", ".af/task-catalog.toml"]);
    git(&repo, &["commit", "-qm", "negative Review gate fixture"]);
    assert_eq!(
        std::fs::read(repo.join(".af/code-policy.toml")).unwrap(),
        code_bytes
    );
    let planned = plan("bugfix/default-codex", "weakened-review-gate", false);
    assert!(planned.status.success(), "{}", text(&planned));
    let weak: Value = serde_json::from_slice(&planned.stdout).unwrap();
    let inspection = temp.path().join("weakened-review-gate.json");
    std::fs::write(&inspection, planned.stdout).unwrap();
    let checked = std::process::Command::new("python3")
        .arg("-I")
        .arg(workspace().join("scripts/issue-workflows.py"))
        .arg("--state")
        .arg(&state)
        .args(["check-plan", "bugfix/default-codex"])
        .arg(&inspection)
        .output()
        .unwrap();
    assert!(
        !checked.status.success(),
        "weakened Review gate must refuse"
    );
    assert!(
        text(&checked).contains("catalog identity"),
        "{}",
        text(&checked)
    );
    let commit = std::process::Command::new("git")
        .current_dir(&repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let commit = String::from_utf8(commit.stdout).unwrap();
    let manifest = temp.path().join("weakened-review-pilot.json");
    std::fs::write(
        &manifest,
        serde_json::to_vec(&json!({
            "schema": "af.issue-workflow-pilot/1",
            "issue": example("known-fix.json")["requirements"]["issue_url"],
            "pull_request": null, "source_commit": commit.trim(),
            "selection": "bugfix/default-codex", "tasks": [weak["task_id"]]
        }))
        .unwrap(),
    )
    .unwrap();
    let isolated = common::af();
    let mut command = std::process::Command::new("python3");
    for (key, value) in isolated.get_envs() {
        if let Some(value) = value {
            command.env(key, value);
        }
    }
    let checked = command
        .arg("-I")
        .arg(workspace().join("scripts/issue-workflows.py"))
        .arg("--af")
        .arg(isolated.get_program())
        .arg("--repo")
        .arg(&repo)
        .arg("--state")
        .arg(&state)
        .arg("reconcile")
        .arg(&manifest)
        .output()
        .unwrap();
    assert!(
        !checked.status.success(),
        "weakened Review reconciliation must refuse"
    );
    assert!(
        text(&checked).contains("catalog identity"),
        "{}",
        text(&checked)
    );
    std::fs::write(&catalog_file, &catalog_bytes).unwrap();
    git(&repo, &["add", ".af/task-catalog.toml"]);
    git(&repo, &["commit", "-qm", "restore approved Review gate"]);
    // Same catalog, changed real policy: native planning may admit it, but every launcher
    // path must refuse it. Only this private repository is changed; no gate is executed.
    let policy_file = repo.join(".af/code-policy.toml");
    let approved_bytes = std::fs::read(&policy_file).unwrap();
    let approved: toml::Value =
        toml::from_str(std::str::from_utf8(&approved_bytes).unwrap()).unwrap();
    let catalog_bytes = std::fs::read(repo.join(".af/task-catalog.toml")).unwrap();
    for (i, case) in ["settings", "command", "removed", "remote"]
        .into_iter()
        .enumerate()
    {
        let settings_file = repo.join(".af/af.toml");
        let settings_bytes =
            std::fs::read(workspace().join("fixtures/issue-workflows/replacement-af.toml"))
                .unwrap();
        std::fs::write(&settings_file, &settings_bytes).unwrap();
        let mut changed = approved.clone();
        match case {
            "settings" => {
                std::fs::write(
                    &settings_file,
                    String::from_utf8(settings_bytes.clone())
                        .unwrap()
                        .replace("bugfix/light-kimi-terra-sol", "default/implementation"),
                )
                .unwrap();
            }
            "command" => {
                changed["checks"]["kernel"]["command"]["program"] =
                    toml::Value::String("/usr/bin/true".into());
                changed["checks"]["kernel"]["command"]["args"] = toml::Value::Array(vec![]);
            }
            "removed" => {
                changed["checks"]
                    .as_table_mut()
                    .unwrap()
                    .remove("markdownlint");
            }
            "remote" => {
                changed["checks"]["kernel"]["remote"]["workflow"] =
                    toml::Value::String(".github/workflows/other.yml".into());
            }
            _ => unreachable!(),
        }
        if case == "settings" {
            std::fs::write(&policy_file, &approved_bytes).unwrap();
        } else {
            std::fs::write(&policy_file, toml::to_string(&changed).unwrap()).unwrap();
        }
        git(&repo, &["add", ".af/code-policy.toml", ".af/af.toml"]);
        git(
            &repo,
            &["commit", "-qm", "negative same-catalog policy fixture"],
        );
        assert_eq!(
            std::fs::read(repo.join(".af/task-catalog.toml")).unwrap(),
            catalog_bytes
        );
        let changed_commit = std::process::Command::new("git")
            .current_dir(&repo)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let changed_commit = String::from_utf8(changed_commit.stdout).unwrap();
        if case == "settings" {
            assert_eq!(std::fs::read(&policy_file).unwrap(), approved_bytes);
            let refused = plan_document(
                json!({"task_id": "retired", "kind": "implement", "goal": "A bounded known fix"}),
                "retired-three-field-default",
                false,
            );
            assert!(!refused.status.success(), "retired default must refuse");
            let launcher = std::process::Command::new("python3")
                .arg("-I")
                .arg(workspace().join("scripts/issue-workflows.py"))
                .arg("--repo")
                .arg(&repo)
                .arg("--state")
                .arg(&state)
                .args(["plan", "bugfix/default-codex", "--file"])
                .arg(repo.join("fixtures/issue-workflows/known-fix.json"))
                .arg("--routing")
                .arg(repo.join("fixtures/issue-workflows/routing-known.json"))
                .output()
                .unwrap();
            assert!(!launcher.status.success());
            assert!(text(&launcher).contains("af.toml"), "{}", text(&launcher));
        }
        if matches!(case, "command" | "settings") {
            let mut pins = Vec::new();
            for (j, file) in ["known-fix.json", "verify.json"].into_iter().enumerate() {
                let mut document = example(file);
                document["base"] = json!({"commit": changed_commit.trim()});
                let file = temp
                    .path()
                    .join(format!("changed-policy-template-{j}.json"));
                std::fs::write(&file, serde_json::to_vec(&document).unwrap()).unwrap();
                let pinned = common::af()
                    .current_dir(&repo)
                    .env("HOME", &home)
                    .env("AF_PROVIDERS_FILE", home.join("providers.toml"))
                    .env("PATH", &path)
                    .args(["scenario", "template-pin", "--file"])
                    .arg(file)
                    .arg("--repo")
                    .arg(&repo)
                    .arg("--task-state")
                    .arg(&state)
                    .args(["--authority-commit", changed_commit.trim(), "--json"])
                    .output()
                    .unwrap();
                assert!(pinned.status.success(), "{}", text(&pinned));
                let file = temp.path().join(format!("changed-policy-pin-{j}.json"));
                std::fs::write(&file, pinned.stdout).unwrap();
                pins.push(file);
            }
            let composed = std::process::Command::new("python3")
                .arg("-I")
                .arg(workspace().join("scripts/issue-workflows.py"))
                .args(["compose", "bugfix/default-codex"])
                .args(pins)
                .arg("--routing")
                .arg(workspace().join("fixtures/issue-workflows/routing-known.json"))
                .output()
                .unwrap();
            assert!(
                !composed.status.success(),
                "changed policy composition must refuse"
            );
            assert!(
                text(&composed).contains(if case == "settings" {
                    "af.toml"
                } else {
                    "code-policy.toml"
                }),
                "{}",
                text(&composed)
            );
        }
        let o = plan("bugfix/default-codex", &format!("policy-{i}"), false);
        if o.status.success() && case != "settings" {
            let inspection = temp.path().join(format!("policy-inspection-{i}.json"));
            std::fs::write(&inspection, o.stdout).unwrap();
            let checked = std::process::Command::new("python3")
                .arg("-I")
                .arg(workspace().join("scripts/issue-workflows.py"))
                .arg("--state")
                .arg(&state)
                .args(["check-plan", "bugfix/default-codex"])
                .arg(&inspection)
                .output()
                .unwrap();
            assert!(
                !checked.status.success(),
                "{case}: changed policy must refuse"
            );
            assert!(
                text(&checked).contains("policy identity"),
                "{}",
                text(&checked)
            );
        } else if !o.status.success() {
            assert_eq!(case, "removed", "{case}: {}", text(&o));
        }
        let checked = std::process::Command::new("python3")
            .args([
                "-I",
                "-c",
                r#"
import importlib.util, sys
spec = importlib.util.spec_from_file_location('w', sys.argv[1])
w = importlib.util.module_from_spec(spec); spec.loader.exec_module(w)
repo, commit = sys.argv[2:]
template = {'repo': repo, 'authority_commit': commit,
            'document': {'pipeline': {'name': 'bugfix/default-codex', 'fallback': 'refuse'}}}
for check in (lambda: w.installed_authority(repo),
              lambda: w.captured_catalog(template)):
    try:
        check()
    except ValueError:
        pass
    else:
        raise AssertionError('accepted changed policy under unchanged catalog')
"#,
            ])
            .arg(workspace().join("scripts/issue-workflows.py"))
            .arg(&repo)
            .arg(changed_commit.trim())
            .output()
            .unwrap();
        assert!(checked.status.success(), "{case}: {}", text(&checked));
    }
    std::fs::write(&policy_file, approved_bytes).unwrap();
    git(&repo, &["add", ".af/code-policy.toml", ".af/af.toml"]);
    git(&repo, &["commit", "-qm", "restore approved fixture policy"]);
    // Delete actual comparison evidence in copied fixture authority, then re-pin that copy.
    // No gate assertion, model contract, production authority or historical Task is relaxed.
    let trial = repo.join(".af/task-packages/issue-internal/experiment-trial/pipeline.toml");
    let mut definition: toml::Value =
        toml::from_str(&std::fs::read_to_string(&trial).unwrap()).unwrap();
    definition["nodes"]
        .as_array_mut()
        .unwrap()
        .retain(|n| n["id"].as_str() != Some("compare"));
    std::fs::write(&trial, toml::to_string(&definition).unwrap()).unwrap();
    let catalog = repo.join(".af/task-catalog.toml");
    let mut installed: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog).unwrap()).unwrap();
    installed["packages"]["issue-internal/experiment-trial"]["digest"] = toml::Value::String(
        af_pipeline_config::lock::package_digest(
            "issue-internal/experiment-trial",
            trial.parent().unwrap(),
        )
        .unwrap(),
    );
    std::fs::write(catalog, toml::to_string(&installed).unwrap()).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "negative comparison fixture"]);
    let refused = plan("perf/research-experiment-codex", "missing-comparison", true);
    assert!(!refused.status.success());
    assert!(
        text(&refused).contains("Condition must refer to another declared receipt producer"),
        "{}",
        text(&refused)
    );
    assert_eq!(fake_models::turns(&auth), "");
}

#[test]
fn pilot_manifest_schema_matches_fixture_and_refuses_unknown_or_lost_identity() {
    let value: Value = serde_json::from_slice(
        &std::fs::read(workspace().join("fixtures/issue-workflows/pilot.example.json")).unwrap(),
    )
    .unwrap();
    let validator = crate::schemas::validator("issue-workflow-pilot-v1.json");
    crate::schemas::valid(&validator, &value);
    for key in ["issue", "source_commit", "tasks"] {
        let mut bad = value.clone();
        bad.as_object_mut().unwrap().remove(key);
        assert!(!validator.is_valid(&bad), "missing {key}");
    }
    for url in [
        "https://github.com/o/r/issues/1?x=1",
        "https://github.com/o/../issues/1",
        "https://github.com/o/r/issues/1#other",
    ] {
        let mut bad = value.clone();
        bad["issue"] = json!(url);
        assert!(!validator.is_valid(&bad), "invalid issue {url}");
    }
    let mut bad = value.clone();
    bad["selection"] = json!("default/implementation");
    assert!(!validator.is_valid(&bad));
    let mut bad = value;
    bad["unexpected"] = json!(true);
    assert!(!validator.is_valid(&bad));
}

#[test]
fn design_acceptance_executes_both_challenges_and_refuses_a_failed_sibling() {
    let temp = tempfile::tempdir().unwrap();
    let (repo, state) = crate::task_report::starter(temp.path());
    install(&repo);
    let home = temp.path().join("home");
    let auth = home.join("codex");
    let (sdk, pi_path, pi_state) = fake_models::install_pi(temp.path(), &home, "kimi-document");
    let script = std::fs::read_to_string(&sdk)
        .unwrap()
        .replace("id: 'k3'", "id: 'k3.1-pro'");
    let start = script
        .find("          const sources=request.inputs.sources[0].payload.sources;")
        .unwrap();
    let end = script[start..]
        .find("        } else if (mode==='retry'")
        .unwrap()
        + start;
    let script = format!(
        "{}{}{}",
        &script[..start],
        r#"
          const i=Object.fromEntries(Object.entries(request.inputs).map(([k,v])=>[k,v[0]]));
          const c=i.checks.payload;
          answer=JSON.stringify({schema:'af.worker-reply/1',outputs:{result:[{
            document_id:i.document.artifact_id,sources_id:c.sources_id,
            requirements_id:i.requirements.artifact_id,check_receipt_id:i.checks.artifact_id,
            source_snapshot_id:i.source.snapshot_id,outcome:state().rejectChallenge?'failed':'passed',
            summary:'Independent Kimi challenge of the original design'}]}});
"#,
        &script[end..]
    );
    std::fs::write(&sdk, script).unwrap();
    let bin = temp.path().join("bin");
    fake_models::install("codex", &bin, &auth);
    let codex = bin.join("codex");
    let original = std::fs::read_to_string(&codex).unwrap();
    let end = original.rfind("sys.exit(64)").unwrap();
    let execution = r#"
request=sys.stdin.read()
if request==PROBE_INPUT:
 message='OK'
else:
 r=json.loads(request); i={k:v[0] for k,v in r['inputs'].items()}
 if 'document' not in i:
  assert i['requirements']['payload']['specification']['original_issue']['requirements']['acceptance']
  reply={'schema':'af.worker-reply/1','outputs':{'draft':[{
   'schema':'af.document-draft/3','title':'Reusable design',
   'sections':[{'heading':'Findings','body':'The original issue requires exact binding.'},
               {'heading':'Recommendation','body':'Retain original issue and both challenges.'}],
   'citations':[{'kind':'repository','location':{'path':'README.md'}}]}]}}
 else:
  c=i['checks']['payload']; assert c['outcome']=='passed'
  summary='Independent Terra challenge'
  if 'terra_challenge' in i or 'kimi_challenge' in i:
   assert 'terra_challenge' in i and 'kimi_challenge' in i
   for port in ('terra_challenge','kimi_challenge'):
    e=i[port]['payload']; assert e['outcome']=='passed'
    assert e['document_id']==i['document']['artifact_id']
    assert e['requirements_id']==i['requirements']['artifact_id']
   summary='Accepted original design after '+i['terra_challenge']['artifact_id']+' '+i['kimi_challenge']['artifact_id']
  reply={'schema':'af.worker-reply/1','outputs':{'result':[{
   'document_id':i['document']['artifact_id'],'sources_id':c['sources_id'],
   'requirements_id':i['requirements']['artifact_id'],'check_receipt_id':i['checks']['artifact_id'],
   'source_snapshot_id':i['source']['snapshot_id'],'outcome':'passed','summary':summary}]}}
 message=json.dumps(reply)
if '-o' in sys.argv:
 with open(sys.argv[sys.argv.index('-o')+1],'w') as f:f.write(message)
print(json.dumps({'type':'thread.started','thread_id':'synthetic-design'}))
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':message}}))
print(json.dumps({'type':'turn.completed','usage':{'input_tokens':10,'cached_input_tokens':0,'output_tokens':5}}))
"#;
    std::fs::write(
        &codex,
        crate::provider_cli::with_probe(&format!("{}{execution}", &original[..end])),
    )
    .unwrap();
    let providers = home.join("providers.toml");
    std::fs::write(&providers, toml::to_string(&json!({"version":4,"providers":[
        {"id":"codex-personal","kind":"codex","auth_dir":auth},
        {"id":"pi-personal","kind":"pi","backend":"kimi-coding","auth_dir":home,"sdk_entrypoint":sdk}
    ]})).unwrap()).unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin)
            .chain(std::env::split_paths(&pi_path))
            .chain([PathBuf::from("/usr/bin"), PathBuf::from("/bin")]),
    )
    .unwrap();
    let task: Value = serde_json::from_slice(
        &std::fs::read(repo.join("fixtures/issue-workflows/design.json")).unwrap(),
    )
    .unwrap();
    let run = |id: &str| {
        let mut task = task.clone();
        task["task_id"] = json!(id);
        let file = temp.path().join(format!("{id}.json"));
        std::fs::write(&file, serde_json::to_vec(&task).unwrap()).unwrap();
        common::af()
            .current_dir(&repo)
            .env("HOME", &home)
            .env("AF_PI_HOST_MODULE", &sdk)
            .env("AF_PROVIDERS_FILE", &providers)
            .env("PATH", &path)
            .args(["task", "start", "--execute", "--file"])
            .arg(file)
            .arg("--state")
            .arg(&state)
            .arg("--json")
            .output()
            .unwrap()
    };
    let output = run("accepted-design");
    assert!(output.status.success(), "{}", text(&output));
    let done: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(done["result"]["acceptance"], "satisfied", "{done}");
    let cas = af_store::Cas::open(state.join("cas")).unwrap();
    let receipt = cas
        .get_json(
            done["result"]["outputs"]["verification"]["artifact_ids"][0]
                .as_str()
                .unwrap(),
        )
        .unwrap();
    let evaluation = cas
        .get_json(receipt["payload"]["evaluation_id"].as_str().unwrap())
        .unwrap();
    let summary = evaluation["payload"]["summary"].as_str().unwrap();
    assert!(summary.starts_with("Accepted original design after sha256:"));
    let challenges: Vec<_> = summary
        .split_whitespace()
        .filter(|s| s.starts_with("sha256:"))
        .collect();
    assert_eq!(challenges.len(), 2);
    assert_ne!(challenges[0], challenges[1]);
    for challenge in challenges {
        assert!(
            evaluation["input_artifacts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|id| id == challenge)
        );
    }
    assert_eq!(
        receipt["payload"]["invocation"]["inputs"]["document"]["artifact_ids"][0],
        done["result"]["outputs"]["report"]["artifact_ids"][0]
    );
    let mut pi: Value = serde_json::from_slice(&std::fs::read(&pi_state).unwrap()).unwrap();
    pi["rejectChallenge"] = json!(true);
    std::fs::write(pi_state, serde_json::to_vec(&pi).unwrap()).unwrap();
    let rejected = run("rejected-design");
    let rejected: Value = serde_json::from_slice(&rejected.stdout).unwrap();
    assert_ne!(rejected["result"]["acceptance"], "satisfied");
    let explain = common::af()
        .current_dir(&repo)
        .args(["task", "explain", "rejected-design", "--json", "--state"])
        .arg(&state)
        .output()
        .unwrap();
    assert!(
        text(&explain).contains("Report evaluation dependency failed"),
        "{}",
        text(&explain)
    );
    assert!(done["result"]["outputs"].get("snapshot").is_none());
    let pipeline = repo.join(".af/task-packages/design/astra-kimi/pipeline.toml");
    let mut definition: toml::Value =
        toml::from_str(&std::fs::read_to_string(&pipeline).unwrap()).unwrap();
    let final_node = definition["nodes"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|n| n["id"].as_str() == Some("verify"))
        .unwrap();
    final_node["inputs"]
        .as_table_mut()
        .unwrap()
        .remove("kimi_challenge");
    std::fs::write(&pipeline, toml::to_string(&definition).unwrap()).unwrap();
    crate::task_report::repin_and_commit(&repo);
    let missing = run("missing-challenge-design");
    assert!(!missing.status.success());
    assert!(
        text(&missing).contains("kimi_challenge"),
        "{}",
        text(&missing)
    );
}

#[test]
fn launcher_freshness_uses_native_catalog_and_refuses_stale_or_unavailable_without_inference() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let auth = home.join("codex");
    let bin = temp.path().join("bin");
    fake_models::install("codex", &bin, &auth);
    let providers = home.join("providers.toml");
    std::fs::write(
        &providers,
        toml::to_string(&json!({"version":4,"providers":[
        {"id":"codex-personal","kind":"codex","auth_dir":auth}]}))
        .unwrap(),
    )
    .unwrap();
    let probe = || {
        std::process::Command::new("python3")
            .env("HOME", &home).env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_STATE_HOME", home.join("state")).env("XDG_CACHE_HOME", home.join("cache"))
            .env("AF_PROVIDERS_FILE", &providers).env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .args(["-I", "-c", "import importlib.util,sys; s=importlib.util.spec_from_file_location('w',sys.argv[1]); w=importlib.util.module_from_spec(s); s.loader.exec_module(w); w.check_latest_models(sys.argv[2], 'bugfix/default-codex')"])
            .arg(workspace().join("scripts/issue-workflows.py")).arg(common::af().get_program())
            .output().unwrap()
    };
    let fresh = probe();
    assert!(fresh.status.success(), "{}", text(&fresh));
    let catalog = auth.join("codex-model-list.json");
    let original = std::fs::read(&catalog).unwrap();
    let mut changed: Value = serde_json::from_slice(&original).unwrap();
    let mut newer = changed["data"][0].clone();
    newer["id"] = json!("gpt-6.2-sol");
    newer["model"] = json!("gpt-6.2-sol");
    changed["data"].as_array_mut().unwrap().push(newer);
    std::fs::write(&catalog, serde_json::to_vec(&changed).unwrap()).unwrap();
    let stale = probe();
    assert!(!stale.status.success());
    assert!(
        text(&stale).contains("superseded or unavailable active model pin"),
        "{}",
        text(&stale)
    );
    std::fs::write(&catalog, original).unwrap();
    std::fs::write(auth.join("catalog-broken"), "broken").unwrap();
    let unavailable = probe();
    assert!(!unavailable.status.success());
    assert!(
        text(&unavailable).contains("latest-model catalog unavailable"),
        "{}",
        text(&unavailable)
    );
    assert_eq!(fake_models::turns(&auth), "");
}

#[test]
fn primitive_codex_requires_separate_captured_experiment_authority() {
    let temp = tempfile::tempdir().unwrap();
    let (repo, state) = crate::task_report::starter(temp.path());
    install(&repo);
    let home = temp.path().join("home");
    let bin = temp.path().join("bin");
    let auth = home.join("codex");
    fake_models::install("codex", &bin, &auth);
    let providers = temp.path().join("providers.toml");
    std::fs::write(
        &providers,
        toml::to_string(&json!({"version":4,"providers":[
            {"id":"codex-personal","kind":"codex","auth_dir":auth}
        ]}))
        .unwrap(),
    )
    .unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin.clone())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let mut task: Value = serde_json::from_slice(
        &std::fs::read(repo.join("fixtures/issue-workflows/known-fix.json")).unwrap(),
    )
    .unwrap();
    task["pipeline"]["name"] = json!("bugfix/primitive-codex");
    let file = temp.path().join("primitive.json");
    let plan = |id: &str| {
        let mut document = task.clone();
        document["task_id"] = json!(id);
        std::fs::write(&file, serde_json::to_vec(&document).unwrap()).unwrap();
        common::af()
            .current_dir(&repo)
            .env("HOME", &home)
            .env("AF_PROVIDERS_FILE", &providers)
            .env("PATH", &path)
            .args(["task", "plan", "--file"])
            .arg(&file)
            .arg("--state")
            .arg(&state)
            .arg("--json")
            .output()
            .unwrap()
    };
    let refused = plan("normal-primitive");
    assert_primitive_model_policy_refusal(&refused);
    copy_private_catalog(
        &workspace().join("fixtures/issue-workflows/experiment-catalog.toml"),
        &repo.join(".af/task-catalog.toml"),
    );
    git(&repo, &["add", ".af/task-catalog.toml"]);
    git(
        &repo,
        &["commit", "-qm", "explicit private experiment authority"],
    );
    let output = plan("experiment-primitive");
    assert!(output.status.success(), "{}", text(&output));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["attempts"], 0);
    let bindings = value["plan"]["bindings"].as_object().unwrap();
    assert_eq!(bindings.len(), 4);
    for (role, model, effort) in [
        ("implementer", "gpt-6.1-terra", "medium"),
        ("bugs", "gpt-6.1-terra", "high"),
        ("correctness", "gpt-6.1-sol", "medium"),
        ("evaluator", "gpt-6.1-sol", "high"),
    ] {
        let slot = format!("root.slots.{role}");
        assert_eq!(bindings[&slot]["execution"]["model"], model);
        assert_eq!(bindings[&slot]["execution"]["effort"], effort);
        assert_eq!(
            value["graph"]["slots"][&slot]["worker"],
            format!("issue-internal/primitive-{role}")
        );
    }
    let graph = value["graph"].to_string();
    for required in [
        "ImplementationTestEvidence@1",
        "ImplementationEvaluationInput@1",
        "TaskReviewRound@1",
        "markdownlint",
        "kernel",
    ] {
        assert!(graph.contains(required), "missing {required}");
    }
    assert!(!graph.contains("publish-gate"));
    let inspection = temp.path().join("inspection.json");
    std::fs::write(&inspection, output.stdout).unwrap();
    let checked = std::process::Command::new("python3")
        .arg("-I")
        .arg(workspace().join("scripts/issue-workflows.py"))
        .arg("--state")
        .arg(&state)
        .args(["check-plan", "bugfix/primitive-codex"])
        .arg(&inspection)
        .output()
        .unwrap();
    assert!(checked.status.success(), "{}", text(&checked));
    assert_eq!(fake_models::turns(&auth), "", "planning must be token-free");
}
