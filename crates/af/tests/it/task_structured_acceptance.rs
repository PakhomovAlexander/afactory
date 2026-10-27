//! A Task file's structured `acceptance` (ADR-0255), against `fixtures/task-runtime/acceptance`:
//! the pagination Task whose Pipeline declares the optional `acceptance` root port and hands it
//! to its evaluator. The items are captured as one `af/TaskAcceptance@1` record, carried
//! verbatim in the `af/TaskCheckReceipt@2`, delivered to the evaluator as its own input, and a
//! passing evaluation names each by its ID or does not verify the Task. A Pipeline without the
//! port is refused at plan time, before anything is recorded.

use std::path::Path;
use std::process::Command;

use review_store::Cas;
use serde_json::{Value, json};

use crate::task_cli;

/// The fixture `name`, made load-safe, its packages pinned and committed, with `edit` applied
/// first; and its Task file at the load-safe limits, with `acceptance` as given.
fn fixture(
    root: &Path,
    name: &str,
    acceptance: Option<Value>,
    edit: &dyn Fn(&Path),
) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let (repo, state) = task_cli::fixture_named(root, name);
    task_cli::load_safe_walls(&repo);
    edit(&repo);
    let path = repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for (package, pin) in catalog["packages"].as_table_mut().unwrap() {
        pin["digest"] = toml::Value::String(
            review_config::lock::package_digest(package, &repo.join(pin["path"].as_str().unwrap()))
                .unwrap(),
        );
    }
    std::fs::write(&path, toml::to_string(&catalog).unwrap()).unwrap();
    for args in [
        ["add", "-A"].as_slice(),
        ["commit", "-qm", "load-safe"].as_slice(),
    ] {
        let output = Command::new("git")
            .current_dir(&repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    let mut ticket: Value =
        serde_json::from_slice(&std::fs::read(repo.join("ticket.json")).unwrap()).unwrap();
    task_cli::load_safe_limits(&mut ticket["limits"]);
    match acceptance {
        Some(items) => ticket["acceptance"] = items,
        None => {
            ticket.as_object_mut().unwrap().remove("acceptance");
        }
    }
    let file = root.join("ticket.json");
    std::fs::write(&file, serde_json::to_vec_pretty(&ticket).unwrap()).unwrap();
    (repo, state, file)
}

fn af(repo: &Path, state: &Path, args: &[&str]) -> (i32, String, String) {
    let output = crate::common::af()
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(args)
        .arg("--state")
        .arg(state)
        .output()
        .unwrap();
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Every artifact of `kind` in the Store, with its ID.
fn artifacts(state: &Path, kind: &str) -> Vec<(String, Value)> {
    let cas = Cas::open(state.join("cas")).unwrap();
    let mut found = Vec::new();
    let mut stack = vec![state.join("cas/objects")];
    while let Some(next) = stack.pop() {
        for entry in std::fs::read_dir(&next).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Some(digest) = path
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|prefix| prefix.to_str())
                .zip(path.file_name().and_then(|name| name.to_str()))
                .map(|(prefix, rest)| format!("sha256:{prefix}{rest}"))
            else {
                continue;
            };
            if let Ok(Some(artifact)) = cas.get_optional_artifact(&digest)
                && artifact.artifact_type == kind
            {
                found.push((digest, artifact.payload));
            }
        }
    }
    found
}

fn items() -> Value {
    json!([
        {"id": "offset", "text": "paginate skips the first offset items"},
        {"id": "limit", "text": "paginate returns at most limit items"}
    ])
}

