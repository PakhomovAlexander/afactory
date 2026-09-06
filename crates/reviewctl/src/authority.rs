//! Trusted campaign bootstrap and immutable Round input selection.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use review_config::Definition;
use review_config::lock::{Lockfile, Registry};
use review_core::{
    AuthorityFileV1, CampaignBudgetV1, CampaignConvergenceV1, CampaignManifestV1,
    CampaignOpenedPayloadV1, CampaignReviewerV1, ChangeSetV1, EventType,
    IntegrationCommittedPayloadV1, ReviewerPackageV1, RoundInputSupersededPayloadV1,
    RoundStartedPayloadV1, SourceSnapshot, SubjectKind, SubjectV1, run_report_closes_round,
};
use review_pipeline::RoundAuthority;
use review_runner::{MAX_CHANGE_SET_BYTES, MAX_PRIOR_FINDINGS_BYTES};
use review_source_git::{Capture, EntryKind, Manifest, Repo, Snapshot};
use review_store::{Cas, EventStore, Ingest, Ledger, LedgerProjection, NewEvent, Status};

use crate::{CampaignMode, Options, campaign_run_id};

pub(super) fn requested_git_timeout(configured: Option<Duration>) -> Duration {
    configured.unwrap_or(Duration::from_secs(
        review_source_git::DEFAULT_GIT_TIMEOUT_SECONDS,
    ))
}

/// Resolve review authority and Subject without creating Campaign state or executing external
/// work. CAS writes live only in the caller's temporary directory.
/// A pipeline read from pinned authority: bytes, parsed definition, and the loaded graph.
pub(super) struct PinnedPipeline {
    pub path: String,
    pub bytes: Vec<u8>,
    pub definition: Definition,
    pub loaded: review_config::Loaded,
}

fn load_pinned_pipeline(
    manifest: &Manifest,
    cas: &Cas,
    lockfile: &Lockfile,
    registry: &Registry,
    layout: &AuthorityLayout,
    path: &str,
) -> Result<PinnedPipeline, String> {
    let bytes = authority_bytes(manifest, cas, path)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| format!("authority pipeline `{path}` is not UTF-8: {error}"))?;
    if layout.root == ".af" {
        validate_af_pipeline_pin(lockfile, path, &bytes)?;
    }
    let definition = Definition::from_toml(text).map_err(|error| error.to_string())?;
    let loaded = definition
        .clone()
        .load_with(lockfile, registry)
        .map_err(|error| error.to_string())?;
    require_demand_set_output(&loaded, path)?;
    Ok(PinnedPipeline {
        path: path.to_string(),
        bytes,
        definition,
        loaded,
    })
}

/// The project policy under `.af/`, when that is the layout in use.
fn captured_project(
    manifest: &Manifest,
    cas: &Cas,
    layout: &AuthorityLayout,
) -> Result<Option<(Vec<u8>, crate::project::ProjectFile)>, String> {
    if layout.root != ".af" {
        return Ok(None);
    }
    let bytes = authority_bytes(manifest, cas, ".af/af.toml")?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| format!("authority project `.af/af.toml` is not UTF-8: {error}"))?;
    let project = crate::project::ProjectFile::parse(text)?;
    Ok(Some((bytes, project)))
}

/// Capture the candidate and, given a Base, the exact Change Set between them. Shared by plan
/// and Campaign open so routing sees the same paths Round 1 will review.
fn capture_candidate(
    options: &Options,
    repo: &Repo,
    cas: &Cas,
    policy: &Snapshot,
    base: Option<(&Snapshot, &str)>,
) -> Result<(Snapshot, String, Option<ChangeSetV1>), String> {
    let capture = Capture::new(repo, cas);
    let candidate_selector = if options.uncommitted {
        "worktree".to_string()
    } else {
        options.candidate.as_deref().unwrap_or("HEAD").to_string()
    };
    let mut candidate = if options.uncommitted {
        capture
            .dirty()
            .map_err(|error| format!("capturing revalidated worktree: {error}"))?
    } else {
        capture
            .committed(&candidate_selector)
            .map_err(|error| format!("capturing candidate `{candidate_selector}`: {error}"))?
    };
    if candidate.repository_id != policy.repository_id {
        return Err("candidate belongs to a different repository than policy".into());
    }
    let mut change_set = None;
    if let Some((base, base_snapshot_id)) = base {
        if candidate.submodules != base.submodules {
            return Err(
                "diff plan refuses changed gitlinks until submodule sandbox policy is explicit"
                    .into(),
            );
        }
        let base_tree = base.tree_id.as_ref().ok_or("Base has no tree authority")?;
        let diff = if candidate.dirty {
            let (head_tree, diff) = repo
                .tree_diff_synthetic_head(base_tree, &candidate.manifest, cas)
                .map_err(|error| error.to_string())?;
            candidate.tree_id = Some(head_tree);
            diff
        } else {
            repo.tree_diff(
                base_tree,
                candidate
                    .tree_id
                    .as_ref()
                    .ok_or("candidate has no tree authority")?,
            )
            .map_err(|error| error.to_string())?
        };
        let (candidate_snapshot_id, _) = publish_snapshot(&candidate, cas)?;
        change_set = Some(diff.change_set(base_snapshot_id, &candidate_snapshot_id)?);
    }
    let (candidate_snapshot_id, _) = publish_snapshot(&candidate, cas)?;
    Ok((candidate, candidate_snapshot_id, change_set))
}

/// The pipeline every static model Worker's first Attempt fits in — or the oversized strategy
/// the project declared, loaded and measured the same way. Refusal stays with the caller: plan
/// reports `fits`, run refuses before admission.
fn apply_oversized_policy(
    cas: &Cas,
    project: Option<&crate::project::ProjectFile>,
    route: &mut crate::project::RouteDecision,
    pipeline: PinnedPipeline,
    change_set: Option<&ChangeSetV1>,
    focus: Option<&str>,
    load: impl Fn(&str) -> Result<PinnedPipeline, String>,
) -> Result<(PinnedPipeline, Vec<WorkerInputSize>), String> {
    let source = change_set.map_or(ChangeSetSource::None, ChangeSetSource::Value);
    let sizes = worker_input_sizes(cas, &pipeline.definition, &pipeline.loaded, source, focus)?;
    if sizes.iter().all(|size| size.fits) || route.policy == "explicit" {
        return Ok((pipeline, sizes));
    }
    let Some(alternative) = project.and_then(|project| project.oversized_pipeline()) else {
        return Ok((pipeline, sizes));
    };
    let alternative_path = crate::project::pipeline_path_for(alternative);
    if alternative_path == pipeline.path {
        return Ok((pipeline, sizes));
    }
    let replacement = load(&alternative_path)?;
    let source = change_set.map_or(ChangeSetSource::None, ChangeSetSource::Value);
    let sizes = worker_input_sizes(
        cas,
        &replacement.definition,
        &replacement.loaded,
        source,
        focus,
    )?;
    route.replaced = Some(pipeline.path.clone());
    route.policy = "oversized";
    route.name = Some(alternative.to_string());
    route.pipeline_path = alternative_path;
    Ok((replacement, sizes))
}

pub(super) fn resolve_plan(
    options: &Options,
    cas: &Cas,
    repo: &Repo,
) -> Result<ResolvedPlan, String> {
    if options.campaign.is_some() || options.restart_round || !options.provider_resumes.is_empty() {
        return Err(
            "review plan has no Campaign state; omit --campaign, --restart-round, and --resume-provider"
                .into(),
        );
    }
    let policy_ref = options
        .policy_rev
        .as_deref()
        .or(options.authority.as_deref())
        .ok_or("review plan requires `--policy-rev REV`")?;
    let capture = Capture::new(repo, cas);
    let policy = capture
        .committed(policy_ref)
        .map_err(|error| format!("capturing policy `{policy_ref}`: {error}"))?;
    let (policy_snapshot_id, _) = publish_snapshot(&policy, cas)?;
    let requested_path = authority_path(&options.repo, &options.pipeline)?;
    let layout = authority_layout(&requested_path, false)?;
    let lock_bytes = authority_bytes(&policy.manifest, cas, &layout.lock)?;
    let lock_text = std::str::from_utf8(&lock_bytes)
        .map_err(|error| format!("authority lock `{}` is not UTF-8: {error}", layout.lock))?;
    let lockfile = Lockfile::from_toml(lock_text).map_err(|error| error.to_string())?;
    if let Some(note) = crate::project::check_lock_af_version(&lockfile, &layout.lock)? {
        eprintln!("af review: note: {note}");
    }
    let project = captured_project(&policy.manifest, cas, &layout)?;
    let registry = Registry::captured(captured_registry(&policy.manifest, cas, &layout.registry)?);

    // Base and candidate before the pipeline: routing decides the pipeline from the changed
    // paths, and sizing needs the exact Change Set.
    let base_ref = options.base.as_deref().or(options.authority.as_deref());
    let (base, base_snapshot_id) = match base_ref {
        Some(base_ref) => {
            let base = if base_ref == policy_ref {
                policy.clone()
            } else {
                capture
                    .committed(base_ref)
                    .map_err(|error| format!("capturing Base `{base_ref}`: {error}"))?
            };
            if base.repository_id != policy.repository_id {
                return Err("Campaign Base belongs to a different repository than policy".into());
            }
            let (base_snapshot_id, _) = publish_snapshot(&base, cas)?;
            (Some(base), Some(base_snapshot_id))
        }
        None => (None, None),
    };
    let (candidate, candidate_snapshot_id, change_set) = capture_candidate(
        options,
        repo,
        cas,
        &policy,
        base.as_ref().zip(base_snapshot_id.as_deref()),
    )?;
    let changed_paths: Vec<String> = change_set
        .as_ref()
        .map(|change_set| change_set.changed_paths.clone())
        .unwrap_or_default();
    let mut route = match (&project, options.pipeline_explicit) {
        (Some((_, project)), false) => project.select_route(&changed_paths)?,
        (Some(_), true) => crate::project::RouteDecision::explicit(&requested_path),
        (None, _) => crate::project::RouteDecision::legacy(&requested_path),
    };
    let load = |path: &str| {
        load_pinned_pipeline(&policy.manifest, cas, &lockfile, &registry, &layout, path)
    };
    let pipeline = load(&route.pipeline_path)?;
    if let Some((project_bytes, _)) = &project {
        validate_af_project(project_bytes, &pipeline.path, &lockfile)?;
    }
    match (pipeline.loaded.subject_kind(), base.is_some()) {
        (SubjectKind::Diff, false) => return Err("diff review plan requires `--base REV`".into()),
        (SubjectKind::WholeTree, _) if options.base.is_some() => {
            return Err("whole-tree review plan does not accept `--base`".into());
        }
        _ => {}
    }
    let (base, base_snapshot_id, change_set) =
        if pipeline.loaded.subject_kind() == SubjectKind::WholeTree {
            (None, None, None)
        } else {
            (base, base_snapshot_id, change_set)
        };
    let (pipeline, input_sizes) = apply_oversized_policy(
        cas,
        project.as_ref().map(|(_, project)| project),
        &mut route,
        pipeline,
        change_set.as_ref(),
        options.focus.as_deref(),
        load,
    )?;
    let candidate_selector = if options.uncommitted {
        "worktree".to_string()
    } else {
        options.candidate.as_deref().unwrap_or("HEAD").to_string()
    };
    let topology = serde_json::json!({
        "nodes": &pipeline.definition.nodes,
        "edges": &pipeline.definition.edges,
    });
    let selectors = serde_json::json!({
        "compatibility_authority": options.authority,
        "policy_rev": policy_ref,
        "base": base_ref,
        "candidate": candidate_selector,
        "uncommitted": options.uncommitted,
    });
    let resolved = serde_json::json!({
        "policy_revision": policy.source_revision,
        "policy_snapshot_id": policy_snapshot_id,
        "base_revision": base.as_ref().and_then(|snapshot| snapshot.source_revision.clone()),
        "base_snapshot_id": base_snapshot_id,
        "candidate_revision": candidate.source_revision,
        "candidate_snapshot_id": candidate_snapshot_id,
    });
    let subject = serde_json::json!({
        "kind": match pipeline.loaded.subject_kind() {
            SubjectKind::Diff => "diff",
            SubjectKind::WholeTree => "whole-tree",
        },
        "empty": change_set.as_ref().is_some_and(|change_set| change_set.changed_paths.is_empty()),
        "changed_paths": change_set.as_ref().map(|change_set| change_set.changed_paths.clone()).unwrap_or_default(),
        "renames": change_set.as_ref().map(|change_set| &change_set.renames).cloned().unwrap_or_default(),
        "patch_bytes": change_set.as_ref().map(|change_set| change_set.canonical_patch().map(|patch| patch.len())).transpose()?.unwrap_or(0),
    });
    Ok(ResolvedPlan {
        selectors,
        resolved,
        subject,
        pipeline_path: pipeline.path,
        topology,
        definition: pipeline.definition,
        loaded: pipeline.loaded,
        change_set,
        route,
        input_sizes,
    })
}

