use af_contracts::task::development::*;
use serde_json::{Value, json};

fn identity() -> String {
    format!("sha256:{}", "a".repeat(64))
}
fn request() -> Value {
    json!({
        "schema": DEVELOPMENT_SCHEMA, "harness":"sh", "binding_identity":identity(),
        "tools":{"sh":identity()}, "environment":{"BUILD_ROOT":"${build}/output"},
        "writable_roots":["source","state","scratch","build","cache","knowledge"],
        "state":"attempt_private", "cache":"attempt_private", "knowledge":"attempt_private",
        "dependencies":"available_only", "network":"deny",
        "limits":{"wall_ms":10000,"output_bytes":4096,"cpu_millicores":1000,"memory_bytes":67108864,"disk_bytes":268435456,"processes":32}
    })
}
fn binding() -> Value {
    json!({"schema":"af.worker-development-binding/3","runtime":"/usr/bin/docker","storage":"tmpfs_volume",
        "image":format!("example.invalid/toolchain@{}",identity()),"checksum_program":"/usr/bin/sha256sum","probe_shell":"/bin/sh",
        "tools":{"sh":{"program":"/bin/sh","identity":identity()}},
        "limits":request()["limits"]})
}
fn validator(name: &str) -> &'static jsonschema::Validator {
    super::validator(name)
}

