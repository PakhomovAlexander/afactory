use super::adapter::{Method, Outcome, OwnedDocument, Request, Response};
use super::*;
use serde_json::{Value, json};
use std::sync::{Arc, Barrier};

pub const DAY: u64 = 86_400_000;
pub const START: u64 = 1_800_000_000_000 / DAY * DAY;
pub fn limits() -> Limits {
    Limits {
        max_tokens: 100,
        max_wall_ms: 10 * DAY,
        max_dispatches: 32,
    }
}
fn lit(value: Datum) -> Binding {
    Binding::Literal {
        value: Box::new(value),
    }
}
pub fn simple() -> (Bundle, Policy) {
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let policy = Policy {
        schema: "af/scenario-policy@5".into(),
        adapters: BTreeMap::new(),
        task_plans: Default::default(),
        task_templates: Default::default(),
        performance_publications: Default::default(),
        decision_keys: BTreeMap::from([("owner".into(), key.pk.to_box().unwrap().into_string())]),
        file_roots: Default::default(),
    };
    let step = Step {
        id: "review".into(),
        inputs: BTreeMap::from([
            ("subject".into(), lit(Datum::Text("issue-1".into()))),
            ("revision".into(), lit(Datum::Text("head-1".into()))),
        ]),
        outputs: BTreeMap::from([("result".into(), Port::Decision)]),
        when: None,
        max_attempts: 1,
        timeout_ms: 9 * DAY,
        on_timeout: Timeout::Fail,
        operation: Kind::Decision {
            owner: "owner".into(),
        },
    };
    let def = Scenario {
        schema: "af/scenario@5".into(),
        inputs: BTreeMap::new(),
        outputs: BTreeMap::from([("ok".into(), Port::Boolean)]),
        steps: vec![step],
        result: BTreeMap::from([("ok".into(), lit(Datum::Boolean(true)))]),
        completion: vec![lit(Datum::Boolean(true))],
        authority: Authority {
            task_plans: Default::default(),
            task_templates: Default::default(),
            grants: Default::default(),
            decision_owners: ["owner".into()].into(),
        },
        limits: limits(),
        admission: None,
    };
    (
        Bundle {
            schema: "af/scenario-bundle@5".into(),
            root: "main".into(),
            definitions: BTreeMap::from([("main".into(), def)]),
        },
        policy,
    )
}

fn publication_step(id: &str, decision: Binding, grant: Grant) -> Step {
    Step {
        id: id.into(),
        inputs: BTreeMap::from([
            ("subject".into(), lit(Datum::Text("issue-1".into()))),
            ("revision".into(), lit(Datum::Text("head-1".into()))),
            ("body".into(), lit(Datum::Text("publication".into()))),
            ("decision".into(), decision),
        ]),
        outputs: BTreeMap::from([("result".into(), Port::Action)]),
        when: None,
        max_attempts: 1,
        timeout_ms: DAY,
        on_timeout: Timeout::Fail,
        operation: Kind::Action {
            grant,
            requires_decision: true,
            required_condition: None,
        },
    }
}

fn publication() -> (Bundle, Policy) {
    let (mut bundle, mut policy) = simple();
    let grant = Grant {
        adapter: "publisher".into(),
        operation: Operation::PublishPr,
        destination: "fixture:repository".into(),
    };
    policy.adapters.insert(
        "publisher".into(),
        Adapter {
            executable: "/installed/publisher".into(),
            digest: digest(b"publisher"),
            operations: [Operation::PublishPr, Operation::Followup].into(),
            destinations: ["fixture:repository".into(), "fixture:other".into()].into(),
            max_wall_ms: 1000,
            max_output_bytes: 4096,
        },
    );
    let definition = bundle.definitions.get_mut("main").unwrap();
    definition.authority.grants.insert(grant.clone());
    definition.steps.push(publication_step(
        "publish",
        Binding::Step {
            step: "review".into(),
            port: "result".into(),
        },
        grant,
    ));
    (bundle, policy)
}
pub fn ant(timezone: &str, hour: u8, minute: u8) -> AntState {
    let (scenario, policy) = simple();
    let definition = Ant {
        schema: "af/ant@5".into(),
        id: "daily".into(),
        scenario,
        inputs: BTreeMap::new(),
        schedule: Daily {
            timezone: timezone.into(),
            hour,
            minute,
            start_unix_ms: START,
        },
        owner: "owner".into(),
        notification: "local:owner".into(),
        execution: Execution {
            overlap: Skip::Skip,
            missed: Skip::Skip,
            lateness_ms: 1000,
            per_run: limits(),
            rolling_tokens: 100,
            rolling_window_ms: 3 * DAY,
        },
    };
    let mut ant = AntState::new(
        AntPin::new(&TestHost::default(), definition, policy).unwrap(),
        START,
    )
    .unwrap();
    ant.change(AntMode::Enabled, START).unwrap();
    ant
}
pub fn schema(name: &str) -> jsonschema::Validator {
    let (file, fragment) = name.split_once('#').unwrap_or((name, ""));
    let dir = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .join("schemas");
    let mut registry = jsonschema::Registry::new();
    for file in [
        "scenario-v5.json",
        "task-contracts-v3.json",
        "scenario-roadmap-proposal-v2.json",
    ] {
        let value: Value = serde_json::from_slice(&std::fs::read(dir.join(file)).unwrap()).unwrap();
        let id = value["$id"].as_str().unwrap().to_owned();
        registry = registry
            .add(id, jsonschema::Resource::from_contents(value))
            .unwrap();
    }
    let registry = registry.prepare().unwrap();
    let mut root: Value = serde_json::from_slice(&std::fs::read(dir.join(file)).unwrap()).unwrap();
    if !fragment.is_empty() {
        root = json!({"$ref": format!("{}#{fragment}", root["$id"].as_str().unwrap())});
    }
    jsonschema::options()
        .with_registry(&registry)
        .build(&root)
        .unwrap()
}
pub fn valid(file: &str, value: &impl serde::Serialize) {
    let value = serde_json::to_value(value).unwrap();
    let validator = schema(file);
    let errors: Vec<_> = validator
        .iter_errors(&value)
        .map(|e| format!("{e} at {}", e.instance_path()))
        .collect();
    assert!(errors.is_empty(), "{file}: {}", errors.join("\n"));
}

#[test]
fn scenario_task_receipts_resolve_current_task_contracts_and_keep_closed_results() {
    // The Pi/main merge removed task-contracts-v1 while Scenario schemas still referenced it.
    // TaskResultV1 itself has not changed; resolve its definition from the current bundle.
    let receipt: TaskReceipt = serde_json::from_value(json!({
        "task_id": "child", "plan_id": digest(b"plan"), "revision_id": digest(b"revision"),
        "result_id": digest(b"result"), "charged_tokens": 10,
        "result": {
            "task_revision_id": digest(b"revision"), "execution": "completed",
            "acceptance": "satisfied", "domain_conclusion": "verified",
            "outputs": {"report": {
                "artifact_ids": [digest(b"report")], "artifact_type": "af/Test@1", "cardinality": "one"
            }},
            "evidence": [digest(b"evidence")], "missing_obligations": []
        }
    }))
    .unwrap();
    Datum::Task(receipt.clone()).validate().unwrap();
    let validator = schema("scenario-v5.json#/$defs/TaskReceipt");
    let mut wire = serde_json::to_value(&receipt).unwrap();
    let errors: Vec<_> = validator
        .iter_errors(&wire)
        .map(|error| error.to_string())
        .collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    assert_eq!(
        serde_json::from_value::<TaskReceipt>(wire.clone()).unwrap(),
        receipt
    );
    wire["result"]["extra"] = json!(true);
    assert!(!validator.is_valid(&wire));
    assert!(serde_json::from_value::<TaskReceipt>(wire).is_err());
}