/// Everything `af review plan` resolves before projecting it to JSON — what `render` reuses, so
/// the two can never disagree about policy, Subject, or pipeline.
pub(super) struct ResolvedPlan {
    pub selectors: serde_json::Value,
    pub resolved: serde_json::Value,
    pub subject: serde_json::Value,
    pub pipeline_path: String,
    pub topology: serde_json::Value,
    pub definition: Definition,
    pub loaded: review_config::Loaded,
    pub change_set: Option<review_core::ChangeSetV1>,
    pub route: crate::project::RouteDecision,
    pub input_sizes: Vec<WorkerInputSize>,
}

fn token_free_effects() -> serde_json::Value {
    serde_json::json!({
        "campaign_state": false,
        "gates": false,
        "provider_admission": false,
        "worker_dispatch": false,
        "token_spend": false,
    })
}

/// Resolve review authority and Subject without creating Campaign state or executing external
/// work, and project the result as `af/review-plan@1`.
pub(super) fn plan(options: &Options, cas: &Cas, repo: &Repo) -> Result<serde_json::Value, String> {
    let plan = resolve_plan(options, cas, repo)?;
    let loaded = &plan.loaded;
    let mut providers = Vec::new();
    for (node, package) in loaded.packages() {
        let runner = Path::new(&package.runner.program)
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        providers.push(serde_json::json!({
            "node": node,
            "runner": runner,
            "required": matches!(runner.as_str(), "claude" | "codex"),
            "binding": options.provider_bindings.get(node),
            "ready": !matches!(runner.as_str(), "claude" | "codex") || options.provider_bindings.contains_key(node),
        }));
    }
    let checks = loaded
        .checks()
        .iter()
        .map(|check| {
            serde_json::json!({
                "name": check.name,
                "required": check.required,
                "command": check.command,
            })
        })
        .collect::<Vec<_>>();
    let convergence = selected_convergence(options.mode, loaded.convergence());
    let input_sizes = &plan.input_sizes;
    let reservations = crate::project::static_reservations(loaded)
        .into_iter()
        .map(|reservation| {
            let mut value = serde_json::to_value(&reservation).expect("a reservation serializes");
            if let Some(size) = input_sizes
                .iter()
                .find(|size| size.node == reservation.node)
            {
                value["input_bytes"] = serde_json::json!(size.input_bytes);
                value["input_tokens"] = serde_json::json!(size.input_tokens);
                value["fits"] = serde_json::json!(size.fits);
            }
            value
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({
        "schema": "af/review-plan@1",
        "token_free": true,
        "selectors": plan.selectors,
        "resolved": plan.resolved,
        "subject": plan.subject,
        "route": plan.route,
        "pipeline": {
            "path": plan.pipeline_path,
            "topology": plan.topology,
            "gates": checks,
            "budgets": loaded.budgets(),
            "reservations": reservations,
            "max_simultaneous_reservation": crate::project::max_simultaneous_reservation(loaded)?,
            "inputs_fit": input_sizes.iter().all(|size| size.fits),
            "convergence": {
                "mode": options.mode.as_str(),
                "clean_rounds": convergence.clean_rounds,
                "max_rounds": convergence.max_rounds,
                "gate": format!("{:?}", convergence.gate).to_lowercase(),
            },
        },
        "providers": providers,
        "external_effects": token_free_effects(),
    }))
}

/// Where a first Attempt's Change Set comes from: the plan's freshly computed value, a Round's
/// validated one, or nothing for a whole-tree Subject.
pub(super) enum ChangeSetSource<'a> {
    Value(&'a review_core::ChangeSetV1),
    Resolved(&'a std::sync::Arc<review_store::ResolvedChangeSet>),
    None,
}

/// One Worker's first-Attempt input, composed by the same function its adapter calls at
/// dispatch, plus what a real Attempt adds that cannot be known before a Campaign exists.
pub(super) struct FirstAttemptInput {
    pub rendered: review_runner::RenderedInput,
    pub runner: String,
    pub not_rendered: Vec<String>,
}

/// Compose `node`'s first-Attempt input token-free. No Campaign state, Gate, Provider, Worker,
/// or spend: the adapter is built from the digest-pinned package and only `render_input` is
/// called on it.
pub(super) fn first_attempt_input(
    cas: &Cas,
    definition: &Definition,
    loaded: &review_config::Loaded,
    node: &str,
    change_set: ChangeSetSource<'_>,
    focus: Option<&str>,
) -> Result<FirstAttemptInput, String> {
    let spec = definition
        .nodes
        .iter()
        .find(|spec| spec.id == node)
        .ok_or_else(|| format!("pipeline has no node `{node}`"))?;
    if !matches!(spec.kind, review_config::NodeKindSpec::Reviewer) {
        return Err(format!(
            "node `{node}` is not a reviewer Worker; only Workers receive an input"
        ));
    }
    let command = loaded
        .reviewers()
        .get(node)
        .ok_or_else(|| format!("node `{node}` has no runner"))?;
    let result_contract = match spec.outputs.as_slice() {
        [review_config::PortContractSpec::Typed(port)] => {
            review_core::ReviewerResultContract::parse_artifact_type(&port.artifact_type)
                .ok_or_else(|| {
                    format!(
                        "node `{node}` output `{}` has unsupported result type `{}`",
                        port.name, port.artifact_type
                    )
                })?
        }
        [review_config::PortContractSpec::Name(_)] => review_core::ReviewerResultContract::V1,
        _ => {
            return Err(format!(
                "reviewer `{node}` must declare exactly one result output"
            ));
        }
    };
    let mut inputs = review_runner::ReviewerInputs {
        result_contract,
        finding_identity_policy: Some(review_core::CANONICAL_FINDING_IDENTITY_POLICY.to_string()),
        ..Default::default()
    };
    let mut not_rendered = Vec::new();
    for port in &spec.inputs {
        let (name, artifact_type) = match port {
            review_config::PortContractSpec::Typed(typed) => {
                (typed.name.as_str(), typed.artifact_type.as_str())
            }
            review_config::PortContractSpec::Name(name) => {
                (name.as_str(), review_core::contract::OPAQUE_V1)
            }
        };
        let is_change_set = artifact_type == review_core::contract::CHANGE_SET_V1
            || (definition.version == 1
                && artifact_type == review_core::contract::OPAQUE_V1
                && name == "change_set");
        if is_change_set {
            let artifact = match &change_set {
                ChangeSetSource::Resolved(resolved) => {
                    review_runner::ReviewerInputArtifact::from_resolved_change_set(
                        std::sync::Arc::clone(resolved),
                    )?
                }
                ChangeSetSource::Value(change_set) => {
                    // Published exactly as a run publishes it, so the artifact ID in the
                    // rendered metadata is the one a Campaign on this Subject would carry.
                    let value =
                        serde_json::to_value(change_set).map_err(|error| error.to_string())?;
                    review_core::json::admit(&value).map_err(|error| error.to_string())?;
                    let encoded = review_store::canonical::canonicalize(&value)
                        .map_err(|error| error.to_string())?;
                    let artifact_id = cas.put(&encoded).map_err(|error| error.to_string())?;
                    review_runner::ReviewerInputArtifact::change_set_from_encoded(
                        artifact_id,
                        &encoded,
                    )?
                }
                ChangeSetSource::None => {
                    not_rendered.push(format!("{name}: a whole-tree Subject has no Change Set"));
                    continue;
                }
            };
            inputs.artifacts.insert(name.to_string(), vec![artifact]);
        } else if name == "prior_findings"
            || artifact_type == review_core::contract::FINDING_SET_V1
            || artifact_type.contains("PriorFindings")
        {
            not_rendered.push(format!(
                "{name}: prior Findings exist only inside a Campaign; a first Attempt receives none"
            ));
        } else {
            not_rendered.push(format!("{name}: {artifact_type} is produced at run time"));
        }
    }
    not_rendered.push(
        "attempt authority: Campaign identifiers are bound at dispatch (a model Worker's `## Attempt authority` section, a command Worker's `attempt_context`)"
            .to_string(),
    );
    let timeout = std::time::Duration::from_secs(1);
    let runner = Path::new(&command.program)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let adapter: Box<dyn review_runner::ReviewerAdapter> = match loaded.packages().get(node) {
        Some(package) => match runner.as_str() {
            "claude" => {
                let mut adapter =
                    review_runner_claude::ClaudeAdapter::from_package(package, timeout)
                        .map_err(|error| format!("{node}: {error}"))?;
                if let Some(focus) = focus {
                    adapter = adapter.with_focus(focus);
                }
                Box::new(adapter)
            }
            "codex" => {
                let mut adapter = review_runner_codex::CodexAdapter::from_package(package, timeout)
                    .map_err(|error| format!("{node}: {error}"))?;
                if let Some(focus) = focus {
                    adapter = adapter.with_focus(focus);
                }
                Box::new(adapter)
            }
            other => {
                return Err(format!(
                    "node `{node}`: no adapter drives `{other}`; this af release knows claude and codex"
                ));
            }
        },
        None => Box::new(review_runner::CommandAdapter::new(command.clone(), timeout)),
    };
    let rendered = adapter
        .render_input(&inputs)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("node `{node}`: this adapter has no fixed input encoding"))?;
    Ok(FirstAttemptInput {
        rendered,
        runner,
        not_rendered,
    })
}

/// A model Worker's first-Attempt input measured against the cap its dispatch reserves.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct WorkerInputSize {
    pub node: String,
    pub input_bytes: usize,
    pub input_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cap_tokens: Option<u64>,
    /// `false` when the input alone leaves no room in the Attempt cap for any output.
    pub fits: bool,
}

/// Measure every packaged model Worker's first-Attempt input against its cap. Command Workers
/// spend no tokens and are not measured.
pub(super) fn worker_input_sizes(
    cas: &Cas,
    definition: &Definition,
    loaded: &review_config::Loaded,
    change_set: ChangeSetSource<'_>,
    focus: Option<&str>,
) -> Result<Vec<WorkerInputSize>, String> {
    let mut sizes = Vec::new();
    for node in loaded.packages().keys() {
        // A Scatter's Workers are sized per shard at run time, and a closeout reviewer sees the
        // whole Subject by design; neither has a first-Attempt input to measure here.
        let is_static_reviewer = definition.nodes.iter().any(|spec| {
            spec.id == *node && matches!(spec.kind, review_config::NodeKindSpec::Reviewer)
        });
        if !is_static_reviewer || loaded.closeouts().contains_key(node) {
            continue;
        }
        let change_set = match &change_set {
            ChangeSetSource::Value(value) => ChangeSetSource::Value(value),
            ChangeSetSource::Resolved(resolved) => ChangeSetSource::Resolved(resolved),
            ChangeSetSource::None => ChangeSetSource::None,
        };
        let input = first_attempt_input(cas, definition, loaded, node, change_set, focus)?;
        let cap_tokens = loaded.attempt_cap_for(node);
        let input_tokens = input.rendered.manifest.estimated_tokens;
        sizes.push(WorkerInputSize {
            node: node.clone(),
            input_bytes: input.rendered.bytes.len(),
            input_tokens,
            cap_tokens,
            fits: cap_tokens.is_none_or(|cap| input_tokens < cap),
        });
    }
    Ok(sizes)
}