#[test]
fn worker_development_contracts_and_machine_binding_have_strict_versioned_schema_parity() {
    let request_schema = validator("worker-development-v3.json");
    let binding_schema = validator("worker-development-binding-v3.json");
    for (value, schema) in [(request(), request_schema), (binding(), binding_schema)] {
        assert!(schema.is_valid(&value));
        let mut unknown = value.clone();
        unknown["unknown"] = json!(true);
        assert!(!schema.is_valid(&unknown));
        let mut old = value.clone();
        old["schema"] = json!("af.worker-development/0");
        assert!(!schema.is_valid(&old));
    }
    let requested: WorkerDevelopmentV3 = serde_json::from_value(request()).unwrap();
    let bound: WorkerDevelopmentBindingV3 = serde_json::from_value(binding()).unwrap();
    requested.validate().unwrap();
    bound.admit(&requested).unwrap();
    assert!(request_schema.is_valid(&serde_json::to_value(requested).unwrap()));
    assert!(binding_schema.is_valid(&serde_json::to_value(bound).unwrap()));
    for field in ["cpu_millicores", "memory_bytes", "disk_bytes", "processes"] {
        let mut missing = request();
        missing["limits"].as_object_mut().unwrap().remove(field);
        assert!(!request_schema.is_valid(&missing));
        assert!(
            serde_json::from_value::<WorkerDevelopmentV3>(missing)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    let mut value = request();
    value["limits"]["cpu_millicores"] = Value::Null;
    assert!(!request_schema.is_valid(&value));
    assert!(serde_json::from_value::<WorkerDevelopmentV3>(value).is_err());
}

#[test]
fn aggregate_disk_bounds_are_mandatory_finite_exact_and_within_machine_authority() {
    for value in [
        Value::Null,
        json!(0),
        json!(1),
        json!(4097),
        json!(8589938688u64),
    ] {
        for (mut document, schema, machine) in [
            (request(), validator("worker-development-v3.json"), false),
            (
                binding(),
                validator("worker-development-binding-v3.json"),
                true,
            ),
        ] {
            document["limits"]["disk_bytes"] = value.clone();
            assert!(!schema.is_valid(&document), "accepted {value}");
            if machine {
                assert!(
                    !serde_json::from_value::<WorkerDevelopmentBindingV3>(document)
                        .is_ok_and(|binding| binding.validate().is_ok())
                );
            } else {
                assert!(
                    !serde_json::from_value::<WorkerDevelopmentV3>(document)
                        .is_ok_and(|request| request.validate_container().is_ok())
                );
            }
        }
    }
    for version in ["1", "2"] {
        let mut old = request();
        old["schema"] = json!(format!("af.worker-development/{version}"));
        assert!(!validator("worker-development-v3.json").is_valid(&old));
        assert!(
            serde_json::from_value::<WorkerDevelopmentV3>(old)
                .unwrap()
                .validate()
                .is_err()
        );
        let mut old = binding();
        old["schema"] = json!(format!("af.worker-development-binding/{version}"));
        assert!(!validator("worker-development-binding-v3.json").is_valid(&old));
        assert!(
            serde_json::from_value::<WorkerDevelopmentBindingV3>(old)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    let mut absent = binding();
    absent["limits"]
        .as_object_mut()
        .unwrap()
        .remove("disk_bytes");
    assert!(!validator("worker-development-binding-v3.json").is_valid(&absent));
    assert!(
        serde_json::from_value::<WorkerDevelopmentBindingV3>(absent)
            .unwrap()
            .validate()
            .is_err()
    );
    let mut unsupported = binding();
    unsupported["storage"] = json!("writable_host_bind");
    assert!(!validator("worker-development-binding-v3.json").is_valid(&unsupported));
    assert!(serde_json::from_value::<WorkerDevelopmentBindingV3>(unsupported).is_err());
}

#[test]
fn worker_storage_recovery_intent_has_strict_versioned_schema_parity() {
    let schema = validator("worker-storage-v1.json");
    let valid = json!({"schema":"af.worker-storage/1","runtime":"/usr/bin/docker","name":"af-storage-Ab123"});
    let lease: WorkerStorageLeaseV1 = serde_json::from_value(valid.clone()).unwrap();
    lease.validate().unwrap();
    assert!(schema.is_valid(&valid));
    assert_eq!(serde_json::to_value(lease).unwrap(), valid);
    for (field, value) in [
        ("schema", "af.worker-storage/0"),
        ("name", "unrelated-volume"),
        ("name", "af-storage-../../host"),
        ("runtime", "relative"),
    ] {
        let mut invalid = valid.clone();
        invalid[field] = json!(value);
        assert!(!schema.is_valid(&invalid));
        assert!(
            serde_json::from_value::<WorkerStorageLeaseV1>(invalid)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    let mut invalid = valid;
    invalid["worker_path"] = json!("/host");
    assert!(!schema.is_valid(&invalid));
    assert!(serde_json::from_value::<WorkerStorageLeaseV1>(invalid).is_err());
}

#[test]
fn development_requests_refuse_path_escapes_reserved_variables_unsupported_quotas_and_authority_widening()
 {
    for value in [
        "${source}/../host",
        "${state}//file",
        "${scratch}/./file",
        "${build}/x\\y",
        "${host}/secret",
        "/host/cache",
        "${state}/x/${source}",
        "${state}suffix",
    ] {
        assert!(symbolic_value(value).is_err(), "accepted {value}");
    }
    for key in [
        "HOME",
        "PATH",
        "TMPDIR",
        "XDG_CACHE_HOME",
        "AF_TOOL",
        "LD_PRELOAD",
        "DYLD_INSERT_LIBRARIES",
        "BASH_ENV",
    ] {
        let mut value = request();
        value["environment"] = json!({key:"injected"});
        assert!(
            serde_json::from_value::<WorkerDevelopmentV3>(value)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    let binding: WorkerDevelopmentBindingV3 = serde_json::from_value(binding()).unwrap();
    for (field, value) in [
        ("wall_ms", json!(10001)),
        ("cpu_millicores", json!(2000)),
        ("memory_bytes", json!(67108865)),
        ("processes", json!(33)),
        ("disk_bytes", json!(268439552)),
    ] {
        let mut widened = request();
        widened["limits"][field] = value;
        assert!(
            binding
                .admit(&serde_json::from_value(widened).unwrap())
                .is_err(),
            "accepted {field}"
        );
    }
    let mut changed: WorkerDevelopmentV3 = serde_json::from_value(request()).unwrap();
    changed.tools.insert("unbound".into(), identity());
    assert!(binding.admit(&changed).is_err());
    let mut unsafe_machine = binding.clone();
    unsafe_machine.checksum_program = "/work/project-build.sh".into();
    assert!(
        unsafe_machine.validate().is_err(),
        "identity preflight cannot execute project code"
    );
    let mut old = request();
    old["schema"] = json!("af.worker-development/0");
    assert!(
        serde_json::from_value::<WorkerDevelopmentV3>(old)
            .unwrap()
            .validate()
            .is_err()
    );
}

#[test]
fn command_evidence_preserves_actual_output_identity_and_explicit_unavailable_observations() {
    let value = json!({"schema":COMMAND_EVIDENCE_SCHEMA,"purpose":"harness","attempt_id":"a".repeat(26),"context_id":identity(),
        "source_snapshot_id":identity(),"input_tree_digest":identity(),"input_manifest_id":identity(),"final_manifest_id":null,"final_tree_digest":null,"experiment":true,
        "command":["/bin/sh","/af/package/harness.sh"],"cwd":"/work","environment_id":identity(),"binding_id":identity(),
        "tool_probe_output_id":identity(),"started_unix_ms":1,"elapsed_ms":0,"exit_code":0,"failure":null,"resources":null,
        "stdout_id":identity(),"stderr_id":identity(),"returned_output_id":identity(),"stdout_total_bytes":16,"stderr_total_bytes":0,"output_cap":4096,
        "redacted":false,"omissions":["final_tree_unavailable","internal_commands_unobserved","internal_model_output_unobserved","resources_are_runtime_client_only"]});
    let schema = validator("worker-command-evidence-v1.json");
    assert!(schema.is_valid(&value));
    for field in [
        "final_tree_digest",
        "final_manifest_id",
        "exit_code",
        "failure",
        "resources",
        "returned_output_id",
    ] {
        let mut missing = value.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(!schema.is_valid(&missing));
        assert!(
            serde_json::from_value::<WorkerCommandEvidenceV1>(missing).is_err(),
            "missing {field} is not an unavailable observation"
        );
    }
    let mut evidence: WorkerCommandEvidenceV1 = serde_json::from_value(value.clone()).unwrap();
    evidence.validate().unwrap();
    assert_eq!(serde_json::to_value(&evidence).unwrap(), value);
    // The host recorder serializes a BTreeSet. Handwritten receipts must use the same
    // canonical ordering, including when the final-tree omission is present.
    for omissions in [
        json!([
            "internal_commands_unobserved",
            "internal_model_output_unobserved",
            "resources_are_runtime_client_only",
            "final_tree_unavailable"
        ]),
        json!([
            "final_tree_unavailable",
            "final_tree_unavailable",
            "internal_commands_unobserved",
            "internal_model_output_unobserved",
            "resources_are_runtime_client_only"
        ]),
    ] {
        let mut noncanonical = value.clone();
        noncanonical["omissions"] = omissions;
        assert!(
            serde_json::from_value::<WorkerCommandEvidenceV1>(noncanonical)
                .unwrap_err()
                .to_string()
                .contains("set entries must be sorted and unique")
        );
    }
    let mut final_tree_available = value.clone();
    final_tree_available["final_tree_digest"] = json!(identity());
    final_tree_available["final_manifest_id"] = json!(identity());
    final_tree_available["omissions"] = json!([
        "internal_commands_unobserved",
        "internal_model_output_unobserved",
        "resources_are_runtime_client_only"
    ]);
    assert!(schema.is_valid(&final_tree_available));
    let complete: WorkerCommandEvidenceV1 =
        serde_json::from_value(final_tree_available.clone()).unwrap();
    complete.validate().unwrap();
    assert_eq!(
        serde_json::to_value(complete).unwrap(),
        final_tree_available
    );
    evidence.resources = Some(CommandResourcesV1 {
        user_ms: 1,
        system_ms: 2,
        max_rss_bytes: None,
    });
    let with_resources = serde_json::to_value(&evidence).unwrap();
    assert!(schema.is_valid(&with_resources));
    assert_eq!(
        serde_json::from_value::<WorkerCommandEvidenceV1>(with_resources.clone()).unwrap(),
        evidence
    );
    let mut missing = with_resources;
    missing["resources"]
        .as_object_mut()
        .unwrap()
        .remove("max_rss_bytes");
    assert!(!schema.is_valid(&missing));
    assert!(serde_json::from_value::<WorkerCommandEvidenceV1>(missing).is_err());
    assert!(schema.is_valid(&serde_json::to_value(&evidence).unwrap()));
    evidence.stdout_total_bytes = 4097;
    assert!(
        evidence.validate().is_err(),
        "a truncated stream cannot be a returned whole answer"
    );
    evidence.returned_output_id = None;
    evidence.failure = Some("stdout truncated".into());
    evidence.validate().unwrap();
    for field in ["prompt", "invented_output"] {
        let mut extra = value.clone();
        extra[field] = json!("unobserved");
        assert!(!schema.is_valid(&extra));
        assert!(serde_json::from_value::<WorkerCommandEvidenceV1>(extra).is_err());
    }
}

#[test]
fn worker_cache_policies_warm_selections_build_provenance_and_capture_receipts_have_schema_parity()
{
    let mut request: WorkerDevelopmentV3 = serde_json::from_value(request()).unwrap();
    let limits = af_contracts::BuildCacheLimitsV1 {
        max_bytes: 4096,
        max_entries: 32,
        max_depth: 8,
        max_path_bytes: 256,
    };
    let policy = DevelopmentCachePolicyV1 {
        role: DevelopmentCacheRoleV1::Build,
        destination: DevelopmentRootV1::Build,
        path: String::new(),
        selection: DevelopmentCacheSelectionV1::Compatible,
        persistence: DevelopmentCachePersistenceV1::AdmittedOutput,
        lifecycle: DevelopmentCacheLifecycleV1::Task,
        limits,
        fingerprints: std::collections::BTreeSet::from(["Cargo.lock".into()]),
        keys: std::collections::BTreeMap::from([("profile".into(), "test".into())]),
        input: Some("cache_in".into()),
        output: Some("cache_out".into()),
        dependency_source: None,
    };
    request.caches.insert("objects".into(), policy.clone());
    request.validate().unwrap();
    let request_value = serde_json::to_value(&request).unwrap();
    assert!(validator("worker-development-v3.json").is_valid(&request_value));
    let mut unsafe_lifecycle = request_value.clone();
    unsafe_lifecycle["caches"]["objects"]["lifecycle"] = json!("machine_shared");
    assert!(!validator("worker-development-v3.json").is_valid(&unsafe_lifecycle));
    assert!(serde_json::from_value::<WorkerDevelopmentV3>(unsafe_lifecycle).is_err());
    let mut bound: WorkerDevelopmentBindingV3 = serde_json::from_value(binding()).unwrap();
    bound.cache_limits = Some(limits);
    bound
        .dependency_sources
        .insert("packages".into(), identity());
    bound.admit(&request).unwrap();
    assert!(
        validator("worker-development-binding-v3.json")
            .is_valid(&serde_json::to_value(&bound).unwrap())
    );
    for invalid in ["/host", "../state", "nested/../state", "home/private"] {
        let mut changed = policy.clone();
        changed.role = DevelopmentCacheRoleV1::Continuation;
        changed.destination = DevelopmentRootV1::State;
        changed.selection = DevelopmentCacheSelectionV1::ExactSource;
        changed.path = invalid.into();
        assert!(changed.validate().is_err());
    }
    for field in ["input", "output"] {
        let mut value = request_value.clone();
        value["caches"]["objects"][field] = json!(null);
        assert!(!validator("worker-development-v3.json").is_valid(&value));
        assert!(serde_json::from_value::<WorkerDevelopmentV3>(value).is_err());
    }
    let provenance = DevelopmentCacheProvenanceV1 {
        run_id: "task:fixture".into(),
        slot: "root.developer".into(),
        name: "objects".into(),
        role: DevelopmentCacheRoleV1::Build,
        compatibility_id: identity(),
        input_tree_digest: identity(),
        output_tree_digest: identity(),
    };
    let cache = af_contracts::BuildCacheV2 {
        kind: af_contracts::BuildCacheKindV1::Worker,
        trust: af_contracts::BuildCacheTrustV1::CandidateBuilt,
        gate_node: "root.implement".into(),
        gate_attempt_id: Some("a".repeat(26)),
        head_snapshot_id: identity(),
        manifest_id: identity(),
        content_digest: identity(),
        entries: 1,
        bytes: 2,
        limits,
        worker: Some(provenance),
    };
    cache.validate().unwrap();
    let cache_value = serde_json::to_value(&cache).unwrap();
    assert!(validator("build-cache-v2.json").is_valid(&cache_value));
    // The existing envelope binds the admitted output independently of input provenance;
    // no new payload field or alternate Snapshot spelling is needed for continuations.
    let output_snapshot = format!("sha256:{}", "b".repeat(64));
    let envelope = af_contracts::ArtifactEnvelope {
        artifact_type: af_contracts::contract::BUILD_CACHE_V2.into(),
        artifact_id: identity(),
        content_id: identity(),
        producer: af_contracts::Producer::Attempt {
            run_id: "task:fixture".into(),
            node_id: cache.gate_node.clone(),
            attempt_id: cache.gate_attempt_id.clone().unwrap(),
        },
        input_artifacts: vec![cache.head_snapshot_id.clone(), output_snapshot.clone()],
        subject_snapshot_id: Some(output_snapshot),
        payload: cache_value.clone(),
    };
    let value = serde_json::to_value(&envelope).unwrap();
    assert!(validator("artifact-envelope-v1.json").is_valid(&value));
    assert_eq!(
        serde_json::from_value::<af_contracts::ArtifactEnvelope>(value).unwrap(),
        envelope
    );
    assert_ne!(
        envelope.subject_snapshot_id.as_ref(),
        Some(&cache.head_snapshot_id)
    );

    {
        let field = "worker";
        let mut value = cache_value.clone();
        value.as_object_mut().unwrap().remove(field);
        assert!(!validator("build-cache-v2.json").is_valid(&value));
        assert!(
            serde_json::from_value::<af_contracts::BuildCacheV2>(value)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    let mut warm = serde_json::json!({"node":"root.implement","round":1,"development":{"objects":{"outcome":"selected","artifact_id":identity(),"manifest_id":identity(),"compatibility_id":identity()}}});
    assert!(validator("warm-set-v3.json").is_valid(&warm));
    serde_json::from_value::<af_contracts::WarmSetV3>(warm.clone())
        .unwrap()
        .validate()
        .unwrap();
    warm["development"]["objects"]["outcome"] = json!("omitted");
    assert!(!validator("warm-set-v3.json").is_valid(&warm));
    assert!(serde_json::from_value::<af_contracts::WarmSetV3>(warm).is_err());
    for (artifact_id, omission) in [
        (Some(identity()), None),
        (None, Some(WorkerCacheCaptureOmissionV1::Cancelled)),
        (None, Some(WorkerCacheCaptureOmissionV1::CaptureRefused)),
    ] {
        let receipt = WorkerCacheCaptureV1 {
            schema: "af.worker-cache-capture/1".into(),
            attempt_id: "a".repeat(26),
            name: "objects".into(),
            artifact_id,
            omission,
        };
        receipt.validate().unwrap();
        let value = serde_json::to_value(&receipt).unwrap();
        assert!(validator("worker-cache-capture-v1.json").is_valid(&value));
        assert_eq!(
            serde_json::from_value::<WorkerCacheCaptureV1>(value.clone()).unwrap(),
            receipt
        );
        let mut both = value;
        both["artifact_id"] = json!(identity());
        both["omission"] = json!("cancelled");
        assert!(!validator("worker-cache-capture-v1.json").is_valid(&both));
        assert!(
            serde_json::from_value::<WorkerCacheCaptureV1>(both)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    let mut old = request_value;
    old["schema"] = json!("af.worker-development/1");
    assert!(!validator("worker-development-v3.json").is_valid(&old));
    assert!(
        serde_json::from_value::<WorkerDevelopmentV3>(old)
            .unwrap()
            .validate()
            .is_err()
    );
}

#[test]
fn a_worker_cache_is_refused_as_gate_capture_evidence() {
    use af_contracts::{
        BuildCacheCapturedPayloadV1, BuildCacheKindV1, EventType, event::validate_event_payload,
    };
    let payload = BuildCacheCapturedPayloadV1 {
        gate_node: "gate".into(),
        gate_attempt_id: Some("a".repeat(26)),
        head_snapshot_id: identity(),
        kind: BuildCacheKindV1::Worker,
        limits: af_contracts::BuildCacheLimitsV1::default(),
        build_cache_artifact_id: Some(identity()),
        refused: None,
        entries: 1,
        bytes: 1,
    };
    let value = serde_json::to_value(payload).unwrap();
    assert!(validate_event_payload(EventType::BuildCacheCapturedV1, &value).is_err());
    let event = serde_json::json!({"event_id":"a".repeat(26),"run_id":"task:fixture","sequence":1,"type":"BuildCacheCaptured@1","occurred_at":"2026-11-04T00:00:00Z","payload":value});
    assert!(!validator("run-event-v1.json").is_valid(&event));
}