/// The items are captured, carried in the receipt, handed to the evaluator as its own input, and
/// a pass that names each by its ID verifies the Task.
#[test]
fn acceptance_reaches_the_receipt_and_the_evaluator_and_is_judged_by_id() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state, file) = fixture(root.path(), "acceptance", Some(items()), &|_| {});
    let (code, stdout, stderr) = af(
        &repo,
        &state,
        &[
            "task",
            "start",
            "--execute",
            "--file",
            file.to_str().unwrap(),
            "--json",
        ],
    );
    // The fixture's evaluator asserts that its `acceptance` input and the receipt's items agree.
    assert_eq!(code, 0, "{stderr}\n{stdout}");
    let shown: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(shown["result"]["acceptance"], "satisfied", "{shown:#}");

    let records = artifacts(&state, "af/TaskAcceptance@1");
    let [(record_id, record)] = records.as_slice() else {
        panic!("one acceptance record: {records:?}");
    };
    crate::schemas::valid(
        &crate::schemas::validator("task-acceptance-v1.json"),
        record,
    );
    assert_eq!(
        *record,
        json!({"schema": "af.task-acceptance/1", "items": items()})
    );
    let receipts = artifacts(&state, "af/TaskCheckReceipt@2");
    let [(_, receipt)] = receipts.as_slice() else {
        panic!("one receipt: {receipts:?}");
    };
    crate::schemas::valid(
        &crate::schemas::validator("task-check-receipt-v2.json"),
        receipt,
    );
    assert_eq!(
        receipt["acceptance"],
        json!({"record_id": record_id, "items": items()})
    );
    // The revision binds the record to the Pipeline's `acceptance` port.
    let (code, inspection, stderr) = af(
        &repo,
        &state,
        &["task", "show", "pagination-acceptance", "--json"],
    );
    assert_eq!(code, 0, "{stderr}");
    let inspection: Value = serde_json::from_str(inspection.trim()).unwrap();
    let cas = Cas::open(state.join("cas")).unwrap();
    let revision = cas
        .get_json(inspection["revision_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        revision["payload"]["inputs"]["acceptance"],
        json!({"artifact_ids": [record_id], "artifact_type": "af/TaskAcceptance@1",
            "cardinality": "one"}),
        "{revision:#}"
    );
    assert!(
        revision["payload"]["provenance"]["input_artifact_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(record_id)),
        "{revision:#}"
    );
    let evaluations = artifacts(&state, "af/TaskEvaluation@1");
    let [(_, evaluation)] = evaluations.as_slice() else {
        panic!("one evaluation: {evaluations:?}");
    };
    let reason = evaluation["reason"].as_str().unwrap();
    assert!(
        reason.contains("offset: met") && reason.contains("limit: met"),
        "{reason}"
    );
}

/// A pass that leaves an item unjudged breaks the evaluator's output contract: the reply is
/// refused with the item named, and the Task does not verify.
#[test]
fn a_pass_that_leaves_an_item_unjudged_does_not_verify() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state, file) = fixture(root.path(), "acceptance", Some(items()), &|repo| {
        std::fs::write(
            repo.join(".af/task-packages/fixture/evaluator/worker.py"),
            "import json,sys\nrequest=json.load(sys.stdin)\n\
             assert request['inputs']['acceptance'][0]['payload']['items'][1]['id']=='limit'\n\
             print(json.dumps({'schema':'af.worker-reply/1','outputs':{'result':[{'outcome':\
             'passed','reason':'offset: met, and the rest looks fine'}]}}))\n",
        )
        .unwrap();
    });
    let (code, stdout, stderr) = af(
        &repo,
        &state,
        &[
            "task",
            "start",
            "--execute",
            "--file",
            file.to_str().unwrap(),
        ],
    );
    assert_ne!(code, 0, "{stderr}\n{stdout}");
    let (code, shown, stderr) = af(&repo, &state, &["task", "show", "pagination-acceptance"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        !shown.lines().next().unwrap().contains("verified"),
        "{shown}"
    );
    assert!(
        shown.contains(
            "The evaluation passes the Task without judging acceptance item `limit` by ID"
        ),
        "{shown}"
    );
    assert!(
        artifacts(&state, "af/VerificationResult@1")
            .iter()
            .all(|(_, result)| result["outcome"] != "passed")
    );
}

