//! The Review domain state the Task host drives: Gate execution bindings, sandboxes and caches,
//! gathered reviewer results and their canonical Ledger reduction, all through the common Task
//! runtime with real checks in real sandboxes and real reviewer processes. Nothing is a stub
//! except the reviewers' judgement, a `command` runner emitting fixed results.

use super::*;
use review_graph::{NodeOutcome, RunReport};
use review_sandbox::{CacheError, CacheErrorKind, CacheKind, CacheLimits, CacheSource};
use review_store::store::task::TaskLease;

pub(super) const APPROVE: &str = r#"{"verdict":"approve","summary":null,"findings":[],"benchmark_demands":[],"dispositions":[]}"#;

/// The ordinary source every Round of these tests reviews.
pub(super) fn source() -> BTreeMap<String, Vec<u8>> {
    BTreeMap::from([
        ("src/main.rs".into(), b"fn main() { loop {} }\n".to_vec()),
        ("build.sh".into(), b"#!/bin/sh\nexit 0\n".to_vec()),
    ])
}

/// A `command` runner running `script` under `/bin/sh -c`.
pub(super) fn runner(script: &str) -> String {
    format!(
        "runner = {{ program = \"/bin/sh\", args = [{{value=\"-c\"}}, {{value={}}}] }}",
        serde_json::to_string(script).unwrap()
    )
}

/// A reviewer that runs `checks` against the tree it was given, then answers `result`. Every
/// prior Finding its input assigns it is answered `not_reproduced`, the disposition that
/// changes no Ledger state, so a later Round's answer covers its assignment exactly.
pub(super) fn answering(checks: &str, result: &serde_json::Value) -> String {
    let result = result.to_string();
    let (head, tail) = result
        .split_once("\"dispositions\":[]")
        .expect("an answer that leaves its dispositions to the fixture");
    let quoted = |text: &str| text.replace('\'', "'\\''");
    format!(
        "{checks} && ids=$(grep -o '\"finding_id\":\"sha256:[0-9a-f]*\"' | cut -d'\"' -f4 | sort -u) \
         && d= && for id in $ids; do \
         d=\"$d${{d:+,}}{{\\\"finding_id\\\":\\\"$id\\\",\\\"position\\\":\\\"not_reproduced\\\",\\\"reason\\\":\\\"not re-examined by the fixture\\\"}}\"; \
         done && printf '%s\"dispositions\":[%s]%s' '{}' \"$d\" '{}'",
        quoted(head),
        quoted(tail),
    )
}

pub(super) fn finding(severity: &str, title: &str, occurrence: Option<&str>) -> serde_json::Value {
    let mut finding = serde_json::json!({
        "severity": severity, "file": "src/main.rs", "line": 1, "title": title,
        "body": "the loop never yields", "fix": "bound the loop", "confidence": 0.9,
    });
    if let Some(occurrence) = occurrence {
        finding["rule_id"] = serde_json::json!("fixture/unbounded-loop@1");
        finding["occurrence_key"] = serde_json::json!(occurrence);
    }
    finding
}

pub(super) fn requesting(findings: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({
        "verdict": "request-changes", "summary": null, "findings": findings,
        "benchmark_demands": [], "dispositions": [],
    })
}

/// Capture Round one of `definition` over `source` and admit its Task, as `af review run`
/// does. Every named reviewer runs as a `command` Worker; a heavy Campaign may continue.
pub(super) fn admit_source(
    cas: &Cas,
    store: &mut EventStore,
    definition: &str,
    source: BTreeMap<String, Vec<u8>>,
    reviewers: &[&str],
    heavy: bool,
) -> (LegacyReviewPlanCompiler, TaskLease) {
    let round = capture::captured_fixture::open_round_authority_with_source(
        cas,
        store,
        definition,
        None,
        review_core::CampaignConvergenceV1 {
            clean_rounds: if heavy { 2 } else { 1 },
            max_rounds: if heavy { 3 } else { 1 },
            gate: "major".into(),
        },
        source,
    );
    let mut settings = plan::settings();
    settings.mode = if heavy { "heavy" } else { "light" }.into();
    settings.executions = reviewers
        .iter()
        .map(|node| {
            (
                (*node).to_owned(),
                review_core::task::plan::WorkerExecutionV1::Command {},
            )
        })
        .collect();
    let compiler = LegacyReviewPlanCompiler::capture(
        cas,
        CapturedLegacyReviewRound::load(cas, store, "review", &round).unwrap(),
        cas.put(b"Review domain host fixture").unwrap(),
        plan::without_probes(settings),
    )
    .unwrap();
    let mut limits = capture::limits();
    limits.max_attempts = 12;
    let task = compiler
        .prepare_revision(cas, "domain-review", limits)
        .unwrap();
    let revision = plan::artifact(cas, review_core::task::TASK_REVISION_V1, &task);
    let (compiled, _) = compiler.compile(cas, &revision).unwrap();
    let plan_id = plan::artifact(cas, review_core::task::EXECUTION_PLAN_V1, &compiled);
    let authority = CapturedTaskAuthority::for_legacy_review(
        &compiler,
        &plan::RefuseExecution,
        &NoTaskDeveloper,
    );
    let lease = store.open_task(cas, &revision, "developer", 60000).unwrap();
    store
        .propose_task_plan(cas, &lease, &plan_id, &authority)
        .unwrap();
    store.admit_task_plan(cas, &lease, &authority).unwrap();
    (compiler, lease)
}

/// Execute the admitted Round on a fresh host, then hand the host and runtime to `body`.
pub(super) fn hosted<'s, 'c, T>(
    cas: &'s Cas,
    shared: &SharedEventStore<'s>,
    compiler: &'c LegacyReviewPlanCompiler,
    lease: &TaskLease,
    configure: impl FnOnce(LegacyReviewTaskHost<'s, 'c>) -> LegacyReviewTaskHost<'s, 'c>,
    body: impl FnOnce(&LegacyReviewTaskHost<'s, 'c>, &TaskRuntime<'s, '_>, RunReport) -> T,
) -> T {
    let host = configure(
        LegacyReviewTaskHost::new(
            cas,
            shared.clone(),
            compiler,
            lease.clone(),
            BTreeMap::new(),
        )
        .unwrap(),
    );
    let authority = CapturedTaskAuthority::for_legacy_review(compiler, &host, &NoTaskDeveloper);
    let runtime =
        TaskRuntime::with_store(shared.clone(), cas, lease.clone(), &authority, &host).unwrap();
    let report = runtime.execute().unwrap();
    body(&host, &runtime, report)
}

pub(super) fn events_of(
    shared: &SharedEventStore<'_>,
    event_type: EventType,
) -> Vec<review_core::RunEvent> {
    shared
        .lock()
        .unwrap()
        .replay("review")
        .unwrap()
        .into_iter()
        .filter(|event| event.event_type == event_type)
        .collect()
}

pub(super) fn completed<'r>(report: &'r RunReport, node: &str) -> &'r review_graph::ArtifactMap {
    match report.outcome(node) {
        Some(NodeOutcome::Completed { outputs }) => outputs,
        other => panic!("{node} did not complete: {other:?}"),
    }
}

