//! Lower a committed portable artifact to an ordinary, receiver-specific Scenario bundle.
use super::*;
use af_contracts::backlog::batch::BatchReference;
use af_contracts::backlog::batch::assessment::{SOURCE_NAME as BATCH_SOURCE, SOURCE_URI};
use af_contracts::task::document::DocumentSourceV1;
use af_contracts::task::report_domain::{MAX_REPORT_SOURCES_FILE_BYTES, ReportSourcesV1};
use af_pipeline_config::task::shared::safe_relative_path;
use af_scenario::contract::{
    Authority, Binding, Bundle, Datum, Grant, Kind, Limits, Operation, Port, Scenario, Step,
    Timeout, digest, hash, name, require,
};

const SCHEMA: &str = "af/scenario-artifact@1";
const BACKLOG_FILE: &str = "scenario_backlog_artifact_file";
const BACKLOG_SCHEMA: &str = "af/scenario-artifact@2";
pub(super) const SOURCES: &str = "scenario_portable_sources";

#[derive(Deserialize)]
#[serde(untagged)]
enum Inputs {
    Report(ReportSourcesV1),
    Backlog(BatchReference),
}

impl Inputs {
    fn bind(
        self,
        repo: &Path,
        artifact: &Artifact,
        artifact_digest: &str,
        source_revision: String,
    ) -> Result<ReportSourcesV1, String> {
        let backlog = artifact.schema == BACKLOG_SCHEMA;
        match self {
            Self::Report(inputs) if !backlog => {
                require(
                    artifact
                        .inputs
                        .values()
                        .all(|kind| *kind == InputType::ReportSource),
                    "Portable report artifact requires report_source inputs",
                )?;
                Ok(inputs)
            }
            Self::Backlog(reference) if backlog => {
                require(
                    artifact.inputs
                        == BTreeMap::from([(BATCH_SOURCE.into(), InputType::BacklogBatch)]),
                    "Portable backlog artifact requires one backlog_batch input",
                )?;
                reference.validate()?;
                require(
                    artifact.definition.limits.max_dispatches >= 3
                        && artifact
                            .definition
                            .task
                            .limits
                            .wall_ms
                            .saturating_add(30_000)
                            <= artifact.definition.limits.max_wall_ms,
                    "Portable backlog bounds must cover assessment, settlement and dispatch",
                )?;
                let checkout = af_project::require_checkout(repo)?;
                require(
                    reference.project_id == checkout.project_id,
                    "Backlog batch reference belongs to a different Project",
                )?;
                let store = af_project::ProjectStore::open_read_only(
                    &crate::xdg_state_root()?,
                    &checkout.project_id,
                )?;
                let batch = store.collect_backlog_batch(
                    reference,
                    artifact_digest,
                    &artifact.definition.limits,
                    source_revision,
                )?;
                let text = serde_json::to_string(&batch).map_err(|e| e.to_string())?;
                let mut inputs = ReportSourcesV1::empty();
                inputs.sources.insert(
                    BATCH_SOURCE.into(),
                    DocumentSourceV1 {
                        title: "Frozen Project backlog batch".into(),
                        uri: format!("{SOURCE_URI}{}", batch.reference.batch_id),
                        revision: digest(text.as_bytes()),
                        text,
                    },
                );
                Ok(inputs)
            }
            _ => Err("Portable Scenario requires its declared input document type".into()),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    schema: String,
    definition: Definition,
    inputs: BTreeMap<String, InputType>,
    packages: BTreeMap<String, TaskPackagePin>,
    schemas: BTreeMap<String, SchemaDependency>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    id: String,
    step: String,
    result: String,
    limits: Limits,
    task: ReportTask,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum InputType {
    ReportSource,
    BacklogBatch,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SchemaDependency {
    path: String,
    digest: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportTask {
    kind: String,
    kind_package: String,
    pipeline: String,
    goal: String,
    limits: FileLimits,
}

pub(super) struct Packages {
    pub pins: BTreeMap<String, TaskPackagePin>,
    pub kind_package: String,
}

pub(super) fn bound_sources(
    requirements: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Result<Option<ReportSourcesV1>, String> {
    requirements
        .and_then(|requirements| requirements.get(SOURCES))
        .map(|value| {
            let sources: ReportSourcesV1 =
                serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
            sources.validate()?;
            Ok(sources)
        })
        .transpose()
}

fn artifact_path(directory: &str, path: &str) -> Result<String, String> {
    require(
        safe_relative_path(path),
        "Portable Scenario dependency path must stay beneath the artifact directory",
    )?;
    Ok(format!("{directory}{path}"))
}

impl Artifact {
    fn bind(
        &self,
        cas: &Cas,
        manifest: &Manifest,
        directory: &str,
        mut inputs: ReportSourcesV1,
    ) -> Result<(serde_json::Value, Packages), String> {
        require(
            matches!(self.schema.as_str(), SCHEMA | BACKLOG_SCHEMA),
            "Unsupported portable Scenario artifact",
        )?;
        for id in [
            &self.definition.id,
            &self.definition.step,
            &self.definition.result,
        ] {
            name(id)?;
        }
        let task = &self.definition.task;
        self.definition.limits.validate()?;
        require(
            task.limits.tokens <= self.definition.limits.max_tokens
                && task.limits.wall_ms > 0
                && task.limits.wall_ms <= self.definition.limits.max_wall_ms,
            "Portable Task exceeds Scenario limits",
        )?;
        require(
            !self.inputs.is_empty()
                && inputs.sources.keys().eq(self.inputs.keys())
                && self.schemas.len() <= 16,
            "Portable Scenario requires the exact declared named report sources",
        )?;
        for (id, dependency) in &self.schemas {
            name(id)?;
            let path = artifact_path(directory, &dependency.path)?;
            let bytes = captured_file(cas, manifest, &path)?;
            require(
                digest(&bytes) == dependency.digest,
                &format!("Portable Scenario schema {id} digest differs"),
            )?;
            let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
            let schema: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| e.to_string())?;
            require(
                schema.is_object(),
                "Portable dependency must be a JSON schema object",
            )?;
            require(
                !inputs.sources.contains_key(id),
                "Portable schema name collides with a caller input",
            )?;
            inputs.sources.insert(
                id.clone(),
                DocumentSourceV1 {
                    title: format!("JSON Schema {id}"),
                    uri: format!("repo:{path}"),
                    revision: dependency.digest.clone(),
                    text,
                },
            );
        }
        inputs.validate()?;
        let pins = self
            .packages
            .iter()
            .map(|(name, pin)| {
                let mut pin = pin.clone();
                pin.path = artifact_path(directory, &pin.path)?;
                Ok((name.clone(), pin))
            })
            .collect::<Result<_, String>>()?;
        let mut document = json!({
            "schema": "af.task-file/1",
            "task_id": "portable-template",
            "kind": task.kind,
            "goal": task.goal,
            "pipeline": {"name": task.pipeline, "fallback": "refuse"},
            "strategy": "small",
            "limits": task.limits,
            "requirements": {"scenario_artifact": hash(self)?}
        });
        document["requirements"][SOURCES] =
            serde_json::to_value(inputs).map_err(|e| e.to_string())?;
        scenario_template::validate_document(&document, None, task.limits.tokens)?;
        Ok((
            document,
            Packages {
                pins,
                kind_package: task.kind_package.clone(),
            },
        ))
    }
}

pub(crate) fn prepare(
    repo: PathBuf,
    state: PathBuf,
    file: &str,
    inputs: &Path,
    local: Option<&Path>,
) -> Result<Bundle, String> {
    require(
        safe_relative_path(file) && file.ends_with(".json"),
        "Portable Scenario file must be a repository-relative JSON path",
    )?;
    let directory = file.rfind('/').map_or("", |slash| &file[..=slash]);
    let bytes = af_scenario::store::read(inputs, MAX_REPORT_SOURCES_FILE_BYTES as u64)?;
    let inputs: Inputs = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let home = tempfile::tempdir().map_err(|e| e.to_string())?;
    let git = Repo::open(&repo, home.path());
    let top = git
        .text(&["rev-parse", "--show-toplevel"])
        .map_err(|e| e.to_string())?;
    let repo = PathBuf::from(
        top.strip_suffix('\n')
            .ok_or("Git omitted repository root")?,
    );
    let commit = git.rev_parse("HEAD").map_err(|e| e.to_string())?;
    let (template, (definition, backlog)) = scenario_template::pin_with(
        repo.clone(),
        state,
        commit.clone(),
        |cas, manifest, delivery| {
            let bytes = captured_file(cas, manifest, file)?;
            let artifact: Artifact = parse(Path::new(file), &bytes)?;
            let inputs = inputs.bind(&repo, &artifact, &digest(&bytes), commit)?;
            inputs.validate()?;
            let (mut document, packages) = artifact.bind(cas, manifest, directory, inputs)?;
            if artifact.schema == BACKLOG_SCHEMA {
                document["requirements"][BACKLOG_FILE] = json!(file);
            }
            let names: BTreeSet<_> = packages.pins.keys().cloned().collect();
            let task = &artifact.definition.task;
            let (authority, settings, compiler) = capture_authority_with_packages(
                cas,
                manifest,
                local,
                &task.kind,
                delivery,
                Some(packages),
            )?;
            validate_closure(
                &compiler,
                &settings,
                task,
                names,
                artifact.schema == BACKLOG_SCHEMA,
            )?;
            Ok((
                document,
                authority,
                settings,
                (artifact.definition, artifact.schema == BACKLOG_SCHEMA),
            ))
        },
    )?;
    let template_id = template.id()?;
    let assessment = step_result(&definition.step);
    let mut scenario = Scenario {
        schema: "af/scenario@11".into(),
        inputs: BTreeMap::new(),
        outputs: BTreeMap::from([(definition.result.clone(), Port::Task)]),
        steps: vec![Step {
            id: definition.step,
            inputs: BTreeMap::new(),
            outputs: BTreeMap::from([("result".into(), Port::Task)]),
            when: None,
            max_attempts: 1,
            timeout_ms: definition.task.limits.wall_ms,
            on_timeout: Timeout::Fail,
            operation: Kind::FreshTask {
                template: Box::new(template.clone()),
                bindings: BTreeMap::new(),
            },
        }],
        result: BTreeMap::from([(definition.result, assessment.clone())]),
        completion: vec![Binding::Literal {
            value: Box::new(Datum::Boolean(true)),
        }],
        authority: Authority {
            task_plans: BTreeSet::new(),
            task_templates: BTreeSet::from([template_id]),
            grants: BTreeSet::new(),
            decision_owners: BTreeSet::new(),
        },
        limits: definition.limits,
        admission: None,
    };
    if backlog {
        let sources = bound_sources(template.document["requirements"].as_object())?
            .ok_or("Missing portable backlog sources")?;
        let source = sources
            .sources
            .get(BATCH_SOURCE)
            .ok_or("Missing frozen backlog batch")?;
        let batch: af_contracts::backlog::batch::assessment::CollectedBatch =
            serde_json::from_str(&source.text).map_err(|e| e.to_string())?;
        let inputs = BTreeMap::from([
            ("assessment".into(), assessment),
            ("subject".into(), literal(batch.reference.batch_id)),
            ("revision".into(), literal(batch.reference.policy_id)),
        ]);
        for (id, operation) in [
            ("settle-backlog", Operation::BacklogSettleV1),
            ("dispatch-backlog", Operation::BacklogDispatchV1),
        ] {
            let mut inputs = inputs.clone();
            if operation == Operation::BacklogDispatchV1 {
                inputs.insert("settlement".into(), step_result("settle-backlog"));
            }
            let grant = Grant {
                adapter: "builtin".into(),
                operation,
                destination: format!("local-backlog:{}", batch.reference.project_id),
            };
            scenario.authority.grants.insert(grant.clone());
            scenario.outputs.insert(id.into(), Port::Action);
            scenario.result.insert(id.into(), step_result(id));
            scenario.steps.push(Step {
                id: id.into(),
                inputs,
                outputs: BTreeMap::from([("result".into(), Port::Action)]),
                when: None,
                max_attempts: 1,
                timeout_ms: 15_000,
                on_timeout: Timeout::Fail,
                operation: Kind::Action {
                    grant,
                    requires_decision: false,
                    required_condition: None,
                },
            });
        }
    }
    let bundle = Bundle {
        schema: "af/scenario-bundle@11".into(),
        root: definition.id.clone(),
        definitions: BTreeMap::from([(definition.id, scenario)]),
    };
    bundle.validate(&crate::scenario::host::AfHost)?;
    Ok(bundle)
}

fn step_result(step: &str) -> Binding {
    Binding::Step {
        step: step.into(),
        port: "result".into(),
    }
}

fn literal(value: String) -> Binding {
    Binding::Literal {
        value: Box::new(Datum::Text(value)),
    }
}

fn validate_closure(
    compiler: &TaskPlanCompiler,
    settings: &RunAuthority,
    task: &ReportTask,
    mut names: BTreeSet<String>,
    backlog: bool,
) -> Result<(), String> {
    require(
        selected_profile(compiler, settings, &task.kind, None)? == TaskKindProfile::Report,
        "Portable Scenario requires a report Task kind",
    )?;
    let pipeline = compiler
        .pipelines()
        .get(&task.pipeline)
        .ok_or("Missing portable Pipeline")?;
    require(
        pipeline.accepts.kinds.contains(&task.kind)
            && pipeline.nodes.iter().all(|node| {
                matches!(
                    node.operator,
                    pipeline::TaskOperatorV1::Worker { .. }
                        | pipeline::TaskOperatorV1::ReportSeal {}
                        | pipeline::TaskOperatorV1::ReportCheck {}
                        | pipeline::TaskOperatorV1::Verify { .. }
                        | pipeline::TaskOperatorV1::ReportAccept {}
                )
            }),
        "Portable report Pipeline must use native report operators and model Workers",
    )?;
    if backlog {
        let authors: BTreeSet<_> = pipeline
            .nodes
            .iter()
            .filter_map(|node| match &node.operator {
                pipeline::TaskOperatorV1::Worker { slot } => Some(slot.clone()),
                _ => None,
            })
            .collect();
        let mut verifiers = pipeline
            .nodes
            .iter()
            .filter_map(|node| match &node.operator {
                pipeline::TaskOperatorV1::Verify { slot } => Some(slot),
                _ => None,
            })
            .peekable();
        require(
            !authors.is_empty()
                && verifiers.peek().is_some()
                && verifiers.all(|slot| {
                    pipeline
                        .slots
                        .get(slot)
                        .is_some_and(|verify| authors.is_subset(&verify.independent_from))
                }),
            "Portable backlog requires verifiers independent of every author slot",
        )?;
    }
    names.remove(&task.kind_package);
    names.remove(&task.pipeline);
    let workers = compiler.startup_workers(&task.pipeline)?;
    require(
        names == workers,
        "Portable Scenario packages must match its Task dependency closure",
    )?;
    for worker_name in workers {
        let worker = compiler
            .worker(&worker_name)
            .ok_or("Missing portable Worker")?;
        require(
            matches!(worker.runner, TaskWorkerRunner::Model { .. })
                && worker
                    .signature
                    .effects
                    .iter()
                    .all(|effect| effect == "read-source"),
            "Portable Workers must be read-only models",
        )?;
        require(
            settings.providers.contains_key(&worker_name),
            &format!("Receiver must bind a Provider for portable Worker {worker_name}"),
        )?;
    }
    Ok(())
}

mod backlog;
pub(crate) use backlog::run_backlog_action;