/// Refuse before any Gate, Provider admission, or Worker dispatch when a Worker's input alone
/// exhausts its Attempt cap: the Attempt could only fail after paying for the whole input.
/// Never truncates; names the bounded alternatives instead.
pub(super) fn refuse_unfit_inputs(sizes: &[WorkerInputSize]) -> Result<(), String> {
    let unfit: Vec<String> = sizes
        .iter()
        .filter(|size| !size.fits)
        .map(|size| {
            format!(
                "`{}` receives {} bytes (about {} tokens) against an Attempt cap of {} tokens",
                size.node,
                size.input_bytes,
                size.input_tokens,
                size.cap_tokens.unwrap_or(0)
            )
        })
        .collect();
    if unfit.is_empty() {
        return Ok(());
    }
    Err(format!(
        "Worker input alone exhausts its Attempt cap, so no output could ever be paid for: {}. Nothing was dispatched or charged. Narrow the Subject (a smaller Base-to-candidate range), raise that Worker's `budget.attempt`, or route the Diff through a Scatter node (pipeline v5) so each shard fits",
        unfit.join("; ")
    ))
}

/// The exact input one Worker would receive on a first Attempt, composed by the same function
/// its adapter calls at dispatch. Token-free: no Campaign state, Gate, Provider, or spend.
#[derive(serde::Serialize)]
pub(super) struct RenderView {
    pub(super) schema: &'static str,
    pub(super) token_free: bool,
    pub(super) node: String,
    pub(super) runner: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) package: Option<serde_json::Value>,
    pub(super) transport: review_runner::InputTransport,
    pub(super) bytes: usize,
    pub(super) estimated_tokens: u64,
    /// The Attempt cap this Worker's dispatch reserves, when the pipeline is capped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) cap_tokens: Option<u64>,
    /// Whether the input leaves room in that cap for any output; absent when uncapped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) fits: Option<bool>,
    pub(super) manifest: review_runner::ContextManifest,
    /// Campaign-bound inputs a real Attempt also receives; listed, never invented.
    pub(super) not_rendered: Vec<String>,
    pub(super) input_is_utf8: bool,
    pub(super) input: String,
    pub(super) external_effects: serde_json::Value,
    #[serde(skip)]
    pub(super) raw: Vec<u8>,
}

pub(super) fn render(options: &Options, cas: &Cas, repo: &Repo) -> Result<RenderView, String> {
    let node = options
        .node
        .clone()
        .ok_or("review render requires `--node NODE`")?;
    let plan = resolve_plan(options, cas, repo)?;
    let change_set = plan
        .change_set
        .as_ref()
        .map_or(ChangeSetSource::None, ChangeSetSource::Value);
    let input = first_attempt_input(
        cas,
        &plan.definition,
        &plan.loaded,
        &node,
        change_set,
        options.focus.as_deref(),
    )
    .map_err(|error| {
        if error.starts_with("pipeline has no node") {
            format!("pipeline `{}` has no node `{node}`", plan.pipeline_path)
        } else {
            error
        }
    })?;
    let package = plan.loaded.packages().get(&node).map(|package| {
        serde_json::json!({
            "name": package.name,
            "version": package.version,
            "digest": package.digest,
        })
    });
    let cap_tokens = package.as_ref().and(plan.loaded.attempt_cap_for(&node));
    let estimated_tokens = input.rendered.manifest.estimated_tokens;
    let (text, input_is_utf8) = match String::from_utf8(input.rendered.bytes.clone()) {
        Ok(text) => (text, true),
        Err(_) => (
            String::from_utf8_lossy(&input.rendered.bytes).into_owned(),
            false,
        ),
    };
    Ok(RenderView {
        schema: "af/review-render@1",
        token_free: true,
        runner: input.runner,
        package,
        transport: input.rendered.transport,
        bytes: input.rendered.bytes.len(),
        estimated_tokens,
        cap_tokens,
        fits: cap_tokens.map(|cap| estimated_tokens < cap),
        manifest: input.rendered.manifest,
        not_rendered: input.not_rendered,
        input_is_utf8,
        input: text,
        external_effects: token_free_effects(),
        raw: input.rendered.bytes,
        node,
    })
}

pub(super) struct PreparedRun {
    pub loaded: review_config::Loaded,
    pub snapshot: Manifest,
    pub run_id: String,
    pub focus: Option<String>,
    pub timeout: Duration,
    pub check_timeout: Duration,
    pub git_timeout: Duration,
    pub convergence: review_store::ConvergencePolicy,
    pub authority: RoundAuthority,
    pub ledger_projection: LedgerProjection,
}

struct OpenCampaign {
    loaded: review_config::Loaded,
    manifest: CampaignManifestV1,
    manifest_id: String,
    opened_event_id: String,
}

struct RoundInput {
    payload: RoundStartedPayloadV1,
    event_id: String,
    snapshot: Manifest,
    prior_count: usize,
    ledger_projection: LedgerProjection,
}

pub(super) fn prepare(
    options: &Options,
    cas: &Cas,
    store: &mut EventStore,
    repo: &Repo,
) -> Result<PreparedRun, String> {
    let run_id = options
        .campaign
        .as_deref()
        .map(campaign_run_id)
        .unwrap_or_else(|| campaign_run_id("local"));
    let pipeline_path = authority_path(&options.repo, &options.pipeline)?;
    let events = store.replay(&run_id).map_err(|error| error.to_string())?;
    let (campaign, events) = if events.is_empty() {
        let campaign = open_new(options, cas, store, repo, &run_id, &pipeline_path)?;
        let events = store.replay(&run_id).map_err(|error| error.to_string())?;
        (campaign, events)
    } else {
        let campaign = resume(options, cas, &events, &pipeline_path)?;
        (campaign, events)
    };

    let round = prepare_round(options, cas, store, repo, &run_id, &campaign, &events)?;
    let authority = RoundAuthority::load(store, cas, &run_id, &round.event_id)?;
    let check_timeout_seconds = campaign
        .manifest
        .check_timeout_seconds
        .unwrap_or(campaign.loaded.check_timeout_seconds());
    let git_timeout_seconds = campaign
        .manifest
        .git_timeout_seconds
        .unwrap_or(review_source_git::DEFAULT_GIT_TIMEOUT_SECONDS);
    let convergence = selected_convergence(options.mode, campaign.loaded.convergence());
    Ok(PreparedRun {
        loaded: campaign.loaded,
        snapshot: round.snapshot,
        run_id,
        focus: campaign.manifest.focus,
        timeout: Duration::from_secs(campaign.manifest.reviewer_timeout_seconds),
        check_timeout: Duration::from_secs(check_timeout_seconds),
        git_timeout: Duration::from_secs(git_timeout_seconds),
        convergence,
        authority,
        ledger_projection: round.ledger_projection,
    })
}