const GATED_REVIEW: &str = r#"
version = 2
[subject]
kind = "whole-tree"
[[checks]]
name = "build"
program = "/bin/sh"
args = [{ value = "./build.sh" }]
[[nodes]]
id = "gate"
kind = "gate"
outputs = ["decision"]
[[nodes]]
id = "generation"
kind = "generation"
outputs = [{ name = "history", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
[[nodes]]
id = "architecture"
kind = "reviewer"
inputs = [{ name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
outputs = [{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
gated_by = "gate"
ARCHITECTURE
[[nodes]]
id = "performance"
kind = "reviewer"
inputs = [{ name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
outputs = [{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
gated_by = "gate"
PERFORMANCE
[[nodes]]
id = "gather"
kind = "gather"
inputs = [{ name = "architecture", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }, { name = "performance", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
outputs = ["reports"]
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = [
  { name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "demands", type = "review.kernel/DemandSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
]
[[edges]]
from = { node = "generation", port = "history" }
to = { node = "architecture", port = "prior_findings" }
[[edges]]
from = { node = "generation", port = "history" }
to = { node = "performance", port = "prior_findings" }
[[edges]]
from = { node = "architecture", port = "result" }
to = { node = "gather", port = "architecture" }
[[edges]]
from = { node = "performance", port = "result" }
to = { node = "gather", port = "performance" }
[[edges]]
from = { node = "gather", port = "reports" }
to = { node = "ledger", port = "reports" }
"#;

/// The gated two-reviewer pipeline, with `gate_binding` prepended as a version-3 `[gate]`.
fn gated_review(architecture: &str, performance: &str, gate_binding: Option<&str>) -> String {
    let definition = GATED_REVIEW
        .replace("ARCHITECTURE", &runner(architecture))
        .replace("PERFORMANCE", &runner(performance));
    match gate_binding {
        Some(binding) => definition.replacen(
            "version = 2\n",
            &format!("version = 3\n[gate]\n{binding}\n"),
            1,
        ),
        None => definition,
    }
}

const TRUSTED: &str =
    "provider = \"trusted_local\"\nrequired_isolation = \"none\"\nmode = \"ephemeral-write\"";

fn container_binding(required_isolation: &str) -> String {
    format!(
        "provider = \"container\"\nrequired_isolation = \"{required_isolation}\"\nmode = \"ephemeral-write\"\nimage = \"{}\"",
        review_sandbox::container::DEFAULT_IMAGE
    )
}

fn clean_reader() -> String {
    answering(
        "cat src/main.rs >/dev/null",
        &serde_json::from_str(APPROVE).unwrap(),
    )
}

/// Both reviewers report through one gather into one canonical Ledger reduction. The same
/// occurrence is one Finding carrying both reports in source order, not completion order, at
/// the higher severity; the same presentation without an occurrence stays two claims.
#[test]
fn gathered_reviewers_reduce_into_one_canonical_ledger_in_source_order() {
    for same_occurrence in [true, false] {
        let occurrence = same_occurrence.then_some("main-loop");
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
        // Each reviewer reads the tree it was given; performance answers last, first in order.
        let architecture = answering(
            "sleep 1; cat src/main.rs >/dev/null",
            &requesting(vec![finding("major", "Unbounded loop", occurrence)]),
        );
        let performance = answering(
            "cat src/main.rs >/dev/null",
            &requesting(vec![finding("blocker", "Unbounded loop", occurrence)]),
        );
        let definition = gated_review(&architecture, &performance, None);
        let (compiler, lease) = admit_source(
            &cas,
            &mut store,
            &definition,
            source(),
            &["architecture", "performance"],
            false,
        );
        let shared = SharedEventStore::new(&mut store);
        let (ledger, conclusion) = hosted(
            &cas,
            &shared,
            &compiler,
            &lease,
            |h| h,
            |host, _, report| {
                assert!(report.complete(), "{report:?}");
                (
                    host.ledger(),
                    host.publish_recorded_round_conclusion(&cas).unwrap(),
                )
            },
        );
        let findings = ledger.findings();
        if same_occurrence {
            assert_eq!(findings.len(), 1, "one occurrence is one Finding");
            let finding = findings[0];
            assert_eq!(finding.title, "Unbounded loop");
            assert_eq!(finding.severity, review_core::Severity::Blocker);
            assert_eq!(finding.status, review_store::Status::Open);
            assert_eq!(
                finding
                    .reports
                    .iter()
                    .map(|report| report.source.as_str())
                    .collect::<Vec<_>>(),
                ["architecture", "performance"],
                "canonical order, not completion order"
            );
        } else {
            assert_eq!(findings.len(), 2, "path and title are not identity");
        }
        // An open blocking Finding cannot converge; a light Campaign has no further Round.
        assert_eq!(
            conclusion.verdict,
            review_pipeline::RunVerdict::Fail(review_store::Verdict::Exhausted)
        );

        // The Ledger edge carries the exact Finding Set its barrier reduced from genesis.
        let outputs = completed(&conclusion.report, "ledger");
        let set_id = &outputs["findings"][0];
        let envelope = cas.get_artifact(set_id).unwrap();
        assert_eq!(&envelope.artifact_id, set_id, "the edge carries the Set ID");
        review_store::validate_envelope(&envelope).unwrap();
        assert_eq!(
            envelope.artifact_type,
            review_core::contract::FINDING_SET_V1
        );
        assert!(envelope.subject_snapshot_id.is_some());
        assert!(
            matches!(
                envelope.producer,
                review_core::Producer::KernelOperation { .. }
            ),
            "a Ledger barrier is a domain operation, not an Attempt"
        );
        let set: review_core::FindingSetV1 = serde_json::from_value(envelope.payload).unwrap();
        set.validate().unwrap();
        assert_eq!(set.selected_report_ids.len(), 2);
        assert_eq!(set.findings.len(), findings.len());
        let opened = shared
            .lock()
            .unwrap()
            .campaign_opened("review")
            .unwrap()
            .unwrap();
        let campaign: review_core::CampaignManifestV1 = serde_json::from_value(
            cas.get_json(opened.payload["campaign_manifest_id"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            set.prior_finding_set_id, campaign.finding_genesis_id,
            "round 1 reduces from the canonical genesis"
        );
        let demands = cas.get_artifact(&outputs["demands"][0]).unwrap();
        assert_eq!(demands.artifact_type, review_core::contract::DEMAND_SET_V1);

        // The gather manifest names each result by its pinned reviewer, and every receipt is
        // bound to the Round and carries each selected output.
        let gathered = cas
            .get_json(&completed(&conclusion.report, "gather")["reports"][0])
            .unwrap();
        for reviewer in ["architecture", "performance"] {
            assert_eq!(
                gathered[reviewer],
                serde_json::json!(completed(&conclusion.report, reviewer)["result"])
            );
        }
        let round = shared
            .lock()
            .unwrap()
            .latest_round_started("review")
            .unwrap()
            .unwrap();
        let receipts = events_of(&shared, EventType::NodeOutputReceiptV1);
        let mut receipted: Vec<_> = receipts
            .iter()
            .map(|receipt| receipt.node_id.as_deref().unwrap())
            .collect();
        receipted.sort_unstable();
        assert_eq!(
            receipted,
            [
                "architecture",
                "gate",
                "gather",
                "generation",
                "ledger",
                "performance"
            ],
            "one receipt per completed node"
        );
        for receipt in receipts {
            let selected: Vec<&str> = receipt.payload["outputs"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|port| port["artifact_ids"].as_array().unwrap())
                .map(|id| id.as_str().unwrap())
                .collect();
            assert!(
                selected
                    .iter()
                    .all(|artifact| receipt.artifact_refs.iter().any(|id| id == artifact)),
                "receipt lost a selected output artifact"
            );
            assert!(receipt.artifact_refs.len() > selected.len());
            assert_eq!(receipt.causation_id.as_ref(), Some(&round.event_id));
            assert!(receipt.correlation_id.is_some());
        }
    }
}

/// Two independent Task-hosted runs of the same review, in separate stores, publish the same
/// Finding Set rows, Finding identity included: none of it depends on the store, clock or
/// process.
#[test]
fn two_independent_runs_of_the_same_review_agree() {
    let rows = || {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
        let architecture = answering(
            "cat src/main.rs >/dev/null",
            &requesting(vec![
                finding("major", "Unbounded loop", Some("main-loop")),
                finding("minor", "Loop has no comment", None),
            ]),
        );
        let performance = answering(
            "cat src/main.rs >/dev/null",
            &requesting(vec![finding(
                "blocker",
                "Unbounded loop",
                Some("main-loop"),
            )]),
        );
        let (compiler, lease) = admit_source(
            &cas,
            &mut store,
            &gated_review(&architecture, &performance, None),
            source(),
            &["architecture", "performance"],
            false,
        );
        let shared = SharedEventStore::new(&mut store);
        let conclusion = hosted(
            &cas,
            &shared,
            &compiler,
            &lease,
            |h| h,
            |host, _, report| {
                assert!(report.complete(), "{report:?}");
                host.publish_recorded_round_conclusion(&cas).unwrap()
            },
        );
        let set: review_core::FindingSetV1 = serde_json::from_value(
            cas.get_artifact(&completed(&conclusion.report, "ledger")["findings"][0])
                .unwrap()
                .payload,
        )
        .unwrap();
        set.findings
            .into_iter()
            .map(|finding| {
                (
                    finding.finding_id,
                    finding.title,
                    finding.severity,
                    finding.source,
                )
            })
            .collect::<Vec<_>>()
    };
    let first = rows();
    assert_eq!(
        first.len(),
        2,
        "one occurrence and one plain claim: {first:?}"
    );
    assert_eq!(first, rows());
}

fn run_report(
    shared: &SharedEventStore<'_>,
) -> (review_core::RunEvent, review_core::RunReportPayloadV6) {
    let events = events_of(shared, EventType::RunReportV6);
    assert_eq!(events.len(), 1, "one canonical Round conclusion");
    let report = serde_json::from_value(events[0].payload.clone()).unwrap();
    (events[0].clone(), report)
}

/// A version-3 Gate may write into its own disposable sandbox. The reviewers never see those
/// writes, the Gate Decision references the full mutation summary, and a resumed host replays
/// the one durable Execution Binding instead of binding again.
#[test]
fn a_gate_binding_keeps_disposable_writes_out_of_reviewer_sandboxes_and_replays_once() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let isolated = answering(
        "test ! -e gate-output && cat src/main.rs >/dev/null",
        &serde_json::from_str(APPROVE).unwrap(),
    );
    let definition = gated_review(&isolated, &isolated, Some(TRUSTED)).replace(
        "args = [{ value = \"./build.sh\" }]",
        "args = [{ value = \"-c\" }, { value = \"printf gate > gate-output\" }]",
    );
    let (compiler, lease) = admit_source(
        &cas,
        &mut store,
        &definition,
        source(),
        &["architecture", "performance"],
        false,
    );
    let shared = SharedEventStore::new(&mut store);
    for publish in [false, true] {
        // A new host has no process memory of the first execution's binding.
        hosted(
            &cas,
            &shared,
            &compiler,
            &lease,
            |h| h,
            |host, _, report| {
                assert!(report.complete(), "{report:?}");
                if publish {
                    host.assemble_recorded_result(&cas).unwrap();
                }
            },
        );
    }
    assert_eq!(events_of(&shared, EventType::GateExecutionBoundV1).len(), 1);
    assert_eq!(events_of(&shared, EventType::CheckCompletedV1).len(), 1);
    let gate = &events_of(&shared, EventType::GateDecisionV1)[0];
    let summaries = gate
        .artifact_refs
        .iter()
        .filter_map(|artifact| cas.get_json(artifact).ok())
        .filter(|value| value.get("count").is_some() && value.get("artifact").is_some())
        .collect::<Vec<_>>();
    assert_eq!(
        summaries.len(),
        1,
        "the Gate Decision references one mutation summary"
    );
    assert_eq!(summaries[0]["count"], 1);
    assert_eq!(summaries[0]["added"], 1);
    let mutations = cas
        .get_json(summaries[0]["artifact"].as_str().unwrap())
        .unwrap();
    assert_eq!(mutations["added"], serde_json::json!(["gate-output"]));

    let (_, report) = run_report(&shared);
    let review_core::RunReportExecutionV6::Bound { execution_bindings } = report.execution else {
        panic!("a bound Gate reports its binding: {:?}", report.execution)
    };
    assert_eq!(execution_bindings.len(), 1);
    let binding = &execution_bindings[0];
    assert_eq!(binding.node, "gate");
    assert_eq!(
        binding.provider,
        review_core::RunExecutionProviderV4::TrustedLocal
    );
    assert_eq!(
        binding.required_isolation,
        review_core::RunIsolationV4::None
    );
    assert_eq!(
        binding.provided_isolation,
        review_core::RunIsolationV4::None
    );
    assert!(binding.admitted);
}

fn cargo_cache(root: &std::path::Path) -> CacheSource {
    let cached = root.join("registry/cache/index/example.crate");
    std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
    std::fs::write(cached, b"offline crate").unwrap();
    CacheSource {
        kind: CacheKind::Cargo,
        source: root.to_path_buf(),
        limits: CacheLimits {
            max_bytes: 1024 * 1024,
            max_files: 100,
            max_copy_bytes: 1024 * 1024,
        },
    }
}

fn cached_review(check: &str) -> String {
    let reader = clean_reader();
    gated_review(
        &reader,
        &reader,
        Some(&format!("{TRUSTED}\ncaches = [\"cargo\"]")),
    )
    .replace(
        "args = [{ value = \"./build.sh\" }]",
        &format!(
            "args = [{{ value = \"-c\" }}, {{ value = {} }}]",
            serde_json::to_string(check).unwrap()
        ),
    )
}

/// The Gate's check sees the offline, bounded Cargo cache it was given. Its receipt is durable
/// before the Gate Decision, a resumed host replays it without resolving machine-local cache
/// policy again, and the Round conclusion reports exactly that receipt: publication re-verifies
/// the Cache Manifest it references, and the Store refuses a conclusion naming any other.
#[test]
fn a_cargo_cache_is_offline_bounded_and_replayed_into_the_round_conclusion() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let definition = cached_review(
        "test \"$CARGO_NET_OFFLINE\" = true && test -f \"$CARGO_HOME/registry/cache/index/example.crate\"",
    );
    let (compiler, lease) = admit_source(
        &cas,
        &mut store,
        &definition,
        source(),
        &["architecture", "performance"],
        false,
    );
    let cache_root = directory.path().join("cargo-cache");
    let source = cargo_cache(&cache_root);
    let shared = SharedEventStore::new(&mut store);
    hosted(
        &cas,
        &shared,
        &compiler,
        &lease,
        |host| host.with_cache_source_resolver(move |_| Ok(source.clone())),
        |_, _, report| assert!(report.complete(), "{report:?}"),
    );
    std::fs::remove_dir_all(&cache_root).unwrap();
    let manifest_id =
        events_of(&shared, EventType::CacheSnapshotMaterializedV1)[0].payload["source_digest"]
            .as_str()
            .unwrap()
            .to_owned();
    let hex = manifest_id.strip_prefix("sha256:").unwrap();
    let manifest_path = directory
        .path()
        .join("cas/objects")
        .join(&hex[..2])
        .join(&hex[2..]);
    let manifest_bytes = std::fs::read(&manifest_path).unwrap();
    let backup = directory.path().join("before-conclusion.sqlite");
    hosted(
        &cas,
        &shared,
        &compiler,
        &lease,
        |host| {
            host.with_cache_source_resolver(|_| -> Result<CacheSource, CacheError> {
                panic!("completed Gate replay must not resolve machine-local cache policy")
            })
        },
        |host, _, report| {
            assert!(report.complete(), "{report:?}");
            // The conclusion re-verifies every Cache Manifest it newly references.
            std::fs::write(&manifest_path, b"corrupt before the Round conclusion").unwrap();
            let error = host.publish_recorded_round_conclusion(&cas).unwrap_err();
            assert!(error.contains("failed verification"), "{error}");
            std::fs::write(&manifest_path, &manifest_bytes).unwrap();
            snapshot_store(directory.path(), &backup);
            host.assemble_recorded_result(&cas).unwrap();
        },
    );

    let snapshots = events_of(&shared, EventType::CacheSnapshotMaterializedV1);
    assert_eq!(snapshots.len(), 1, "resume reuses the durable receipt");
    let receipt: review_core::RunCacheSnapshotV5 =
        serde_json::from_value(snapshots[0].payload.clone()).unwrap();
    assert_eq!(receipt.node, "gate");
    assert_eq!(receipt.kind, review_core::RunCacheKindV5::Cargo);
    assert_eq!((receipt.files, receipt.bytes), (1, 13));
    assert!(cas.verify(&receipt.source_digest).is_ok());
    let receipt_artifact = snapshots[0]
        .artifact_refs
        .iter()
        .find(|artifact| cas.get_json(artifact).ok().as_ref() == Some(&snapshots[0].payload))
        .expect("exact cache receipt artifact");
    let gate = &events_of(&shared, EventType::GateDecisionV1)[0];
    assert!(gate.artifact_refs.contains(receipt_artifact));
    assert!(snapshots[0].sequence < gate.sequence);
    let (event, report) = run_report(&shared);
    assert!(event.artifact_refs.contains(&receipt.source_digest));
    let review_core::RunReportExecutionV6::Cached {
        cache_snapshots,
        cache_failures,
        ..
    } = report.execution
    else {
        panic!(
            "a cache-aware Gate reports its caches: {:?}",
            report.execution
        )
    };
    assert_eq!(cache_snapshots, vec![receipt]);
    assert!(cache_failures.is_empty());

    // The Store admits only the durable materialization facts: a well-formed conclusion that
    // names another Cache Manifest for the same Gate cache is refused.
    let forged = forged_cache_manifest(&cas);
    let error =
        refuse_forged_conclusion(&cas, &backup, &compiler, &lease, &event, |report, refs| {
            let review_core::RunReportExecutionV6::Cached {
                cache_snapshots, ..
            } = &mut report.execution
            else {
                unreachable!()
            };
            cache_snapshots[0] = review_core::RunCacheSnapshotV5 {
                source_digest: forged.clone(),
                bytes: 0,
                files: 1,
                ..cache_snapshots[0].clone()
            };
            refs.push(forged.clone());
        });
    assert!(
        error.contains("Cache Snapshots differ from the durable materialization facts"),
        "{error}"
    );
}

/// A Round conclusion reports exactly the outputs each node's durable receipt recorded: the
/// Store refuses one that claims any other output for a completed node.
#[test]
fn a_round_conclusion_must_match_every_durable_output_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let reader = clean_reader();
    let (compiler, lease) = admit_source(
        &cas,
        &mut store,
        &gated_review(&reader, &reader, None),
        source(),
        &["architecture", "performance"],
        false,
    );
    let backup = directory.path().join("before-conclusion.sqlite");
    let shared = SharedEventStore::new(&mut store);
    hosted(
        &cas,
        &shared,
        &compiler,
        &lease,
        |h| h,
        |host, _, report| {
            assert!(report.complete(), "{report:?}");
            snapshot_store(directory.path(), &backup);
            host.assemble_recorded_result(&cas).unwrap();
        },
    );
    let (event, _) = run_report(&shared);
    let unreceipted = cas.put(b"unreceipted output").unwrap();
    let error = refuse_forged_conclusion(&cas, &backup, &compiler, &lease, &event, |report, _| {
        let architecture = report
            .outcomes
            .iter_mut()
            .find(|outcome| outcome.node == "architecture")
            .unwrap();
        architecture.outcome = review_core::RunNodeOutcomeV2::Completed {
            output_artifacts: vec![unreceipted.clone()],
        };
    });
    assert!(
        error.contains("RunReport@6 contradicts the receipt for node 'architecture'"),
        "{error}"
    );
}

/// A node that never recorded a durable output receipt cannot be reported complete: the Store
/// refuses a conclusion that claims outputs for a reviewer which failed before publishing.
#[test]
fn a_round_conclusion_cannot_complete_a_node_without_a_durable_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let (compiler, lease) = admit_source(
        &cas,
        &mut store,
        &gated_review(&clean_reader(), "cat >/dev/null; exit 1", None),
        source(),
        &["architecture", "performance"],
        false,
    );
    let backup = directory.path().join("before-conclusion.sqlite");
    let shared = SharedEventStore::new(&mut store);
    hosted(
        &cas,
        &shared,
        &compiler,
        &lease,
        |h| h,
        |host, _, report| {
            assert!(!report.complete(), "{report:?}");
            snapshot_store(directory.path(), &backup);
            host.publish_recorded_round_conclusion(&cas).unwrap();
        },
    );
    let receipted: Vec<_> = events_of(&shared, EventType::NodeOutputReceiptV1)
        .into_iter()
        .filter_map(|event| event.node_id)
        .collect();
    assert!(
        !receipted.iter().any(|node| node == "performance"),
        "{receipted:?}"
    );
    let (event, report) = run_report(&shared);
    assert!(matches!(
        report.outcomes.iter().find(|o| o.node == "performance"),
        Some(review_core::RunNodeReportV2 {
            outcome: review_core::RunNodeOutcomeV2::Failed { .. },
            ..
        })
    ));
    let unreceipted = cas.put(b"unreceipted output").unwrap();
    let error = refuse_forged_conclusion(&cas, &backup, &compiler, &lease, &event, |report, _| {
        let performance = report
            .outcomes
            .iter_mut()
            .find(|outcome| outcome.node == "performance")
            .unwrap();
        performance.outcome = review_core::RunNodeOutcomeV2::Completed {
            output_artifacts: vec![unreceipted.clone()],
        };
        // Keep the conclusion self-consistent: the forged node is no longer missing.
        let review_core::RunVerdictV3::Incomplete { missing_nodes } = &mut report.verdict else {
            panic!(
                "a failed reviewer leaves the Round incomplete: {:?}",
                report.verdict
            )
        };
        missing_nodes.retain(|missing| missing.node != "performance");
        assert!(!missing_nodes.is_empty(), "downstream nodes stay missing");
    });
    assert!(
        error.contains("RunReport@6 completed node 'performance' without a durable receipt"),
        "{error}"
    );
}

/// Copy the Campaign store at `root` to `backup`, as it is now.
fn snapshot_store(root: &std::path::Path, backup: &std::path::Path) {
    rusqlite::Connection::open(root.join("events.sqlite"))
        .unwrap()
        .execute("VACUUM INTO ?1", [backup.to_str().unwrap()])
        .unwrap();
}

/// A valid Cargo Cache Manifest that no Gate materialized.
fn forged_cache_manifest(cas: &Cas) -> String {
    let manifest = review_core::CacheManifestV1 {
        kind: review_core::RunCacheKindV5::Cargo,
        path_encoding: review_core::CachePathEncodingV1::PercentV2,
        entries: vec![review_core::CacheManifestEntryV1 {
            path: "registry/cache/forged.crate".into(),
            content: cas.put(b"").unwrap(),
            size: 0,
        }],
    };
    cas.put_json(&serde_json::to_value(manifest).unwrap())
        .unwrap()
}

/// Publish a `forge`d copy of the recorded Round conclusion `honest` through the trusted Task
/// entry point, on `backup`: the Campaign store as it was before that conclusion was published.
/// Returns the refusal and checks that nothing was appended.
fn refuse_forged_conclusion(
    cas: &Cas,
    backup: &std::path::Path,
    compiler: &LegacyReviewPlanCompiler,
    lease: &TaskLease,
    honest: &review_core::RunEvent,
    forge: impl FnOnce(&mut review_core::RunReportPayloadV6, &mut Vec<String>),
) -> String {
    let mut store = EventStore::open(backup).unwrap();
    let shared = SharedEventStore::new(&mut store);
    let host = LegacyReviewTaskHost::new(
        cas,
        shared.clone(),
        compiler,
        lease.clone(),
        BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::for_legacy_review(compiler, &host, &NoTaskDeveloper);
    let mut payload: review_core::RunReportPayloadV6 =
        serde_json::from_value(honest.payload.clone()).unwrap();
    let mut refs = honest.artifact_refs.clone();
    forge(&mut payload, &mut refs);
    let report_id = payload.task_accounting.task_report_id.clone();
    let mut event = review_store::NewEvent::new(
        EventType::RunReportV6,
        serde_json::to_value(payload).unwrap(),
    )
    .caused_by(honest.causation_id.clone().unwrap())
    .correlating(honest.correlation_id.clone().unwrap())
    .referencing(refs);
    event.occurred_at = honest.occurred_at.clone();
    let before = shared.lock().unwrap().replay("review").unwrap();
    assert!(
        !before
            .iter()
            .any(|e| e.event_type == EventType::RunReportV6),
        "the backup predates the honest conclusion"
    );
    let error = shared
        .lock()
        .unwrap()
        .publish_task_review_report(cas, lease, &report_id, event, &authority)
        .unwrap_err()
        .to_string();
    assert_eq!(shared.lock().unwrap().replay("review").unwrap(), before);
    error
}

/// Every resolver failure fails the Gate with its own public reason, and neither the Gate
/// outcome nor the Campaign log carries the operator-only host detail. The Store refuses a
/// conclusion that trades the settled failure for a Cache Snapshot no Gate materialized.
#[test]
fn cache_failures_keep_their_reason_and_never_record_the_host_detail() {
    use review_core::RunCacheFailureReasonV5 as Reason;
    let cases = [
        (
            CacheErrorKind::PolicyUnavailable,
            Reason::PolicyUnavailable,
            "cache policy unavailable",
        ),
        (
            CacheErrorKind::SourceUnavailable,
            Reason::SourceUnavailable,
            "cache source unavailable",
        ),
        (
            CacheErrorKind::UnsafeContent,
            Reason::UnsafeContent,
            "cache source refused by safety policy",
        ),
        (
            CacheErrorKind::LimitExceeded,
            Reason::LimitExceeded,
            "cache source exceeds configured limits",
        ),
        (
            CacheErrorKind::CopyLimitExceeded,
            Reason::CopyLimitExceeded,
            "cache source cannot be materialized within its copy limit",
        ),
        (
            CacheErrorKind::ConcurrentChange,
            Reason::ConcurrentChange,
            "cache source changed during snapshot",
        ),
        (
            CacheErrorKind::MaterializationFailed,
            Reason::MaterializationFailed,
            "cache materialization failed",
        ),
    ];
    for (index, (kind, reason, public)) in cases.into_iter().enumerate() {
        let directory = tempfile::tempdir().unwrap();
        let backup = directory.path().join("before-conclusion.sqlite");
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
        let (compiler, lease) = admit_source(
            &cas,
            &mut store,
            &cached_review("exit 0"),
            source(),
            &["architecture", "performance"],
            false,
        );
        let shared = SharedEventStore::new(&mut store);
        hosted(
            &cas,
            &shared,
            &compiler,
            &lease,
            |host| {
                host.with_cache_source_resolver(move |_| -> Result<CacheSource, CacheError> {
                    Err(CacheError::new(kind, "/tmp/operator-only-cache-detail"))
                })
            },
            |host, _, report| {
                assert!(!report.complete());
                snapshot_store(directory.path(), &backup);
                let conclusion = host.publish_recorded_round_conclusion(&cas).unwrap();
                let Some(NodeOutcome::Failed { error, .. }) = conclusion.report.outcome("gate")
                else {
                    panic!("{kind:?}: the Gate did not fail: {:?}", conclusion.report)
                };
                assert!(error.contains(public), "{kind:?}: {error}");
                assert!(!error.contains("/tmp/operator-only"), "{kind:?}: {error}");
            },
        );
        let events = shared.lock().unwrap().replay("review").unwrap();
        assert!(
            !serde_json::to_string(&events)
                .unwrap()
                .contains("/tmp/operator-only")
        );
        let (event, report) = run_report(&shared);
        let review_core::RunReportExecutionV6::Cached {
            cache_snapshots,
            cache_failures,
            ..
        } = report.execution
        else {
            panic!("{kind:?}: a cache failure reports its caches")
        };
        assert!(cache_snapshots.is_empty());
        assert_eq!(cache_failures.len(), 1);
        assert_eq!(cache_failures[0].reason, reason, "{kind:?}");
        assert!(events_of(&shared, EventType::TaskReviewResultSelectedV1).is_empty());
        if index > 0 {
            continue;
        }
        let forged = forged_cache_manifest(&cas);
        let error =
            refuse_forged_conclusion(&cas, &backup, &compiler, &lease, &event, |report, refs| {
                let review_core::RunReportExecutionV6::Cached {
                    cache_snapshots,
                    cache_failures,
                    ..
                } = &mut report.execution
                else {
                    unreachable!()
                };
                cache_failures.clear();
                cache_snapshots.push(review_core::RunCacheSnapshotV5 {
                    node: "gate".into(),
                    kind: review_core::RunCacheKindV5::Cargo,
                    source_digest: forged.clone(),
                    bytes: 0,
                    files: 1,
                    materialization: review_core::RunCacheMaterializationV5::Copy,
                });
                refs.push(forged.clone());
            });
        assert!(
            error.contains("differs from its settled Gate cache failures"),
            "{error}"
        );
    }
}

/// A shell script standing in for a container runtime: `info` succeeds, and a run records
/// its argv beside the script instead of starting a container.
fn recording_container_runtime(root: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let runtime = root.join("recording-container-runtime");
    std::fs::write(
        &runtime,
        b"#!/bin/sh\nif [ \"${1:-}\" = info ]; then exit 0; fi\nprintf '%s\\n' \"$@\" > \"$0.args\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o755)).unwrap();
    let log = root.join("recording-container-runtime.args");
    (runtime, log)
}

fn bound(shared: &SharedEventStore<'_>) -> Vec<review_core::RunExecutionBindingV4> {
    events_of(shared, EventType::GateExecutionBoundV1)
        .into_iter()
        .map(|event| serde_json::from_value(event.payload).unwrap())
        .collect()
}

/// An unusable container provider is recorded as an unadmitted binding and runs no check. Cache
/// policy stays lazy, so the cache the Gate never reached is reported as a setup failure.
#[test]
fn an_unusable_container_gate_is_not_admitted_and_resolves_no_cache() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let reader = clean_reader();
    let definition = gated_review(
        &reader,
        &reader,
        Some(&format!(
            "{}\ncaches = [\"cargo\"]",
            container_binding("none")
        )),
    );
    let (compiler, lease) = admit_source(
        &cas,
        &mut store,
        &definition,
        source(),
        &["architecture", "performance"],
        false,
    );
    let missing = directory.path().join("missing-container-runtime");
    let shared = SharedEventStore::new(&mut store);
    hosted(
        &cas,
        &shared,
        &compiler,
        &lease,
        |host| {
            host.with_container_provider(review_sandbox::ContainerProvider::with_runtime(&missing))
                .with_cache_source_resolver(|_| -> Result<CacheSource, CacheError> {
                    panic!("cache policy must remain lazy when Gate setup fails")
                })
        },
        |host, _, report| {
            assert!(!report.complete());
            let conclusion = host.publish_recorded_round_conclusion(&cas).unwrap();
            assert!(matches!(
                conclusion.report.outcome("gate"),
                Some(NodeOutcome::Failed { error, .. }) if error.contains("container provider unavailable")
            ));
        },
    );
    assert!(events_of(&shared, EventType::CheckCompletedV1).is_empty());
    let (_, report) = run_report(&shared);
    let review_core::RunReportExecutionV6::Cached {
        execution_bindings,
        cache_snapshots,
        cache_failures,
    } = report.execution
    else {
        panic!("a cache-aware Gate reports its caches")
    };
    assert_eq!(execution_bindings.len(), 1);
    let binding = &execution_bindings[0];
    assert_eq!(
        binding.provider,
        review_core::RunExecutionProviderV4::Container
    );
    assert_eq!(
        binding.provided_isolation,
        review_core::RunIsolationV4::None
    );
    assert!(binding.image.is_some());
    assert!(!binding.admitted);
    assert!(cache_snapshots.is_empty());
    assert_eq!(cache_failures.len(), 1);
    assert_eq!(
        cache_failures[0].reason,
        review_core::RunCacheFailureReasonV5::GateSetupFailed
    );
}

/// When the container provider is usable the Gate's check runs through the container
/// invocation, never on the host: the recording runtime receives the exact hardened argv.
#[test]
fn a_usable_container_gate_runs_its_check_through_the_container_runtime() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let reader = clean_reader();
    let definition = gated_review(&reader, &reader, Some(&container_binding("none")));
    let (compiler, lease) = admit_source(
        &cas,
        &mut store,
        &definition,
        source(),
        &["architecture", "performance"],
        false,
    );
    let (runtime, runtime_log) = recording_container_runtime(directory.path());
    let shared = SharedEventStore::new(&mut store);
    hosted(
        &cas,
        &shared,
        &compiler,
        &lease,
        |host| {
            host.with_container_provider(review_sandbox::ContainerProvider::with_runtime(&runtime))
        },
        |host, _, report| {
            assert!(report.complete(), "{report:?}");
            host.assemble_recorded_result(&cas).unwrap();
        },
    );
    let bindings = bound(&shared);
    assert_eq!(bindings.len(), 1);
    assert!(bindings[0].admitted);
    assert_eq!(
        bindings[0].provided_isolation,
        review_core::RunIsolationV4::Container
    );
    let (_, report) = run_report(&shared);
    assert_eq!(report.execution.bindings(), bindings.as_slice());

    let argv = std::fs::read_to_string(runtime_log)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert_eq!(
        argv.len(),
        20,
        "unexpected container runtime argv: {argv:?}"
    );
    assert_eq!(&argv[..3], ["run", "--rm", "--name"]);
    assert!(argv[3].starts_with("af-gate-"), "{:?}", argv[3]);
    assert_eq!(
        &argv[4..8],
        ["--network=none", "--env-file", "/dev/null", "--user"]
    );
    let (uid, gid) = argv[8].split_once(':').expect("numeric uid:gid");
    assert!(!uid.is_empty() && uid.bytes().all(|byte| byte.is_ascii_digit()));
    assert!(!gid.is_empty() && gid.bytes().all(|byte| byte.is_ascii_digit()));
    assert_eq!(
        &argv[9..16],
        [
            "-e",
            "LC_ALL=C",
            "-e",
            "TZ=UTC",
            "--workdir",
            "/work",
            "--volume"
        ]
    );
    assert!(argv[16].ends_with(":/work:rw"), "{:?}", argv[16]);
    assert_eq!(argv[17], review_sandbox::container::DEFAULT_IMAGE);
    assert_eq!(&argv[18..], ["/bin/sh", "./build.sh"]);
}

/// The live container probe: a real runtime isolates the Gate's check, and the Round
/// conclusion records the container isolation it actually provided.
#[test]
#[ignore = "needs a live container runtime; run with the container probe gate"]
fn a_container_gate_executes_on_the_task_host() {
    use review_sandbox::Availability;
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let reader = clean_reader();
    let definition = gated_review(&reader, &reader, Some(&container_binding("container"))).replace(
        "args = [{ value = \"./build.sh\" }]",
        "args = [{ value = \"-c\" }, { value = \"test -f src/main.rs\" }]",
    );
    let (compiler, lease) = admit_source(
        &cas,
        &mut store,
        &definition,
        source(),
        &["architecture", "performance"],
        false,
    );
    let provider = review_sandbox::ContainerProvider::detect();
    assert!(
        matches!(provider.availability(), Availability::Usable { .. }),
        "{}",
        provider.availability().reason()
    );
    let shared = SharedEventStore::new(&mut store);
    hosted(
        &cas,
        &shared,
        &compiler,
        &lease,
        |host| host.with_container_provider(provider),
        |host, _, report| {
            assert!(report.complete(), "{report:?}");
            assert_eq!(
                host.assemble_recorded_result(&cas).unwrap().acceptance,
                review_core::task::TaskAcceptanceV1::Satisfied
            );
        },
    );
    let (_, report) = run_report(&shared);
    let binding = &report.execution.bindings()[0];
    assert_eq!(
        binding.provided_isolation,
        review_core::RunIsolationV4::Container
    );
    assert_eq!(
        binding.image.as_deref(),
        Some(review_sandbox::container::DEFAULT_IMAGE)
    );
    assert!(binding.admitted);
}

const DATA_FLOW_REVIEW: &str = r#"
version = 2
[subject]
kind = "whole-tree"
[[checks]]
name = "build"
program = "/bin/sh"
args = [{ value = "./build.sh" }]
[[nodes]]
id = "gate"
kind = "gate"
outputs = ["decision"]
[[nodes]]
id = "generation"
kind = "generation"
outputs = [{ name = "history", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
[[nodes]]
id = "gather"
kind = "reviewer"
inputs = [{ name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
outputs = [{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
gated_by = "gate"
AWKWARD
[[nodes]]
id = "sidecar"
kind = "reviewer"
inputs = [{ name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
outputs = [{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
gated_by = "gate"
SIDECAR
[[nodes]]
id = "evidence"
kind = "gather"
inputs = ["decision"]
outputs = ["reports"]
[[nodes]]
id = "collect"
kind = "gather"
inputs = [{ name = "reports", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
outputs = ["reports"]
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = [
  { name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "demands", type = "review.kernel/DemandSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
]
[[edges]]
from = { node = "gate", port = "decision" }
to = { node = "evidence", port = "decision" }
[[edges]]
from = { node = "generation", port = "history" }
to = { node = "gather", port = "prior_findings" }
[[edges]]
from = { node = "generation", port = "history" }
to = { node = "sidecar", port = "prior_findings" }
[[edges]]
from = { node = "gather", port = "result" }
to = { node = "collect", port = "reports" }
[[edges]]
from = { node = "collect", port = "reports" }
to = { node = "ledger", port = "reports" }
"#;

/// The plan is the data flow. A reviewer named `gather` still runs as a reviewer and keeps its
/// own provenance through a gather's port label; a reviewer whose result feeds no edge runs
/// but contributes nothing to the Ledger; and a gather of a non-reviewer input labels the
/// artifact with its pinned upstream node.
#[test]
fn the_ledger_reduces_only_what_the_plan_wires_to_it() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let definition = DATA_FLOW_REVIEW
        .replace(
            "AWKWARD",
            &runner(&answering(
                "cat src/main.rs >/dev/null",
                &requesting(vec![finding(
                    "major",
                    "Found by the awkwardly named reviewer",
                    None,
                )]),
            )),
        )
        .replace(
            "SIDECAR",
            &runner(&answering(
                "cat src/main.rs >/dev/null",
                &requesting(vec![finding("blocker", "Unwired", None)]),
            )),
        );
    let (compiler, lease) = admit_source(
        &cas,
        &mut store,
        &definition,
        source(),
        &["gather", "sidecar"],
        false,
    );
    let shared = SharedEventStore::new(&mut store);
    let (ledger, conclusion) = hosted(
        &cas,
        &shared,
        &compiler,
        &lease,
        |h| h,
        |host, _, report| {
            assert!(report.complete(), "{report:?}");
            (
                host.ledger(),
                host.publish_recorded_round_conclusion(&cas).unwrap(),
            )
        },
    );
    let findings = ledger.findings();
    assert_eq!(findings.len(), 1, "only the wired reviewer's finding lands");
    assert_eq!(findings[0].title, "Found by the awkwardly named reviewer");
    assert_eq!(
        findings[0].reports[0].source, "gather",
        "the gather input-port label must not replace reviewer provenance"
    );
    let selected: Vec<_> = events_of(&shared, EventType::TaskReviewResultSelectedV1)
        .into_iter()
        .filter_map(|event| event.node_id)
        .collect();
    assert_eq!(
        selected,
        ["gather", "sidecar"],
        "the unwired reviewer still ran"
    );
    assert_eq!(
        cas.get_json(&completed(&conclusion.report, "evidence")["reports"][0])
            .unwrap(),
        serde_json::json!({"gate": completed(&conclusion.report, "gate")["decision"]}),
        "the manifest labels a deterministic artifact with its pinned upstream node"
    );
}

const PROPOSING_REVIEW: &str = r#"
version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "generation"
kind = "generation"
outputs = [{ name = "history", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
[[nodes]]
id = "correctness"
kind = "reviewer"
inputs = [{ name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }]
outputs = [{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
CORRECTNESS
[[nodes]]
id = "gather"
kind = "gather"
inputs = [{ name = "correctness", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }]
outputs = ["reports"]
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = ["reports"]
outputs = [
  { name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
  { name = "demands", type = "review.kernel/DemandSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" },
]
[[edges]]
from = { node = "generation", port = "history" }
to = { node = "correctness", port = "prior_findings" }
[[edges]]
from = { node = "correctness", port = "result" }
to = { node = "gather", port = "correctness" }
[[edges]]
from = { node = "gather", port = "reports" }
to = { node = "ledger", port = "reports" }
"#;

/// Run one light Round whose reviewer edits `src/lib.rs` and declares that edit as a
/// Proposal, with an unrelated line appended to the declared patch when `mismatch`.
fn run_proposal(mismatch: bool) -> (tempfile::TempDir, Vec<review_core::RunEvent>) {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let before = b"pub fn value() -> u8 { 1 }\n";
    let after = b"pub fn value() -> u8 { 2 }\n";
    let manifest = |bytes: &[u8]| {
        review_source_git::Manifest::new(vec![review_source_git::Entry {
            path: "src/lib.rs".into(),
            kind: review_source_git::EntryKind::File,
            content: cas.put(bytes).unwrap(),
            size: bytes.len() as u64,
        }])
        .unwrap()
    };
    let mut patch = String::from_utf8(
        review_source_git::manifest_diff(&manifest(before), &manifest(after), &cas)
            .unwrap()
            .patch()
            .into(),
    )
    .unwrap();
    if mismatch {
        patch.push_str("# unrelated declaration\n");
    }
    let mut reply = requesting(vec![serde_json::json!({
        "severity": "major", "file": "src/lib.rs", "line": 1, "title": "value is wrong",
        "body": "the old value violates the contract", "fix": "return two", "confidence": 1.0,
    })]);
    reply["proposal"] = serde_json::json!({
        "patch": patch, "report_indexes": [0], "paths": ["src/lib.rs"],
        "description": "return the required value", "auto_apply_nominated": true,
    });
    let definition = PROPOSING_REVIEW.replace(
        "CORRECTNESS",
        &runner(&answering(
            "printf 'pub fn value() -> u8 { 2 }\\n' > src/lib.rs",
            &reply,
        )),
    );
    let (compiler, lease) = admit_source(
        &cas,
        &mut store,
        &definition,
        BTreeMap::from([("src/lib.rs".into(), before.to_vec())]),
        &["correctness"],
        false,
    );
    let shared = SharedEventStore::new(&mut store);
    hosted(
        &cas,
        &shared,
        &compiler,
        &lease,
        |h| h,
        |_, _, report| {
            assert!(report.complete(), "{report:?}");
        },
    );
    let events = shared.lock().unwrap().replay("review").unwrap();
    drop(shared);
    drop(store);
    (directory, events)
}

/// A Proposal whose declared patch is exactly the sealed sandbox diff is prepared by the
/// Worker and finalized once at the Ledger barrier; the Store refuses an acceptance that
/// names another Proposal.
#[test]
fn an_exact_sealed_diff_becomes_one_finalized_proposal() {
    let (directory, events) = run_proposal(false);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == EventType::ProposalPreparedV1)
            .count(),
        1
    );
    let accepted: Vec<_> = events
        .iter()
        .filter(|event| event.event_type == EventType::ProposalAcceptedV1)
        .collect();
    assert_eq!(accepted.len(), 1);
    let accepted = accepted[0];
    let payload: review_core::ProposalAcceptedPayloadV1 =
        serde_json::from_value(accepted.payload.clone()).unwrap();
    let cas = Cas::open_existing(directory.path().join("cas")).unwrap();
    let envelope = cas.get_artifact(&payload.proposal_artifact_id).unwrap();
    assert_eq!(envelope.artifact_id, payload.proposal_id);
    let proposal: review_core::PatchProposal = serde_json::from_value(envelope.payload).unwrap();
    assert_eq!(proposal.paths, ["src/lib.rs"]);
    assert_eq!(proposal.finding_refs.len(), 1);
    assert_eq!(
        proposal.finding_refs[0].kind,
        review_core::ClaimRefKind::Report
    );

    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let mut forged = payload;
    forged.proposal_id = format!("sha256:{}", "f".repeat(64));
    let error = store
        .append(
            "review",
            &cas,
            review_store::NewEvent::new(
                EventType::ProposalAcceptedV1,
                serde_json::to_value(forged).unwrap(),
            )
            .node(accepted.node_id.as_deref().unwrap())
            .attempt(accepted.attempt_id.as_deref().unwrap())
            .caused_by(accepted.causation_id.as_deref().unwrap())
            .correlating(accepted.correlation_id.as_deref().unwrap())
            .referencing(accepted.artifact_refs.clone()),
        )
        .unwrap_err();
    assert!(error.to_string().contains("Proposal envelope"), "{error}");
}

/// A declaration that differs from the sealed diff is refused with its reason and never
/// becomes a Proposal.
#[test]
fn a_declaration_unequal_to_the_sealed_diff_is_refused_and_absent() {
    let (_directory, events) = run_proposal(true);
    let refused = events
        .iter()
        .find(|event| event.event_type == EventType::ProposalRefusedV1)
        .expect("the mismatched declaration is refused");
    let payload: review_core::ProposalRefusedPayloadV1 =
        serde_json::from_value(refused.payload.clone()).unwrap();
    assert_eq!(
        payload.reason,
        review_core::ProposalRefusalReasonV1::PatchMismatch
    );
    assert!(events.iter().all(|event| !matches!(
        event.event_type,
        EventType::ProposalPreparedV1 | EventType::ProposalAcceptedV1
    )));
}

/// An answer that cannot be admitted is refused before selection and only that reviewer
/// retries. The retry learns the typed failure class, never the reviewer's own bytes: a
/// report outside FindingReport path admission, an unparseable answer and invalid
/// non-Finding metadata alike.
#[test]
fn an_unadmissible_answer_is_refused_before_selection_and_only_that_reviewer_retries() {
    let bad_path = requesting(vec![serde_json::json!({
        "severity": "major", "file": "./src/main.rs", "line": 1, "title": "bad path",
        "body": "body", "fix": "fix", "confidence": 0.9,
    })])
    .to_string();
    // A Benchmark Demand with an empty claim is refused before selection like a bad Finding.
    let bad_metadata = serde_json::json!({
        "verdict": "approve", "summary": null, "findings": [], "dispositions": [],
        "benchmark_demands": [{
            "claim": "", "why": "reason", "suggested_method": "measure the fixture loop",
        }],
    })
    .to_string();
    for (first, echoed) in [
        (bad_path, "./src/main.rs"),
        ("{not a ReviewerResult".to_owned(), "not a ReviewerResult"),
        (bad_metadata, "measure the fixture loop"),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path().join("cas")).unwrap();
        let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
        let inputs = directory.path().join("inputs");
        std::fs::create_dir(&inputs).unwrap();
        let script = format!(
            "n=$(ls '{dir}' | wc -l | tr -d ' '); n=$((n + 1)); cat > '{dir}/'$n.json; \
             if test $n = 1; then printf '%s' '{first}'; else printf '%s' '{APPROVE}'; fi",
            dir = inputs.display(),
        );
        let definition = gated_review(&script, &clean_reader(), None);
        let (compiler, lease) = admit_source(
            &cas,
            &mut store,
            &definition,
            source(),
            &["architecture", "performance"],
            false,
        );
        let shared = SharedEventStore::new(&mut store);
        let begun = hosted(
            &cas,
            &shared,
            &compiler,
            &lease,
            |h| h,
            |_, runtime, report| {
                assert!(report.complete(), "{report:?}");
                runtime
                    .projection()
                    .unwrap()
                    .execution
                    .unwrap()
                    .budget
                    .begun_attempts()
            },
        );
        assert_eq!(
            begun, 4,
            "Gate, both reviewers, and one retry of the refused one"
        );
        let retry: serde_json::Value =
            serde_json::from_slice(&std::fs::read(inputs.join("2.json")).unwrap()).unwrap();
        let refused = retry["refused_attempts"].as_array().unwrap();
        assert_eq!(refused.len(), 1);
        let feedback: review_core::task::feedback::TaskRetryFeedbackV1 =
            serde_json::from_str(refused[0].as_str().unwrap()).unwrap();
        assert_eq!(
            feedback.code,
            review_core::task::feedback::TaskFeedbackCodeV1::InvalidOutputContract
        );
        let first_input: serde_json::Value =
            serde_json::from_slice(&std::fs::read(inputs.join("1.json")).unwrap()).unwrap();
        assert!(first_input.get("refused_attempts").is_none());
        assert_ne!(
            Some(feedback.attempt_id.as_str()),
            events_of(&shared, EventType::TaskReviewResultSelectedV1)
                .iter()
                .find(|event| event.node_id.as_deref() == Some("architecture"))
                .and_then(|event| event.attempt_id.as_deref()),
            "the refused Attempt is never selected"
        );
        let retry = retry.to_string();
        assert!(
            !retry.contains(echoed),
            "durable retry feedback must not echo reviewer-controlled bytes: {retry}"
        );
    }
}
