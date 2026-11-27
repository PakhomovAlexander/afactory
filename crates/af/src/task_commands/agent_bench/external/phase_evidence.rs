//! Derive phase readiness from the checked subject and independent grading ledgers.
use super::*;
use af_contracts::task::event::TaskChangeV1;
use af_store::store::task::{read_task_transition, task_run_id};
use num_rational::BigRational;
use serde_json::Value;

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Overhead {
    pub(super) tokens: u64,
    pub(super) wall_ms: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PhaseEvidence {
    schema: String,
    phase: Phase,
    slots: usize,
    collected: usize,
    resolved: usize,
    correct_v1: usize,
    correct_v2: usize,
    paired_native_token_difference: Option<RelativeDifference>,
    paired_wall_difference: Option<RelativeDifference>,
    overhead: Option<Overhead>,
    ready: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RelativeDifference {
    numerator: String,
    denominator: String,
    within_bounds: bool,
}

pub(super) fn lookup(
    cas: &Cas,
    store: &EventStore,
    next: Phase,
    pins: &Pins,
    id: &str,
) -> Result<(Predecessor, Overhead, TaskProjection), String> {
    let expected = next
        .predecessor()
        .ok_or("Smoke has no prerequisite phase")?;
    let grade = store
        .task_projection(cas, id)
        .map_err(|e| e.to_string())?
        .ok_or("Predecessor Task is missing from this Store")?;
    let TaskPhaseV1::Finished { result_id } = &grade.phase else {
        return Err("Predecessor Task has no accepted result".into());
    };
    if grade.revision.kind != grading::KIND {
        return Err("Predecessor must be the completed independent grading Task".into());
    }
    let subject_id = grading::policy(cas, &grade)?.subject_task;
    let subject = store
        .task_projection(cas, &subject_id)
        .map_err(|e| e.to_string())?
        .ok_or("Missing graded predecessor subject")?;
    let plan = read_plan(cas, &subject)?;
    if plan.phase != expected || plan.pins != *pins {
        return Err(
            "Predecessor requires complete preceding phase evidence and identical input pins"
                .into(),
        );
    }
    let report = grading::replay(cas, store, &grade)?;
    let evidence: PhaseEvidence = serde_json::from_value(report["phase_evidence"].clone())
        .map_err(|_| "Invalid replayed phase evidence")?;
    if evidence.phase != expected || !evidence.ready {
        return Err("Predecessor requires complete preceding phase evidence".into());
    }
    let result: TaskResultV1 = artifact(cas, result_id, TASK_RESULT_V1)?;
    if !grade.admitted
        || result.task_revision_id != grade.revision_id
        || result.execution != TaskExecutionV1::Completed
        || result.acceptance != TaskAcceptanceV1::Satisfied
    {
        return Err("Predecessor has no completed confirmed grading result".into());
    }
    let overhead = evidence
        .overhead
        .ok_or("Predecessor pilot overhead is unknown")?;
    let previous = Predecessor {
        task_id: grade.task_id,
        revision_id: grade.revision_id,
        plan_id: grade.revision.authority.policy_id,
        result_id: result_id.clone(),
    };
    Ok((previous, overhead, subject))
}

fn elapsed_ms(store: &EventStore, task: &TaskProjection) -> Result<Option<u64>, String> {
    let (TaskPhaseV1::Finished { result_id }, Some(start)) =
        (&task.phase, task.clock_started_unix_ms())
    else {
        return Ok(None);
    };
    let run = task_run_id(&task.task_id).map_err(|e| e.to_string())?;
    for event in store
        .replay(&run)
        .map_err(|e| e.to_string())?
        .into_iter()
        .rev()
    {
        if event.event_type != af_contracts::EventType::TaskTransitionV7 {
            continue;
        }
        let transition = read_task_transition(&event).map_err(|e| e.to_string())?;
        if matches!(transition.change, TaskChangeV1::Finished { result_id: id } if id == *result_id)
        {
            return Ok(transition.now_unix_ms.checked_sub(start));
        }
    }
    Ok(None)
}

pub(super) fn ancestral_homes(
    cas: &Cas,
    store: &EventStore,
    predecessor: &Predecessor,
) -> Result<Vec<PathBuf>, String> {
    let mut homes = Vec::new();
    let mut cursor = Some(predecessor.task_id.clone());
    for _ in 0..2 {
        let Some(id) = cursor.take() else { break };
        let grade = store
            .task_projection(cas, &id)
            .map_err(|e| e.to_string())?
            .ok_or("Missing ancestral grader")?;
        homes.extend(grading::used_homes(cas, &grade)?);
        let subject = store
            .task_projection(cas, &grading::policy(cas, &grade)?.subject_task)
            .map_err(|e| e.to_string())?
            .ok_or("Missing ancestral subject")?;
        homes.extend(
            execution::read_configuration(cas, &subject)?
                .ok_or("Missing ancestral subject bindings")?
                .auth_homes()
                .iter()
                .cloned(),
        );
        cursor = read_plan(cas, &subject)?
            .predecessor
            .map(|previous| previous.task_id);
    }
    if cursor.is_some() {
        return Err("Pilot exceeds its phase scope".into());
    }
    Ok(homes)
}

pub(super) fn before_grading(
    store: &EventStore,
    task: &TaskProjection,
    rows: &[Value],
    mut budget: Overhead,
) -> Result<Overhead, String> {
    // Each native subject receipt already includes setup, execution and
    // exit/drain. Charge only the elapsed Task wall outside those intervals.
    let subject_ms: u64 = rows
        .iter()
        .filter_map(|row| row["elapsed_ns"].as_u64())
        .map(|ns| ns / 1_000_000)
        .sum();
    let wall = elapsed_ms(store, task)?.ok_or("Subject Task lacks a checked finish time")?;
    budget.wall_ms = budget
        .wall_ms
        .checked_add(wall.saturating_sub(subject_ms))
        .ok_or("Pilot overhead wall overflow")?;
    Ok(budget)
}

pub(super) fn summarize(
    store: &EventStore,
    task: &TaskProjection,
    plan: &ExternalPlan,
    rows: &[Value],
    grade_tokens: Option<u64>,
    policy: &grading::Policy,
) -> Result<PhaseEvidence, String> {
    let collected = rows
        .iter()
        .filter(|row| row["subject_status"] == "collected")
        .count();
    let resolved = rows
        .iter()
        .filter(|row| {
            row["subject_status"] == "collected"
                && matches!(
                    row["decision"].as_str(),
                    Some("correct" | "incorrect" | "disqualified")
                )
                && row["decision_receipt"].is_string()
        })
        .count();
    let correct = |variant| {
        rows.iter()
            .filter(|row| {
                row["variant"] == variant
                    && row["subject_status"] == "collected"
                    && row["decision"] == "correct"
            })
            .count()
    };
    let native = weighted(plan, rows, "subject_inclusive_tokens")?;
    let elapsed = weighted(plan, rows, "subject_elapsed_ns")?;
    let overhead = grade_tokens
        .zip(elapsed_ms(store, task)?)
        .and_then(|(tokens, wall_ms)| {
            Some(Overhead {
                tokens: policy.prior_overhead_tokens.checked_add(tokens)?,
                wall_ms: policy.prior_overhead_ms.checked_add(wall_ms)?,
            })
        });
    let ready = matches!(task.phase, TaskPhaseV1::Finished { .. })
        && rows.len() == plan.phase.slots()
        && collected == rows.len()
        && resolved == rows.len()
        && overhead.is_some_and(|used| {
            used.tokens < grading::OVERHEAD_TOKEN_LIMIT && used.wall_ms < grading::OVERHEAD_WALL_MS
        })
        && native
            .as_ref()
            .is_some_and(|value| plan.phase != Phase::Aa || value.within_bounds)
        && elapsed
            .as_ref()
            .is_some_and(|value| plan.phase != Phase::Aa || value.within_bounds);
    Ok(PhaseEvidence {
        schema: "af.agent-bench-phase-evidence/1".into(),
        phase: plan.phase,
        slots: rows.len(),
        collected,
        resolved,
        correct_v1: correct("V1"),
        correct_v2: correct("V2"),
        paired_native_token_difference: native,
        paired_wall_difference: elapsed,
        overhead,
        ready,
    })
}

fn mean(values: Vec<BigRational>) -> BigRational {
    let count = BigRational::from_integer(values.len().into());
    values.into_iter().sum::<BigRational>() / count
}

fn weighted(
    plan: &ExternalPlan,
    rows: &[Value],
    field: &str,
) -> Result<Option<RelativeDifference>, String> {
    if rows.len() != plan.phase.slots() {
        return Ok(None);
    }
    let mut families = BTreeMap::<&str, BTreeMap<&str, Vec<BigRational>>>::new();
    for (pair, slots) in rows.chunks_exact(2).zip(plan.slots.chunks_exact(2)) {
        let (base, head) = if slots[0].variant == Variant::V1 {
            (&pair[0], &pair[1])
        } else {
            (&pair[1], &pair[0])
        };
        let Some((base_value, head_value)) = base[field].as_u64().zip(head[field].as_u64()) else {
            return Ok(None);
        };
        let family = base["family"].as_str().ok_or("Missing evaluator family")?;
        if Some(family) != head["family"].as_str() {
            return Err("Paired slots differ in evaluator family".into());
        }
        let delta = i128::from(head_value) - i128::from(base_value);
        let relative = BigRational::new(delta.into(), base_value.max(1).into());
        families
            .entry(family)
            .or_default()
            .entry(&slots[0].case_id)
            .or_default()
            .push(relative);
    }
    let value = mean(
        families
            .into_values()
            .map(|cases| mean(cases.into_values().map(mean).collect()))
            .collect(),
    );
    let bound = BigRational::new(1.into(), 5.into());
    Ok(Some(RelativeDifference {
        numerator: value.numer().to_string(),
        denominator: value.denom().to_string(),
        within_bounds: -&bound <= value && value <= bound,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_pipeline_compiles_both_full_phase_widths() {
        let tmp = tempfile::tempdir().unwrap();
        let cas = Cas::open(tmp.path()).unwrap();
        let identity = cas.put(b"fixture authority").unwrap();
        let source = source_input(
            &cas,
            &Manifest::new(vec![]).unwrap(),
            identity.clone(),
            None,
        )
        .unwrap();
        let slot_type = "af/BenchmarkWidthFixtureSlot@1";
        let receipt_type = "af/BenchmarkWidthFixtureReceipt@1";
        let slot = put(&cas, slot_type, &json!({}), vec![], None).unwrap();
        for (kind, obligation, count, wall_ms) in [
            (
                KIND,
                "collected",
                FULL_PHASE_SLOTS,
                execution::subject_wall_ms(Phase::Aa),
            ),
            (
                grading::KIND,
                grading::OBLIGATION,
                FULL_PHASE_SLOTS * 3,
                WALL_MS,
            ),
        ] {
            let compiler = compile_slots(
                &cas,
                &identity,
                &identity,
                SlotPipeline {
                    kind,
                    count,
                    slot_type,
                    receipt_type,
                    obligation,
                    bindings_type: None,
                    tokens: execution::INCLUSIVE_LIMIT,
                    wall_ms,
                },
            )
            .unwrap();
            let mut inputs = BTreeMap::from([("source".into(), source.clone())]);
            let mut required_outputs = BTreeMap::new();
            for index in 0..count {
                let name = node_name(index);
                inputs.insert(
                    name.clone(),
                    ArtifactInputV1 {
                        artifact_type: slot_type.into(),
                        cardinality: PortCardinality::One,
                        artifact_ids: vec![slot.clone()],
                        snapshot_id: None,
                    },
                );
                required_outputs.insert(
                    name,
                    RequiredOutputV1 {
                        artifact_type: receipt_type.into(),
                        cardinality: PortCardinality::One,
                    },
                );
            }
            let tokens = count as u64 * execution::INCLUSIVE_LIMIT;
            let revision = TaskRevisionV1 {
                task_id: format!("fixture-{kind}"),
                revision: 1,
                previous_revision_id: None,
                kind: kind.into(),
                goal: "Compile full fixture width".into(),
                inputs,
                required_outputs,
                acceptance: BTreeMap::from([(
                    obligation.into(),
                    AcceptanceObligationV1 {
                        evidence_type: receipt_type.into(),
                        verifier_policy: identity.clone(),
                    },
                )]),
                provenance: TaskProvenanceV1 {
                    adapter_id: identity.clone(),
                    input_artifact_ids: vec![identity.clone()],
                },
                authority: TaskAuthorityV1 {
                    policy_id: identity.clone(),
                    allowed_effects: BTreeSet::from([
                        "read-source".into(),
                        "execute-checks".into(),
                    ]),
                    data_destinations: BTreeSet::new(),
                },
                limits: TaskLimitsV1 {
                    tokens,
                    max_attempts: count as u32,
                    deadline_unix_ms: clock().unwrap() + count as u64 * wall_ms + WALL_MS,
                    verification: VerificationReserveV1 {
                        tokens,
                        attempts: count as u32,
                        wall_ms: count as u64 * wall_ms,
                    },
                },
                strategy: "paired".into(),
                pipeline: Some(PipelineChoiceV1 {
                    name: PIPELINE.into(),
                    fallback: PipelineFallbackV1::Refuse,
                }),
                facts: BTreeMap::new(),
            };
            let id = put(
                &cas,
                af_contracts::task::TASK_REVISION_V1,
                &revision,
                vec![],
                None,
            )
            .unwrap();
            let (_, graph) = compiler.compile(&cas, &id, PIPELINE).unwrap();
            assert_eq!(graph.nodes.len(), count + 1); // One kernel root-input boundary.
            assert_eq!(graph.outputs.len(), count);
            assert_eq!(graph.max_parallel, 1);
        }
    }

    #[test]
    fn full_aa_weights_all_thirty_six_pairs_and_uses_an_inclusive_exact_threshold() {
        let digest = file_sha256(b"synthetic fixture");
        let input = || InputPin {
            sha256: digest.clone(),
            object_id: digest.clone(),
        };
        let mut plan = ExternalPlan {
            schema: PLAN_SCHEMA.into(),
            phase: Phase::Aa,
            pins: Pins {
                source_revision: "fixture".into(),
                source_digest: digest.clone(),
                bundle: input(),
                schedule: input(),
                project_graph: input(),
            },
            common_instruction: String::new(),
            slots: Vec::new(),
            predecessor: None,
        };
        let mut rows = Vec::new();
        for repetition in 1..=REPETITIONS {
            for case in 1..=CASES_PER_REPETITION {
                let case_id = format!("N{case:02}");
                let family = if case <= 2 { "small" } else { "large" };
                let order = if case % 2 == 0 {
                    [Variant::V1, Variant::V2]
                } else {
                    [Variant::V2, Variant::V1]
                };
                for variant in order {
                    plan.slots.push(ExternalSlot {
                        index: plan.slots.len(),
                        pair_id: format!("R{repetition}-{case_id}"),
                        question: "Question?".into(),
                        case_id: case_id.clone(),
                        repetition,
                        variant,
                        tool_set_id: ToolSet::Text.identity(),
                        tools: vec![],
                    });
                    let tokens = if family == "small" && variant == Variant::V2 {
                        140
                    } else {
                        100
                    };
                    rows.push(json!({"family":family, "tokens":tokens}));
                }
            }
        }
        let relative = weighted(&plan, &rows, "tokens").unwrap().unwrap();
        assert!(relative.within_bounds);
        assert_eq!(
            (relative.numerator.as_str(), relative.denominator.as_str()),
            ("1", "5")
        );

        rows[0]["tokens"] = json!(141);
        assert!(
            !weighted(&plan, &rows, "tokens")
                .unwrap()
                .unwrap()
                .within_bounds
        );
        rows[0]["tokens"] = Value::Null;
        assert!(weighted(&plan, &rows, "tokens").unwrap().is_none());
        rows.pop();
        assert!(weighted(&plan, &rows, "tokens").unwrap().is_none());
    }
}