fn open_new(
    options: &Options,
    cas: &Cas,
    store: &mut EventStore,
    repo: &Repo,
    run_id: &str,
    pipeline_path: &str,
) -> Result<OpenCampaign, String> {
    let authority_ref = options
        .policy_rev
        .as_deref()
        .or(options.authority.as_deref())
        .ok_or(
            "a new Campaign requires trusted invocation policy `--policy-rev REV`; \
             compatibility `--authority REV` expands to policy and Base; continuation reuses \
             stored authority and does not resolve the ref again",
        )?;
    let snapshot = Capture::new(repo, cas)
        .committed(authority_ref)
        .map_err(|error| format!("capturing authority `{authority_ref}`: {error}"))?;
    let (authority_snapshot_id, authority_manifest_id) = publish_snapshot(&snapshot, cas)?;

    let layout = authority_layout(pipeline_path, false)?;
    let lock_path = layout.lock.clone();
    let lock_bytes = authority_bytes(&snapshot.manifest, cas, &lock_path)?;
    let lock_text = std::str::from_utf8(&lock_bytes)
        .map_err(|error| format!("authority lock `{lock_path}` is not UTF-8: {error}"))?;
    let lockfile = Lockfile::from_toml(lock_text).map_err(|error| error.to_string())?;
    if let Some(note) = crate::project::check_lock_af_version(&lockfile, &lock_path)? {
        eprintln!("af review: note: {note}");
    }
    let project = captured_project(&snapshot.manifest, cas, &layout)?;
    let registry = Registry::captured(captured_registry(
        &snapshot.manifest,
        cas,
        &layout.registry,
    )?);

    // Base before the pipeline: routing decides the pipeline from the changed paths, and the
    // oversized policy needs the exact Change Set, before anything is pinned.
    let base_ref = options.base.as_deref().or(options.authority.as_deref());
    let base = match base_ref {
        Some(base_ref) if base_ref == authority_ref => Some(snapshot.clone()),
        Some(base_ref) => {
            let base = Capture::new(repo, cas)
                .committed(base_ref)
                .map_err(|error| format!("capturing Base `{base_ref}`: {error}"))?;
            if base.repository_id != snapshot.repository_id {
                return Err("Campaign Base belongs to a different repository than policy".into());
            }
            Some(base)
        }
        None => None,
    };
    let (base_snapshot_id, base_manifest_id) = match &base {
        Some(base) => {
            let (snapshot_id, manifest_id) = publish_snapshot(base, cas)?;
            (Some(snapshot_id), Some(manifest_id))
        }
        None => (None, None),
    };
    let routing_applies = !options.pipeline_explicit
        && project
            .as_ref()
            .is_some_and(|(_, project)| project.routes_configured());
    let change_set = match (routing_applies, &base, base_snapshot_id.as_deref()) {
        (true, Some(base), Some(base_snapshot_id)) => {
            capture_candidate(
                options,
                repo,
                cas,
                &snapshot,
                Some((base, base_snapshot_id)),
            )?
            .2
        }
        _ => None,
    };
    let changed_paths: Vec<String> = change_set
        .as_ref()
        .map(|change_set| change_set.changed_paths.clone())
        .unwrap_or_default();
    let mut route = match (&project, options.pipeline_explicit) {
        (Some((_, project)), false) => project.select_route(&changed_paths)?,
        (Some(_), true) => crate::project::RouteDecision::explicit(pipeline_path),
        (None, _) => crate::project::RouteDecision::legacy(pipeline_path),
    };
    let load = |path: &str| {
        load_pinned_pipeline(&snapshot.manifest, cas, &lockfile, &registry, &layout, path)
    };
    let pipeline = load(&route.pipeline_path)?;
    match (pipeline.loaded.subject_kind(), base.is_some()) {
        (SubjectKind::Diff, false) => {
            return Err("a new diff Campaign requires `--base REV`".into());
        }
        (SubjectKind::WholeTree, _) if options.base.is_some() => {
            return Err(
                "whole-tree review does not accept a Base; select a whole-tree pipeline with --policy-rev only"
                    .into(),
            );
        }
        _ => {}
    }
    let (pipeline, _input_sizes) = if routing_applies {
        apply_oversized_policy(
            cas,
            project.as_ref().map(|(_, project)| project),
            &mut route,
            pipeline,
            change_set.as_ref(),
            options.focus.as_deref(),
            load,
        )?
    } else {
        (pipeline, Vec::new())
    };
    let (base_snapshot_id, base_manifest_id) =
        if pipeline.loaded.subject_kind() == SubjectKind::Diff {
            (base_snapshot_id, base_manifest_id)
        } else {
            (None, None)
        };
    super::run_progress(
        options,
        format_args!(
            "route    {} => {}{}{}",
            route.policy,
            route.pipeline_path,
            route
                .name
                .as_deref()
                .map(|name| format!(" ({name})"))
                .unwrap_or_default(),
            if route.changed_paths > 0 {
                format!("; {} changed path(s)", route.changed_paths)
            } else {
                String::new()
            }
        ),
    );
    let selected_pipeline_path = pipeline.path.clone();
    let pipeline_path: &str = selected_pipeline_path.as_str();
    let pipeline_bytes = pipeline.bytes;
    let loaded = pipeline.loaded;
    super::run_progress(
        options,
        format_args!(
            "selectors policy={} base={} candidate={}",
            authority_ref,
            options
                .base
                .as_deref()
                .or(options.authority.as_deref())
                .unwrap_or("-"),
            if options.uncommitted {
                "worktree"
            } else {
                options.candidate.as_deref().unwrap_or("HEAD")
            }
        ),
    );

    let pipeline_artifact_id = cas
        .put(&pipeline_bytes)
        .map_err(|error| error.to_string())?;
    let lock_artifact_id = cas.put(&lock_bytes).map_err(|error| error.to_string())?;
    let project_policy_ids = match &project {
        Some((project_bytes, _)) => {
            validate_af_project(project_bytes, pipeline_path, &lockfile)?;
            vec![cas.put(project_bytes).map_err(|error| error.to_string())?]
        }
        None => Vec::new(),
    };
    let finding_genesis_id = cas
        .put_json(&serde_json::json!({
            "kind": "finding-set-genesis@1",
            "authority_snapshot_id": authority_snapshot_id,
            "findings": [],
        }))
        .map_err(|error| error.to_string())?;
    let demand_genesis_id = cas
        .put_json(&serde_json::json!({
            "kind": "demand-set-genesis@1",
            "authority_snapshot_id": authority_snapshot_id,
            "demands": [],
        }))
        .map_err(|error| error.to_string())?;

    let mut package_artifacts: BTreeMap<(String, String), (String, Vec<String>)> = BTreeMap::new();
    let mut reviewers = Vec::new();
    for (node, package) in loaded.packages() {
        let key = (package.name.clone(), package.digest.clone());
        let (package_artifact_id, _) = match package_artifacts.get(&key) {
            Some(existing) => existing.clone(),
            None => {
                let mut files = BTreeMap::new();
                let mut file_ids = Vec::new();
                for (path, bytes) in package.files() {
                    let artifact_id = cas.put(bytes).map_err(|error| error.to_string())?;
                    files.insert(path.clone(), artifact_id.clone());
                    file_ids.push(artifact_id);
                }
                let artifact = ReviewerPackageV1 {
                    name: package.name.clone(),
                    version: package.version.clone(),
                    digest: package.digest.clone(),
                    files,
                };
                artifact.validate()?;
                let artifact_id = cas
                    .put_json(&serde_json::to_value(&artifact).map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())?;
                package_artifacts.insert(key.clone(), (artifact_id.clone(), file_ids.clone()));
                (artifact_id, file_ids)
            }
        };
        reviewers.push(CampaignReviewerV1 {
            node: node.clone(),
            name: package.name.clone(),
            version: package.version.clone(),
            digest: package.digest.clone(),
            package_artifact_id,
        });
    }

    let mut execution_policy_ids = BTreeSet::from([pipeline_artifact_id.clone()]);
    execution_policy_ids.extend(
        reviewers
            .iter()
            .map(|reviewer| reviewer.package_artifact_id.clone()),
    );
    let convergence = selected_convergence(options.mode, loaded.convergence());
    let budgets = loaded.budgets().map(|budget| CampaignBudgetV1 {
        attempt_tokens: budget.attempt,
        run_tokens: budget.run,
    });
    let manifest = CampaignManifestV1 {
        authority_snapshot_id: authority_snapshot_id.clone(),
        subject_kind: loaded.subject_kind(),
        base_snapshot_id,
        pipeline: AuthorityFileV1 {
            path: pipeline_path.to_string(),
            artifact_id: pipeline_artifact_id.clone(),
        },
        reviewer_lock: AuthorityFileV1 {
            path: lock_path,
            artifact_id: lock_artifact_id.clone(),
        },
        reviewers,
        execution_policy_ids: execution_policy_ids.into_iter().collect(),
        project_policy_ids: project_policy_ids.clone(),
        convergence: CampaignConvergenceV1 {
            clean_rounds: convergence.clean_rounds,
            max_rounds: convergence.max_rounds,
            gate: format!("{:?}", convergence.gate).to_lowercase(),
        },
        reviewer_timeout_seconds: options
            .timeout
            .unwrap_or(Duration::from_secs(1800))
            .as_secs(),
        check_timeout_seconds: Some(loaded.check_timeout_seconds()),
        git_timeout_seconds: Some(requested_git_timeout(options.git_timeout).as_secs()),
        budgets,
        focus: options.focus.clone(),
        finding_identity_policy: review_core::CANONICAL_FINDING_IDENTITY_POLICY.to_string(),
        finding_genesis_id,
        demand_genesis_id,
    };
    manifest.validate()?;
    let manifest_id = cas
        .put_json(&serde_json::to_value(&manifest).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    let mut refs = vec![
        authority_snapshot_id.clone(),
        authority_manifest_id,
        manifest_id.clone(),
        pipeline_artifact_id,
        lock_artifact_id,
        manifest.finding_genesis_id.clone(),
        manifest.demand_genesis_id.clone(),
    ];
    refs.extend(base_manifest_id);
    refs.extend(manifest.base_snapshot_id.clone());
    refs.extend(project_policy_ids);
    for (package_id, file_ids) in package_artifacts.values() {
        refs.push(package_id.clone());
        refs.extend(file_ids.iter().cloned());
    }
    refs.sort();
    refs.dedup();
    let payload = CampaignOpenedPayloadV1 {
        campaign_manifest_id: manifest_id.clone(),
        authority_snapshot_id: authority_snapshot_id.clone(),
    };
    let opened = store
        .append(
            run_id,
            cas,
            NewEvent::new(
                EventType::CampaignOpenedV1,
                serde_json::to_value(payload).map_err(|error| error.to_string())?,
            )
            .correlating(manifest_id.clone())
            .referencing(refs),
        )
        .map_err(|error| error.to_string())?;
    super::run_progress(options, format_args!("authority {authority_snapshot_id}"));
    super::run_progress(options, format_args!("manifest  {manifest_id}"));
    Ok(OpenCampaign {
        loaded,
        manifest,
        manifest_id,
        opened_event_id: opened.event_id,
    })
}

fn resume(
    options: &Options,
    cas: &Cas,
    events: &[review_core::RunEvent],
    pipeline_path: &str,
) -> Result<OpenCampaign, String> {
    let mut opened = events
        .iter()
        .filter(|event| event.event_type == EventType::CampaignOpenedV1);
    let event = opened.next().ok_or(
        "campaign state predates CampaignOpened@1; start a new Campaign rather than inventing authority",
    )?;
    if opened.next().is_some() {
        return Err("campaign contains more than one CampaignOpened@1 event".into());
    }
    let payload: CampaignOpenedPayloadV1 =
        serde_json::from_value(event.payload.clone()).map_err(|error| error.to_string())?;
    payload.validate()?;
    let manifest: CampaignManifestV1 = serde_json::from_value(
        cas.get_json(&payload.campaign_manifest_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    manifest.validate()?;
    if manifest.authority_snapshot_id != payload.authority_snapshot_id {
        return Err("CampaignOpened@1 disagrees with its CampaignManifest authority".into());
    }
    // A routed Campaign pinned the pipeline routing chose; only an explicit, different request
    // is a conflict.
    if manifest.pipeline.path != pipeline_path && options.pipeline_explicit {
        return Err(format!(
            "campaign is pinned to pipeline `{}`; `{pipeline_path}` requires a new Campaign",
            manifest.pipeline.path
        ));
    }
    if options
        .focus
        .as_ref()
        .is_some_and(|focus| Some(focus) != manifest.focus.as_ref())
    {
        return Err("invocation focus differs from the pinned Campaign manifest".into());
    }
    if options
        .timeout
        .is_some_and(|timeout| timeout.as_secs() != manifest.reviewer_timeout_seconds)
    {
        return Err("reviewer timeout differs from the pinned Campaign manifest".into());
    }
    let pinned_git_timeout = manifest
        .git_timeout_seconds
        .unwrap_or(review_source_git::DEFAULT_GIT_TIMEOUT_SECONDS);
    if requested_git_timeout(options.git_timeout).as_secs() != pinned_git_timeout {
        return Err("Git capture timeout differs from the pinned Campaign manifest".into());
    }

    let pipeline = cas
        .get(&manifest.pipeline.artifact_id)
        .map_err(|error| error.to_string())?;
    let lock = cas
        .get(&manifest.reviewer_lock.artifact_id)
        .map_err(|error| error.to_string())?;
    let pipeline = std::str::from_utf8(&pipeline).map_err(|error| error.to_string())?;
    let lockfile =
        Lockfile::from_toml(std::str::from_utf8(&lock).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let mut packages: BTreeMap<String, BTreeMap<String, Vec<u8>>> = BTreeMap::new();
    let mut captured: BTreeMap<String, (ReviewerPackageV1, BTreeMap<String, Vec<u8>>)> =
        BTreeMap::new();
    for binding in &manifest.reviewers {
        if !captured.contains_key(&binding.package_artifact_id) {
            let package: ReviewerPackageV1 = serde_json::from_value(
                cas.get_json(&binding.package_artifact_id)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            package.validate()?;
            let mut files = BTreeMap::new();
            for (path, artifact_id) in &package.files {
                files.insert(
                    path.clone(),
                    cas.get(artifact_id).map_err(|error| error.to_string())?,
                );
            }
            let recomputed = review_config::lock::package_digest_from_files(&files);
            if recomputed != package.digest {
                return Err(format!(
                    "captured reviewer package `{}` claims digest {} but contains {recomputed}",
                    package.name, package.digest
                ));
            }
            captured.insert(binding.package_artifact_id.clone(), (package, files));
        }
        let (package, files) = captured
            .get(&binding.package_artifact_id)
            .expect("captured package inserted");
        if package.name != binding.name
            || package.version != binding.version
            || package.digest != binding.digest
        {
            return Err(format!(
                "captured reviewer package for node `{}` disagrees with CampaignManifest@1",
                binding.node
            ));
        }
        if packages
            .insert(package.name.clone(), files.clone())
            .is_some_and(|prior| &prior != files)
        {
            return Err(format!(
                "CampaignManifest@1 binds package `{}` to inconsistent bytes",
                package.name
            ));
        }
    }
    let registry = Registry::captured(packages);
    let loaded = Definition::from_toml(pipeline)
        .map_err(|error| error.to_string())?
        .load_with(&lockfile, &registry)
        .map_err(|error| error.to_string())?;
    if loaded.subject_kind() != manifest.subject_kind {
        return Err("captured pipeline disagrees with CampaignManifest Subject kind".into());
    }
    validate_manifest_authority(cas, &manifest, &loaded, &captured, options.mode)?;
    super::run_progress(
        options,
        format_args!("authority {} (pinned)", manifest.authority_snapshot_id),
    );
    super::run_progress(
        options,
        format_args!("manifest  {} (resumed)", payload.campaign_manifest_id),
    );
    Ok(OpenCampaign {
        loaded,
        manifest,
        manifest_id: payload.campaign_manifest_id,
        opened_event_id: event.event_id.clone(),
    })
}

fn validate_manifest_authority(
    cas: &Cas,
    manifest: &CampaignManifestV1,
    loaded: &review_config::Loaded,
    captured: &BTreeMap<String, (ReviewerPackageV1, BTreeMap<String, Vec<u8>>)>,
    mode: CampaignMode,
) -> Result<(), String> {
    let convergence = selected_convergence(mode, loaded.convergence());
    if manifest.convergence.clean_rounds != convergence.clean_rounds
        || manifest.convergence.max_rounds != convergence.max_rounds
        || manifest.convergence.gate != format!("{:?}", convergence.gate).to_lowercase()
    {
        return Err(format!(
            "CampaignManifest convergence differs from requested {} mode; resume with the mode that opened this Campaign",
            mode.as_str()
        ));
    }
    let budgets = loaded.budgets().map(|budget| CampaignBudgetV1 {
        attempt_tokens: budget.attempt,
        run_tokens: budget.run,
    });
    if manifest.budgets != budgets {
        return Err("CampaignManifest budgets differ from captured pipeline authority".into());
    }
    if manifest
        .check_timeout_seconds
        .unwrap_or(loaded.check_timeout_seconds())
        != loaded.check_timeout_seconds()
    {
        return Err(
            "CampaignManifest check timeout differs from captured pipeline authority".into(),
        );
    }
    if manifest.reviewers.len() != loaded.packages().len() {
        return Err("CampaignManifest reviewer bindings are incomplete".into());
    }
    for (node, package) in loaded.packages() {
        let binding = manifest
            .reviewers
            .iter()
            .find(|binding| binding.node == *node)
            .ok_or_else(|| format!("CampaignManifest has no reviewer binding for `{node}`"))?;
        if binding.name != package.name
            || binding.version != package.version
            || binding.digest != package.digest
        {
            return Err(format!(
                "CampaignManifest reviewer binding for `{node}` differs from resolved authority"
            ));
        }
    }
    let expected_execution: BTreeSet<String> =
        std::iter::once(manifest.pipeline.artifact_id.clone())
            .chain(
                manifest
                    .reviewers
                    .iter()
                    .map(|binding| binding.package_artifact_id.clone()),
            )
            .collect();
    if manifest
        .execution_policy_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        != expected_execution
    {
        return Err("CampaignManifest execution policy IDs are not the resolved authority".into());
    }
    for policy in &manifest.project_policy_ids {
        cas.get(policy).map_err(|error| error.to_string())?;
    }
    for (id, kind) in [
        (&manifest.finding_genesis_id, "finding-set-genesis@1"),
        (&manifest.demand_genesis_id, "demand-set-genesis@1"),
    ] {
        let root = cas.get_json(id).map_err(|error| error.to_string())?;
        if root["kind"] != kind || root["authority_snapshot_id"] != manifest.authority_snapshot_id {
            return Err(format!("CampaignManifest has an invalid `{kind}` root"));
        }
    }

    let authority: SourceSnapshot = serde_json::from_value(
        cas.get_json(&manifest.authority_snapshot_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let authority_manifest_id = authority
        .artifact_manifest
        .ok_or("Authority Snapshot has no artifact manifest")?;
    let tree: Manifest = serde_json::from_value(
        cas.get_json(&authority_manifest_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if tree.content_digest() != authority.content_digest
        || tree
            .get(&manifest.pipeline.path)
            .map(|entry| &entry.content)
            != Some(&manifest.pipeline.artifact_id)
        || tree
            .get(&manifest.reviewer_lock.path)
            .map(|entry| &entry.content)
            != Some(&manifest.reviewer_lock.artifact_id)
    {
        return Err("CampaignManifest authority files are not reachable from its Snapshot".into());
    }
    let layout = authority_layout(&manifest.pipeline.path, true)?;
    for (package, _) in captured.values() {
        for (path, artifact_id) in &package.files {
            let authority_path = format!("{}/{}/{path}", layout.registry, package.name);
            if tree.get(&authority_path).map(|entry| &entry.content) != Some(artifact_id) {
                return Err(format!(
                    "captured reviewer file `{authority_path}` is not authority Snapshot content"
                ));
            }
        }
    }
    Ok(())
}

fn selected_convergence(
    mode: CampaignMode,
    configured: &review_store::ConvergencePolicy,
) -> review_store::ConvergencePolicy {
    match mode {
        CampaignMode::Light => review_store::ConvergencePolicy {
            clean_rounds: 1,
            max_rounds: 1,
            gate: configured.gate,
        },
        CampaignMode::Heavy => *configured,
    }
}

fn require_demand_set_output(
    loaded: &review_config::Loaded,
    pipeline_path: &str,
) -> Result<(), String> {
    if loaded.node_kind_has_output_type(
        review_graph::NodeKind::Ledger,
        review_core::contract::DEMAND_SET_V1,
    ) {
        Ok(())
    } else {
        Err(format!(
            "canonical pipeline `{pipeline_path}` Ledger node must declare a review.kernel/DemandSet@1 output"
        ))
    }
}

fn prepare_round(
    options: &Options,
    cas: &Cas,
    store: &mut EventStore,
    repo: &Repo,
    run_id: &str,
    campaign: &OpenCampaign,
    events: &[review_core::RunEvent],
) -> Result<RoundInput, String> {
    let authority_snapshot: SourceSnapshot = serde_json::from_value(
        cas.get_json(&campaign.manifest.authority_snapshot_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let repository_id = authority_snapshot.repository_id.clone();
    let ledger_projection =
        LedgerProjection::from_events(run_id, events, cas).map_err(|error| error.to_string())?;
    let mut closed_rounds = 0_u32;
    for event in events {
        if run_report_closes_round(event)
            .map_err(|error| format!("decoding {}: {error}", event.event_type))?
            .unwrap_or(false)
        {
            closed_rounds += 1;
        }
    }
    let target_round = closed_rounds + 1;
    if closed_rounds >= campaign.manifest.convergence.max_rounds {
        return match options.mode {
            CampaignMode::Light => Err(
                "light Campaign already completed its single review Round; fix its concrete findings and run the deterministic project gate, then stop. Do not start another Campaign; --heavy requires a new Campaign and an explicit human choice"
                    .into(),
            ),
            CampaignMode::Heavy => Err(
                "heavy Campaign already exhausted its pinned Round limit; do not start another Campaign without an explicit human decision"
                    .into(),
            ),
        };
    }
    let mut starts: Vec<(&review_core::RunEvent, RoundStartedPayloadV1)> = events
        .iter()
        .filter(|event| event.event_type == EventType::RoundStartedV1)
        .filter_map(|event| {
            serde_json::from_value::<RoundStartedPayloadV1>(event.payload.clone())
                .ok()
                .filter(|payload| payload.round == target_round)
                .map(|payload| (event, payload))
        })
        .collect();
    starts.sort_by_key(|(_, payload)| payload.epoch);
    let existing = starts.last().cloned();

    let round = match (existing, options.restart_round) {
        (Some((event, payload)), false) => load_round(
            options,
            cas,
            event.event_id.clone(),
            payload,
            &repository_id,
            ledger_projection,
        )?,
        (None, true) => {
            return Err("--restart-round requires an incomplete Round to supersede".into());
        }
        (existing, _restart) => {
            let integrated = existing
                .is_none()
                .then(|| next_integrated_head(events))
                .flatten();
            match integrated {
                Some((event_id, committed)) => start_integrated_round(
                    options,
                    cas,
                    store,
                    run_id,
                    campaign,
                    IntegratedRoundRequest {
                        round: target_round,
                        committed_event_id: event_id,
                        committed,
                        ledger_projection,
                    },
                )?,
                None => capture_round(
                    options,
                    cas,
                    store,
                    repo,
                    run_id,
                    campaign,
                    RoundCaptureRequest {
                        round: target_round,
                        superseded: existing.as_ref().map(|(event, payload)| (*event, payload)),
                        ledger_projection,
                    },
                )?,
            }
        }
    };

    let mut round = round;
    {
        let mut ingest =
            Ingest::from_projection(store, cas, run_id.to_string(), round.ledger_projection)
                .map_err(|error| error.to_string())?
                .under_round(&round.event_id);
        while ingest.ledger().round < target_round {
            ingest.advance().map_err(|error| error.to_string())?;
        }
        round.ledger_projection = ingest.into_projection();
    }
    super::run_progress(
        options,
        format_args!(
            "round    {} (epoch {})",
            round.payload.round, round.payload.epoch
        ),
    );
    if round.prior_count > 0 {
        super::run_progress(
            options,
            format_args!("prior    {} findings carried", round.prior_count),
        );
    }
    Ok(round)
}

struct RoundCaptureRequest<'a> {
    round: u32,
    superseded: Option<(&'a review_core::RunEvent, &'a RoundStartedPayloadV1)>,
    ledger_projection: LedgerProjection,
}

struct IntegratedRoundRequest {
    round: u32,
    committed_event_id: String,
    committed: IntegrationCommittedPayloadV1,
    ledger_projection: LedgerProjection,
}

fn outstanding_attempts_for_supersession(
    events: &[review_core::RunEvent],
    round_event_id: &str,
) -> Result<Vec<(String, String, Option<u64>)>, String> {
    let mut live = BTreeMap::new();
    for event in events
        .iter()
        .filter(|event| event.causation_id.as_deref() == Some(round_event_id))
    {
        let Some(attempt) = event.attempt_id.clone() else {
            continue;
        };
        match event.event_type {
            EventType::AttemptDispatchedV1 => {
                live.insert(
                    attempt,
                    (
                        event
                            .node_id
                            .clone()
                            .ok_or("AttemptDispatched@1 has no node ID")?,
                        event.payload["reserved"].as_u64(),
                        0_u64,
                    ),
                );
            }
            EventType::ReviewerExecutionBoundV1 => {
                let binding: review_core::ReviewerExecutionBindingV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                let authorized = review_core::broker_authority_usage(&binding.operations)?;
                if let Some((_, charged, _)) = live.get_mut(&attempt) {
                    *charged = Some(charged.unwrap_or(0).max(authorized));
                }
            }
            EventType::BrokerOperationCompletedV1 => {
                let receipt: review_core::BrokerOperationReceiptV1 =
                    serde_json::from_value(event.payload.clone())
                        .map_err(|error| error.to_string())?;
                if let Some((_, _, observed)) = live.get_mut(&attempt) {
                    *observed = observed
                        .checked_add(receipt.charged_usage)
                        .ok_or("broker receipt usage overflow")?;
                }
            }
            EventType::AttemptAdmittedV1
            | EventType::AttemptFailedV1
            | EventType::AttemptFencedV1
            | EventType::AttemptReleasedV1 => {
                live.remove(&attempt);
            }
            _ => {}
        }
    }
    Ok(live
        .into_iter()
        .map(|(attempt, (node, charged, observed))| {
            let charged = charged
                .map(|charged| charged.max(observed))
                .or((observed > 0).then_some(observed));
            (node, attempt, charged)
        })
        .collect())
}

fn next_integrated_head(
    events: &[review_core::RunEvent],
) -> Option<(String, IntegrationCommittedPayloadV1)> {
    let latest_subject_id = events.iter().rev().find_map(|event| {
        (event.event_type == EventType::RoundStartedV1)
            .then(|| {
                serde_json::from_value::<RoundStartedPayloadV1>(event.payload.clone())
                    .ok()
                    .map(|payload| payload.subject_id)
            })
            .flatten()
    })?;
    events.iter().rev().find_map(|event| {
        (event.event_type == EventType::IntegrationCommittedV1)
            .then(|| {
                serde_json::from_value::<IntegrationCommittedPayloadV1>(event.payload.clone())
                    .ok()
                    .filter(|payload| payload.prior_subject_id == latest_subject_id)
                    .map(|payload| (event.event_id.clone(), payload))
            })
            .flatten()
    })
}

fn start_integrated_round(
    options: &Options,
    cas: &Cas,
    store: &mut EventStore,
    run_id: &str,
    campaign: &OpenCampaign,
    request: IntegratedRoundRequest,
) -> Result<RoundInput, String> {
    let IntegratedRoundRequest {
        round,
        committed_event_id,
        committed,
        mut ledger_projection,
    } = request;
    committed.validate()?;
    let subject: SubjectV1 = serde_json::from_value(
        cas.get_json(&committed.derived_subject_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    subject.validate()?;
    if subject.head_snapshot_id != committed.derived_snapshot_id
        || subject.kind != campaign.loaded.subject_kind()
    {
        return Err("committed Integration derived Subject contradicts Campaign authority".into());
    }
    let snapshot: SourceSnapshot = serde_json::from_value(
        cas.get_json(&subject.head_snapshot_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if !snapshot.is_derived()
        || snapshot.parent_snapshot_id.as_deref() != Some(committed.prior_snapshot_id.as_str())
    {
        return Err("committed Integration does not name a derived child of its prior head".into());
    }
    let manifest_id = snapshot
        .artifact_manifest
        .as_deref()
        .ok_or("derived Snapshot has no exact Manifest")?;
    let manifest: Manifest = serde_json::from_value(
        cas.get_json(manifest_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    manifest.validate().map_err(|error| error.to_string())?;
    if manifest.content_digest() != snapshot.content_digest {
        return Err("derived Snapshot Manifest contradicts its content digest".into());
    }
    let prior_findings = serde_json::Value::Array(prior_rows(ledger_projection.ledger()));
    let prior_count = prior_findings.as_array().map_or(0, Vec::len);
    let prior_finding_set = serde_json::json!({
        "subject_id": committed.derived_subject_id,
        "round": round,
        "prior_findings": prior_findings,
    });
    let prior_bytes = serde_json::to_string_pretty(&prior_finding_set)
        .map_err(|error| error.to_string())?
        .len();
    if prior_bytes > MAX_PRIOR_FINDINGS_BYTES {
        return Err(format!(
            "exact prior Finding Set is {prior_bytes} bytes; maximum is {MAX_PRIOR_FINDINGS_BYTES} bytes and partitioning is required"
        ));
    }
    let prior_finding_set_id = cas
        .put_json(&prior_finding_set)
        .map_err(|error| error.to_string())?;
    let prior_demand_set_id =
        latest_demand_set_id(store, cas, run_id, &campaign.manifest.demand_genesis_id)?;
    let payload = RoundStartedPayloadV1 {
        round,
        epoch: 1,
        campaign_manifest_id: campaign.manifest_id.clone(),
        subject_id: committed.derived_subject_id.clone(),
        prior_finding_set_id,
        prior_demand_set_id,
    };
    payload.validate()?;
    let mut refs = vec![
        campaign.manifest.authority_snapshot_id.clone(),
        campaign.manifest_id.clone(),
        subject.head_snapshot_id.clone(),
        payload.subject_id.clone(),
        payload.prior_finding_set_id.clone(),
        payload.prior_demand_set_id.clone(),
        manifest_id.to_string(),
    ];
    refs.extend(subject.base_snapshot_id.clone());
    refs.extend(subject.change_set_id.clone());
    let started = store
        .append(
            run_id,
            cas,
            NewEvent::new(
                EventType::RoundStartedV1,
                serde_json::to_value(&payload).map_err(|error| error.to_string())?,
            )
            .caused_by(committed_event_id)
            .correlating(&payload.subject_id)
            .referencing(refs),
        )
        .map_err(|error| error.to_string())?;
    ledger_projection
        .apply_event(&started, cas)
        .map_err(|error| error.to_string())?;
    super::run_progress(
        options,
        format_args!("snapshot {} (integrated)", snapshot.content_digest),
    );
    Ok(RoundInput {
        payload,
        event_id: started.event_id,
        snapshot: manifest,
        prior_count,
        ledger_projection,
    })
}

fn capture_round(
    options: &Options,
    cas: &Cas,
    store: &mut EventStore,
    repo: &Repo,
    run_id: &str,
    campaign: &OpenCampaign,
    request: RoundCaptureRequest<'_>,
) -> Result<RoundInput, String> {
    let RoundCaptureRequest {
        round,
        superseded,
        mut ledger_projection,
    } = request;
    let dispatched_attempts: Vec<(String, String, Option<u64>)> = if let Some((old_event, _)) =
        superseded
    {
        let events = store.replay(run_id).map_err(|error| error.to_string())?;
        if events.iter().any(|event| {
            event.sequence > old_event.sequence
                && (event.event_type == EventType::FindingReportedV1
                    || (event.event_type == EventType::FindingResolvedV1
                        && event.causation_id.as_deref() == Some(old_event.event_id.as_str())))
        }) {
            return Err(
                "cannot supersede an incomplete Round after it published finding state; start a new Campaign"
                    .into(),
            );
        }
        outstanding_attempts_for_supersession(&events, &old_event.event_id)?
    } else {
        Vec::new()
    };
    let capture = Capture::new(repo, cas);
    let candidate_ref = options.candidate.as_deref().unwrap_or("HEAD");
    let mut snapshot = if options.uncommitted {
        capture
            .dirty()
            .map_err(|error| format!("capturing revalidated worktree: {error}"))?
    } else {
        capture
            .committed(candidate_ref)
            .map_err(|error| format!("capturing candidate `{candidate_ref}`: {error}"))?
    };
    let authority_snapshot: SourceSnapshot = serde_json::from_value(
        cas.get_json(&campaign.manifest.authority_snapshot_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if snapshot.repository_id != authority_snapshot.repository_id {
        return Err(
            "candidate HEAD belongs to a different repository than the Campaign authority".into(),
        );
    }
    let base_snapshot = if campaign.loaded.subject_kind() == SubjectKind::Diff {
        Some(
            serde_json::from_value::<SourceSnapshot>(
                cas.get_json(
                    campaign
                        .manifest
                        .base_snapshot_id
                        .as_deref()
                        .ok_or("diff Campaign has no pinned Base Snapshot")?,
                )
                .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?,
        )
    } else {
        None
    };
    if let Some(base_snapshot) = &base_snapshot
        && snapshot.submodules != base_snapshot.submodules
    {
        let mut paths: Vec<String> = snapshot
            .submodules
            .iter()
            .chain(&base_snapshot.submodules)
            .map(|submodule| submodule.path.clone())
            .collect();
        paths.sort();
        paths.dedup();
        return Err(format!(
            "diff capture refuses changed gitlinks until submodule sandbox policy is explicit: {}",
            paths.join(", ")
        ));
    }
    let tree_diff = if campaign.loaded.subject_kind() == SubjectKind::Diff {
        let base_snapshot = base_snapshot.as_ref().expect("diff Base was loaded");
        let base_manifest_id = base_snapshot
            .artifact_manifest
            .as_deref()
            .ok_or("Campaign Base Snapshot has no artifact manifest")?;
        let base_manifest: Manifest = serde_json::from_value(
            cas.get_json(base_manifest_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let base_tree = capture
            .rehydrate_committed(base_snapshot, &base_manifest)
            .map_err(|error| format!("rehydrating pinned Base: {error}"))?;
        if snapshot.dirty {
            let (head_tree, diff) = repo
                .tree_diff_synthetic_head(&base_tree, &snapshot.manifest, cas)
                .map_err(|error| error.to_string())?;
            snapshot.tree_id = Some(head_tree);
            Some(diff)
        } else {
            Some(
                repo.tree_diff(
                    &base_tree,
                    snapshot
                        .tree_id
                        .as_ref()
                        .ok_or("committed head has no tree authority")?,
                )
                .map_err(|error| error.to_string())?,
            )
        }
    } else {
        if snapshot.dirty {
            snapshot.tree_id = Some(
                repo.synthetic_tree(&snapshot.manifest, cas)
                    .map_err(|error| error.to_string())?,
            );
        }
        None
    };
    if tree_diff
        .as_ref()
        .is_some_and(|diff| diff.changes.is_empty())
    {
        return Err(
            "refusing empty Diff before Gates, Provider admission, or Worker dispatch; select a different Base/candidate or a whole-tree pipeline"
                .into(),
        );
    }
    match &tree_diff {
        Some(diff) => super::run_progress(
            options,
            format_args!(
                "subject   Diff ({} changed records, {} patch bytes)",
                diff.changes.len(),
                diff.patch().len()
            ),
        ),
        None => super::run_progress(options, format_args!("subject   WholeTree")),
    }
    let (head_snapshot_id, manifest_id) = publish_snapshot(&snapshot, cas)?;
    let change_set_id = match tree_diff {
        Some(diff) => {
            // Base64 alone expands every three raw bytes to four encoded bytes. Refuse before
            // building path arrays, base64, serde Values, and canonical JSON when the patch
            // already cannot fit the authoritative encoded Change Set bound below.
            let raw_patch_limit = maximum_raw_patch_bytes();
            if raw_patch_exceeds_change_set_bound(diff.patch().len()) {
                return Err(format!(
                    "exact Change Set patch is {} raw bytes; maximum encodable patch is {} raw bytes and partitioning is required",
                    diff.patch().len(),
                    raw_patch_limit
                ));
            }
            let base_snapshot_id = campaign
                .manifest
                .base_snapshot_id
                .as_deref()
                .ok_or("diff Campaign has no pinned Base Snapshot")?;
            let change_set = diff.change_set(base_snapshot_id, &head_snapshot_id)?;
            let value = serde_json::to_value(&change_set).map_err(|error| error.to_string())?;
            review_core::json::admit(&value).map_err(|error| error.to_string())?;
            let encoded =
                review_store::canonical::canonicalize(&value).map_err(|error| error.to_string())?;
            if encoded.len() > MAX_CHANGE_SET_BYTES {
                return Err(format!(
                    "exact Change Set is {} bytes; maximum is {} bytes and partitioning is required",
                    encoded.len(),
                    MAX_CHANGE_SET_BYTES
                ));
            }
            Some(cas.put(&encoded).map_err(|error| error.to_string())?)
        }
        None => None,
    };
    let mut source_refs = vec![
        campaign.manifest.authority_snapshot_id.clone(),
        campaign.manifest_id.clone(),
        head_snapshot_id.clone(),
        manifest_id.clone(),
    ];
    source_refs.extend(change_set_id.clone());
    let source_captured = store
        .append(
            run_id,
            cas,
            NewEvent::new(
                EventType::SourceCapturedV1,
                snapshot
                    .to_payload(Some(&manifest_id))
                    .map_err(|error| error.to_string())?,
            )
            .caused_by(campaign.opened_event_id.clone())
            .correlating(head_snapshot_id.clone())
            .referencing(source_refs),
        )
        .map_err(|error| error.to_string())?;
    ledger_projection
        .apply_event(&source_captured, cas)
        .map_err(|error| error.to_string())?;
    let subject = match campaign.loaded.subject_kind() {
        SubjectKind::WholeTree => SubjectV1::whole_tree(&head_snapshot_id),
        SubjectKind::Diff => SubjectV1::diff(
            &head_snapshot_id,
            campaign
                .manifest
                .base_snapshot_id
                .as_deref()
                .ok_or("diff Campaign has no pinned Base Snapshot")?,
            change_set_id
                .as_deref()
                .ok_or("diff Campaign produced no Change Set")?,
        ),
    };
    subject.validate()?;
    let subject_id = cas
        .put_json(&serde_json::to_value(&subject).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;

    let prior_findings = serde_json::Value::Array(prior_rows(ledger_projection.ledger()));
    let prior_count = prior_findings.as_array().map_or(0, Vec::len);
    let prior_finding_set = serde_json::json!({
        "subject_id": subject_id,
        "round": round,
        "prior_findings": prior_findings,
    });
    let prior_bytes = serde_json::to_string_pretty(&prior_finding_set)
        .map_err(|error| error.to_string())?
        .len();
    if prior_bytes > MAX_PRIOR_FINDINGS_BYTES {
        return Err(format!(
            "exact prior Finding Set is {prior_bytes} bytes; maximum is {MAX_PRIOR_FINDINGS_BYTES} bytes and partitioning is required"
        ));
    }
    let prior_finding_set_id = cas
        .put_json(&prior_finding_set)
        .map_err(|error| error.to_string())?;
    let prior_demand_set_id = if let Some((_, old)) = superseded {
        old.prior_demand_set_id.clone()
    } else if campaign.manifest.finding_identity_policy
        == review_core::CANONICAL_FINDING_IDENTITY_POLICY
    {
        latest_demand_set_id(store, cas, run_id, &campaign.manifest.demand_genesis_id)?
    } else {
        cas.put_json(&serde_json::json!({
            "subject_id": subject_id,
            "round": round,
            "demands": ledger_projection.ledger().demand_views(),
        }))
        .map_err(|error| error.to_string())?
    };
    let payload = RoundStartedPayloadV1 {
        round,
        epoch: superseded.map_or(1, |(_, old)| old.epoch + 1),
        campaign_manifest_id: campaign.manifest_id.clone(),
        subject_id: subject_id.clone(),
        prior_finding_set_id,
        prior_demand_set_id,
    };
    payload.validate()?;
    let started = if let Some((old_event, old)) = superseded {
        let superseded = RoundInputSupersededPayloadV1 {
            round,
            old_epoch: old.epoch,
            new_epoch: payload.epoch,
            campaign_manifest_id: campaign.manifest_id.clone(),
            old_subject_id: old.subject_id.clone(),
            replacement_subject_id: payload.subject_id.clone(),
        };
        superseded.validate()?;
        let mut replacement_refs = vec![
            campaign.manifest.authority_snapshot_id.clone(),
            campaign.manifest_id.clone(),
            old.subject_id.clone(),
            payload.subject_id.clone(),
            payload.prior_finding_set_id.clone(),
            payload.prior_demand_set_id.clone(),
        ];
        replacement_refs.extend(subject.base_snapshot_id.clone());
        replacement_refs.extend(subject.change_set_id.clone());
        let mut batch = vec![
            NewEvent::new(
                EventType::RoundInputSupersededV1,
                serde_json::to_value(superseded).map_err(|error| error.to_string())?,
            )
            .caused_by(old_event.event_id.clone())
            .correlating(payload.subject_id.clone())
            .referencing(replacement_refs),
        ];
        batch.extend(
            dispatched_attempts
                .into_iter()
                .map(|(node, attempt, charged)| {
                    NewEvent::new(
                        EventType::AttemptFencedV1,
                        serde_json::json!({
                            "reason": "Round input superseded",
                            "charged": charged,
                        }),
                    )
                    .node(node)
                    .attempt(attempt)
                    .caused_by(old_event.event_id.clone())
                }),
        );
        let mut round_refs = vec![
            campaign.manifest.authority_snapshot_id.clone(),
            campaign.manifest_id.clone(),
            head_snapshot_id.clone(),
            payload.subject_id.clone(),
            payload.prior_finding_set_id.clone(),
            payload.prior_demand_set_id.clone(),
        ];
        round_refs.extend(subject.base_snapshot_id.clone());
        round_refs.extend(subject.change_set_id.clone());
        batch.push(
            NewEvent::new(
                EventType::RoundStartedV1,
                serde_json::to_value(&payload).map_err(|error| error.to_string())?,
            )
            .caused_by(old_event.event_id.clone())
            .correlating(subject_id.clone())
            .referencing(round_refs),
        );
        let appended = store
            .append_batch(run_id, cas, &batch)
            .map_err(|error| error.to_string())?;
        for event in &appended {
            ledger_projection
                .apply_event(event, cas)
                .map_err(|error| error.to_string())?;
        }
        appended
            .last()
            .cloned()
            .ok_or("supersession batch did not publish its replacement Round")?
    } else {
        let mut round_refs = vec![
            campaign.manifest.authority_snapshot_id.clone(),
            campaign.manifest_id.clone(),
            head_snapshot_id.clone(),
            payload.subject_id.clone(),
            payload.prior_finding_set_id.clone(),
            payload.prior_demand_set_id.clone(),
        ];
        round_refs.extend(subject.base_snapshot_id.clone());
        round_refs.extend(subject.change_set_id.clone());
        let started = store
            .append(
                run_id,
                cas,
                NewEvent::new(
                    EventType::RoundStartedV1,
                    serde_json::to_value(&payload).map_err(|error| error.to_string())?,
                )
                .caused_by(campaign.opened_event_id.clone())
                .correlating(subject_id)
                .referencing(round_refs),
            )
            .map_err(|error| error.to_string())?;
        ledger_projection
            .apply_event(&started, cas)
            .map_err(|error| error.to_string())?;
        started
    };
    super::run_progress(
        options,
        format_args!("snapshot {}", snapshot.content_digest),
    );
    Ok(RoundInput {
        payload,
        event_id: started.event_id,
        snapshot: snapshot.manifest,
        prior_count,
        ledger_projection,
    })
}

fn maximum_raw_patch_bytes() -> usize {
    MAX_CHANGE_SET_BYTES.saturating_mul(3) / 4
}

fn raw_patch_exceeds_change_set_bound(raw_bytes: usize) -> bool {
    raw_bytes > maximum_raw_patch_bytes()
}

fn load_round(
    options: &Options,
    cas: &Cas,
    event_id: String,
    payload: RoundStartedPayloadV1,
    repository_id: &str,
    ledger_projection: LedgerProjection,
) -> Result<RoundInput, String> {
    payload.validate()?;
    let subject: SubjectV1 = serde_json::from_value(
        cas.get_json(&payload.subject_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    subject.validate()?;
    if subject.kind == SubjectKind::Diff {
        let change_set: ChangeSetV1 = serde_json::from_value(
            cas.get_json(
                subject
                    .change_set_id
                    .as_deref()
                    .ok_or("diff Subject has no Change Set ID")?,
            )
            .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        change_set.validate()?;
        if change_set.base_snapshot_id != subject.base_snapshot_id.as_deref().unwrap_or_default()
            || change_set.head_snapshot_id != subject.head_snapshot_id
        {
            return Err("ChangeSet@1 does not match the resumed Subject".into());
        }
    }
    let snapshot: SourceSnapshot = serde_json::from_value(
        cas.get_json(&subject.head_snapshot_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if snapshot.repository_id != repository_id {
        return Err("captured Round Subject belongs to a different repository".into());
    }
    let manifest_id = snapshot
        .artifact_manifest
        .ok_or("captured head Snapshot has no artifact manifest")?;
    let manifest: Manifest = serde_json::from_value(
        cas.get_json(&manifest_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if manifest.content_digest() != snapshot.content_digest {
        return Err("captured head manifest disagrees with SourceSnapshot content digest".into());
    }
    let prior_count = validate_round_set(
        cas,
        &payload.prior_finding_set_id,
        &payload.subject_id,
        payload.round,
        "prior_findings",
    )?;
    validate_round_set(
        cas,
        &payload.prior_demand_set_id,
        &payload.subject_id,
        payload.round,
        "demands",
    )?;
    super::run_progress(
        options,
        format_args!("snapshot {} (reused)", snapshot.content_digest),
    );
    Ok(RoundInput {
        payload,
        event_id,
        snapshot: manifest,
        prior_count,
        ledger_projection,
    })
}

fn validate_round_set(
    cas: &Cas,
    artifact_id: &str,
    subject_id: &str,
    round: u32,
    items_field: &str,
) -> Result<usize, String> {
    let value = cas
        .get_json(artifact_id)
        .map_err(|error| error.to_string())?;
    if items_field == "demands" {
        if let Ok(envelope) = serde_json::from_value::<review_core::ArtifactEnvelope>(value.clone())
        {
            envelope.validate().map_err(|error| error.to_string())?;
            if envelope.artifact_type != review_core::contract::DEMAND_SET_V1 {
                return Err("Round demands set is not a DemandSet@1 artifact".into());
            }
            let payload: review_core::DemandSetV1 =
                serde_json::from_value(envelope.payload).map_err(|error| error.to_string())?;
            payload.validate()?;
            return Ok(payload.demands.len());
        }
        if value["kind"].as_str() == Some("demand-set-genesis@1") {
            return value["demands"]
                .as_array()
                .map(Vec::len)
                .ok_or_else(|| "Demand Set genesis does not contain an array".to_string());
        }
    }
    let object = value
        .as_object()
        .ok_or_else(|| format!("Round {items_field} set is not an object"))?;
    let expected = BTreeSet::from(["subject_id", "round", items_field]);
    let actual: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    if actual != expected
        || value["subject_id"].as_str() != Some(subject_id)
        || value["round"].as_u64() != Some(u64::from(round))
    {
        return Err(format!(
            "Round {items_field} set does not match its Subject and round"
        ));
    }
    value[items_field]
        .as_array()
        .map(Vec::len)
        .ok_or_else(|| format!("Round {items_field} set does not contain an array"))
}

fn latest_demand_set_id(
    store: &EventStore,
    cas: &Cas,
    run_id: &str,
    genesis_id: &str,
) -> Result<String, String> {
    for event in store
        .replay(run_id)
        .map_err(|error| error.to_string())?
        .into_iter()
        .rev()
    {
        if event.event_type != EventType::NodeOutputReceiptV1 {
            continue;
        }
        let receipt: review_core::NodeOutputReceiptPayloadV1 =
            serde_json::from_value(event.payload).map_err(|error| error.to_string())?;
        for port in receipt.outputs.into_iter().rev() {
            for artifact_id in port.artifact_ids.into_iter().rev() {
                let value = cas
                    .get_json(&artifact_id)
                    .map_err(|error| error.to_string())?;
                let Ok(envelope) = serde_json::from_value::<review_core::ArtifactEnvelope>(value)
                else {
                    continue;
                };
                if envelope.artifact_type != review_core::contract::DEMAND_SET_V1 {
                    continue;
                }
                envelope.validate().map_err(|error| error.to_string())?;
                let payload: review_core::DemandSetV1 =
                    serde_json::from_value(envelope.payload).map_err(|error| error.to_string())?;
                payload.validate()?;
                return Ok(artifact_id);
            }
        }
    }
    cas.verify(genesis_id).map_err(|error| error.to_string())?;
    Ok(genesis_id.to_string())
}

fn prior_rows(ledger: &Ledger) -> Vec<serde_json::Value> {
    ledger
        .finding_views()
        .iter()
        .filter(|finding| {
            !finding.authority_diagnostic
                && !matches!(finding.status, Status::Rejected | Status::Wontfix)
        })
        .map(|finding| {
            let severity = format!("{:?}", finding.severity).to_lowercase();
            let effective_severity = finding
                .convergence_severity
                .map(|effective| format!("{effective:?}").to_lowercase());
            let scope = finding.convergence_scope_label();
            let mut row = serde_json::json!({
                "key": finding.key,
                "severity": severity,
                "status": finding.status.as_str(),
                "line": finding.identity_line,
                "title": finding.title,
                "body": finding.body,
                "source": finding.source,
                "last_seen_round": finding.last_seen_round,
            });
            let object = row.as_object_mut().expect("prior row is an object");
            let (file, line, location_unrecorded) =
                prior_location(&finding.identity_file, finding.identity_line);
            object.insert("file".into(), file);
            object.insert("line".into(), line);
            if location_unrecorded {
                object.insert("location_unrecorded".into(), serde_json::Value::Bool(true));
            }
            if scope != "in" {
                object.insert("scope".into(), serde_json::Value::String(scope.into()));
            }
            if effective_severity.as_deref() != Some(severity.as_str()) {
                object.insert(
                    "effective_severity".into(),
                    effective_severity.map_or(serde_json::Value::Null, serde_json::Value::String),
                );
            }
            row
        })
        .collect()
}

fn prior_location(file: &str, line: Option<i64>) -> (serde_json::Value, serde_json::Value, bool) {
    if file == review_core::legacy::CHANGE_WIDE_SENTINEL {
        return (serde_json::Value::Null, serde_json::Value::Null, false);
    }
    if review_core::is_valid_repo_path(file) {
        (
            serde_json::Value::String(file.to_string()),
            line.map_or(serde_json::Value::Null, |line| line.into()),
            false,
        )
    } else {
        (serde_json::Value::Null, serde_json::Value::Null, true)
    }
}

fn publish_snapshot(snapshot: &Snapshot, cas: &Cas) -> Result<(String, String), String> {
    let manifest_id = cas
        .put_json(&serde_json::to_value(&snapshot.manifest).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    let snapshot_id = cas
        .put_json(
            &snapshot
                .to_payload(Some(&manifest_id))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    Ok((snapshot_id, manifest_id))
}

fn authority_bytes(manifest: &Manifest, cas: &Cas, path: &str) -> Result<Vec<u8>, String> {
    let entry = manifest
        .get(path)
        .ok_or_else(|| format!("Authority Snapshot has no `{path}`"))?;
    if entry.kind == EntryKind::Symlink {
        return Err(format!("authority file `{path}` is a symlink"));
    }
    cas.get(&entry.content).map_err(|error| error.to_string())
}

fn captured_registry(
    manifest: &Manifest,
    cas: &Cas,
    registry: &str,
) -> Result<BTreeMap<String, BTreeMap<String, Vec<u8>>>, String> {
    let prefix = format!("{registry}/");
    let mut packages: BTreeMap<String, BTreeMap<String, Vec<u8>>> = BTreeMap::new();
    for entry in &manifest.entries {
        let Some(relative) = entry.path.strip_prefix(&prefix) else {
            continue;
        };
        let Some((name, path)) = relative.split_once('/') else {
            continue;
        };
        if name.is_empty() || path.is_empty() {
            continue;
        }
        if entry.kind == EntryKind::Symlink {
            return Err(format!(
                "reviewer package `{name}` contains symlink `{}` in the Authority Snapshot",
                entry.path
            ));
        }
        packages.entry(name.to_string()).or_default().insert(
            path.to_string(),
            cas.get(&entry.content).map_err(|error| error.to_string())?,
        );
    }
    Ok(packages)
}

pub(crate) struct AuthorityLayout {
    pub(crate) root: String,
    pub(crate) lock: String,
    pub(crate) registry: String,
}

/// The authority files a pipeline path implies. `recorded` says the path comes from a stored
/// Campaign Manifest: such a Campaign may still name the retired `.review/` layout and stays
/// replayable, while a new invocation may not (ADR-0043, since v0.8.0).
fn authority_layout(pipeline: &str, recorded: bool) -> Result<AuthorityLayout, String> {
    let root = Path::new(pipeline)
        .parent()
        .and_then(Path::parent)
        .and_then(Path::to_str)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| "the pipeline path must live under `.af/pipelines/`".to_string())?;
    match root {
        ".af" => Ok(AuthorityLayout {
            root: root.to_string(),
            lock: ".af/af.lock".to_string(),
            registry: ".af/workers".to_string(),
        }),
        ".review" if recorded => Ok(AuthorityLayout {
            root: root.to_string(),
            lock: ".review/review.lock".to_string(),
            registry: ".review/reviewers".to_string(),
        }),
        ".review" => Err(
            "`.review/` authority is no longer read for new Campaigns (since v0.8.0, ADR-0043) — fix: `af onboard --migrate --apply` moves it to `.af/`; commit the result and delete `.review/`"
                .to_string(),
        ),
        _ => Err("the pipeline path must live under `.af/pipelines/`".to_string()),
    }
}

/// The layout for a pipeline path given on the command line.
pub(crate) fn authority_paths(pipeline: &str) -> Result<AuthorityLayout, String> {
    authority_layout(pipeline, false)
}

/// A pipeline may be requested explicitly when the project can select it (default, routes,
/// oversized) or when the lock pins it: every `.af/pipelines/*.toml` that `af onboard
/// --refresh-lock` saw is declared policy, whether or not a route ever picks it.
fn validate_af_project(
    bytes: &[u8],
    pipeline_path: &str,
    lockfile: &Lockfile,
) -> Result<(), String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| format!("authority project `.af/af.toml` is not UTF-8: {error}"))?;
    let project = crate::project::ProjectFile::parse(text)?;
    let candidates = project.pipeline_candidates();
    let name = Path::new(pipeline_path)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if !candidates.contains(name) && !lockfile.pipelines.contains_key(name) {
        return Err(format!(
            "authority declares pipelines {} (default, routes, oversized) and pins {} in `.af/af.lock`, but invocation requested `{pipeline_path}` — fix: add the file under `.af/pipelines/` and run `af onboard --refresh-lock` at the policy revision",
            candidates
                .iter()
                .map(|candidate| format!("`.af/pipelines/{candidate}.toml`"))
                .collect::<Vec<_>>()
                .join(", "),
            if lockfile.pipelines.is_empty() {
                "nothing".to_string()
            } else {
                lockfile
                    .pipelines
                    .keys()
                    .map(|pinned| format!("`{pinned}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ));
    }
    Ok(())
}

fn validate_af_pipeline_pin(
    lockfile: &Lockfile,
    pipeline_path: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let name = Path::new(pipeline_path)
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or("authority pipeline has no UTF-8 name")?;
    let pin = lockfile
        .pipelines
        .get(name)
        .ok_or_else(|| {
            format!(
                "pipeline `{name}` is not pinned in `.af/af.lock` — fix: af onboard --refresh-lock, then commit the lock at the policy revision"
            )
        })?;
    let found = review_store::canonical::blob_content_id(bytes);
    if pin.digest != found {
        return Err(format!(
            "pipeline `{name}` does not match `.af/af.lock`: locked {}, found {found}",
            pin.digest
        ));
    }
    Ok(())
}

fn authority_path(repo: &Path, pipeline: &Path) -> Result<String, String> {
    let relative: PathBuf = if pipeline.is_absolute() {
        let root = if repo.is_absolute() {
            repo.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|error| error.to_string())?
                .join(repo)
        };
        pipeline
            .strip_prefix(&root)
            .map_err(|_| "the pipeline path must be inside --repo")?
            .to_path_buf()
    } else {
        pipeline.to_path_buf()
    };
    let mut components = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(value) => components.push(
                value
                    .to_str()
                    .ok_or("the pipeline path must be valid UTF-8")?,
            ),
            Component::CurDir => {}
            _ => return Err("the pipeline path must be repository-relative without `..`".into()),
        }
    }
    if components.is_empty() {
        return Err("the pipeline path is empty".into());
    }
    Ok(components.join("/"))
}

#[cfg(test)]
mod tests {
    use review_core::{IntegrationCommittedPayloadV1, RoundStartedPayloadV1};

    fn attempt_event(
        sequence: u64,
        event_type: review_core::EventType,
        payload: serde_json::Value,
    ) -> review_core::RunEvent {
        review_core::RunEvent {
            event_id: format!("event-{sequence}"),
            run_id: "run".into(),
            sequence,
            event_type,
            occurred_at: "2026-08-31T00:00:00Z".into(),
            node_id: Some("reviewer".into()),
            attempt_id: Some("a".repeat(26)),
            causation_id: Some("round".into()),
            correlation_id: None,
            artifact_refs: vec![],
            payload,
        }
    }

    #[test]
    fn omitted_git_timeout_resolves_to_the_capture_default() {
        assert_eq!(
            super::requested_git_timeout(None).as_secs(),
            review_source_git::DEFAULT_GIT_TIMEOUT_SECONDS
        );
        assert_eq!(
            super::requested_git_timeout(Some(std::time::Duration::from_secs(17))).as_secs(),
            17
        );
    }

    #[test]
    fn raw_patch_bound_refuses_before_base64_amplification() {
        let limit = super::maximum_raw_patch_bytes();
        assert!(!super::raw_patch_exceeds_change_set_bound(limit));
        assert!(super::raw_patch_exceeds_change_set_bound(limit + 1));
        assert!(limit * 4 / 3 >= review_core::MAX_CHANGE_SET_BYTES);
    }

    #[test]
    fn prior_findings_never_echo_a_path_live_admission_would_refuse() {
        assert_eq!(
            super::prior_location("src/main.rs", Some(7)),
            (
                serde_json::Value::String("src/main.rs".into()),
                serde_json::json!(7),
                false
            )
        );
        assert_eq!(
            super::prior_location("./src/main.rs", Some(7)),
            (serde_json::Value::Null, serde_json::Value::Null, true)
        );
        assert_eq!(
            super::prior_location(review_core::legacy::CHANGE_WIDE_SENTINEL, Some(7)),
            (serde_json::Value::Null, serde_json::Value::Null, false)
        );
    }

    #[test]
    fn supersession_fence_covers_observed_usage_above_broker_authority() {
        let attempt = "a".repeat(26);
        let operation = review_core::BrokerOperationPolicyV1 {
            name: "model_inference".into(),
            destination: "provider.test".into(),
            method: "responses.create".into(),
            max_request_bytes: 1024,
            max_response_bytes: 1024,
            max_calls: 1,
            max_usage: 100,
        };
        let events = vec![
            attempt_event(
                1,
                review_core::EventType::AttemptDispatchedV1,
                serde_json::json!({"reserved": null}),
            ),
            attempt_event(
                2,
                review_core::EventType::ReviewerExecutionBoundV1,
                serde_json::to_value(review_core::ReviewerExecutionBindingV1 {
                    node: "reviewer".into(),
                    attempt_id: attempt.clone(),
                    lease_epoch: 1,
                    credential_mode: review_core::BrokerCredentialModeV1::Brokered,
                    auto_apply: false,
                    broker_handle: Some("b".repeat(26)),
                    operations: vec![operation],
                    admitted: true,
                })
                .unwrap(),
            ),
            attempt_event(
                3,
                review_core::EventType::BrokerOperationCompletedV1,
                serde_json::to_value(review_core::BrokerOperationReceiptV1 {
                    handle_id: "b".repeat(26),
                    node: "reviewer".into(),
                    attempt_id: attempt.clone(),
                    lease_epoch: 1,
                    operation: "model_inference".into(),
                    destination: "provider.test".into(),
                    method: "responses.create".into(),
                    ordinal: 1,
                    outcome: review_core::BrokerOperationOutcomeV1::Failed,
                    failure_reason: Some(review_core::BrokerFailureReasonV1::UsageOverrun),
                    request_digest: format!("sha256:{}", "c".repeat(64)),
                    response_digest: Some(format!("sha256:{}", "d".repeat(64))),
                    request_bytes: 7,
                    response_bytes: 8,
                    reserved_usage: 100,
                    charged_usage: 101,
                })
                .unwrap(),
            ),
        ];

        assert_eq!(
            super::outstanding_attempts_for_supersession(&events, "round").unwrap(),
            vec![("reviewer".into(), attempt, Some(101))]
        );
    }

    #[test]
    fn only_an_integration_from_the_latest_subject_becomes_the_next_head() {
        let digest = |byte: char| format!("sha256:{}", byte.to_string().repeat(64));
        let round = |sequence: u64, subject_id: String| review_core::RunEvent {
            event_id: format!("round-{sequence}"),
            run_id: "run".into(),
            sequence,
            event_type: review_core::EventType::RoundStartedV1,
            occurred_at: "2026-09-01T00:00:00Z".into(),
            node_id: None,
            attempt_id: None,
            causation_id: None,
            correlation_id: None,
            artifact_refs: vec![],
            payload: serde_json::to_value(RoundStartedPayloadV1 {
                round: sequence as u32,
                epoch: 1,
                campaign_manifest_id: digest('1'),
                subject_id,
                prior_finding_set_id: digest('2'),
                prior_demand_set_id: digest('3'),
            })
            .unwrap(),
        };
        let commit = |sequence: u64, prior: String, derived: String| review_core::RunEvent {
            event_id: format!("commit-{sequence}"),
            run_id: "run".into(),
            sequence,
            event_type: review_core::EventType::IntegrationCommittedV1,
            occurred_at: "2026-09-01T00:00:00Z".into(),
            node_id: None,
            attempt_id: None,
            causation_id: None,
            correlation_id: None,
            artifact_refs: vec![],
            payload: serde_json::to_value(IntegrationCommittedPayloadV1 {
                batch_id: format!("batch-{sequence}"),
                prior_subject_id: prior,
                derived_subject_id: derived,
                prior_snapshot_id: digest('4'),
                derived_snapshot_id: digest('5'),
                proposal_ids: vec![digest('6')],
                attestation_ids: vec![digest('7')],
                expected_finding_set_id: digest('8'),
                expected_demand_set_id: digest('9'),
                policy_id: digest('a'),
                semantic_closure_id: digest('b'),
            })
            .unwrap(),
        };
        let subject_one = digest('c');
        let subject_two = digest('d');
        let mut events = vec![
            round(1, subject_one.clone()),
            commit(2, subject_one, subject_two.clone()),
        ];
        assert_eq!(
            super::next_integrated_head(&events)
                .unwrap()
                .1
                .derived_subject_id,
            subject_two
        );
        events.push(round(3, subject_two));
        assert!(super::next_integrated_head(&events).is_none());
    }
}