#[test]
fn contracts_roundtrip_schemas_and_old_versions_fail_closed() {
    let (bundle, policy) = simple();
    let pin = Pin::new(&TestHost::default(), bundle.clone(), policy.clone()).unwrap();
    let mut run = Run::new(
        &TestHost::default(),
        "run".into(),
        pin.clone(),
        BTreeMap::new(),
        limits(),
        START,
        None,
    )
    .unwrap();
    run.prepare(START).unwrap();
    run.validate(&TestHost::default()).unwrap();
    let ant = ant("UTC", 12, 0);
    for (file, value) in [
        (
            "scenario-v5.json",
            serde_json::to_value(&bundle.definitions["main"]).unwrap(),
        ),
        ("scenario-bundle-v5.json", json!(bundle)),
        ("scenario-policy-v5.json", json!(policy)),
        ("scenario-pin-v5.json", json!(pin)),
        ("scenario-run-v5.json", json!(run)),
        ("ant-v5.json", json!(ant.pin.definition)),
        ("ant-pin-v5.json", json!(ant.pin)),
        ("ant-state-v5.json", json!(ant)),
        (
            "scenario-list-v5.json",
            json!({"schema":"af/scenario-list@5","runs":{"run":run}}),
        ),
        (
            "ant-list-v5.json",
            json!({"schema":"af/ant-list@5","ants":{"daily":ant}}),
        ),
        (
            "ant-tick-v5.json",
            json!({"schema":"af/ant-tick@5","now_unix_ms":START,"started":[],"busy":[],"refused":{},"runs":{}}),
        ),
    ] {
        valid(file, &value);
        let mut old = value.clone();
        old["schema"] = json!("af/unknown@0");
        assert!(!schema(file).is_valid(&old), "{file} admitted old version");
    }
    let text = serde_json::to_string(&run).unwrap();
    let replay: Run = serde_json::from_str(&text).unwrap();
    assert_eq!(run, replay);
    let mut old = run.clone();
    old.schema = "af/scenario-run@0".into();
    assert!(old.validate(&TestHost::default()).is_err());
    let mut wrong = json!(bundle);
    wrong["extra"] = json!(true);
    assert!(serde_json::from_value::<Bundle>(wrong).is_err());
    let request = Request {
        schema: "af/scenario-adapter-request@4".into(),
        method: Method::Reconcile,
        intent_id: digest(b"intent"),
        input_digest: digest(b"inputs"),
        operation: Operation::Notify,
        destination: "local:owner".into(),
        inputs: BTreeMap::new(),
    };
    let response = Response {
        schema: "af/scenario-adapter-response@1".into(),
        intent_id: request.intent_id.clone(),
        input_digest: request.input_digest.clone(),
        outcome: Outcome::Absent,
        evidence: "no prior effect".into(),
        observation: None,
    };
    valid("scenario-adapter-request-v4.json", &request);
    valid("scenario-adapter-response-v1.json", &response);
    valid(
        "scenario-owned-file-v1.json",
        &OwnedDocument {
            schema: "af/scenario-owned-file@1".into(),
            owner: digest(b"owner"),
            content: "owned".into(),
        },
    );
    if let Some(Waiting::Decision { request, .. }) = &run.waiting {
        valid("scenario-decision-v1.json", request);
    }
}

#[test]
fn recursion_invalid_ports_unreachable_definitions_and_widened_children_are_rejected() {
    let (mut bundle, _) = simple();
    let child = bundle.definitions["main"].clone();
    let mut invoke = child.steps[0].clone();
    invoke.operation = Kind::Scenario {
        scenario: "child".into(),
    };
    invoke.inputs.clear();
    invoke.outputs = child.outputs.clone();
    bundle.definitions.get_mut("main").unwrap().steps = vec![invoke.clone()];
    bundle.definitions.insert("child".into(), child);
    bundle.validate(&TestHost::default()).unwrap();
    let mut bad = bundle.clone();
    bad.definitions.get_mut("child").unwrap().steps[0].operation = Kind::Scenario {
        scenario: "main".into(),
    };
    assert!(
        bad.validate(&TestHost::default())
            .unwrap_err()
            .contains("Recursive")
    );
    let mut bad = bundle.clone();
    bad.definitions.get_mut("main").unwrap().steps[0]
        .outputs
        .insert("wrong".into(), Port::Text);
    assert!(bad.validate(&TestHost::default()).is_err());
    let mut bad = bundle.clone();
    bad.definitions
        .get_mut("child")
        .unwrap()
        .authority
        .task_plans
        .insert(digest(b"widened-child-plan"));
    assert!(
        bad.validate(&TestHost::default())
            .unwrap_err()
            .contains("widens")
    );
    let mut bad = bundle.clone();
    bad.definitions.get_mut("child").unwrap().limits.max_tokens += 1;
    assert!(
        bad.validate(&TestHost::default())
            .unwrap_err()
            .contains("widens")
    );
    let mut bad = bundle.clone();
    bad.definitions
        .get_mut("child")
        .unwrap()
        .authority
        .decision_owners
        .insert("intruder".into());
    assert!(
        bad.validate(&TestHost::default())
            .unwrap_err()
            .contains("widens")
    );
    let mut bad = bundle.clone();
    bad.definitions.get_mut("main").unwrap().completion[0] = Binding::Step {
        step: "future".into(),
        port: "missing".into(),
    };
    assert!(bad.validate(&TestHost::default()).is_err());
    let mut bad = bundle;
    bad.definitions
        .insert("unreachable".into(), bad.definitions["child"].clone());
    assert!(bad.validate(&TestHost::default()).is_err());
}

#[test]
fn signed_decisions_bind_exact_revision_owner_pin_and_deadline() {
    let (bundle, mut policy) = simple();
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    policy
        .decision_keys
        .insert("owner".into(), key.pk.to_box().unwrap().into_string());
    let mut run = Run::new(
        &TestHost::default(),
        "run".into(),
        Pin::new(&TestHost::default(), bundle, policy).unwrap(),
        BTreeMap::new(),
        limits(),
        START,
        None,
    )
    .unwrap();
    run.prepare(START).unwrap();
    let Some(Waiting::Decision { request, .. }) = &run.waiting else {
        panic!("missing decision");
    };
    let request = request.clone();
    let signature = minisign::sign(
        Some(&key.pk),
        &key.sk,
        request.signing_bytes().unwrap().as_slice(),
        None,
        None,
    )
    .unwrap()
    .into_string();
    let receipt = DecisionReceipt {
        schema: "af/scenario-signed-decision@1".into(),
        request,
        signature,
    };
    valid("scenario-signed-decision-v1.json", &receipt);
    for field in ["revision", "owner", "pin_id", "run_id", "step"] {
        let mut wrong = json!(receipt);
        wrong["request"][field] = json!("other");
        let wrong: DecisionReceipt = serde_json::from_value(wrong).unwrap();
        assert!(
            run.clone().apply_decision(wrong, START).is_err(),
            "accepted changed {field}"
        );
    }
    assert!(
        run.clone()
            .apply_decision(receipt.clone(), receipt.request.expires_unix_ms)
            .is_err()
    );
    run.apply_decision(receipt, START).unwrap();
    run.prepare(START).unwrap();
    run.prepare(START).unwrap();
    assert_eq!(run.status, Status::Succeeded);
    run.validate(&TestHost::default()).unwrap();
}

