//! Lower a committed portable artifact to an ordinary, receiver-specific Scenario bundle.
use super::*;
use af_contracts::task::document::DocumentSourceV1;
use af_contracts::task::report_domain::{MAX_REPORT_SOURCES_FILE_BYTES, ReportSourcesV1};
use af_pipeline_config::task::shared::safe_relative_path;
use af_scenario::contract::{
    Authority, Binding, Bundle, Datum, Kind, Limits, Port, Scenario, Step, Timeout, digest, hash,
    name, require,
};

const SCHEMA: &str = "af/scenario-artifact@1";
pub(super) const SOURCES: &str = "scenario_portable_sources";

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

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum InputType {
    ReportSource,
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
            self.schema == SCHEMA,
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
    let inputs: ReportSourcesV1 = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    inputs.validate()?;
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
    let (template, definition) =
        scenario_template::pin_with(repo, state, commit, |cas, manifest, delivery| {
            let artifact: Artifact = parse(Path::new(file), &captured_file(cas, manifest, file)?)?;
            let (document, packages) = artifact.bind(cas, manifest, directory, inputs)?;
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
            validate_closure(&compiler, &settings, task, names)?;
            Ok((document, authority, settings, artifact.definition))
        })?;
    let template_id = template.id()?;
    let bundle = Bundle {
        schema: "af/scenario-bundle@10".into(),
        root: definition.id.clone(),
        definitions: BTreeMap::from([(
            definition.id,
            Scenario {
                schema: "af/scenario@10".into(),
                inputs: BTreeMap::new(),
                outputs: BTreeMap::from([(definition.result.clone(), Port::Task)]),
                steps: vec![Step {
                    id: definition.step.clone(),
                    inputs: BTreeMap::new(),
                    outputs: BTreeMap::from([("result".into(), Port::Task)]),
                    when: None,
                    max_attempts: 1,
                    timeout_ms: definition.task.limits.wall_ms,
                    on_timeout: Timeout::Fail,
                    operation: Kind::FreshTask {
                        template: Box::new(template),
                        bindings: BTreeMap::new(),
                    },
                }],
                result: BTreeMap::from([(
                    definition.result,
                    Binding::Step {
                        step: definition.step,
                        port: "result".into(),
                    },
                )]),
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
            },
        )]),
    };
    bundle.validate(&crate::scenario::host::AfHost)?;
    Ok(bundle)
}

fn validate_closure(
    compiler: &TaskPlanCompiler,
    settings: &RunAuthority,
    task: &ReportTask,
    mut names: BTreeSet<String>,
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
