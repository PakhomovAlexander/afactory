//! Scenario templates use the ordinary Task-file adapter and its input resolver. Authority is
//! recaptured from one exact commit before planning; generated plans retain their approval gate.
use super::*;
use crate::scenario::contract::{TaskReceipt, TaskTarget};
use crate::scenario::template::Template;

pub(crate) fn validate_document(
    document: &serde_json::Value,
    bindings: Option<&serde_json::Value>,
    maximum: u64,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(document).map_err(|e| e.to_string())?;
    let file = parse_task_file(Path::new("template.json"), &bytes)?;
    task_file_identity(Path::new("template.json"), &file)?;
    if file
        .base
        .as_ref()
        .is_some_and(|b| b.commit.len() != 40 || !b.commit.bytes().all(|c| c.is_ascii_hexdigit()))
    {
        return Err("Template base must name an exact commit".into());
    }
    if file
        .pipeline
        .as_ref()
        .is_some_and(|p| p.fallback != PipelineFallbackV1::Refuse)
    {
        return Err(
            "Template pipeline fallback must be refuse; no generated or default plan authority"
                .into(),
        );
    }
    if bytes.len() > 65536
        || document
            .get("requirements")
            .and_then(|v| v.as_object())
            .is_some_and(|r| r.contains_key("scenario_evidence") || r.contains_key("scenario_goal"))
        || file.inputs.is_some()
        || file.continue_from.is_some()
        || file.issue.is_some()
    {
        return Err(
            "Template must be bounded and cannot prefill inputs, continuation or issue authority"
                .into(),
        );
    }
    let limits = file
        .limits
        .as_ref()
        .ok_or("Template must declare all native Task limits")?;
    if limits.tokens != maximum
        || file.pipeline.is_none()
        || file.strategy.is_none()
        || (file.kind == "implement" && file.verification.is_none())
    {
        return Err(
            "Template must pin its pipeline, strategy, implementation verification and exact token limit".into(),
        );
    }
    if let Some(value) = bindings {
        let local: bindings::LocalBindings =
            serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
        if value["schema"] != "af.task-bindings/1" || !local.packages.is_empty() {
            return Err(
                "Template bindings may pin slots and Providers, never mutable local packages"
                    .into(),
            );
        }
    }
    Ok(())
}
fn with_bindings<T>(
    value: Option<&serde_json::Value>,
    f: impl FnOnce(Option<&Path>) -> Result<T, String>,
) -> Result<T, String> {
    let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
    let path = directory.path().join("bindings.json");
    if let Some(value) = value {
        std::fs::write(&path, serde_json::to_vec(value).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        f(Some(&path))
    } else {
        f(None)
    }
}
fn capture(template: &Template) -> Result<(String, String, String, TaskKindProfile), String> {
    if crate::resolve_filesystem_path(&template.repo)? != template.repo
        || crate::resolve_filesystem_path(&template.state)? != template.state
    {
        return Err("Pinned template repository or Task Store path changed".into());
    }
    let (_, state) = state_path(&template.repo, Some(&template.state))?;
    crate::storage::owned::prepare(&state, true)?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let home = tempfile::tempdir().map_err(|e| e.to_string())?;
    let repo = Repo::open(&template.repo, home.path());
    let source = Capture::new(&repo, &cas)
        .committed(&template.authority_commit)
        .map_err(|e| e.to_string())?;
    let file: TaskFile =
        serde_json::from_value(template.document.clone()).map_err(|e| e.to_string())?;
    let delivery = captured_delivery_policy(&cas, &source.manifest)?;
    let (authority, settings, compiler) = with_bindings(template.bindings.as_ref(), |local| {
        capture_authority(&cas, &source.manifest, local, &file.kind, delivery)
    })?;
    let profile = selected_profile(&compiler, &settings, &file.kind, file.verification)?;
    Ok((
        authority,
        source.repository_id,
        source.content_digest,
        profile,
    ))
}
pub(crate) fn pin(
    repo: PathBuf,
    state: PathBuf,
    authority_commit: String,
    document: serde_json::Value,
    bindings: Option<serde_json::Value>,
) -> Result<Template, String> {
    let maximum = document["limits"]["tokens"]
        .as_u64()
        .ok_or("Template must declare tokens")?;
    validate_document(&document, bindings.as_ref(), maximum)?;
    let mut template = Template {
        schema: "af/scenario-task-template@1".into(),
        repo: crate::resolve_filesystem_path(&repo)?,
        state: crate::resolve_filesystem_path(&state)?,
        authority_commit: authority_commit.to_ascii_lowercase(),
        repository_id: String::new(),
        source_content_digest: String::new(),
        authority_id: String::new(),
        document,
        bindings,
        max_tokens: maximum,
    };
    let (authority, repository, content, _) = capture(&template)?;
    template.authority_id = authority;
    template.repository_id = repository;
    template.source_content_digest = content;
    template.validate()?;
    Ok(template)
}
pub(crate) fn check_predecessor(
    template: &Template,
    receipt: &TaskReceipt,
    port: &str,
    artifact_type: &str,
) -> Result<(), String> {
    let opened = open_task(&receipt.task_id, &template.repo, Some(&template.state))?;
    let (result, _) = opened.view()?;
    if opened.task.revision_id != receipt.revision_id
        || opened.task.plan_id.as_deref() != Some(&receipt.plan_id)
        || !matches!(&opened.task.phase,TaskPhaseV1::Finished { result_id } if result_id == &receipt.result_id)
        || result.as_ref() != Some(&receipt.result)
        || receipt.result.execution != TaskExecutionV1::Completed
        || receipt.result.acceptance != TaskAcceptanceV1::Satisfied
    {
        return Err(
            "Predecessor is not the exact accepted native result in this Task Store".into(),
        );
    }
    let output = receipt
        .result
        .outputs
        .get(port)
        .ok_or("Predecessor output is missing")?;
    if output.artifact_type != artifact_type || output.artifact_ids.is_empty() {
        return Err("Predecessor output has the wrong type or no artifacts".into());
    }
    for id in &output.artifact_ids {
        let envelope = opened.cas.get_artifact(id).map_err(|e| e.to_string())?;
        validate_envelope(&envelope)?;
        if envelope.artifact_type != artifact_type {
            return Err("Predecessor envelope type differs".into());
        }
        if artifact_type == SOURCE_TREE_V1 {
            let tree: review_source_git::task::SourceTree =
                serde_json::from_value(envelope.payload).map_err(|e| e.to_string())?;
            let (snapshot, _) =
                review_source_git::task::read_snapshot(&opened.cas, &tree.snapshot_id)?;
            let origin = review_source_git::task::read_origin(&opened.cas, &snapshot.origin_id)?;
            if origin.repository_id() != template.repository_id {
                return Err("Predecessor source belongs to a different repository".into());
            }
        }
    }
    // The native resolver additionally checks exact Snapshot provenance and root-port types.
    Ok(())
}

pub(crate) fn check_experiment_source(
    template: &Template,
    receipt: &TaskReceipt,
    port: &str,
    expected: &str,
) -> Result<(), String> {
    let opened = open_task(&receipt.task_id, &template.repo, Some(&template.state))?;
    let output = receipt
        .result
        .outputs
        .get(port)
        .ok_or("Native experiment source output absent")?;
    let [artifact] = output.artifact_ids.as_slice() else {
        return Err("Native experiment source requires one SourceTree artifact".into());
    };
    let snapshot = input_bindings::source_tree_snapshot(&opened.cas, artifact)?;
    if output.snapshot_id.as_deref() != Some(&snapshot) {
        return Err("Experiment source output disagrees with its artifact".into());
    }
    experiment_source_matches(&opened.cas, &snapshot, expected)
}

fn experiment_source_matches(cas: &Cas, source: &str, expected: &str) -> Result<(), String> {
    if !review_core::is_digest(expected) {
        return Err("Native experiment evidence revision is not an exact Snapshot".into());
    }
    let mut current = source.to_owned();
    let (root, _) = review_source_git::task::read_snapshot(cas, source)?;
    let root_origin = review_source_git::task::read_origin(cas, &root.origin_id)?;
    let mut seen = BTreeSet::new();
    for _ in 0..64 {
        if !seen.insert(current.clone()) {
            return Err("Cyclic experiment source provenance".into());
        }
        let (snapshot, _) = review_source_git::task::read_snapshot(cas, &current)?;
        let origin = review_source_git::task::read_origin(cas, &snapshot.origin_id)?;
        if origin.repository_id() != root_origin.repository_id() {
            return Err("Experiment source provenance changed repository".into());
        }
        if snapshot.content_digest != root.content_digest {
            return Err("Task source changed from the verified experiment Snapshot".into());
        }
        if current == expected {
            return Ok(());
        }
        let Some(bound) = origin.bound_from() else {
            return Err("Task source differs from the verified experiment Snapshot".into());
        };
        current = bound.snapshot_id.clone();
    }
    Err("Experiment source provenance exceeds its bound".into())
}
pub(crate) fn plan(
    template: &Template,
    document: serde_json::Value,
    performance: bool,
    predecessors: &BTreeMap<String, (TaskReceipt, String)>,
) -> Result<TaskTarget, String> {
    let (authority, repository, content, profile) = capture(template)?;
    if (authority, repository, content)
        != (
            template.authority_id.clone(),
            template.repository_id.clone(),
            template.source_content_digest.clone(),
        )
    {
        return Err("Template catalog, pipeline, Provider or source authority changed".into());
    }
    let id = document["task_id"]
        .as_str()
        .ok_or("No fresh Task ID")?
        .to_owned();
    with_bindings(template.bindings.as_ref(), |local| {
        let options = StartOptions {
            file: PathBuf::from("scenario-task.json"),
            bindings: local.map(Path::to_path_buf),
            source_bindings: None,
            repo: template.repo.clone(),
            state: Some(template.state.clone()),
            authority: template.authority_commit.clone(),
            uncommitted: false,
            json: true,
            plan_only: true,
            timeout_secs: None,
            optimization_history: None,
            fit: false,
            replace: false,
        };
        let code = start_kind_bytes(
            options,
            serde_json::to_vec(&document).map_err(|e| e.to_string())?,
            None,
            Presentation::Silent,
        )?;
        if code != 0 {
            return Err(format!("Native template planning refused with exit {code}"));
        }
        Ok(())
    })?;
    let opened = open_task(&id, &template.repo, Some(&template.state))?;
    validate_recorded_plan(template, &opened, predecessors)?;
    if performance
        && matches!(
            profile,
            TaskKindProfile::Implementation
                | TaskKindProfile::ReviewedImplementation
                | TaskKindProfile::RepairAllowedImplementation
                | TaskKindProfile::Review
                | TaskKindProfile::OptimizationCandidate
        )
    {
        let plan_id = opened
            .task
            .plan_id
            .as_deref()
            .ok_or("Template Task has no native plan")?;
        let plan: ExecutionPlanV1 = artifact(&opened.cas, plan_id, EXECUTION_PLAN_V1)?;
        performance_roles(
            &plan,
            if profile == TaskKindProfile::Review {
                "verification"
            } else {
                "implement"
            },
        )?;
    }
    if opened.task.revision.limits.tokens > template.max_tokens {
        return Err("Native plan widens template budget".into());
    }
    Ok(TaskTarget {
        repo: template.repo.clone(),
        state: template.state.clone(),
        task_id: id,
        plan_id: opened
            .task
            .plan_id
            .clone()
            .ok_or("Template Task has no plan")?,
        revision_id: opened.task.revision_id.clone(),
        max_tokens: template.max_tokens,
    })
}

/// `af task plan` deliberately reuses a matching Task file. A Scenario additionally pins
/// authority and exact predecessor receipts: the same file text alone cannot authorize reuse.
fn validate_recorded_plan(
    template: &Template,
    opened: &OpenedTask,
    predecessors: &BTreeMap<String, (TaskReceipt, String)>,
) -> Result<(), String> {
    if opened.task.revision.authority.policy_id != template.authority_id {
        return Err("Recorded fresh Task widens pinned template authority".into());
    }
    let file: TaskFile =
        serde_json::from_value(template.document.clone()).map_err(|e| e.to_string())?;
    let limits = file.limits.ok_or("Template has no pinned limits")?;
    let revision = &opened.task.revision;
    if revision.limits.tokens != limits.tokens
        || revision.limits.max_attempts != limits.max_attempts
        || revision.limits.verification != limits.verification
        || opened.task.wall_ms() > limits.wall_ms
        || revision.kind != file.kind
        || revision.strategy != file.strategy.as_deref().unwrap_or_default()
        || revision.pipeline != file.pipeline
    {
        return Err(
            "Recorded fresh Task widens or changes pinned template limits or selection".into(),
        );
    }
    let bindings = recorded_input_bindings(&opened.cas, &opened.task.revision)?;
    let recorded = bindings.as_ref().map(|b| &b.bindings);
    if recorded.map_or(0, |b| b.len()) != predecessors.len() {
        return Err("Recorded fresh Task predecessor set differs".into());
    }
    for (input, (receipt, port)) in predecessors {
        let binding = recorded
            .and_then(|b| b.get(input))
            .ok_or("Missing recorded predecessor binding")?;
        let reference = binding
            .task
            .as_ref()
            .ok_or("Recorded binding lost native Task provenance")?;
        if reference.task_id != receipt.task_id
            || reference.task_revision_id != receipt.revision_id
            || reference.result_id != receipt.result_id
            || reference.port != *port
            || reference.acceptance != TaskAcceptanceV1::Satisfied
            || reference.domain_conclusion != receipt.result.domain_conclusion
        {
            return Err("Recorded fresh Task predecessor receipt changed".into());
        }
        let output = receipt
            .result
            .outputs
            .get(port)
            .ok_or("Predecessor output disappeared")?;
        if !output.artifact_ids.contains(&binding.artifact_id) {
            return Err("Recorded binding names a different predecessor artifact".into());
        }
    }
    if !predecessors.contains_key("source")
        && let Some(source) = opened.task.revision.inputs.get("source")
    {
        let snapshot = source
            .snapshot_id
            .as_deref()
            .ok_or("Fresh Task source has no Snapshot")?;
        let (snapshot, manifest) = review_source_git::task::read_snapshot(&opened.cas, snapshot)?;
        let origin = review_source_git::task::read_origin(&opened.cas, &snapshot.origin_id)?;
        if manifest.content_digest() != template.source_content_digest
            || origin.repository_id() != template.repository_id
            || origin.source_revision() != Some(template.authority_commit.as_str())
            || snapshot.parent_snapshot_id.is_some()
        {
            return Err("Recorded fresh Task source differs from pinned committed source".into());
        }
    }
    Ok(())
}

/// The owner policy for performance loops. Command-only native fixtures need no Provider;
/// a model-bearing implementation/verification plan must name the complete judging team.
fn performance_roles(plan: &ExecutionPlanV1, kind: &str) -> Result<(), String> {
    performance_executions(
        plan.bindings
            .iter()
            .map(|(slot, binding)| (slot.as_str(), &binding.execution)),
        kind,
    )
}
fn performance_executions<'a>(
    bindings: impl IntoIterator<Item = (&'a str, &'a WorkerExecutionV1)>,
    kind: &str,
) -> Result<(), String> {
    let mut roles = BTreeSet::new();
    for (slot, execution) in bindings {
        let WorkerExecutionV1::Model {
            provider,
            provider_kind,
            model,
            effort,
            ..
        } = execution
        else {
            continue;
        };
        let role = slot.rsplit('.').next().ok_or("Missing performance role")?;
        let (expected_model, expected_effort) = match role {
            "implementer" => ("gpt-6.1-sol", "medium"),
            "bugs" => ("gpt-6.1-terra", "high"),
            "correctness" => ("gpt-6-sol", "high"),
            "evaluator" => ("gpt-6-astra", "medium"),
            _ => {
                return Err(format!(
                    "Performance roadmap refuses model role {role}; no advisory/default-light fallback"
                ));
            }
        };
        if provider != "codex-personal"
            || provider_kind != "codex"
            || model != expected_model
            || effort != expected_effort
        {
            return Err(format!(
                "Performance role {role} requires {expected_model} {expected_effort} on codex-personal"
            ));
        }
        roles.insert(role);
    }
    if !roles.is_empty()
        && ((kind == "verification" && roles.contains("implementer"))
            || (kind == "implement" && !roles.contains("implementer")))
    {
        return Err(
            "Performance implementation requires its implementer; verification is read-only".into(),
        );
    }
    if !roles.is_empty()
        && !["bugs", "correctness", "evaluator"]
            .iter()
            .all(|r| roles.contains(r))
    {
        return Err(
            "Performance loops require bugs, correctness and evaluator; no default-light fallback"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_experiment_evidence_follows_exact_unchanged_source_provenance_and_refuses_other_trees()
     {
        use review_source_git::task::capture_snapshot;
        use review_source_git::{Entry, EntryKind, Manifest};
        let directory = tempfile::tempdir().unwrap();
        let cas = Cas::open(directory.path()).unwrap();
        let empty = Manifest::new(vec![]).unwrap();
        let origin = cas
            .put_json(
                &json!({"schema":"af.task-source-origin/1","repository_id":"fixture",
            "source_revision":null,"content_digest":empty.content_digest()}),
            )
            .unwrap();
        let original = capture_snapshot(&cas, &empty, &origin, None).unwrap();
        let bound = cas.put_json(&json!({"schema":"af.task-source-origin/2","repository_id":"fixture",
            "content_digest":empty.content_digest(),"bound_from":{"artifact_id":format!("sha256:{}","1".repeat(64)),
                "snapshot_id":original}})).unwrap();
        let rerooted = capture_snapshot(&cas, &empty, &bound, None).unwrap();
        experiment_source_matches(&cas, &rerooted, &original).unwrap();
        let foreign_origin = cas
            .put_json(
                &json!({"schema":"af.task-source-origin/1","repository_id":"other",
            "source_revision":null,"content_digest":empty.content_digest()}),
            )
            .unwrap();
        let unrelated = capture_snapshot(&cas, &empty, &foreign_origin, None).unwrap();
        assert!(experiment_source_matches(&cas, &unrelated, &original).is_err());
        let content = cas.put(b"changed").unwrap();
        let changed = Manifest::new(vec![Entry {
            path: "changed".into(),
            kind: EntryKind::File,
            content,
            size: 7,
        }])
        .unwrap();
        let changed = capture_snapshot(&cas, &changed, &bound, Some(&rerooted)).unwrap();
        assert!(experiment_source_matches(&cas, &changed, &original).is_err());
    }
    fn team() -> BTreeMap<String, WorkerExecutionV1> {
        [
            ("implementer", "gpt-6.1-sol", "medium"),
            ("bugs", "gpt-6.1-terra", "high"),
            ("correctness", "gpt-6-sol", "high"),
            ("evaluator", "gpt-6-astra", "medium"),
        ]
        .into_iter()
        .map(|(role, model, effort)| {
            (
                format!("root.slots.{role}"),
                WorkerExecutionV1::Model {
                    provider: "codex-personal".into(),
                    provider_kind: "codex".into(),
                    principal_id: "fixture".into(),
                    model: model.into(),
                    effort: effort.into(),
                },
            )
        })
        .collect()
    }
    fn check(team: &BTreeMap<String, WorkerExecutionV1>, kind: &str) -> Result<(), String> {
        performance_executions(
            team.iter()
                .map(|(name, execution)| (name.as_str(), execution)),
            kind,
        )
    }
    #[test]
    fn performance_team_is_exact_and_verification_has_no_implementer_or_advisory() {
        let team = team();
        check(&team, "implement").unwrap();
        assert!(check(&team, "verification").is_err());
        let mut verification = team.clone();
        verification.remove("root.slots.implementer");
        check(&verification, "verification").unwrap();
        assert!(check(&verification, "implement").is_err());
        for (slot, field, value) in [
            ("bugs", "effort", "medium"),
            ("evaluator", "model", "gpt-6-sol"),
            ("correctness", "provider", "default"),
        ] {
            let mut wrong = team.clone();
            let execution = wrong.get_mut(&format!("root.slots.{slot}")).unwrap();
            let WorkerExecutionV1::Model {
                model,
                effort,
                provider,
                ..
            } = execution
            else {
                unreachable!()
            };
            match field {
                "model" => *model = value.into(),
                "effort" => *effort = value.into(),
                _ => *provider = value.into(),
            };
            assert!(check(&wrong, "implement").is_err());
        }
        let mut advisory = team.clone();
        advisory.insert("root.slots.uix".into(), team["root.slots.bugs"].clone());
        assert!(check(&advisory, "implement").is_err());
        let mut light = team;
        light.remove("root.slots.bugs");
        assert!(check(&light, "implement").is_err());
    }
    #[test]
    fn template_refuses_mutable_inputs_local_packages_fallbacks_and_missing_limits() {
        let doc = serde_json::json!({"schema":"af.task-file/1","task_id":"template","kind":"implement","goal":"Bounded loop","pipeline":{"name":"fixture/implementation","fallback":"refuse"},"strategy":"small","verification":"evaluation","limits":{"tokens":20,"max_attempts":1,"wall_ms":60000,"verification":{"tokens":5,"attempts":1,"wall_ms":10000}}});
        validate_document(&doc, None, 20).unwrap();
        let mut native_verification = doc.clone();
        native_verification["kind"] = json!("verification");
        native_verification
            .as_object_mut()
            .unwrap()
            .remove("verification");
        validate_document(&native_verification, None, 20).unwrap();
        let mut research = native_verification.clone();
        research["kind"] = json!("document");
        validate_document(&research, None, 20).unwrap();
        let mut missing_verification = doc.clone();
        missing_verification
            .as_object_mut()
            .unwrap()
            .remove("verification");
        assert!(validate_document(&missing_verification, None, 20).is_err());
        for (field, value) in [
            (
                "inputs",
                json!({"source":{"task":"other","port":"snapshot"}}),
            ),
            ("base", json!({"commit":"HEAD"})),
            (
                "pipeline",
                json!({"name":"fixture/implementation","fallback":"generate"}),
            ),
            ("limits", json!(null)),
        ] {
            let mut wrong = doc.clone();
            wrong[field] = value;
            assert!(validate_document(&wrong, None, 20).is_err(), "{field}");
        }
        assert!(validate_document(&doc,Some(&json!({"schema":"af.task-bindings/1","packages":{"local/wider":{"path":"mutable","version":"1.0.0","digest":format!("sha256:{}","a".repeat(64))}}})),20).is_err());
    }
}