#[test]
fn publication_approval_rejects_mixed_operation_consumers_across_aliases_and_child_ports() {
    for other_operation in [Operation::PublishPr, Operation::Followup, Operation::Notify] {
        let (bundle, mut policy) = publication();
        policy
            .adapters
            .get_mut("publisher")
            .unwrap()
            .operations
            .insert(Operation::Notify);
        Pin::new(&TestHost::default(), bundle.clone(), policy.clone()).unwrap();
        for (operation, destination) in [
            (Operation::PublishPr, "fixture:repository"),
            (other_operation, "fixture:other"),
        ] {
            let mut fanout = bundle.clone();
            let definition = fanout.definitions.get_mut("main").unwrap();
            let mut second = definition.steps[1].clone();
            second.id = "second".into();
            let Kind::Action { grant, .. } = &mut second.operation else {
                unreachable!()
            };
            grant.operation = operation;
            grant.destination = destination.into();
            definition.authority.grants.insert(grant.clone());
            definition.steps.push(second);
            // Refuse at pin admission, before a Run can accept a signature or dispatch either
            // Action. Consumer order must not change the approval boundary.
            for reverse in [false, true] {
                let mut candidate = fanout.clone();
                if reverse {
                    candidate
                        .definitions
                        .get_mut("main")
                        .unwrap()
                        .steps
                        .swap(1, 2);
                }
                assert!(
                    Pin::new(&TestHost::default(), candidate, policy.clone())
                        .unwrap_err()
                        .contains("multiple Action consumers"),
                    "accepted {operation:?} consumer; reversed: {reverse}"
                );
            }
        }
        // A child consumer of any operation cannot share its parent publication approval.
        let mut nested = bundle.clone();
        let mut child = nested.definitions["main"].clone();
        child.inputs = BTreeMap::from([("approval".into(), Port::Decision)]);
        child.steps.remove(0);
        let Kind::Action { grant, .. } = &mut child.steps[0].operation else {
            unreachable!()
        };
        grant.operation = other_operation;
        child.authority.grants.insert(grant.clone());
        nested
            .definitions
            .get_mut("main")
            .unwrap()
            .authority
            .grants
            .insert(grant.clone());
        child.steps[0].inputs.insert(
            "decision".into(),
            Binding::Input {
                port: "approval".into(),
            },
        );
        let mut invoke = child.steps[0].clone();
        invoke.id = "child".into();
        invoke.operation = Kind::Scenario {
            scenario: "child".into(),
        };
        invoke.inputs = BTreeMap::from([(
            "approval".into(),
            Binding::Step {
                step: "review".into(),
                port: "result".into(),
            },
        )]);
        invoke.outputs = child.outputs.clone();
        nested.definitions.insert("child".into(), child);
        nested
            .definitions
            .get_mut("main")
            .unwrap()
            .steps
            .push(invoke);
        assert!(
            Pin::new(&TestHost::default(), nested.clone(), policy.clone())
                .unwrap_err()
                .contains("multiple Action consumers")
        );
        // Only the child's Action consumes it now: receipt forwarding remains supported.
        nested.definitions.get_mut("main").unwrap().steps.remove(1);
        Pin::new(&TestHost::default(), nested.clone(), policy.clone()).unwrap();
        let mut aliased = nested;
        let child = aliased.definitions.get_mut("child").unwrap();
        child.inputs.insert("alias".into(), Port::Decision);
        let mut second = child.steps[0].clone();
        second.id = "second".into();
        second.operation = bundle.definitions["main"].steps[1].operation.clone();
        second.inputs.insert(
            "decision".into(),
            Binding::Input {
                port: "alias".into(),
            },
        );
        child.steps.push(second);
        let invocation = &mut aliased.definitions.get_mut("main").unwrap().steps[1];
        invocation
            .inputs
            .insert("alias".into(), invocation.inputs["approval"].clone());
        assert!(
            Pin::new(&TestHost::default(), aliased, policy.clone())
                .unwrap_err()
                .contains("multiple Action consumers")
        );

        // Exporting a child's already-consumed approval does not reset its publication use.
        let mut exported = bundle.clone();
        let child = exported.definitions.get_mut("main").unwrap();
        child.outputs.insert("approval".into(), Port::Decision);
        child.result.insert(
            "approval".into(),
            Binding::Step {
                step: "review".into(),
                port: "result".into(),
            },
        );
        let mut root = child.clone();
        let mut invoke = root.steps[0].clone();
        invoke.id = "child".into();
        invoke.operation = Kind::Scenario {
            scenario: "main".into(),
        };
        invoke.inputs.clear();
        invoke.outputs = child.outputs.clone();
        let mut second = root.steps[1].clone();
        let Kind::Action { grant, .. } = &mut second.operation else {
            unreachable!()
        };
        grant.operation = other_operation;
        root.authority.grants.insert(grant.clone());
        second.inputs.insert(
            "decision".into(),
            Binding::Step {
                step: "child".into(),
                port: "approval".into(),
            },
        );
        root.outputs.remove("approval");
        root.result.remove("approval");
        root.steps = vec![invoke, second];
        exported.root = "parent".into();
        exported.definitions.insert("parent".into(), root);
        assert!(
            Pin::new(&TestHost::default(), exported.clone(), policy.clone())
                .unwrap_err()
                .contains("multiple Action consumers")
        );
        // Reusing a definition creates distinct Decision invocations, each with its own consumer.
        let root = exported.definitions.get_mut("parent").unwrap();
        let mut second_child = root.steps[0].clone();
        second_child.id = "second-child".into();
        root.steps[1] = second_child;
        Pin::new(&TestHost::default(), exported, policy).unwrap();
    }
}

#[test]
fn nonpublication_decision_consumers_remain_valid_until_a_publication_shares_the_receipt() {
    let (mut bundle, mut policy) = publication();
    policy
        .adapters
        .get_mut("publisher")
        .unwrap()
        .operations
        .insert(Operation::Notify);
    let definition = bundle.definitions.get_mut("main").unwrap();
    let publish = definition.steps[1].clone();
    let mut notify = publish.clone();
    let Kind::Action { grant, .. } = &mut notify.operation else {
        unreachable!()
    };
    grant.operation = Operation::Notify;
    definition.authority.grants.insert(grant.clone());
    definition.steps.truncate(1);
    for id in ["first", "second", "third"] {
        notify.id = id.into();
        definition.steps.push(notify.clone());
    }
    Pin::new(&TestHost::default(), bundle.clone(), policy.clone()).unwrap();
    bundle
        .definitions
        .get_mut("main")
        .unwrap()
        .steps
        .push(publish);
    assert!(
        Pin::new(&TestHost::default(), bundle, policy)
            .unwrap_err()
            .contains("multiple Action consumers")
    );
}

#[test]
fn publication_dispatch_preserves_decision_forwarding_into_and_out_of_children() {
    for into_child in [true, false] {
        let (mut bundle, mut policy) = publication();
        let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        policy
            .decision_keys
            .insert("owner".into(), key.pk.to_box().unwrap().into_string());
        let mut child = bundle.definitions["main"].clone();
        let root = bundle.definitions.get_mut("main").unwrap();
        let mut invoke = root.steps[0].clone();
        invoke.id = "child".into();
        invoke.operation = Kind::Scenario {
            scenario: "child".into(),
        };
        invoke.inputs.clear();
        if into_child {
            child.inputs.insert("approval".into(), Port::Decision);
            child.steps.remove(0);
            child.steps[0].inputs.insert(
                "decision".into(),
                Binding::Input {
                    port: "approval".into(),
                },
            );
            invoke.inputs.insert(
                "approval".into(),
                Binding::Step {
                    step: "review".into(),
                    port: "result".into(),
                },
            );
            invoke.outputs = child.outputs.clone();
            root.steps[1] = invoke;
        } else {
            child.steps.pop();
            child.outputs = BTreeMap::from([("approval".into(), Port::Decision)]);
            child.result = BTreeMap::from([(
                "approval".into(),
                Binding::Step {
                    step: "review".into(),
                    port: "result".into(),
                },
            )]);
            invoke.outputs = child.outputs.clone();
            root.steps[0] = invoke;
            root.steps[1].inputs.insert(
                "decision".into(),
                Binding::Step {
                    step: "child".into(),
                    port: "approval".into(),
                },
            );
        }
        bundle.definitions.insert("child".into(), child);
        let mut run = Run::new(
            &TestHost::default(),
            "forwarded".into(),
            Pin::new(&TestHost::default(), bundle, policy).unwrap(),
            BTreeMap::new(),
            limits(),
            START,
            None,
        )
        .unwrap();
        run.prepare(START).unwrap();
        let Some(Waiting::Decision { request, .. }) = &run.waiting else {
            panic!("missing decision")
        };
        let request = request.clone();
        assert_eq!(
            request.step,
            if into_child {
                "root/review"
            } else {
                "root/child/review"
            }
        );
        let signature = minisign::sign(
            Some(&key.pk),
            &key.sk,
            request.signing_bytes().unwrap().as_slice(),
            None,
            None,
        )
        .unwrap()
        .into_string();
        run.apply_decision(
            DecisionReceipt {
                schema: "af/scenario-signed-decision@1".into(),
                request,
                signature,
            },
            START,
        )
        .unwrap();
        let dispatch = (0..4)
            .find_map(|_| run.prepare(START).unwrap())
            .expect("publication dispatch after completing the Decision and child frame");
        assert_eq!(dispatch.step, "publish");
        assert_eq!(
            dispatch.frame,
            if into_child { "root/child" } else { "root" }
        );
        run.validate(&TestHost::default()).unwrap();
    }
}

