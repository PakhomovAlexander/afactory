//! Reuse one captured domain across operations, then mutate original evidence between calls.
use review_core::task::execution::TaskInvocationV1;
use review_core::task::plan::ExecutionPlanV1;
use review_graph::task::CompiledTask;
use review_pipeline::task::TaskOperatorHost;
use review_pipeline::task::review::ReviewTaskDomain;
use review_store::Cas;
use serde_json::Value;
use std::path::Path;

pub fn refuses_changed_history(state: &Path, invocation: &Value, original_round: &Value) {
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let input: TaskInvocationV1 = serde_json::from_value(invocation.clone()).unwrap();
    let plan: ExecutionPlanV1 =
        serde_json::from_value(cas.get_json(&input.plan_id).unwrap()["payload"].clone()).unwrap();
    let graph: CompiledTask =
        serde_json::from_value(cas.get_json(&plan.compiled_graph_id).unwrap()["payload"].clone())
            .unwrap();
    let policy = original_round["policy_id"].as_str().unwrap();
    let definition = graph.nodes[&input.node].clone();
    let domain = || ReviewTaskDomain::captured(&cas, policy, graph.clone()).unwrap();
    let warm = domain();
    let run = |host: &ReviewTaskDomain| host.execute(&cas, &input, &definition, None, None);
    let expected = run(&warm).outputs.unwrap();
    // Selected raw Reviewer Results cannot be reconstructed by a reducer. Both original
    // reviewers must remain readable even when their canonical Round was already restored.
    for id in original_round["selected_results"]
        .as_object()
        .unwrap()
        .values()
    {
        let id = id.as_str().unwrap();
        let hex = id.strip_prefix("sha256:").unwrap();
        let path = state.join("cas/objects").join(&hex[..2]).join(&hex[2..]);
        let original = std::fs::read(&path).unwrap();
        for missing in [false, true] {
            assert_eq!(run(&warm).outputs.unwrap(), expected);
            if missing {
                std::fs::remove_file(&path).unwrap();
            } else {
                std::fs::write(&path, b"corrupted original Reviewer Result").unwrap();
            }
            let cold_refused = run(&domain()).outputs.is_err();
            let warm_refused = run(&warm).outputs.is_err();
            // Restore before asserting so even a failing probe leaves its fixture inspectable.
            std::fs::write(&path, &original).unwrap();
            assert!(
                cold_refused && warm_refused,
                "{} accepted changed history {id}; missing={missing}, cold={cold_refused}, warm={warm_refused}",
                input.node
            );
            assert_eq!(run(&warm).outputs.unwrap(), expected);
        }
    }
}
