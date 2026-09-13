//! Fixed implementation v1 compiles into a captured public Pipeline. This is a version
//! adapter only: all dispatch, accounting, acceptance and delivery use the common runtime.
use super::*;
use review_core::task::pipeline::{PipelineDefinitionV1, TaskOperatorV1};
use review_runner::task::legacy::LegacyTaskProtocol;

pub(crate) fn start_legacy(options: crate::task::TaskOptions) -> Result<i32, String> {
    let started = clock()?;
    let (repo, state) = state_path(&options.repo, options.state.as_deref())?;
    std::fs::create_dir_all(&state).map_err(|e| e.to_string())?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let store = EventStore::open(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    let git_home = tempfile::tempdir().map_err(|e| e.to_string())?;
    let source_repo = Repo::open(&repo, git_home.path());
    let policy_source = Capture::new(&source_repo, &cas)
        .committed(&options.authority)
        .map_err(|e| e.to_string())?;
    let loaded = crate::task::load_authority(&options, &policy_source.manifest, &cas)?;
    let worker_wall = options
        .timeout
        .unwrap_or(std::time::Duration::from_secs(
            loaded.pipeline.timeout_seconds,
        ))
        .as_millis();
    let worker_wall: u64 = worker_wall
        .try_into()
        .map_err(|_| "Worker timeout overflow")?;
    let checks: BTreeMap<_, _> = loaded
        .pipeline
        .check_definitions()
        .into_iter()
        .map(|c| (c.name.clone(), c))
        .collect();
    let check_wall = loaded
        .pipeline
        .check_timeout_seconds
        .checked_mul(1000)
        .and_then(|n| n.checked_mul(checks.len() as u64))
        .ok_or("Check timeout overflow")?;
    let code_policy = CodeTaskPolicy {
        schema: "af.code-task-policy/1".into(),
        checks,
        check_wall_ms: check_wall,
        check_process_wall_ms: Some(
            loaded
                .pipeline
                .check_timeout_seconds
                .checked_mul(1000)
                .ok_or("Check timeout overflow")?,
        ),
        require_container: false,
    };
    code_policy.validate()?;
    let code_policy_id = cas
        .put_json(&serde_json::to_value(&code_policy).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let legacy_authority = json!({"schema":"af.fixed-implementation-adapter/1","pipeline_id":loaded.pipeline_artifact_id,"lock_id":loaded.lock_artifact_id,"project_id":loaded.project_artifact_id,"workers":[loaded.implementer_authority,loaded.evaluator_authority]});
    let mut pipeline: PipelineDefinitionV1 =
        toml::from_str(include_str!("legacy-assets/implementation/pipeline.toml"))
            .map_err(|e| e.to_string())?;
    pipeline.name = "compat/implementation".into();
    for (slot, definition) in &mut pipeline.slots {
        definition.worker = format!("compat/{slot}");
    }
    for node in &mut pipeline.nodes {
        if let TaskOperatorV1::Check { checks } = &mut node.operator {
            *checks = code_policy.checks.keys().cloned().collect();
        }
    }
    let mut package_files = BTreeMap::new();
    package_files.insert(
        pipeline.name.clone(),
        BTreeMap::from([
            (
                "pipeline.toml".into(),
                toml::to_string(&pipeline)
                    .map_err(|e| e.to_string())?
                    .into_bytes(),
            ),
            (
                "legacy-authority.json".into(),
                serde_json::to_vec(&legacy_authority).map_err(|e| e.to_string())?,
            ),
        ]),
    );
    for (role, package, template, input, output, protocol) in [
        (
            "implementer",
            &loaded.implementer,
            include_str!("legacy-assets/implementer/worker.toml"),
            include_bytes!("legacy-assets/implementer/input.schema.json").as_slice(),
            include_bytes!("legacy-assets/implementer/outputs/report.schema.json").as_slice(),
            LegacyTaskProtocol::ImplementV1,
        ),
        (
            "evaluator",
            &loaded.evaluator,
            include_str!("legacy-assets/evaluator/worker.toml"),
            include_bytes!("legacy-assets/evaluator/input.schema.json").as_slice(),
            include_bytes!("legacy-assets/evaluator/outputs/result.schema.json").as_slice(),
            LegacyTaskProtocol::EvaluateV1,
        ),
    ] {
        // Old native packages carried ambient credentials and caller-owned security flags.
        // They need explicit model bindings in the new catalog before they can be admitted.
        let program = Path::new(&package.runner.program)
            .file_name()
            .and_then(|p| p.to_str())
            .unwrap_or_default();
        if matches!(program, "claude" | "codex") {
            return Err("Fixed v1 model Workers require migration to explicit Task catalog Provider bindings; use af task --file after migration".into());
        }
        let mut manifest: TaskWorkerManifest =
            toml::from_str(template).map_err(|e| e.to_string())?;
        manifest.name = format!("compat/{role}");
        manifest
            .signature
            .attempt
            .as_mut()
            .ok_or("Missing compatibility Worker allowance")?
            .wall_ms = worker_wall;
        if role == "evaluator" {
            manifest.signature.evidence =
                BTreeMap::from([("result".into(), BTreeSet::from([code_policy_id.clone()]))]);
        }
        manifest.runner = TaskWorkerRunner::LegacyTaskCommand {
            command: serde_json::from_value(
                serde_json::to_value(&package.runner).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?,
            protocol,
        };
        let mut files = package.files().clone();
        for reserved in [
            "worker.toml",
            "input.schema.json",
            "instructions.md",
            "legacy-authority.json",
            "outputs/report.schema.json",
            "outputs/result.schema.json",
        ] {
            if files.contains_key(reserved) {
                return Err(format!(
                    "Legacy Worker has a reserved compatibility path: {reserved}"
                ));
            }
        }
        files.insert(
            "worker.toml".into(),
            toml::to_string(&manifest)
                .map_err(|e| e.to_string())?
                .into_bytes(),
        );
        files.insert("input.schema.json".into(), input.to_vec());
        files.insert(
            "instructions.md".into(),
            package
                .file("reviewer.md")
                .ok_or("Legacy Worker lacks reviewer.md")?
                .to_vec(),
        );
        files.insert(
            format!(
                "outputs/{}.schema.json",
                if role == "implementer" {
                    "report"
                } else {
                    "result"
                }
            ),
            output.to_vec(),
        );
        files.insert(
            "legacy-authority.json".into(),
            serde_json::to_vec(&legacy_authority).map_err(|e| e.to_string())?,
        );
        package_files.insert(manifest.name, files);
    }
    let mut virtual_files = BTreeMap::new();
    let mut pins = BTreeMap::new();
    for (name, files) in package_files {
        let path = format!(".af/task-compat/{name}");
        let digest = review_config::lock::package_digest_from_files(&files);
        pins.insert(
            name,
            TaskPackagePin {
                version: "1.0.0".into(),
                digest,
                path: path.clone(),
            },
        );
        virtual_files.extend(files.into_iter().map(|(p, b)| (format!("{path}/{p}"), b)));
    }
    let catalog = TaskCatalog {
        schema: "af.task-catalog/1".into(),
        provider_admission: None,
        selection: BTreeMap::new(),
        no_match: review_config::task::selection::NoMatchPolicy::Refuse,
        developers: None,
        planner: None,
        code_policy: Some(".af/task-compat/code-policy.toml".into()),
        document_policy: None,
        review: None,
        packages: pins,
        kinds: BTreeMap::new(),
        imports: BTreeSet::new(),
        independence: IndependencePolicyV1::default(),
        providers: BTreeMap::new(),
    };
    virtual_files.insert(
        catalog.code_policy.clone().expect("legacy code policy"),
        toml::to_string(&code_policy)
            .map_err(|e| e.to_string())?
            .into_bytes(),
    );
    virtual_files.insert(
        ".af/task-catalog.toml".into(),
        toml::to_string(&catalog)
            .map_err(|e| e.to_string())?
            .into_bytes(),
    );
    let mut entries = Vec::new();
    for (path, bytes) in virtual_files {
        entries.push(review_source_git::Entry {
            path,
            kind: EntryKind::File,
            content: cas.put(&bytes).map_err(|e| e.to_string())?,
            size: bytes.len() as u64,
        });
    }
    let authority_manifest = Manifest::new(entries).map_err(|e| e.to_string())?;
    let authority = capture_authority(&cas, &authority_manifest, None, "implement")?;
    let source = if options.uncommitted {
        Capture::new(&source_repo, &cas)
            .dirty()
            .map_err(|e| e.to_string())?
    } else {
        policy_source
    };
    let file = TaskFile {
        issue: None,
        requirements: None,
        document_sources: None,
        schema: "af.task-file/1".into(),
        task_id: crate::task::task_id(&repo, &source.content_digest, &loaded.pipeline_artifact_id),
        kind: "implement".into(),
        verification: Some(FileVerification::Evaluation),
        goal: options.goal,
        pipeline: Some(PipelineChoiceV1 {
            name: pipeline.name,
            fallback: PipelineFallbackV1::Refuse,
        }),
        strategy: "light".into(),
        facts: BTreeMap::new(),
        limits: FileLimits {
            tokens: loaded.pipeline.run_tokens,
            max_attempts: 3,
            wall_ms: worker_wall
                .checked_mul(2)
                .and_then(|n| n.checked_add(check_wall))
                // V1 bounded individual processes, but had no overall Task deadline. Bound
                // capture/admission separately without reducing either declared Worker limit.
                .and_then(|n| n.checked_add(60_000))
                .ok_or("Task timeout overflow")?,
            verification: VerificationReserveV1 {
                tokens: 0,
                attempts: 2,
                wall_ms: worker_wall
                    .checked_add(check_wall)
                    .ok_or("Verifier timeout overflow")?,
            },
        },
    };
    let bytes = serde_json::to_vec(&file).map_err(|e| e.to_string())?;
    let options = StartOptions {
        source_bindings: None,
        file: PathBuf::new(),
        bindings: None,
        repo,
        state: Some(state),
        authority: options.authority,
        uncommitted: options.uncommitted,
        json: options.json,
        plan_only: false,
        timeout_secs: None,
    };
    start_captured(
        options,
        file,
        bytes,
        started,
        cas,
        store,
        source,
        authority,
        Some(loaded.pipeline.attempt_tokens),
    )
}