#[test]
fn publication_dispatch_checks_decision_source_and_changed_grants_invalidate_approval() {
    let (bundle, mut policy) = publication();
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    policy
        .decision_keys
        .insert("owner".into(), key.pk.to_box().unwrap().into_string());
    policy.adapters.insert(
        "other-publisher".into(),
        policy.adapters["publisher"].clone(),
    );
    let mut run = Run::new(
        &TestHost::default(),
        "publication".into(),
        Pin::new(&TestHost::default(), bundle.clone(), policy.clone()).unwrap(),
        BTreeMap::new(),
        limits(),
        START,
        None,
    )
    .unwrap();
    run.prepare(START).unwrap();
    let Some(Waiting::Decision { request, .. }) = &run.waiting else {
        panic!("missing decision")
    };
    let sign = |request: DecisionRequest| {
        let signature = minisign::sign(
            Some(&key.pk),
            &key.sk,
            request.signing_bytes().unwrap().as_slice(),
            None,
            None,
        )
        .unwrap()
        .into_string();
        DecisionReceipt {
            schema: "af/scenario-signed-decision@1".into(),
            request,
            signature,
        }
    };
    let receipt = sign(request.clone());
    run.apply_decision(receipt.clone(), START).unwrap();
    run.prepare(START).unwrap();
    let mut wrong_source = run.clone();
    let mut unrelated = receipt.request.clone();
    unrelated.step = "root/another-decision".into();
    wrong_source.frames.get_mut("root").unwrap().steps[0]
        .outputs
        .insert("result".into(), Datum::Decision(sign(unrelated)));
    assert!(
        wrong_source
            .prepare(START)
            .unwrap_err()
            .contains("unrelated human decision")
    );
    for field in ["adapter", "operation", "destination"] {
        let mut changed = json!(bundle);
        let grant = &mut changed["definitions"]["main"]["steps"][1]["operation"]["grant"];
        grant[field] = json!(match field {
            "adapter" => "other-publisher",
            "operation" => "followup",
            _ => "fixture:other",
        });
        let grant = grant.clone();
        changed["definitions"]["main"]["authority"]["grants"] = json!([grant]);
        let pin = Pin::new(
            &TestHost::default(),
            serde_json::from_value(changed).unwrap(),
            policy.clone(),
        )
        .unwrap();
        assert_ne!(pin.revision, run.pin.revision);
        let mut changed_run = run.clone();
        changed_run.pin = pin;
        assert!(
            changed_run
                .prepare(START)
                .unwrap_err()
                .contains("unrelated"),
            "accepted changed {field}"
        );
    }
    assert_eq!(run.prepare(START).unwrap().unwrap().step, "publish");
}

#[test]
fn timezone_due_time_duplicate_ticks_and_waits_count_as_active() {
    let mut ant = ant("+03:00", 9, 0);
    let mut runs = BTreeMap::new();
    assert!(
        ant.tick(&TestHost::default(), START + 6 * 3_600_000 - 1, &mut runs)
            .unwrap()
            .is_empty()
    );
    let ids = ant
        .tick(&TestHost::default(), START + 6 * 3_600_000, &mut runs)
        .unwrap();
    assert_eq!(ids.len(), 1);
    runs.get_mut(&ids[0])
        .unwrap()
        .prepare(START + 6 * 3_600_000)
        .unwrap();
    assert_eq!(runs[&ids[0]].status, Status::Waiting);
    assert!(
        ant.tick(&TestHost::default(), START + 6 * 3_600_000, &mut runs)
            .unwrap()
            .is_empty()
    );
    assert!(
        ant.tick(&TestHost::default(), START + DAY + 6 * 3_600_000, &mut runs)
            .unwrap()
            .is_empty()
    );
    assert_eq!(ant.occurrences.len(), 2);
    assert_eq!(ant.occurrences[1].outcome, OccurrenceOutcome::SkippedBusy);
    ant.validate(&TestHost::default(), &runs).unwrap();
    assert!(Zone::load("+24:00").is_err());
    assert!(Zone::load("../etc/passwd").is_err());
}

#[test]
fn pinned_iana_dst_gap_is_skipped_and_fold_has_only_one_occurrence() {
    let mut ant = ant("Europe/Berlin", 2, 30);
    let mut runs = BTreeMap::new();
    // 2026-03-29: 02:30 does not exist. 2026-10-25: 02:30 repeats.
    for (start, gap) in [(1_774_742_400_000_u64, true), (1_792_886_400_000, false)] {
        ant.pin.definition.schedule.start_unix_ms = start;
        ant.pin.revision = hash(&(&ant.pin.definition, &ant.pin.scenario, &ant.pin.zone)).unwrap();
        ant = AntState::new(ant.pin.clone(), start).unwrap();
        ant.change(AntMode::Enabled, start).unwrap();
        let ids = ant
            .tick(
                &TestHost::default(),
                start + if gap { 90 * 60 * 1000 } else { 30 * 60 * 1000 },
                &mut runs,
            )
            .unwrap();
        assert_eq!(ant.occurrences.len(), 1);
        if gap {
            assert!(ids.is_empty());
            assert_eq!(
                ant.occurrences[0].outcome,
                OccurrenceOutcome::SkippedMissingTime
            );
        } else {
            assert_eq!(ids.len(), 1);
            assert!(
                ant.tick(&TestHost::default(), start + 90 * 60 * 1000, &mut runs)
                    .unwrap()
                    .is_empty()
            );
        }
        runs.clear();
    }
}

#[test]
fn missed_occurrences_pause_and_disable_never_create_a_catchup_burst() {
    let mut ant = ant("UTC", 12, 0);
    let mut runs = BTreeMap::new();
    let ids = ant
        .tick(
            &TestHost::default(),
            START + 3 * DAY + 12 * 3_600_000,
            &mut runs,
        )
        .unwrap();
    assert_eq!(ids.len(), 1);
    assert!(
        ant.occurrences[..3]
            .iter()
            .all(|o| o.outcome == OccurrenceOutcome::SkippedMissed)
    );
    ant.change(AntMode::Paused, START + 4 * DAY).unwrap();
    ant.tick(
        &TestHost::default(),
        START + 4 * DAY + 12 * 3_600_000,
        &mut runs,
    )
    .unwrap();
    assert_eq!(
        ant.occurrences.last().unwrap().outcome,
        OccurrenceOutcome::SkippedPaused
    );
    assert!(runs[&ids[0]].status.active());
    ant.change(AntMode::Disabled, START + 5 * DAY).unwrap();
    ant.change(AntMode::Enabled, START + 6 * DAY).unwrap();
    ant.tick(
        &TestHost::default(),
        START + 6 * DAY + 12 * 3_600_000,
        &mut runs,
    )
    .unwrap();
    assert_eq!(
        ant.occurrences[5].outcome,
        OccurrenceOutcome::SkippedDisabled
    );
    assert_eq!(ant.occurrences[6].outcome, OccurrenceOutcome::SkippedBusy);
    ant.validate(&TestHost::default(), &runs).unwrap();
}

#[test]
fn pause_or_disable_after_due_time_before_first_tick_prevents_launch() {
    let due = START + 12 * 3_600_000;
    for (mode, expected) in [
        (AntMode::Paused, OccurrenceOutcome::SkippedPaused),
        (AntMode::Disabled, OccurrenceOutcome::SkippedDisabled),
    ] {
        let mut ant = ant("UTC", 12, 0);
        let mut runs = BTreeMap::new();
        ant.change(mode, due + 100).unwrap();
        assert!(
            ant.tick(&TestHost::default(), due + 200, &mut runs)
                .unwrap()
                .is_empty()
        );
        assert!(runs.is_empty());
        assert_eq!(ant.occurrences[0].outcome, expected);
        assert!(
            ant.tick(&TestHost::default(), due + 200, &mut runs)
                .unwrap()
                .is_empty()
        );
        ant.validate(&TestHost::default(), &runs).unwrap();
        // Enabling later does not resurrect the skipped occurrence.
        ant.change(AntMode::Enabled, due + 300).unwrap();
        assert!(
            ant.tick(&TestHost::default(), due + 400, &mut runs)
                .unwrap()
                .is_empty()
        );
        assert_eq!(ant.occurrences.len(), 1);
        assert_eq!(
            ant.tick(&TestHost::default(), due + DAY, &mut runs)
                .unwrap()
                .len(),
            1
        );
        ant.validate(&TestHost::default(), &runs).unwrap();
    }
}