/// A Pipeline that does not declare the `acceptance` port is refused at plan time, with the
/// Pipeline named, and nothing is recorded; malformed items are refused with the field named.
#[test]
fn acceptance_is_refused_where_no_evaluator_could_judge_it() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state, file) = fixture(root.path(), "pagination", Some(items()), &|_| {});
    let (code, stdout, stderr) = af(
        &repo,
        &state,
        &["task", "plan", "--file", file.to_str().unwrap()],
    );
    assert_eq!(code, 1, "{stderr}\n{stdout}");
    assert!(
        stderr.contains(
            "acceptance: the Pipeline fixture/implementation does not declare the optional \
             `acceptance` input, so its evaluator could never judge the items by their IDs"
        ),
        "{stderr}"
    );
    assert!(
        !state.join("cas/objects").exists() || artifacts(&state, "af/TaskAcceptance@1").is_empty(),
        "nothing is recorded"
    );
    // Under `--json` the refusal is the typed `task_file_invalid` diagnosis of the Task file's
    // `acceptance` field, as `limits.wall_ms` is refused: the file as given, the field, its item
    // IDs and the command that plans the file again.
    let shown = file.to_str().unwrap();
    let (code, stdout, stderr) = af(&repo, &state, &["task", "start", "--file", shown, "--json"]);
    assert_eq!(code, 1, "{stderr}\n{stdout}");
    let document: Value = serde_json::from_str(&stdout).unwrap();
    crate::schemas::valid(&crate::schemas::validator("error-v2.json"), &document);
    assert_eq!(document["cause"], "task_file_invalid", "{document}");
    assert_eq!(
        document["details"],
        json!({"file": shown, "field": "acceptance", "value": ["offset", "limit"]}),
        "{document}"
    );
    let next = document["next_action"].as_str().unwrap();
    assert!(
        next.starts_with(&format!(
            "af task plan --file {}",
            shell_words::quote(shown)
        )),
        "{document}"
    );
    assert!(
        document["error"]
            .as_str()
            .unwrap()
            .starts_with(&format!("Task file {shown}: acceptance: the Pipeline")),
        "{document}"
    );

    let root = tempfile::tempdir().unwrap();
    for (acceptance, refusal) in [
        (
            json!([{"id": "a", "text": "x"}, {"id": "a", "text": "y"}]),
            "acceptance names item id \"a\" twice",
        ),
        (json!([]), "acceptance lists 0 items"),
        (json!([{"id": "a"}]), "acceptance[0]: missing field `text`"),
    ] {
        let directory = root.path().join(format!("case-{}", refusal.len()));
        std::fs::create_dir_all(&directory).unwrap();
        let (repo, state, file) =
            fixture(&directory, "acceptance", Some(acceptance.clone()), &|_| {});
        let (code, stdout, stderr) = af(
            &repo,
            &state,
            &["task", "plan", "--file", file.to_str().unwrap()],
        );
        assert_ne!(code, 0, "{acceptance}: {stdout}");
        assert!(stderr.contains(refusal), "{acceptance}: {stderr}");
    }
}

/// A Pipeline that declares the `acceptance` root port but hands it to no `verify` node delivers
/// the items to nobody: the plan is refused when it is compiled, and nothing runs (ADR-0255).
#[test]
fn a_declared_acceptance_port_no_evaluator_takes_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state, file) = fixture(root.path(), "acceptance", Some(items()), &|repo| {
        let path = repo.join(".af/task-packages/fixture/implementation/pipeline.toml");
        let text = std::fs::read_to_string(&path).unwrap();
        let wired = "[nodes.inputs.acceptance]\nkind = \"input\"\nport = \"acceptance\"\n\n";
        assert!(text.contains(wired), "{text}");
        std::fs::write(&path, text.replace(wired, "")).unwrap();
    });
    let (code, stdout, stderr) = af(
        &repo,
        &state,
        &["task", "plan", "--file", file.to_str().unwrap()],
    );
    assert_ne!(code, 0, "{stderr}\n{stdout}");
    let flat = format!("{stdout} {stderr}")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        flat.contains(
            "declares the `acceptance` input but hands it to no `verify` evaluator node, so no \
             evaluator could judge the items by their IDs"
        ),
        "{stderr}\n{stdout}"
    );
}

/// A Task file without `acceptance` records no acceptance and binds no port: its receipt has no
/// `acceptance`, and the evaluator receives none.
#[test]
fn a_task_file_without_acceptance_plans_and_runs_as_before() {
    let root = tempfile::tempdir().unwrap();
    let (repo, state, file) = fixture(root.path(), "acceptance", None, &|repo| {
        // The fixture's evaluator, without the acceptance it would otherwise judge.
        std::fs::write(
            repo.join(".af/task-packages/fixture/evaluator/worker.py"),
            "import json,sys\nrequest=json.load(sys.stdin)\n\
             assert 'acceptance' not in request['inputs']\n\
             assert 'acceptance' not in request['inputs']['checks'][0]['payload']\n\
             print(json.dumps({'schema':'af.worker-reply/1','outputs':{'result':[{'outcome':\
             'passed','reason':'Verified offset and limit on the sealed source'}]}}))\n",
        )
        .unwrap();
    });
    let (code, stdout, stderr) = af(
        &repo,
        &state,
        &[
            "task",
            "start",
            "--execute",
            "--file",
            file.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "{stderr}\n{stdout}");
    assert!(artifacts(&state, "af/TaskAcceptance@1").is_empty());
    let receipts = artifacts(&state, "af/TaskCheckReceipt@2");
    assert!(
        receipts
            .iter()
            .all(|(_, receipt)| receipt.get("acceptance").is_none()),
        "{receipts:?}"
    );
}
