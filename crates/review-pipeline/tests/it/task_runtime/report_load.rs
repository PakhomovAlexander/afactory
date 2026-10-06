use super::*;
use review_graph::NodeOutcome;

#[test]
#[ignore = "bounded remote report-lock measurement; not a correctness gate"]
fn report_lock_load_measurement() {
    let repetitions: usize = std::env::var("AF_REPORT_REPETITIONS")
        .unwrap()
        .parse()
        .unwrap();
    let requested: usize = std::env::var("AF_REPORT_NODES").unwrap().parse().unwrap();
    assert!([1, 50, 63, 200].contains(&requested));
    if requested == 200 {
        let refusal = std::panic::catch_unwind(|| {
            Fixture::with_load_nodes("import sys; sys.exit(17)", false, "fixture", 200)
        });
        assert!(refusal.is_err(), "200 static nodes unexpectedly admitted");
        eprintln!("REPORT_ADMISSION_REFUSED nodes=200 production_static_cap=64");
        return;
    }
    let mut f = Fixture::with_load_nodes(
        "import sys; sys.stdin.read(); print('deterministic failed command'); sys.exit(17)",
        false,
        "fixture",
        requested,
    );
    let host = CapturedTaskHost::capture_with_models(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &DocumentDomain,
        &BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "load-fixture", 15_000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    let runtime = TaskRuntime::new(&mut f.store, &f.cas, lease, &authority, &host).unwrap();
    eprintln!(
        "REPORT_SETUP {}",
        json!({"requested_failed_nodes":requested,"graph_nodes":f.graph.nodes.len(),"plan_bytes":serde_json::to_vec(&f.plan).unwrap().len(),"repetitions":repetitions})
    );
    let mut report = runtime.execute().unwrap();
    assert_eq!(
        report
            .outcomes
            .iter()
            .filter(|(_, o)| matches!(o, NodeOutcome::Failed { .. }))
            .count(),
        requested
    );
    let session_result = runtime.diagnostic_report_session(|| {
    for sample in 0..repetitions {
        // Fresh diagnostics on genuine failed nodes: avoid measuring only CAS cache hits.
        for (node, outcome) in &mut report.outcomes {
            if let NodeOutcome::Failed { error, .. } = outcome {
                *error = format!(
                    "load sample {sample} node {node}: deterministic failure {}",
                    "x".repeat(4096)
                );
            }
        }
        eprintln!(
            "REPORT_SAMPLE_BEGIN {}",
            json!({"failed_nodes":requested,"sample":sample,"loadavg":std::fs::read_to_string("/proc/loadavg").unwrap()})
        );
        let result = runtime.diagnostic_report_capture(&report);
        eprintln!(
            "REPORT_SAMPLE_END {}",
            json!({"failed_nodes":requested,"sample":sample,"error":result.err()})
        );
    }
    Ok(())
    });
    eprintln!(
        "REPORT_SESSION_END {}",
        json!({"error":session_result.err()})
    );
}