#[test]
fn pause_or_disable_then_enable_before_first_tick_durably_skips_only_that_day() {
    let due = START + 12 * 3_600_000;
    for (mode, expected) in [
        (AntMode::Paused, OccurrenceOutcome::SkippedPaused),
        (AntMode::Disabled, OccurrenceOutcome::SkippedDisabled),
    ] {
        let mut ant = ant("UTC", 12, 0);
        let mut runs = BTreeMap::new();
        ant.change(mode, due + 100).unwrap();
        ant.change(AntMode::Enabled, due + 150).unwrap();
        // Recover the transitions before the first tick, then recover its skipped receipt.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state");
        Store::isolated(&path)
            .unwrap()
            .update(|world| {
                world.ants.insert("daily".into(), ant);
                Ok(())
            })
            .unwrap();
        let store = Store::isolated(&path).unwrap();
        let mut ant = store.read().unwrap().ants.remove("daily").unwrap();
        assert!(
            ant.tick(&TestHost::default(), due + 200, &mut runs)
                .unwrap()
                .is_empty()
        );
        assert!(runs.is_empty());
        assert_eq!(ant.occurrences.len(), 1);
        assert_eq!(ant.occurrences[0].outcome, expected);
        ant.validate(&TestHost::default(), &runs).unwrap();
        store
            .update(|world| {
                world.ants.insert("daily".into(), ant);
                Ok(())
            })
            .unwrap();
        let mut ant = Store::isolated(&path)
            .unwrap()
            .read()
            .unwrap()
            .ants
            .remove("daily")
            .unwrap();
        ant.validate(&TestHost::default(), &runs).unwrap();
        assert!(
            ant.tick(&TestHost::default(), due + 300, &mut runs)
                .unwrap()
                .is_empty()
        );
        assert_eq!(ant.occurrences.len(), 1);
        assert_eq!(ant.occurrences[0].outcome, expected);
        assert_eq!(
            ant.tick(&TestHost::default(), due + DAY, &mut runs)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(ant.occurrences.len(), 2);
        ant.validate(&TestHost::default(), &runs).unwrap();
    }
}

#[test]
fn tick_retains_historical_due_modes_and_missed_outcomes_after_mode_changes() {
    let due = START + 12 * 3_600_000;
    for (mode, expected) in [
        (AntMode::Paused, OccurrenceOutcome::SkippedPaused),
        (AntMode::Disabled, OccurrenceOutcome::SkippedDisabled),
    ] {
        let mut ant = ant("UTC", 12, 0);
        let mut runs = BTreeMap::new();
        ant.change(mode, due - 100).unwrap();
        ant.change(AntMode::Enabled, due + 100).unwrap();
        assert!(
            ant.tick(&TestHost::default(), due + 200, &mut runs)
                .unwrap()
                .is_empty()
        );
        assert_eq!(ant.occurrences[0].outcome, expected);
        ant.validate(&TestHost::default(), &runs).unwrap();
    }
    let mut ant = ant("UTC", 12, 0);
    ant.change(AntMode::Paused, due + 100).unwrap();
    let mut runs = BTreeMap::new();
    assert!(
        ant.tick(&TestHost::default(), due + DAY, &mut runs)
            .unwrap()
            .is_empty()
    );
    assert_eq!(ant.occurrences[0].outcome, OccurrenceOutcome::SkippedMissed);
    assert_eq!(ant.occurrences[1].outcome, OccurrenceOutcome::SkippedPaused);
    ant.validate(&TestHost::default(), &runs).unwrap();
}

#[test]
fn per_run_and_rolling_budgets_reserve_waiting_work_and_release_only_after_window() {
    let mut ant = ant("UTC", 12, 0);
    let mut runs = BTreeMap::new();
    let target = TaskTarget {
        repo: "/repo".into(),
        state: "/state".into(),
        task_id: "task".into(),
        plan_id: digest(b"plan"),
        revision_id: digest(b"revision"),
        max_tokens: 100,
    };
    let mut bundle = ant.pin.scenario.bundle.clone();
    let task = &mut bundle.definitions.get_mut("main").unwrap().steps[0];
    task.operation = Kind::Task {
        target: target.clone(),
    };
    task.inputs.clear();
    task.outputs = BTreeMap::from([("result".into(), Port::Task)]);
    let mut policy = ant.pin.scenario.policy.clone();
    policy.task_plans.insert(target.plan_id.clone());
    for def in bundle.definitions.values_mut() {
        def.authority.task_plans.insert(target.plan_id.clone());
    }
    let mut definition = ant.pin.definition.clone();
    definition.scenario = bundle;
    ant = AntState::new(
        AntPin::new(&TestHost::default(), definition, policy).unwrap(),
        START,
    )
    .unwrap();
    ant.change(AntMode::Enabled, START).unwrap();
    let id = ant
        .tick(&TestHost::default(), START + 12 * 3_600_000, &mut runs)
        .unwrap()
        .pop()
        .unwrap();
    let run = runs.get_mut(&id).unwrap();
    assert!(run.prepare(START + 12 * 3_600_000).unwrap().is_some());
    assert_eq!(run.tokens().unwrap(), 100);
    run.fail("fixture definitive Task failure; reservation retained".into());
    ant.tick(
        &TestHost::default(),
        START + DAY + 12 * 3_600_000,
        &mut runs,
    )
    .unwrap();
    assert_eq!(
        ant.occurrences.last().unwrap().outcome,
        OccurrenceOutcome::SkippedBudget
    );
    ant.validate(&TestHost::default(), &runs).unwrap();
    let ids = ant
        .tick(
            &TestHost::default(),
            START + 4 * DAY + 12 * 3_600_000,
            &mut runs,
        )
        .unwrap();
    assert_eq!(ids.len(), 1);
    let mut too_big = ant.pin.definition.clone();
    too_big.execution.per_run.max_tokens += 1;
    assert!(too_big.validate(&TestHost::default()).is_err());
}

#[test]
fn simultaneous_scheduler_transactions_admit_one_occurrence_and_locks_exclude_two_drivers() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::isolated(&dir.path().join("state")).unwrap();
    let ant = ant("UTC", 12, 0);
    store
        .update(|w| {
            w.ants.insert("daily".into(), ant);
            Ok(())
        })
        .unwrap();
    let barrier = Arc::new(Barrier::new(3));
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..2 {
            let barrier = barrier.clone();
            let store = &store;
            handles.push(scope.spawn(move || {
                barrier.wait();
                store
                    .update(|world| {
                        let ant = world.ants.get_mut("daily").unwrap();
                        ant.tick(
                            &TestHost::default(),
                            START + 12 * 3_600_000,
                            &mut world.runs,
                        )
                    })
                    .unwrap()
            }));
        }
        barrier.wait();
        assert_eq!(
            handles
                .into_iter()
                .map(|h| h.join().unwrap().len())
                .sum::<usize>(),
            1
        );
    });
    let world = store.read().unwrap();
    assert_eq!(world.runs.len(), 1);
    assert_eq!(world.ants["daily"].occurrences.len(), 1);
    let id = world.runs.keys().next().unwrap();
    let _held = store.lock_run(id).unwrap();
    assert!(store.lock_run(id).is_err());
    valid("scenario-store-v6.json", &world);
}

