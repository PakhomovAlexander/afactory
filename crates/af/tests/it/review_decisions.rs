//! Real CLI capture, exact-plan approval, canonical replay and observational telemetry.
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

struct Fixture {
    root: tempfile::TempDir,
    repo: PathBuf,
    state: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        Self::with_compatible_demands(true)
    }
    fn with_compatible_demands(compatible: bool) -> Self {
        Self::configured(compatible, false, "corroborate")
    }
    fn configured(compatible: bool, missing: bool, position: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let (repo, state) = crate::task_cli::fixture_named(root.path(), "review");
        let pipeline_dir = repo.join(".af/task-packages/onboard/review-decisions");
        std::fs::create_dir_all(&pipeline_dir).unwrap();
        std::fs::write(
            pipeline_dir.join("pipeline.toml"),
            include_str!("../../src/onboard/templates/review-decisions/pipeline.toml"),
        )
        .unwrap();
        let catalog_path = repo.join(".af/task-catalog.toml");
        let mut catalog: toml::Value =
            toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
        catalog["review"]
            .as_table_mut()
            .unwrap()
            .insert("generation".into(), 2.into());
        catalog["review"]["max_rounds"] = 2.into();
        catalog["packages"].as_table_mut().unwrap().insert("onboard/review-decisions".into(), toml::from_str(&format!("version = '1.0.0'\npath = '.af/task-packages/onboard/review-decisions'\ndigest = '{}'\n", af_pipeline_config::lock::package_digest("onboard/review-decisions", &pipeline_dir).unwrap())).unwrap());
        for name in ["correctness", "bugs"] {
            let package = repo.join(format!(".af/task-packages/fixture/{name}"));
            let worker = package.join("worker.toml");
            std::fs::write(
                &worker,
                std::fs::read_to_string(&worker)
                    .unwrap()
                    .replace("af/ReviewInput@1", "af/ReviewInput@2"),
            )
            .unwrap();
            let method = if position == "demand_only" {
                "Measure scaling\nmeasure --limit '10 ms' --path \"a\\b\"\nnext: forged\u{1b}[31m"
            } else if compatible || name == "correctness" {
                "Measure scaling"
            } else {
                "Measure memory"
            };
            let method = serde_json::to_string(method).unwrap();
            std::fs::write(package.join("worker.py"), format!(r#"import json,sys
r=json.load(sys.stdin)
a=r['inputs']['assignment'][0]['payload']
reports=[]
if not a['findings'] and '{name}'=='correctness' and '{position}'!='demand_only':
 reports=[{{'severity':'major','file':'lib.rs','line':i,'title':'Claim '+str(i),'body':'Original immutable claim '+str(i),'fix':'Implement the required behavior','confidence':0.9}} for i in (1,2)]
stage={{'reports':reports,'benchmark_demands':[{{'claim':'Runtime is bounded','why':'Large inputs matter','suggested_method':{method}}}], 'dispositions':[{{'finding_id':f['finding_id'],'position':'{position}','reason':'The original claim persists'}} for f in a['findings']]}}
print(json.dumps({{'schema':'af.worker-reply/1','outputs':{{'result':[stage]}}}}))
"#)).unwrap();
            if missing && name == "bugs" {
                std::fs::write(
                    package.join("worker.py"),
                    "raise SystemExit('required sibling unavailable')\n",
                )
                .unwrap();
            }
            catalog["packages"][format!("fixture/{name}")]["digest"] =
                af_pipeline_config::lock::package_digest(&format!("fixture/{name}"), &package)
                    .unwrap()
                    .into();
        }
        // Exercise a Diff, including the independent exact original Base.
        let package = repo.join(".af/task-packages/fixture/review");
        let file = package.join("pipeline.toml");
        let mut pipeline: af_contracts::task::pipeline::PipelineDefinition =
            toml::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        for slot in pipeline.slots.values_mut() {
            slot.input_type = "af/ReviewInput@2".into();
        }
        pipeline.contract.inputs.insert("base".into(), serde_json::from_value(json!({"artifact_type":"af/SourceTree@1","cardinality":"one","optional":true,"affinity":{"kind":"unbound"},"covers":[]})).unwrap());
        for node in &mut pipeline.nodes {
            if matches!(
                node.operator,
                af_contracts::task::pipeline::TaskOperatorV1::ReviewBind {}
            ) {
                node.inputs.insert(
                    "base".into(),
                    serde_json::from_value(json!({"kind":"input","port":"base"})).unwrap(),
                );
            }
        }
        std::fs::write(file, toml::to_string(&pipeline).unwrap()).unwrap();
        catalog["packages"]["fixture/review"]["digest"] =
            af_pipeline_config::lock::package_digest("fixture/review", &package)
                .unwrap()
                .into();
        std::fs::write(catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "native human decision authority"]);
        std::fs::write(repo.join("lib.rs"), "pub fn changed() {}\n// second line\n").unwrap();
        let fixture = Self { root, repo, state };
        let (code, review) = fixture.invoke(&[
            "review",
            "run",
            "--file",
            "review.json",
            "--base",
            "HEAD",
            "--uncommitted",
        ]);
        if missing {
            assert_ne!(code, 0, "{review:#}");
            assert_eq!(review["result"]["domain_conclusion"], "incomplete");
        } else {
            assert_eq!(code, 3, "{review:#}");
        }
        assert_eq!(review["attempts"], 3);
        fixture
    }
    fn invoke(&self, args: &[&str]) -> (i32, Value) {
        let output = crate::common::af()
            .current_dir(&self.repo)
            .args(args)
            .args(["--json", "--state"])
            .arg(&self.state)
            .env("AF_TELEMETRY__LEVEL", "content")
            .env("AF_TELEMETRY__DIR", self.root.path().join("telemetry"))
            .output()
            .unwrap();
        let value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| json!({"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr)}));
        (output.status.code().unwrap(), value)
    }
    fn context(&self, task: &str) -> Value {
        let (code, value) = self.invoke(&["review", "decision-context", task]);
        assert_eq!(code, 0, "{value:#}");
        crate::schemas::valid(
            &crate::schemas::validator("review-decision-context-v1.json"),
            &value,
        );
        value
    }
    fn batch(&self, context: &Value, actions: Vec<Value>) -> Value {
        let mut batch = json!({"schema":"af.review-decisions/1","review_id":context["review_id"],"policy_id":context["review"]["policy_id"],"subject_id":context["review"]["subject_id"],"snapshot_id":context["review"]["snapshot_id"],"actor":"operator","reason":"Apply reviewed exact human decisions","actions":actions});
        if let Some(base) = context["subject"].get("base_snapshot_id") {
            batch["base_snapshot_id"] = base.clone();
        }
        batch
    }
    fn inherited_satisfactions_are_not_new(&self, id: &str, from: &str) {
        let mut file: Value =
            serde_json::from_slice(&std::fs::read(self.repo.join("review.json")).unwrap()).unwrap();
        file["task_id"] = id.into();
        file["inputs"] = json!({"history":{"task":from,"port":"history"}});
        let path = self.root.path().join(format!("{id}.json"));
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        let (code, result) = self.invoke(&[
            "review",
            "run",
            "--file",
            path.to_str().unwrap(),
            "--base",
            "HEAD",
            "--uncommitted",
        ]);
        assert_eq!(code, 3, "{result:#}");
        let ledger = crate::telemetry_ledger::read_ledger(
            &self.root.path().join("telemetry"),
            &af_store::store::task::task_run_id(id).unwrap(),
        );
        assert_eq!(ledger.coverage, "complete", "{:?}", ledger.coverage_reasons);
        assert!(!ledger.demands.is_empty());
        assert!(
            ledger
                .demands
                .iter()
                .all(|d| d.lifecycle.iter().all(|e| e.event != "evidence_satisfied")),
            "a discovery Round must not re-emit inherited satisfaction or reuse: {:?}",
            ledger.demands
        );
    }
    fn refused(&self, id: &str, result: &Value, diagnostic: &str) {
        assert_eq!(result["phase"]["kind"], "finished", "{result:#}");
        assert_eq!(result["result"]["acceptance"], "inconclusive", "{result:#}");
        assert!(
            result["result"]["outputs"].get("review").is_none(),
            "{result:#}"
        );
        assert!(result.to_string().contains(diagnostic), "{result:#}");
        let cas = af_store::Cas::open_existing(self.state.join("cas")).unwrap();
        let store = af_store::EventStore::open_read_only(self.state.join("events.sqlite")).unwrap();
        let task = store.task_projection(&cas, id).unwrap().unwrap();
        let af_contracts::task::TaskPhaseV1::Finished { result_id } = task.phase else {
            panic!("refused decision did not finish");
        };
        let canonical = cas.get_artifact(&result_id).unwrap();
        assert!(
            canonical.payload["outputs"].get("review").is_none(),
            "{canonical:?}"
        );
    }
    fn decide(&self, id: &str, from: &str, batch: &Value, execute: bool) -> (i32, Value) {
        let file = self.root.path().join(format!("{id}.json"));
        std::fs::write(&file, serde_json::to_vec_pretty(batch).unwrap()).unwrap();
        let mut args = vec![
            "review",
            "decide",
            id,
            "--from",
            from,
            "--file",
            file.to_str().unwrap(),
        ];
        if execute {
            args.push("--execute");
        }
        self.invoke(&args)
    }
}
fn git(repo: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}
fn demand(context: &Value, index: usize) -> String {
    context["demand_set"]["demands"][index]["demand_id"]
        .as_str()
        .unwrap()
        .into()
}

#[test]
fn human_evidence_satisfaction_reuse_and_waiver_are_native_zero_token_tasks() {
    let f = Fixture::new();
    let original = f.context("review-cli");
    assert_eq!(
        original["demand_set"]["demands"].as_array().unwrap().len(),
        2
    );
    let first = demand(&original, 0);
    let batch = f.batch(&original, vec![json!({"kind":"attach","demand_id":first,"content":"Measured scaling: bounded. password=hunter2"}), json!({"kind":"satisfy","demand_id":first,"evidence":{"kind":"attached","action":0},"reason":"Exact current Subject measurement"})]);
    crate::schemas::valid(
        &crate::schemas::validator("review-decisions-v1.json"),
        &batch,
    );
    let (code, plan) = f.decide("satisfy", "review-cli", &batch, false);
    assert_eq!(code, 0, "{plan:#}");
    assert_eq!(plan["attempts"], 0);
    assert_eq!(plan["chargeable_tokens"], "0");
    assert_eq!(
        plan["plan"]["inputs"]["source"]["snapshot_id"],
        original["review"]["snapshot_id"]
    );
    assert_eq!(
        plan["plan"]["inputs"]["base"]["snapshot_id"],
        original["subject"]["base_snapshot_id"]
    );
    let (code, result) = f.invoke(&[
        "review",
        "resume",
        "satisfy",
        "--confirm-plan",
        plan["plan_id"].as_str().unwrap(),
    ]);
    assert_eq!(code, 3, "{result:#}");
    assert_eq!(result["attempts"], 0);
    assert_eq!(result["chargeable_tokens"], "0");
    assert_eq!(result["result"]["acceptance"], "satisfied");
    assert_eq!(
        f.invoke(&["review", "resume", "satisfy", "--execute"]).1,
        result,
        "cold replay must not apply the batch again"
    );
    let satisfied = f.context("satisfy");
    assert_eq!(
        satisfied["finding_set"]["findings"],
        original["finding_set"]["findings"]
    );
    assert_eq!(
        satisfied["review"]["selected_results"],
        original["review"]["selected_results"]
    );
    assert_eq!(satisfied["demand_set"]["demands"][0]["status"], "satisfied");
    let second = demand(&satisfied, 1);
    let evidence = satisfied["demand_set"]["demands"][0]["evidence_ids"][0].clone();
    let reused = f.batch(&satisfied, vec![json!({"kind":"reuse_evidence","demand_id":second,"evidence_id":evidence,"reason":"Explicit compatible cross-Demand reuse"})]);
    assert_eq!(f.decide("reuse", "satisfy", &reused, true).0, 3);
    let reused = f.context("reuse");
    assert!(
        reused["demand_set"]["demands"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["status"] == "satisfied")
    );
    assert_eq!(
        reused["review"]["round"], original["review"]["round"],
        "human decisions spend no discovery Round"
    );
    f.inherited_satisfactions_are_not_new("inherited-reuse", "reuse");
    let waived = f.batch(&reused, vec![json!({"kind":"waive","waiver":{"demand_id":second,"subject_id":reused["review"]["subject_id"],"actor":"operator","policy_revision":reused["review"]["policy_id"],"reason":"Explicit operator risk acceptance"}})]);
    assert_eq!(f.decide("waive", "reuse", &waived, true).0, 3);
    let waived = f.context("waive");
    assert_eq!(waived["demand_set"]["demands"][1]["status"], "waived");
    assert_eq!(
        f.context("review-cli"),
        original,
        "original claims, history and authority are immutable"
    );
    let (code, original_again) = f.invoke(&["review", "show", "review-cli"]);
    assert_eq!(code, 0);
    assert_eq!(original_again["attempts"], 3);
    let telemetry = f.root.path().join("telemetry");
    let ledger = crate::telemetry_ledger::read_ledger(
        &telemetry,
        &af_store::store::task::task_run_id("satisfy").unwrap(),
    );
    // Planning is not a recorded execution. The later recording must name the initial
    // unobserved prefix rather than manufacture a complete telemetry history.
    assert_eq!(ledger.coverage, "incomplete");
    assert!(
        ledger
            .coverage_reasons
            .iter()
            .any(|r| r.contains("sequences 0–2 were not mirrored")),
        "{:?}",
        ledger.coverage_reasons
    );
    let ledger = crate::telemetry_ledger::read_ledger(
        &telemetry,
        &af_store::store::task::task_run_id("reuse").unwrap(),
    );
    assert_eq!(ledger.coverage, "complete", "{:?}", ledger.coverage_reasons);
    assert!(ledger.demands.iter().any(|d| {
        d.lifecycle
            .iter()
            .any(|e| e.event == "evidence_satisfied" && e.evidence_id.is_some())
    }));
    let waived_ledger = crate::telemetry_ledger::read_ledger(
        &telemetry,
        &af_store::store::task::task_run_id("waive").unwrap(),
    );
    assert!(
        waived_ledger
            .demands
            .iter()
            .any(|d| d.lifecycle.iter().any(|e| e.event == "waived"))
    );
    assert!(ledger.demands.iter().any(|d| d.status == "satisfied"
        && d.subject_id == original["review"]["subject_id"].as_str().unwrap()
        && d.snapshot_id == original["review"]["snapshot_id"].as_str().unwrap()));
    let cas = af_store::Cas::open_existing(f.state.join("cas")).unwrap();
    let mut forged = cas
        .get_artifact(satisfied["review_id"].as_str().unwrap())
        .unwrap();
    forged.payload["invocation"]["inputs"]["original_conclusion"]["artifact_ids"][0] =
        json!(format!("sha256:{}", "f".repeat(64)));
    let id = cas
        .put_artifact(
            forged.artifact_type,
            forged.producer,
            forged.input_artifacts,
            forged.subject_snapshot_id,
            forged.payload,
        )
        .unwrap()
        .0;
    assert!(
        af_runtime::task::review::ReviewTaskDomain::recorded_ledger(&cas, &id)
            .unwrap_err()
            .contains("exact approved root inputs")
    );
}

#[test]
fn one_batch_attach_satisfy_waive_keeps_evidence_lifecycle_and_replay_identity() {
    let f = Fixture::new();
    let original = f.context("review-cli");
    let first = demand(&original, 0);
    let batch = f.batch(&original, vec![
        json!({"kind":"attach","demand_id":first,"content":"Exact current measurement"}),
        json!({"kind":"satisfy","demand_id":first,"evidence":{"kind":"attached","action":0},"reason":"Measured this Subject"}),
        json!({"kind":"waive","waiver":{"demand_id":first,"subject_id":original["review"]["subject_id"],"actor":"operator","policy_revision":original["review"]["policy_id"],"reason":"Explicitly waive after recording measurement"}}),
    ]);
    let (code, result) = f.decide("satisfy-waive", "review-cli", &batch, true);
    assert_eq!(code, 3, "{result:#}");
    assert_eq!(result["result"]["acceptance"], "satisfied");
    let current = f.context("satisfy-waive");
    let cas = af_store::Cas::open_existing(f.state.join("cas")).unwrap();
    let canonical = af_runtime::task::review::ReviewTaskDomain::recorded_ledger(
        &cas,
        current["review_id"].as_str().unwrap(),
    )
    .unwrap();
    let record = canonical.demand(first.as_str()).unwrap();
    assert_eq!(record.evidence.len(), 1);
    assert_eq!(record.satisfactions.len(), 1);
    assert_eq!(record.waivers.len(), 1);
    let evidence = &record.evidence[0];
    assert_eq!(
        record.satisfactions[0].satisfaction.evidence_id,
        evidence.artifact_id
    );
    assert_eq!(
        evidence.evidence.subject_id,
        original["review"]["subject_id"]
    );
    for id in [
        &evidence.record_id,
        &record.satisfactions[0].record_id,
        &record.waivers[0].record_id,
    ] {
        let envelope = cas.get_artifact(id).unwrap();
        assert_eq!(
            envelope.subject_snapshot_id.as_deref(),
            original["review"]["snapshot_id"].as_str()
        );
    }
    let telemetry = f.root.path().join("telemetry");
    let run = af_store::store::task::task_run_id("satisfy-waive").unwrap();
    let before = crate::telemetry_ledger::read_ledger(&telemetry, &run);
    assert_eq!(before.coverage, "complete", "{:?}", before.coverage_reasons);
    let view = before
        .demands
        .iter()
        .find(|d| d.demand_id == first)
        .unwrap();
    assert_eq!(view.status, "waived");
    assert_eq!(view.subject_id, evidence.evidence.subject_id);
    assert_eq!(view.snapshot_id, original["review"]["snapshot_id"]);
    let lifecycle: Vec<_> = view
        .lifecycle
        .iter()
        .filter(|e| e.event != "opened")
        .collect();
    assert_eq!(lifecycle.len(), 2, "{lifecycle:?}");
    assert_eq!(lifecycle[0].event, "evidence_satisfied");
    assert_eq!(
        lifecycle[0].evidence_id.as_deref(),
        Some(evidence.artifact_id.as_str())
    );
    assert_eq!(lifecycle[1].event, "waived");
    assert!(lifecycle[0].sequence.is_some());
    assert_eq!(
        lifecycle[0].sequence, lifecycle[1].sequence,
        "one committed native Round"
    );
    assert_eq!(
        f.invoke(&["review", "resume", "satisfy-waive", "--execute"])
            .1,
        result
    );
    let after = crate::telemetry_ledger::read_ledger(&telemetry, &run);
    assert_eq!(
        after.demands, before.demands,
        "cold replay emits no duplicate lifecycle events"
    );
    f.inherited_satisfactions_are_not_new("inherited-waived", "satisfy-waive");
    assert_eq!(f.context("review-cli"), original);
}

#[test]
fn human_resolutions_challenges_policy_time_and_grouping_preserve_original_claims() {
    let f = Fixture::new();
    let original = f.context("review-cli");
    let attached = f.batch(&original, vec![json!({"kind":"attach","demand_id":demand(&original,0),"content":"Inspected the claimed behavior: evidence one"}), json!({"kind":"attach","demand_id":demand(&original,0),"content":"New counter-evidence from a distinct observation"})]);
    assert_eq!(f.decide("attach", "review-cli", &attached, true).0, 3);
    let context = f.context("attach");
    let claims: Vec<_> = context["finding_view_ids"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let key = &claims[0];
    let evidence = context["demand_set"]["demands"][0]["evidence_ids"]
        .as_array()
        .unwrap();
    let resolution = json!({"finding_id":key,"expected_finding_view_id":context["finding_view_ids"][key],"subject_id":context["review"]["subject_id"],"outcome":"rejected","actor":"operator","policy_revision":context["review"]["policy_id"],"reason":"The claim is disproved by this exact observation","evidence_ids":[evidence[0]]});
    assert_eq!(
        f.decide(
            "reject",
            "attach",
            &f.batch(
                &context,
                vec![json!({"kind":"resolve","resolution":resolution})]
            ),
            true
        )
        .0,
        3
    );
    let rejected = f.context("reject");
    assert_eq!(
        rejected["finding_set"]["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["finding_id"] == *key)
            .unwrap()["status"],
        "rejected"
    );
    let challenge = json!({"finding_id":key,"resolution_id":rejected["resolutions"][key]["artifact_id"],"subject_id":rejected["review"]["subject_id"],"kind":"new_evidence","actor":"operator","reason":"Materially new observation challenges the rejection","evidence_ids":[evidence[1]]});
    assert_eq!(
        f.decide(
            "challenge",
            "reject",
            &f.batch(
                &rejected,
                vec![json!({"kind":"challenge","challenge":challenge})]
            ),
            true
        )
        .0,
        3
    );

    let mut repeated = challenge.clone();
    repeated["evidence_ids"] = json!([evidence[0]]);
    let (code, result) = f.decide(
        "repeat-evidence",
        "reject",
        &f.batch(
            &rejected,
            vec![json!({"kind":"challenge","challenge":repeated})],
        ),
        true,
    );
    assert_ne!(code, 0, "{result:#}");
    f.refused(
        "repeat-evidence",
        &result,
        "Challenge has no materially new evidence, severity, scope or expiry",
    );
    assert_eq!(f.context("reject"), rejected);
    let challenged = f.context("challenge");
    let tracked = json!({"finding_id":key,"expected_finding_view_id":challenged["finding_view_ids"][key],"subject_id":challenged["review"]["subject_id"],"outcome":"wontfix_tracked","actor":"operator","policy_revision":challenged["review"]["policy_id"],"reason":"Track explicitly within the bounded risk window","evidence_ids":[evidence[0]],"max_accepted_severity":"major","tracking_reference":"local:issue-17","expires_at_policy_time":3});
    assert_eq!(
        f.decide(
            "tracked",
            "challenge",
            &f.batch(
                &challenged,
                vec![json!({"kind":"resolve","resolution":tracked})]
            ),
            true
        )
        .0,
        3
    );

    let mut insufficient = tracked.clone();
    insufficient["max_accepted_severity"] = "minor".into();
    let (code, result) = f.decide(
        "ceiling-refused",
        "challenge",
        &f.batch(
            &challenged,
            vec![json!({"kind":"resolve","resolution":insufficient})],
        ),
        true,
    );
    assert_ne!(code, 0, "{result:#}");
    f.refused(
        "ceiling-refused",
        &result,
        "tracked-wontfix severity ceiling is below the current Finding severity",
    );
    let mut stale = tracked.clone();
    stale["expected_finding_view_id"] = context["finding_view_ids"][key].clone();
    let (code, result) = f.decide(
        "view-refused",
        "challenge",
        &f.batch(
            &challenged,
            vec![json!({"kind":"resolve","resolution":stale})],
        ),
        true,
    );
    assert_ne!(code, 0, "{result:#}");
    f.refused(
        "view-refused",
        &result,
        "Resolution changed its exact current Finding view",
    );
    let tracked = f.context("tracked");
    let advance = f.batch(&tracked, vec![json!({"kind":"policy_time","time":{"tick":3,"actor":"operator","reason":"Risk acceptance has expired"}})]);
    assert_eq!(f.decide("expire", "tracked", &advance, true).0, 3);
    let expired = f.context("expire");
    assert_eq!(expired["policy_time"], 3);
    let retrograde = f.batch(&expired, vec![json!({"kind":"policy_time","time":{"tick":3,"actor":"operator","reason":"The same tick cannot advance time"}})]);
    let (code, result) = f.decide("retrograde", "expire", &retrograde, true);
    assert_ne!(code, 0, "{result:#}");
    f.refused(
        "retrograde",
        &result,
        "policy time must advance monotonically",
    );
    assert_eq!(f.context("expire"), expired);
    let rejected_ledger = crate::telemetry_ledger::read_ledger(
        &f.root.path().join("telemetry"),
        &af_store::store::task::task_run_id("reject").unwrap(),
    );
    assert!(
        rejected_ledger
            .findings
            .iter()
            .any(|finding| finding.dispositions.iter().any(|d| d.status == "rejected"))
    );
    assert_eq!(
        expired["finding_set"]["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["finding_id"] == *key)
            .unwrap()["status"],
        "contested"
    );
    let group = json!({"kind":"group","grouping":{"from":claims[0],"into":claims[1],"action":"group","round":expired["review"]["round"]},"from_view_id":expired["finding_view_ids"][&claims[0]],"into_view_id":expired["finding_view_ids"][&claims[1]]});
    assert_eq!(
        f.decide(
            "group",
            "expire",
            &f.batch(&expired, vec![group.clone()]),
            true
        )
        .0,
        3
    );
    let grouped = f.context("group");
    let mut ungroup = group;
    ungroup["grouping"]["action"] = "ungroup".into();
    ungroup["from_view_id"] = grouped["finding_view_ids"][&claims[0]].clone();
    ungroup["into_view_id"] = grouped["finding_view_ids"][&claims[1]].clone();
    assert_eq!(
        f.decide("ungroup", "group", &f.batch(&grouped, vec![ungroup]), true)
            .0,
        3
    );
    let ungrouped = f.context("ungroup");
    assert_eq!(
        ungrouped["finding_set"]["findings"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(f.context("review-cli"), original);
}

#[test]
fn human_decisions_refuse_forged_authority_cross_demand_satisfaction_and_partial_batches() {
    let f = Fixture::new();
    let original = f.context("review-cli");
    let first = demand(&original, 0);
    let base = f.batch(
        &original,
        vec![json!({"kind":"attach","demand_id":first,"content":"Observation bytes"})],
    );
    for fault in [
        "policy",
        "subject",
        "snapshot",
        "base",
        "review",
        "actor",
        "unknown_demand",
        "missing_evidence",
        "cross_demand",
        "fixed",
    ] {
        let mut batch = base.clone();
        let bogus = format!("sha256:{}", "f".repeat(64));
        match fault {
            "policy" => batch["policy_id"] = bogus.into(),
            "subject" => batch["subject_id"] = bogus.into(),
            "snapshot" => batch["snapshot_id"] = bogus.into(),
            "base" => batch["base_snapshot_id"] = bogus.into(),
            "review" => batch["review_id"] = bogus.into(),
            "actor" => batch["actions"] = json!([{"kind":"waive","waiver":{"demand_id":first,"subject_id":batch["subject_id"],"actor":"somebody-else","policy_revision":batch["policy_id"],"reason":"Mismatched actor"}}]),
            "unknown_demand" => batch["actions"][0]["demand_id"] = bogus.into(),
            "missing_evidence" => batch["actions"] = json!([{"kind":"satisfy","demand_id":first,"evidence":{"kind":"recorded","artifact_id":bogus},"reason":"Missing CAS Evidence cannot satisfy"}]),
            "cross_demand" => batch["actions"].as_array_mut().unwrap().push(json!({"kind":"satisfy","demand_id":demand(&original,1),"evidence":{"kind":"attached","action":0},"reason":"Implicit reuse must fail"})),
            "fixed" => {
                let (key, view) = original["finding_view_ids"].as_object().unwrap().iter().next().unwrap();
                batch["actions"] = json!([{"kind":"resolve","resolution":{"finding_id":key,"expected_finding_view_id":view,"subject_id":batch["subject_id"],"outcome":"fixed","actor":"operator","policy_revision":batch["policy_id"],"reason":"A human cannot manufacture independent verification","evidence_ids":[bogus],"verification_id":bogus}}]);
            }
            _ => unreachable!(),
        }
        let id = format!("refuse-{fault}");
        let (code, result) = f.decide(&id, "review-cli", &batch, true);
        assert_ne!(code, 0, "{fault}: {result:#}");
        assert!(
            result
                .get("result")
                .is_none_or(|r| r["domain_conclusion"] != "pass")
        );
        assert_eq!(
            f.context("review-cli"),
            original,
            "failed batch {fault} must leave original state intact"
        );
        let cas = af_store::Cas::open_existing(f.state.join("cas")).unwrap();
        let store = af_store::EventStore::open_read_only(f.state.join("events.sqlite")).unwrap();
        if let Some(task) = store.task_projection(&cas, &id).unwrap() {
            assert!(
                task.execution
                    .as_ref()
                    .is_none_or(|e| e.attempt_accounting().is_empty())
            );
            if let af_contracts::task::TaskPhaseV1::Finished { result_id } = &task.phase {
                let result = cas.get_artifact(result_id).unwrap();
                assert!(
                    result.payload["outputs"].get("review").is_none(),
                    "a rejected batch must not publish a partial canonical Round"
                );
            }
        }
    }
}

#[test]
fn satisfied_evidence_becomes_stale_on_a_real_new_subject_and_requires_explicit_reuse() {
    let f = Fixture::new();
    let original = f.context("review-cli");
    let first = demand(&original, 0);
    let batch = f.batch(&original, vec![json!({"kind":"attach","demand_id":first,"content":"Measured original Subject"}), json!({"kind":"satisfy","demand_id":first,"evidence":{"kind":"attached","action":0},"reason":"Original measurement"})]);
    let (code, result) = f.decide("satisfy", "review-cli", &batch, true);
    assert_eq!(code, 3, "{result:#}");
    let satisfied = f.context("satisfy");
    std::fs::write(
        f.repo.join("lib.rs"),
        "pub fn changed_again() {}\n// changed measurement context\n",
    )
    .unwrap();
    let mut file: Value =
        serde_json::from_slice(&std::fs::read(f.repo.join("review.json")).unwrap()).unwrap();
    file["task_id"] = "later".into();
    file["inputs"] = json!({"history":{"task":"satisfy","port":"history"}});
    let path = f.root.path().join("later.json");
    std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
    let (code, result) = f.invoke(&[
        "review",
        "run",
        "--file",
        path.to_str().unwrap(),
        "--base",
        "HEAD",
        "--uncommitted",
    ]);
    assert_eq!(code, 3, "{result:#}");
    let later = f.context("later");
    assert_ne!(
        later["review"]["subject_id"],
        original["review"]["subject_id"]
    );
    assert_eq!(
        later["subject"]["base_snapshot_id"],
        original["subject"]["base_snapshot_id"]
    );
    let demand = later["demand_set"]["demands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["demand_id"] == first)
        .unwrap();
    assert_eq!(demand["status"], "stale");
    let rejected = f.batch(&later, vec![json!({"kind":"satisfy","demand_id":first,"evidence":{"kind":"recorded","artifact_id":satisfied["demand_set"]["demands"][0]["evidence_ids"][0]},"reason":"Stale Evidence must not silently become current"})]);
    let (code, result) = f.decide("stale-refused", "later", &rejected, true);
    assert_ne!(code, 0, "{result:#}");
    f.refused(
        "stale-refused",
        &result,
        "Stale Evidence requires explicit reuse authority",
    );
    assert_eq!(f.context("later"), later);
    let reuse = f.batch(&later, vec![json!({"kind":"reuse","demand_id":first,"satisfaction_id":satisfied["demand_set"]["demands"][0]["satisfaction_ids"][0],"reason":"Explicitly retain this measurement across the exact Subject change"})]);
    let (code, result) = f.decide("reuse-stale", "later", &reuse, true);
    assert_eq!(code, 3, "{result:#}");
    let reused = f.context("reuse-stale");
    assert_eq!(
        reused["demand_set"]["demands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["demand_id"] == first)
            .unwrap()["status"],
        "satisfied"
    );
    let telemetry = f.root.path().join("telemetry");
    let ledger = crate::telemetry_ledger::read_ledger(
        &telemetry,
        &af_store::store::task::task_run_id("reuse-stale").unwrap(),
    );
    let summary = ledger
        .demands
        .iter()
        .find(|d| d.demand_id == first)
        .unwrap();
    assert_eq!(ledger.coverage, "complete", "{:?}", ledger.coverage_reasons);
    let events: Vec<_> = summary
        .lifecycle
        .iter()
        .filter(|event| event.event == "evidence_satisfied")
        .collect();
    assert_eq!(
        events.len(),
        1,
        "explicit reuse emits exactly its new admission: {summary:?}"
    );
    assert_eq!(
        events[0].evidence_id.as_deref(),
        satisfied["demand_set"]["demands"][0]["evidence_ids"][0].as_str()
    );
    assert!(
        events[0].sequence.is_some(),
        "the native publication orders the lifecycle event"
    );
    let stale_ledger = crate::telemetry_ledger::read_ledger(
        &telemetry,
        &af_store::store::task::task_run_id("later").unwrap(),
    );
    let stale = stale_ledger
        .demands
        .iter()
        .find(|d| d.demand_id == first)
        .unwrap();
    assert_eq!(stale.status, "stale");
    assert_eq!(
        stale
            .lifecycle
            .iter()
            .filter(|e| e.event == "stale")
            .count(),
        1,
        "{stale:?}"
    );
    assert!(
        !stale
            .lifecycle
            .iter()
            .any(|e| e.event == "evidence_satisfied"),
        "stale history must not claim a new satisfaction: {stale:?}"
    );
    assert_eq!(
        summary.subject_id,
        original["review"]["subject_id"].as_str().unwrap()
    );
    assert_eq!(
        summary.snapshot_id,
        original["review"]["snapshot_id"].as_str().unwrap()
    );
}

#[test]
fn cross_demand_reuse_refuses_an_incompatible_measurement_method() {
    let f = Fixture::with_compatible_demands(false);
    let original = f.context("review-cli");
    let batch = f.batch(&original, vec![json!({"kind":"attach","demand_id":demand(&original,0),"content":"A method-specific observation"})]);
    let (code, result) = f.decide("attach", "review-cli", &batch, true);
    assert_eq!(code, 3, "{result:#}");
    let attached = f.context("attach");
    let batch = f.batch(&attached, vec![json!({"kind":"reuse_evidence","demand_id":demand(&attached,1),"evidence_id":attached["demand_set"]["demands"][0]["evidence_ids"][0],"reason":"Incompatible methods must be refused"})]);
    let (code, result) = f.decide("incompatible", "attach", &batch, true);
    assert_ne!(code, 0, "{result:#}");
    f.refused(
        "incompatible",
        &result,
        "Cross-Demand reuse requires identical claim, method, reason and requirement",
    );
    assert_eq!(f.context("attach"), attached);
}

#[test]
fn missing_siblings_supply_no_human_decision_authority_or_partial_round() {
    let f = Fixture::configured(true, true, "corroborate");
    let (code, context) = f.invoke(&["review", "decision-context", "review-cli"]);
    assert_ne!(code, 0, "{context:#}");
    let (_, shown) = f.invoke(&["review", "show", "review-cli"]);
    let round = &shown["review_rounds"][0];
    assert_eq!(round["conclusion"], "incomplete");
    let batch = json!({"schema":"af.review-decisions/1","review_id":shown["result"]["outputs"]["review"]["artifact_ids"][0],"policy_id":round["policy_id"],"subject_id":round["subject_id"],"snapshot_id":round["snapshot_id"],"base_snapshot_id":shown["plan"]["inputs"]["base"]["snapshot_id"],"actor":"operator","reason":"Incomplete review cannot be bypassed","actions":[{"kind":"policy_time","time":{"tick":1,"actor":"operator","reason":"Must refuse"}}]});
    let (code, result) = f.decide("partial-refused", "review-cli", &batch, true);
    assert_ne!(code, 0, "{result:#}");
    let cas = af_store::Cas::open_existing(f.state.join("cas")).unwrap();
    let store = af_store::EventStore::open_read_only(f.state.join("events.sqlite")).unwrap();
    assert!(
        store
            .task_projection(&cas, "partial-refused")
            .unwrap()
            .is_none()
    );
}

#[test]
fn telemetry_failure_cannot_change_a_committed_human_disposition() {
    let f = Fixture::new();
    let original = f.context("review-cli");
    let batch = f.batch(&original, vec![json!({"kind":"waive","waiver":{"demand_id":demand(&original,0),"subject_id":original["review"]["subject_id"],"actor":"operator","policy_revision":original["review"]["policy_id"],"reason":"Explicit authorized risk acceptance"}})]);
    let (code, plan) = f.decide("observer-fails", "review-cli", &batch, false);
    assert_eq!(code, 0, "{plan:#}");
    let output = crate::common::af()
        .current_dir(&f.repo)
        .args([
            "review",
            "resume",
            "observer-fails",
            "--confirm-plan",
            plan["plan_id"].as_str().unwrap(),
            "--json",
            "--state",
        ])
        .arg(&f.state)
        .env("AF_TELEMETRY__LEVEL", "spans")
        .env(
            "AF_TELEMETRY__DIR",
            f.root.path().join("failed-observer-telemetry"),
        )
        .env(af_runtime::telemetry::FAIL_SETTING, "1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["attempts"], 0);
    assert_eq!(result["chargeable_tokens"], "0");
    assert_eq!(
        f.context("observer-fails")["demand_set"]["demands"][0]["status"],
        "waived"
    );
    assert_eq!(f.context("review-cli"), original);
}

#[test]
fn a_reviewer_named_decisions_keeps_ordinary_required_review_semantics() {
    use af_contracts::task::pipeline::TaskOperatorV1;
    let f = Fixture::new();
    let catalog_path = f.repo.join(".af/task-catalog.toml");
    let mut catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(&catalog_path).unwrap()).unwrap();
    let reviewers = catalog["review"]["reviewers"].as_table_mut().unwrap();
    let requirement = reviewers.remove("correctness").unwrap();
    reviewers.insert("decisions".into(), requirement);
    let package = f.repo.join(".af/task-packages/fixture/review");
    let file = package.join("pipeline.toml");
    let mut pipeline: af_contracts::task::pipeline::PipelineDefinition =
        toml::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let slot = pipeline.slots.remove("correctness").unwrap();
    pipeline.slots.insert("decisions".into(), slot);
    for node in &mut pipeline.nodes {
        match &mut node.operator {
            TaskOperatorV1::Verify { slot } if slot == "correctness" => {
                *slot = "decisions".into();
                node.inputs.insert(
                    "assignment".into(),
                    serde_json::from_value(json!({"kind":"node","node":"bind","port":"decisions"}))
                        .unwrap(),
                );
            }
            TaskOperatorV1::ReviewReduce {} => {
                let value = node.inputs.remove("correctness").unwrap();
                node.inputs.insert("decisions".into(), value);
            }
            _ => {}
        }
    }
    std::fs::write(file, toml::to_string(&pipeline).unwrap()).unwrap();
    catalog["packages"]["fixture/review"]["digest"] =
        af_pipeline_config::lock::package_digest("fixture/review", &package)
            .unwrap()
            .into();
    for name in ["correctness", "bugs"] {
        let package = f.repo.join(format!(".af/task-packages/fixture/{name}"));
        std::fs::write(package.join("worker.py"), "print('{\"schema\":\"af.worker-reply/1\",\"outputs\":{\"result\":[{\"reports\":[],\"benchmark_demands\":[],\"dispositions\":[]}]}}')\n").unwrap();
        catalog["packages"][format!("fixture/{name}")]["digest"] =
            af_pipeline_config::lock::package_digest(&format!("fixture/{name}"), &package)
                .unwrap()
                .into();
    }
    std::fs::write(catalog_path, toml::to_string(&catalog).unwrap()).unwrap();
    git(&f.repo, &["add", "-A"]);
    git(
        &f.repo,
        &["commit", "-qm", "ordinary reviewer named decisions"],
    );
    let mut file: Value =
        serde_json::from_slice(&std::fs::read(f.repo.join("review.json")).unwrap()).unwrap();
    file["task_id"] = "named-review".into();
    let path = f.root.path().join("named-review.json");
    std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
    let (code, result) = f.invoke(&["review", "run", "--file", path.to_str().unwrap()]);
    assert_eq!(code, 0, "{result:#}");
    assert_eq!(result["attempts"], 3);
    let context = f.context("named-review");
    let batch = f.batch(&context, vec![json!({"kind":"policy_time","time":{"tick":1,"actor":"operator","reason":"Retain the ordinary review's required checks"}})]);
    let (code, result) = f.decide("named-human", "named-review", &batch, true);
    assert_eq!(code, 0, "{result:#}");
    let ledger = crate::telemetry_ledger::read_ledger(
        &f.root.path().join("telemetry"),
        &af_store::store::task::task_run_id("named-human").unwrap(),
    );
    assert_eq!(ledger.coverage, "complete", "{:?}", ledger.coverage_reasons);
}

/// A waiver is an explicit disposition of the original Demand, not cached measurement
/// evidence that becomes stale when a later native discovery Task changes its head.
#[test]
fn waiver_remains_current_when_the_native_subject_advances() {
    let f = Fixture::new();
    let original = f.context("review-cli");
    let first = demand(&original, 0);
    let batch = f.batch(&original, vec![json!({"kind":"waive","waiver":{
        "demand_id":first,"subject_id":original["review"]["subject_id"],"actor":"operator",
        "policy_revision":original["review"]["policy_id"],"reason":"Accept this required Demand explicitly"}})]);
    let (code, result) = f.decide("waived", "review-cli", &batch, true);
    assert_eq!(code, 3, "{result:#}");
    let waived = f.context("waived");
    std::fs::write(f.repo.join("lib.rs"), "pub fn new_head() {}\n").unwrap();
    let mut file: Value =
        serde_json::from_slice(&std::fs::read(f.repo.join("review.json")).unwrap()).unwrap();
    file["task_id"] = "advanced".into();
    file["inputs"] = json!({"history":{"task":"waived","port":"history"}});
    let path = f.root.path().join("advanced.json");
    std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
    let (code, result) = f.invoke(&[
        "review",
        "run",
        "--file",
        path.to_str().unwrap(),
        "--base",
        "HEAD",
        "--uncommitted",
    ]);
    assert_eq!(code, 3, "{result:#}");
    let advanced = f.context("advanced");
    assert_ne!(
        advanced["review"]["subject_id"],
        original["review"]["subject_id"]
    );
    assert_ne!(
        advanced["review"]["snapshot_id"],
        original["review"]["snapshot_id"]
    );
    let demand = advanced["demand_set"]["demands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["demand_id"] == first)
        .unwrap();
    assert_eq!(demand["status"], "waived");
    assert_eq!(
        demand["subject_id"], advanced["review"]["subject_id"],
        "the Demand Set is a current-Subject projection"
    );
    let prior = waived["demand_set"]["demands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["demand_id"] == first)
        .unwrap();
    assert_eq!(
        demand["waiver_ids"], prior["waiver_ids"],
        "the original waiver stays current without replacement"
    );
    let cas = af_store::Cas::open_existing(f.state.join("cas")).unwrap();
    let ledger = af_runtime::task::review::ReviewTaskDomain::recorded_ledger(
        &cas,
        advanced["review_id"].as_str().unwrap(),
    )
    .unwrap();
    let record = ledger.demand(&first).unwrap();
    assert_eq!(
        record.demand.subject_id,
        original["review"]["subject_id"].as_str().unwrap()
    );
    assert_eq!(record.waivers.len(), 1);
    assert_eq!(
        record.waivers[0].waiver.subject_id,
        original["review"]["subject_id"].as_str().unwrap()
    );
    assert_eq!(
        f.context("waived"),
        waived,
        "original decision and accounting do not change"
    );
    assert_eq!(f.context("review-cli"), original);
    let (_, replay) = f.invoke(&["review", "resume", "advanced", "--execute"]);
    assert_eq!(
        replay, result,
        "cold replay retains the same waived Demand and accounting"
    );
    assert_eq!(f.context("advanced"), advanced);
}

#[test]
fn native_ledger_keeps_original_unresolved_findings_after_not_reproduced_and_dispute() {
    for position in ["not_reproduced", "dispute"] {
        let f = Fixture::configured(true, false, position);
        let original = f.context("review-cli");
        let (code, later) = f.invoke(&[
            "review",
            "continue",
            "ledger-later",
            "--from",
            "review-cli",
            "--execute",
        ]);
        assert_eq!(code, 3, "{position}: {later:#}");
        let context = f.context("ledger-later");
        assert_eq!(context["review"]["round"], 2);
        let (code, value) = f.invoke(&["review", "ledger", "ledger-later"]);
        assert_eq!(code, 0, "{value:#}");
        crate::schemas::valid(&crate::schemas::validator("review-ledger-v3.json"), &value);
        let typed: af_contracts::task::review_ledger::ReviewLedgerV3 =
            serde_json::from_value(value.clone()).unwrap();
        typed.validate().unwrap();
        let mut incomplete = typed.clone();
        incomplete.reports.clear();
        assert!(
            incomplete.validate().is_err(),
            "original Report provenance is mandatory"
        );
        let mut partial = typed.clone();
        let round = partial.inspection.round.as_mut().unwrap();
        round
            .reviewers
            .iter_mut()
            .find(|r| r.reviewer == "correctness")
            .unwrap()
            .findings
            .pop();
        round.counts.major -= 1;
        partial.inspection.validate().unwrap();
        assert!(
            partial.validate().is_err(),
            "a self-consistent Round report still cannot substitute for the complete Ledger"
        );
        let mut wrong_severity = typed.clone();
        let round = wrong_severity.inspection.round.as_mut().unwrap();
        round
            .reviewers
            .iter_mut()
            .find(|r| r.reviewer == "correctness")
            .unwrap()
            .findings[0]
            .severity = af_contracts::Severity::Minor;
        round.counts.major -= 1;
        round.counts.minor += 1;
        wrong_severity.inspection.validate().unwrap();
        assert!(
            wrong_severity.validate().is_err(),
            "Ledger counts must retain canonical severity"
        );
        assert_eq!(value["schema"], "af/review-ledger@3");
        assert_eq!(
            value["inspection"]["round"]["counts"],
            json!({"blocker":0,"major":2,"minor":0})
        );
        assert_eq!(value["finding_set"]["payload"], context["finding_set"]);
        let current = value["inspection"]["round"]["reviewers"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|r| r["findings"].as_array().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(current.len(), 2);
        let expected_status = if position == "dispute" {
            "contested"
        } else {
            "open"
        };
        for first in original["finding_set"]["findings"].as_array().unwrap() {
            let id = first["finding_id"].as_str().unwrap();
            let finding = current.iter().find(|f| f["finding_id"] == id).unwrap();
            assert_eq!(finding["status"], expected_status);
            assert_eq!(
                finding["evidence"].as_str().unwrap().lines().next(),
                first["body"].as_str()
            );
            for report_id in first["report_ids"].as_array().unwrap() {
                let report_id = report_id.as_str().unwrap();
                let report = &value["reports"][report_id];
                assert_eq!(report["artifact_id"], report_id);
                assert_eq!(
                    report["subject_snapshot_id"],
                    original["review"]["snapshot_id"]
                );
                assert!(
                    report["producer"]["node_id"]
                        .as_str()
                        .unwrap()
                        .ends_with(".nodes.correctness")
                );
            }
        }
        for format in ["text", "sarif"] {
            let output = crate::common::af()
                .current_dir(&f.repo)
                .args([
                    "review",
                    "ledger",
                    "ledger-later",
                    "--format",
                    format,
                    "--state",
                ])
                .arg(&f.state)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            let text = String::from_utf8(output.stdout).unwrap();
            for first in original["finding_set"]["findings"].as_array().unwrap() {
                assert!(
                    text.contains(first["finding_id"].as_str().unwrap()),
                    "{text}"
                );
                for report in first["report_ids"].as_array().unwrap() {
                    assert!(text.contains(report.as_str().unwrap()), "{text}");
                }
            }
            if format == "sarif" {
                let log: Value = serde_json::from_str(&text).unwrap();
                crate::schemas::valid(&crate::schemas::validator("findings-sarif-v1.json"), &log);
                let results = log["runs"][0]["results"].as_array().unwrap();
                assert_eq!(results.len(), 2);
                assert!(
                    results
                        .iter()
                        .all(|r| r["properties"]["status"] == expected_status)
                );
            } else {
                assert!(text.contains("2 major"), "{text}");
            }
        }
        // The current-Round report view remains intentionally empty, while the canonical
        // Ledger retains the two unresolved original claims.
        let (code, report) = f.invoke(&["task", "findings", "ledger-later"]);
        assert_eq!(code, 0, "{report:#}");
        assert_eq!(report["round"]["counts"]["major"], 0);
        let (_, selected) = f.invoke(&["review", "ledger", "review-cli", "--round", "1"]);
        assert_eq!(selected["finding_set"]["payload"], original["finding_set"]);
    }
}

/// Issue #785: an exact recorded required Demand explains changes_requested even without Findings.
#[test]
fn task_findings_exposes_required_demands_without_ordinary_findings() {
    let f = Fixture::configured(true, false, "demand_only");
    let context = f.context("review-cli");
    let (code, findings) = f.invoke(&["task", "findings", "review-cli"]);
    assert_eq!(code, 0, "{findings:#}");
    crate::schemas::valid(
        &crate::schemas::validator("task-findings-v3.json"),
        &findings,
    );
    let round = &findings["round"];
    assert_eq!(round["conclusion"], "changes_requested");
    assert_eq!(round["counts"], json!({"blocker":0,"major":0,"minor":0}));
    assert_eq!(round["demand_set_id"], context["review"]["demand_set_id"]);
    assert_eq!(round["demand_set"], context["demand_set"]);
    assert!(
        !round["demand_set"]["demands"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        round["demand_set"]["demands"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["status"] == "open" && d["requirement"] == "required")
    );
    let output = crate::common::af()
        .current_dir(&f.repo)
        .args(["task", "findings", "review-cli", "--state"])
        .arg(&f.state)
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    for needle in [
        "changes_requested",
        "no findings",
        "required, open",
        "Runtime is bounded",
        "Large inputs matter",
        "    method:\n      Measure scaling\n      measure --limit '10 ms' --path \"a\\b\"\n",
        r"      next: forged\u{1b}[31m",
    ] {
        assert!(text.contains(needle), "{text}");
    }
    for demand in context["demand_set"]["demands"].as_array().unwrap() {
        assert!(
            text.contains(demand["demand_id"].as_str().unwrap()),
            "{text}"
        );
    }
    assert!(!text.contains("\nnext: forged"), "{text}");
    assert!(!text.contains('\u{1b}'), "{text}");
    // Reading never mutates the round or its obligations.
    assert_eq!(f.context("review-cli"), context);
}
