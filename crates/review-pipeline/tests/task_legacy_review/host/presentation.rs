use super::*;

#[test]
fn presentation_retains_original_review_ports_and_selected_transport_fields_without_writes() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let reply = serde_json::json!({"verdict":"request-changes","summary":null,"findings":[{"severity":"major","file":".af/pipelines/review.toml","line":1,"title":"Missing required behavior","body":"The required behavior is absent","fix":"Implement the missing behavior","confidence":0.9}],"benchmark_demands":[],"disputes":[]});
    let (compiler, lease) = admit(
        &cas,
        &mut store,
        &command_pipeline_returning(&reply.to_string()),
    );
    let shared = SharedEventStore::new(&mut store);
    let host = LegacyReviewTaskHost::new(
        &cas,
        shared.clone(),
        &compiler,
        lease.clone(),
        BTreeMap::new(),
    )
    .unwrap();
    assert!(host.ledger().findings().is_empty());
    assert!(host.selected_attempt_evidence().unwrap().is_empty());
    let authority = CapturedTaskAuthority::for_legacy_review(&compiler, &host, &NoTaskDeveloper);
    let runtime =
        TaskRuntime::with_store(shared.clone(), &cas, lease.clone(), &authority, &host).unwrap();
    assert!(runtime.execute().unwrap().complete());
    let conclusion = host.publish_recorded_round_conclusion(&cas).unwrap();
    let task_run = review_store::store::task::task_run_id("host-review").unwrap();
    let events = shared.lock().unwrap().replay("review").unwrap();
    let task_events = shared.lock().unwrap().replay(&task_run).unwrap();
    assert_eq!(
        conclusion
            .report
            .outcomes
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["generation", "reviewer", "ledger"]
    );
    assert!(conclusion.report.blocked_gates.is_empty());
    for (node, outcome) in &conclusion.report.outcomes {
        let review_graph::NodeOutcome::Completed { outputs } = outcome else {
            panic!("{outcome:?}")
        };
        let receipt = events
            .iter()
            .find(|event| {
                event.event_type == EventType::NodeOutputReceiptV1
                    && event.node_id.as_ref() == Some(node)
            })
            .unwrap();
        let receipt: review_core::NodeOutputReceiptPayloadV1 =
            serde_json::from_value(receipt.payload.clone()).unwrap();
        let original: review_graph::ArtifactMap = receipt
            .outputs
            .into_iter()
            .map(|output| (output.port, output.artifact_ids))
            .collect();
        assert_eq!(outputs, &original);
    }
    let evidence = host.selected_attempt_evidence().unwrap();
    assert_eq!(evidence.len(), 1);
    let selected = events
        .iter()
        .find(|event| event.event_type == EventType::TaskReviewResultSelectedV1)
        .unwrap();
    let selection: review_core::task::review_compat::TaskReviewResultSelectedV1 =
        serde_json::from_value(selected.payload.clone()).unwrap();
    let provenance: review_core::task::review_compat::TaskReviewAttemptProvenanceV1 =
        serde_json::from_value(
            cas.get_artifact(&selection.provenance_artifact_id)
                .unwrap()
                .payload,
        )
        .unwrap();
    let context: review_core::task::review_compat::TaskReviewContextV1 =
        serde_json::from_value(cas.get_artifact(&selection.context_id).unwrap().payload).unwrap();
    let expected = review_pipeline::AttemptEvidence {
        node: "reviewer".into(),
        attempt_id: selected.attempt_id.clone().unwrap(),
        cost_tokens: 0,
        usage: review_runner::task::usage::read_task_usage(
            &cas,
            provenance.usage_id.as_ref().unwrap(),
        )
        .unwrap(),
        context_manifest: serde_json::from_value(
            cas.get_json(&context.context_manifest_id).unwrap(),
        )
        .unwrap(),
        raw_artifact: provenance.raw_artifact_id,
        result_artifact: selection.result_artifact_id,
    };
    assert_eq!(evidence, [expected]);
    assert_eq!(host.ledger().findings().len(), 1);
    // Fresh presentation shares the same durable facts, with no legacy Kernel construction.
    let reopened =
        LegacyReviewTaskHost::new(&cas, shared.clone(), &compiler, lease, BTreeMap::new()).unwrap();
    assert_eq!(reopened.selected_attempt_evidence().unwrap(), evidence);
    assert_eq!(
        reopened.ledger().findings()[0].key,
        host.ledger().findings()[0].key
    );
    assert_eq!(
        reopened
            .publish_recorded_round_conclusion(&cas)
            .unwrap()
            .report,
        conclusion.report
    );
    assert_eq!(shared.lock().unwrap().replay("review").unwrap(), events);
    assert_eq!(
        shared.lock().unwrap().replay(&task_run).unwrap(),
        task_events
    );
}