#[test]
fn corrupt_journal_checksum_pins_and_lifecycle_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state");
    let store = Store::isolated(&path).unwrap();
    let (bundle, policy) = simple();
    let run = Run::new(
        &TestHost::default(),
        "run".into(),
        Pin::new(&TestHost::default(), bundle, policy).unwrap(),
        BTreeMap::new(),
        limits(),
        START,
        None,
    )
    .unwrap();
    store
        .update(|w| {
            w.runs.insert("run".into(), run);
            Ok(())
        })
        .unwrap();
    let mut corrupt = store.read().unwrap();
    corrupt.runs.get_mut("run").unwrap().status = Status::Succeeded;
    assert!(corrupt.validate(&TestHost::default()).is_err());
    let mut corrupt = store.read().unwrap();
    corrupt.runs.get_mut("run").unwrap().pin.definitions.clear();
    assert!(corrupt.validate(&TestHost::default()).is_err());
    let c = rusqlite::Connection::open(path.join("runs/scenarios.sqlite")).unwrap();
    c.execute("UPDATE journal SET digest='corrupt'", [])
        .unwrap();
    assert!(store.read().unwrap_err().contains("checksum"));
}

#[test]
fn typed_projections_drive_conditional_skips_and_evidenced_no_change_completion() {
    let (mut bundle, policy) = simple();
    let predicate = Binding::Equals {
        left: Box::new(Binding::Input {
            port: "outcome".into(),
        }),
        right: Box::new(lit(Datum::Text("no_worthwhile_optimization".into()))),
    };
    let def = bundle.definitions.get_mut("main").unwrap();
    def.inputs.insert("outcome".into(), Port::Text);
    def.steps[0].when = Some(Binding::Equals {
        left: Box::new(Binding::Input {
            port: "outcome".into(),
        }),
        right: Box::new(lit(Datum::Text("worth_implementing".into()))),
    });
    def.completion = vec![predicate];
    let mut run = Run::new(
        &TestHost::default(),
        "no-change".into(),
        Pin::new(&TestHost::default(), bundle.clone(), policy.clone()).unwrap(),
        BTreeMap::from([(
            "outcome".into(),
            Datum::Text("no_worthwhile_optimization".into()),
        )]),
        limits(),
        START,
        None,
    )
    .unwrap();
    run.prepare(START).unwrap();
    run.prepare(START).unwrap();
    assert_eq!(run.status, Status::Succeeded);
    assert!(run.frames["root"].steps[0].skipped.is_some());
    run.validate(&TestHost::default()).unwrap();
    // Typed projections use actual receipt fields, without admitting arbitrary JSON pointers.
    let observation = Datum::Observation(Observation {
        subject: "pr-1".into(),
        revision: "head-1".into(),
        condition: "merged".into(),
        satisfied: true,
        evidence: "exact service receipt".into(),
    });
    assert_eq!(
        Projection::ObservationSatisfied.apply(observation).unwrap(),
        Datum::Boolean(true)
    );
    assert!(
        Projection::TaskConclusion
            .apply(Datum::Text("untyped claim".into()))
            .is_err()
    );
    let mut invalid = bundle;
    invalid.definitions.get_mut("main").unwrap().completion = vec![Binding::Project {
        value: Box::new(lit(Datum::Text("claim".into()))),
        projection: Projection::ObservationSatisfied,
    }];
    assert!(invalid.validate(&TestHost::default()).is_err());
    let mut false_bundle = run.pin.bundle.clone();
    false_bundle.definitions.get_mut("main").unwrap().completion = vec![lit(Datum::Boolean(false))];
    let mut false_completion = Run::new(
        &TestHost::default(),
        "false-completion".into(),
        Pin::new(&TestHost::default(), false_bundle, run.pin.policy.clone()).unwrap(),
        run.frames["root"].inputs.clone(),
        limits(),
        START,
        None,
    )
    .unwrap();
    false_completion.prepare(START).unwrap();
    assert!(
        false_completion
            .prepare(START)
            .unwrap_err()
            .contains("completion condition is false")
    );
}

#[test]
fn nested_dispatch_inherits_ancestor_deadline_and_retry_dispatch_bounds() {
    let (mut bundle, mut policy) = simple();
    let mut child = bundle.definitions["main"].clone();
    let target = TaskTarget {
        repo: "/repo".into(),
        state: "/state".into(),
        task_id: "task".into(),
        plan_id: digest(b"plan"),
        revision_id: digest(b"revision"),
        max_tokens: 50,
    };
    policy.task_plans.insert(target.plan_id.clone());
    for def in bundle.definitions.values_mut() {
        def.authority.task_plans.insert(target.plan_id.clone());
    }
    child.authority.task_plans.insert(target.plan_id.clone());
    child.steps[0].operation = Kind::Task { target };
    child.steps[0].inputs.clear();
    child.steps[0].outputs = BTreeMap::from([("result".into(), Port::Task)]);
    child.steps[0].max_attempts = 2;
    child.limits.max_dispatches = 1;
    let root = bundle.definitions.get_mut("main").unwrap();
    root.steps[0].operation = Kind::Scenario {
        scenario: "child".into(),
    };
    root.steps[0].inputs.clear();
    root.steps[0].outputs = child.outputs.clone();
    root.steps[0].timeout_ms = 10;
    bundle.definitions.insert("child".into(), child);
    let pin = Pin::new(&TestHost::default(), bundle, policy).unwrap();
    let mut run = Run::new(
        &TestHost::default(),
        "nested".into(),
        pin,
        BTreeMap::new(),
        limits(),
        START,
        None,
    )
    .unwrap();
    let first = run.prepare(START).unwrap().unwrap();
    assert_eq!(first.timeout_ms, 10);
    assert_eq!(run.tokens().unwrap(), 50);
    // Uncertain outcomes reuse the same identity; they do not get another retry allowance.
    let child = run.frames.get_mut("root/review").unwrap();
    child.steps[0].intents[0].phase = Phase::Uncertain;
    assert!(
        run.prepare(START)
            .unwrap_err()
            .contains("Child dispatch allowance")
    );
    let mut expired = run.clone();
    expired.status = Status::Running;
    assert!(expired.prepare(START + 10).unwrap().is_none());
    assert_eq!(expired.status, Status::Failed);
    assert_eq!(expired.task_charges.len(), 1);
}

#[test]
fn independent_executions_with_the_same_label_have_distinct_intents_and_decision_requests() {
    let (bundle, policy) = simple();
    let pin = Pin::new(&TestHost::default(), bundle.clone(), policy.clone()).unwrap();
    let mut first = Run::new(
        &TestHost::default(),
        "same".into(),
        pin.clone(),
        BTreeMap::new(),
        limits(),
        START,
        None,
    )
    .unwrap();
    let mut second = Run::new(
        &TestHost::default(),
        "same".into(),
        pin,
        BTreeMap::new(),
        limits(),
        START,
        None,
    )
    .unwrap();
    first.prepare(START).unwrap();
    second.prepare(START).unwrap();
    assert_ne!(first.execution_id, second.execution_id);
    assert_ne!(first.waiting, second.waiting);
    let mut bundle = bundle;
    let target = TaskTarget {
        repo: "/repo".into(),
        state: "/state".into(),
        task_id: "task".into(),
        plan_id: digest(b"plan"),
        revision_id: digest(b"revision"),
        max_tokens: 50,
    };
    let mut policy = policy;
    policy.task_plans.insert(target.plan_id.clone());
    for def in bundle.definitions.values_mut() {
        def.authority.task_plans.insert(target.plan_id.clone());
    }
    let step = &mut bundle.definitions.get_mut("main").unwrap().steps[0];
    step.operation = Kind::Task { target };
    step.inputs.clear();
    step.outputs = BTreeMap::from([("result".into(), Port::Task)]);
    let pin = Pin::new(&TestHost::default(), bundle, policy).unwrap();
    let mut first = Run::new(
        &TestHost::default(),
        "same".into(),
        pin.clone(),
        BTreeMap::new(),
        limits(),
        START,
        None,
    )
    .unwrap();
    let mut second = Run::new(
        &TestHost::default(),
        "same".into(),
        pin,
        BTreeMap::new(),
        limits(),
        START,
        None,
    )
    .unwrap();
    assert_ne!(
        first.prepare(START).unwrap().unwrap().intent.id,
        second.prepare(START).unwrap().unwrap().intent.id
    );
}

