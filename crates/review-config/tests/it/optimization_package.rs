use review_config::task::catalog::{TaskWorkerManifest, TaskWorkerRunner};
use review_config::task::kind::TaskKindManifest;
use review_core::task::pipeline::PipelineDefinitionV1;

#[test]
fn report_only_optimizer_package_has_exact_public_contract_and_no_worker_slots() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/self-optimizer/catalog");
    let pipeline: PipelineDefinitionV1 =
        toml::from_str(&std::fs::read_to_string(root.join("optimization/pipeline.toml")).unwrap())
            .unwrap();
    pipeline.validate().unwrap();
    assert!(pipeline.slots.is_empty());
    assert_eq!(pipeline.max_parallel, 1);
    assert_eq!(pipeline.contract.inputs.len(), 1);
    assert_eq!(
        pipeline.contract.inputs["history"].artifact_type,
        "af/OptimizationHistory@1"
    );
    assert_eq!(
        pipeline.contract.outputs["report"].artifact_type,
        "af/OptimizationReport@1"
    );
    let kind: TaskKindManifest =
        toml::from_str(&std::fs::read_to_string(root.join("optimization-kind/kind.toml")).unwrap())
            .unwrap();
    kind.validate().unwrap();
    assert_eq!(kind.kind, "optimize");
}

#[test]
fn light_optimizer_ships_role_scoped_model_workers_with_captured_instructions() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/self-optimizer/catalog");
    let pipeline: PipelineDefinitionV1 = toml::from_str(
        &std::fs::read_to_string(root.join("optimization-light-model/pipeline.toml")).unwrap(),
    )
    .unwrap();
    pipeline.validate().unwrap();
    assert_eq!(
        pipeline.slots["diagnose"].worker,
        "builtin/optimization-light-diagnose-model"
    );
    assert_eq!(
        pipeline.slots["propose"].worker,
        "builtin/optimization-light-propose-model"
    );
    for (directory, role) in [
        ("optimization-light-diagnose-model", "diagnose"),
        ("optimization-light-propose-model", "propose"),
    ] {
        let package = root.join(directory);
        let worker: TaskWorkerManifest =
            toml::from_str(&std::fs::read_to_string(package.join("worker.toml")).unwrap()).unwrap();
        assert!(matches!(worker.runner, TaskWorkerRunner::Model { .. }));
        assert_eq!(
            worker.signature.roles,
            std::collections::BTreeSet::from([role.into()])
        );
        let instructions = std::fs::read_to_string(package.join("instructions.md")).unwrap();
        assert!(instructions.len() > 200);
        assert!(
            worker
                .signature
                .contract
                .inputs
                .contains_key("configuration")
        );
        assert!(!worker.signature.contract.inputs.contains_key("source"));
        if role == "propose" {
            assert!(instructions.contains(".af/cache/cargo.json"));
            assert!(
                instructions
                    .contains(r#"{"schema":"af.sandbox-cache-selection/1","kind":"cargo"}"#)
            );
        }
    }
}