#[test]
fn observed_budget_breach_is_retained_and_prevents_any_further_dispatch() {
    let (mut bundle, mut policy) = simple();
    let target = TaskTarget {
        repo: "/repo".into(),
        state: "/state".into(),
        task_id: "task".into(),
        plan_id: digest(b"plan"),
        revision_id: digest(b"revision"),
        max_tokens: 100,
    };
    policy.task_plans.insert(target.plan_id.clone());
    for def in bundle.definitions.values_mut() {
        def.authority.task_plans.insert(target.plan_id.clone());
    }
    let step = &mut bundle.definitions.get_mut("main").unwrap().steps[0];
    step.operation = Kind::Task { target };
    step.inputs.clear();
    step.outputs = BTreeMap::from([("result".into(), Port::Task)]);
    let mut run = Run::new(
        &TestHost::default(),
        "breach".into(),
        Pin::new(&TestHost::default(), bundle, policy).unwrap(),
        BTreeMap::new(),
        limits(),
        START,
        None,
    )
    .unwrap();
    run.prepare(START).unwrap();
    run.task_charges.values_mut().next().unwrap().charged = Some(101);
    assert!(run.validate(&TestHost::default()).is_err());
    run.fail("Task exceeded its Scenario token reservation; exact charge retained".into());
    run.validate(&TestHost::default()).unwrap();
    assert_eq!(run.tokens().unwrap(), 101);
    let dispatches = run.dispatches;
    assert!(run.prepare(START).unwrap().is_none());
    assert_eq!(run.dispatches, dispatches);
}

#[test]
fn one_refused_timezone_horizon_does_not_block_other_ant_occurrences() {
    let stale = ant("Europe/Berlin", 12, 0);
    let horizon = stale
        .pin
        .zone
        .valid_until_unix_seconds
        .expect("IANA DST zone has an explicit horizon") as u64
        * 1000;
    let day = horizon / DAY * DAY + DAY;
    let now = day + 12 * 3_600_000;
    let mut healthy_definition = ant("UTC", 12, 0).pin.definition;
    healthy_definition.id = "healthy".into();
    healthy_definition.schedule.start_unix_ms = day;
    let (_, policy) = simple();
    let mut healthy = AntState::new(
        AntPin::new(&TestHost::default(), healthy_definition, policy).unwrap(),
        day,
    )
    .unwrap();
    healthy.change(AntMode::Enabled, day).unwrap();
    let before = stale.clone();
    let mut ants = BTreeMap::from([("daily".into(), stale), ("healthy".into(), healthy)]);
    let mut runs = BTreeMap::new();
    let (started, refused) =
        super::schedule::tick_all(&TestHost::default(), &mut ants, &mut runs, now);
    assert_eq!(started.len(), 1);
    assert!(refused["daily"].contains("horizon"));
    assert_eq!(ants["daily"], before);
    assert_eq!(runs.len(), 1);
    ants["healthy"]
        .validate(&TestHost::default(), &runs)
        .unwrap();
}

#[test]
fn corrupt_timezone_offsets_and_calendar_cursors_are_refused_without_overflow() {
    let mut zone = Zone::load("UTC").unwrap();
    zone.initial_offset = i32::MIN;
    assert!(zone.validate().is_err());
    let mut ant = ant("UTC", 12, 0);
    ant.next_day = i64::MIN;
    assert!(
        ant.tick(&TestHost::default(), START, &mut BTreeMap::new())
            .is_err()
    );
    assert!(
        ant.validate(&TestHost::default(), &BTreeMap::new())
            .is_err()
    );
}

#[test]
fn negative_offset_schedule_at_epoch_skips_only_instants_before_its_start() {
    let mut binding = ant("-03:00", 9, 0).pin.definition;
    binding.schedule.start_unix_ms = 1;
    binding.id = "epoch".into();
    let (_, policy) = simple();
    let mut ant = AntState::new(
        AntPin::new(&TestHost::default(), binding, policy).unwrap(),
        1,
    )
    .unwrap();
    ant.change(AntMode::Enabled, 1).unwrap();
    let mut runs = BTreeMap::new();
    assert_eq!(
        ant.tick(&TestHost::default(), 12 * 3_600_000, &mut runs)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(ant.occurrences.len(), 1);
    assert_eq!(ant.occurrences[0].due_unix_ms, 12 * 3_600_000);
    ant.validate(&TestHost::default(), &runs).unwrap();
}

#[test]
fn uncertain_dispatch_reuses_intent_and_definitive_failures_exhaust_bounded_attempts() {
    let (mut bundle, mut policy) = simple();
    let target = TaskTarget {
        repo: "/repo".into(),
        state: "/state".into(),
        task_id: "task".into(),
        plan_id: digest(b"plan"),
        revision_id: digest(b"revision"),
        max_tokens: 50,
    };
    policy.task_plans.insert(target.plan_id.clone());
    let definition = bundle.definitions.get_mut("main").unwrap();
    definition
        .authority
        .task_plans
        .insert(target.plan_id.clone());
    let step = &mut definition.steps[0];
    step.operation = Kind::Task { target };
    step.inputs.clear();
    step.outputs = BTreeMap::from([("result".into(), Port::Task)]);
    step.max_attempts = 2;
    let mut run = Run::new(
        &TestHost::default(),
        "bounded".into(),
        Pin::new(&TestHost::default(), bundle, policy).unwrap(),
        BTreeMap::new(),
        limits(),
        START,
        None,
    )
    .unwrap();
    let first = run.prepare(START).unwrap().unwrap();
    run.frames.get_mut("root").unwrap().steps[0].intents[0].phase = Phase::Uncertain;
    let recovered = run.prepare(START).unwrap().unwrap();
    assert_eq!(first.intent.id, recovered.intent.id);
    let intent = &mut run.frames.get_mut("root").unwrap().steps[0].intents[0];
    intent.phase = Phase::Failed;
    intent
        .evidence
        .push("definitive unsuccessful attempt".into());
    run = serde_json::from_value(serde_json::to_value(&run).unwrap()).unwrap();
    run.validate(&TestHost::default()).unwrap();
    let second = run.prepare(START).unwrap().unwrap();
    assert_ne!(first.intent.id, second.intent.id);
    assert_eq!(run.task_charges.len(), 1);
    assert_eq!(run.tokens().unwrap(), 50);
    let progress = &mut run.frames.get_mut("root").unwrap().steps[0];
    assert_eq!(progress.intents.len(), 2);
    assert_eq!(progress.intents[0].evidence.len(), 1);
    progress.intents[1].phase = Phase::Failed;
    assert!(run.prepare(START).unwrap_err().contains("bounded retries"));
}

#[test]
fn wait_timeout_dispositions_retain_history_and_cannot_fabricate_completion_evidence() {
    let (mut bundle, mut policy) = simple();
    let grant = Grant {
        adapter: "observer".into(),
        operation: Operation::Observe,
        destination: "service:fixture".into(),
    };
    policy.adapters.insert(
        "observer".into(),
        Adapter {
            executable: "/installed/observer".into(),
            digest: digest(b"observer"),
            operations: [Operation::Observe].into(),
            destinations: ["service:fixture".into()].into(),
            max_wall_ms: 1000,
            max_output_bytes: 4096,
        },
    );
    let definition = bundle.definitions.get_mut("main").unwrap();
    definition.authority.grants.insert(grant.clone());
    definition.completion = vec![Binding::Project {
        value: Box::new(Binding::Step {
            step: "review".into(),
            port: "result".into(),
        }),
        projection: Projection::ObservationSatisfied,
    }];
    let step = &mut definition.steps[0];
    step.operation = Kind::Wait {
        grant,
        condition: "merged".into(),
    };
    step.outputs = BTreeMap::from([("result".into(), Port::Observation)]);
    step.timeout_ms = 1;
    let pin = Pin::new(&TestHost::default(), bundle.clone(), policy.clone()).unwrap();
    let mut failed = Run::new(
        &TestHost::default(),
        "fail-timeout".into(),
        pin,
        BTreeMap::new(),
        limits(),
        START,
        None,
    )
    .unwrap();
    failed.prepare(START).unwrap();
    failed.prepare(START + 1).unwrap();
    assert_eq!(failed.status, Status::Failed);
    assert_eq!(failed.frames["root"].steps[0].intents.len(), 1);
    bundle.definitions.get_mut("main").unwrap().steps[0].on_timeout = Timeout::Skip;
    let mut skipped = Run::new(
        &TestHost::default(),
        "skip-timeout".into(),
        Pin::new(&TestHost::default(), bundle, policy).unwrap(),
        BTreeMap::new(),
        limits(),
        START,
        None,
    )
    .unwrap();
    skipped.prepare(START).unwrap();
    skipped.prepare(START + 1).unwrap();
    assert!(skipped.frames["root"].steps[0].skipped.is_some());
    assert!(skipped.frames["root"].steps[0].outputs.is_empty());
    assert!(
        skipped
            .prepare(START + 1)
            .unwrap_err()
            .contains("missing, skipped or incomplete evidence")
    );
}

#[test]
fn inert_performance_bundles_have_native_typed_ports_feasible_whole_loop_bounds_and_no_publication()
{
    let root = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    for file in [
        "performance-candidate.json",
        "performance-no-worthwhile.json",
    ] {
        let value: Value = serde_json::from_slice(
            &std::fs::read(root.join("fixtures/scenarios").join(file)).unwrap(),
        )
        .unwrap();
        valid("scenario-bundle-v5.json", &value);
        let bundle: Bundle = serde_json::from_value(value).unwrap();
        bundle.validate(&TestHost::default()).unwrap();
        let definition = &bundle.definitions["performance"];
        let sum: u64 = definition
            .steps
            .iter()
            .filter_map(|s| match &s.operation {
                Kind::FreshTask { template, .. } => {
                    Some(template.max_tokens * u64::from(s.max_attempts))
                }
                _ => None,
            })
            .sum();
        assert_eq!(sum, 23_000_000);
        assert!(sum <= definition.limits.max_tokens);
        assert!(definition.authority.grants.iter().all(|g| matches!(
            g.operation,
            Operation::Observe | Operation::ExperimentSubmitV1
        )));
    }
}

#[test]
fn typed_selection_is_lazy_across_skips_and_keeps_decision_ownership_static() {
    let (mut bundle, policy) = simple();
    let def = bundle.definitions.get_mut("main").unwrap();
    def.steps[0].when = Some(lit(Datum::Boolean(false)));
    let skipped = Binding::Project {
        value: Box::new(Binding::Step {
            step: "review".into(),
            port: "result".into(),
        }),
        projection: Projection::DecisionApproved,
    };
    let selection = Binding::Select {
        condition: Box::new(lit(Datum::Boolean(false))),
        when_true: Box::new(skipped),
        when_false: Box::new(lit(Datum::Boolean(true))),
    };
    def.result.insert("ok".into(), selection.clone());
    def.completion = vec![selection];
    bundle.validate(&TestHost::default()).unwrap();
    valid("scenario-bundle-v5.json", &bundle);
    let mut run = Run::new(
        &TestHost::default(),
        "selection".into(),
        Pin::new(&TestHost::default(), bundle.clone(), policy).unwrap(),
        BTreeMap::new(),
        limits(),
        START,
        None,
    )
    .unwrap();
    for _ in 0..3 {
        run.prepare(START).unwrap();
    }
    run.validate(&TestHost::default()).unwrap();
    assert_eq!(run.status, Status::Succeeded);
    assert_eq!(
        run.frames["root"].outputs.as_ref().unwrap()["ok"],
        Datum::Boolean(true)
    );
    let def = bundle.definitions.get_mut("main").unwrap();
    let Binding::Select { condition, .. } = def.result.get_mut("ok").unwrap() else {
        panic!()
    };
    *condition = Box::new(lit(Datum::Text("truthy".into())));
    assert!(bundle.validate(&TestHost::default()).is_err());
    let def = bundle.definitions.get_mut("main").unwrap();
    let Binding::Select {
        condition,
        when_true,
        ..
    } = def.result.get_mut("ok").unwrap()
    else {
        panic!()
    };
    *condition = Box::new(lit(Datum::Boolean(true)));
    *when_true = Box::new(lit(Datum::Text("different type".into())));
    assert!(bundle.validate(&TestHost::default()).is_err());
    let def = bundle.definitions.get_mut("main").unwrap();
    def.outputs.insert("ok".into(), Port::Decision);
    def.result.insert(
        "ok".into(),
        Binding::Select {
            condition: Box::new(lit(Datum::Boolean(true))),
            when_true: Box::new(Binding::Step {
                step: "review".into(),
                port: "result".into(),
            }),
            when_false: Box::new(Binding::Step {
                step: "review".into(),
                port: "result".into(),
            }),
        },
    );
    assert!(
        bundle
            .validate(&TestHost::default())
            .unwrap_err()
            .contains("non-Decision")
    );
}

/// Domain tests use opaque native Task documents. The real Task-file grammar, capture,
/// planning and execution stay covered by `af`'s Scenario CLI integration tests. Unexpected
/// native work here is a test failure, rather than a call into the operator's machine state.
#[derive(Default)]
pub(super) struct TestHost {
    pub reject_templates: std::sync::atomic::AtomicBool,
    pub charge: Option<(u64, bool)>,
}
impl crate::Host for TestHost {
    fn validate_template(&self, _: &Value, _: Option<&Value>, _: u64) -> Result<()> {
        require(
            !self
                .reject_templates
                .load(std::sync::atomic::Ordering::Relaxed),
            "native template refused",
        )
    }
    fn check_predecessor(
        &self,
        _: &template::Template,
        _: &TaskReceipt,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<()> {
        panic!("unexpected native predecessor lookup")
    }
    fn check_experiment_source(
        &self,
        _: &template::Template,
        _: &TaskReceipt,
        _: &str,
        _: &str,
    ) -> Result<()> {
        panic!("unexpected native source lookup")
    }
    fn plan_template(
        &self,
        _: &template::Template,
        _: Value,
        _: bool,
        _: &BTreeMap<String, (TaskReceipt, String)>,
    ) -> Result<TaskTarget> {
        panic!("unexpected native planning")
    }
    fn run_task(
        &self,
        _: &TaskTarget,
        _: u64,
        _: &std::sync::atomic::AtomicBool,
    ) -> Result<adapter::Settlement> {
        panic!("unexpected native Task dispatch")
    }
    fn active_task_run(&self, _: &TaskTarget) -> Option<u64> {
        panic!("unexpected native active run lookup")
    }
    fn cancel_task_run(&self, _: &TaskTarget, _: u64) -> Result<()> {
        panic!("unexpected native cancellation")
    }
    fn task_charge(&self, _: &TaskTarget) -> Result<Option<(u64, bool)>> {
        Ok(self.charge)
    }
    fn validate_performance_proof(
        &self,
        _: &performance::ProofRequest,
        _: &performance::Scope,
    ) -> Result<()> {
        panic!("unexpected native publication proof")
    }
    fn default_store_path(&self) -> Result<PathBuf> {
        // Store::isolated always supplies its own absolute path.
        Ok(PathBuf::from("/unused-scenario-state"))
    }
    fn resolve_path(&self, path: &std::path::Path) -> Result<PathBuf> {
        Ok(path.to_path_buf())
    }
    fn prepare_store(&self, path: &std::path::Path) -> Result<()> {
        af_store::local_fs::create_private_dir_all(path).map_err(|e| e.to_string())
    }
    fn private_dir(&self, path: &std::path::Path) -> Result<()> {
        af_store::local_fs::create_private_dir_all(path).map_err(|e| e.to_string())
    }
    fn register_store(&self, _: &std::path::Path) {
        panic!("isolated Store must not register machine state")
    }
    fn hold_store(&self, path: &std::path::Path) -> Result<Box<dyn Send>> {
        let file = af_store::local_fs::private_file_options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path.join("running.lock"))
            .map_err(|e| e.to_string())?;
        nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockSharedNonblock)
            .map(|guard| Box::new(guard) as Box<dyn Send>)
            .map_err(|(_, e)| e.to_string())
    }
}
