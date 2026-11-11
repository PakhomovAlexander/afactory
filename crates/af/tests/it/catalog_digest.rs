//! `af catalog digest` prints exactly the digest a catalog pin records, through the same recipe
//! and validation Task capture uses, and reads nothing but the package directory.

use crate::common::{Sandbox, Signer, VERSION, af, af_binary, err, out, write};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Output;

fn workspace() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn digest(cwd: &Path, package: &Path) -> Output {
    af().current_dir(cwd)
        .args(["catalog", "digest"])
        .arg(package)
        .output()
        .unwrap()
}

/// Every `[packages]` pin of one catalog file: name, digest and package path.
fn pins(catalog: &Path) -> Vec<(String, String, String)> {
    let text = std::fs::read_to_string(catalog).unwrap();
    let value: toml::Table = toml::from_str(&text).unwrap();
    value["packages"]
        .as_table()
        .unwrap()
        .iter()
        .map(|(name, pin)| {
            (
                name.clone(),
                pin["digest"].as_str().unwrap().to_owned(),
                pin["path"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn assert_reproduces(repo: &Path, catalog: &Path) -> usize {
    let pins = pins(catalog);
    for (name, pin, path) in &pins {
        let output = digest(repo, Path::new(path));
        assert!(output.status.success(), "{name}: {}", err(&output));
        assert_eq!(out(&output), format!("{pin}\n"), "{name} at {path}");
        assert!(output.stderr.is_empty(), "{name}: {}", err(&output));
    }
    pins.len()
}

/// Every file of a directory tree with its bytes and modification time.
fn tree(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
                files.insert(path.clone(), (std::fs::read(&path).unwrap(), modified));
            }
        }
    }
    files
}

/// A private, writable copy of a package: bytes only. `std::fs::copy` would carry over the
/// source's file modes, and the checkout under test may be read-only.
fn copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy(&entry.path(), &target);
        } else {
            std::fs::write(target, std::fs::read(entry.path()).unwrap()).unwrap();
        }
    }
}

/// AF's own catalog publishes the pins the issue's workaround matched (ten when it was filed;
/// the catalog has grown since). Every pin it holds now, and every pin of the runtime fixture
/// catalogs, reproduces byte for byte. `bound-inputs` and `continue` are excluded: their
/// catalogs carry placeholder digests their tests replace before planning.
#[test]
fn every_published_package_pin_reproduces() {
    let root = workspace();
    let own = assert_reproduces(&root, &root.join(".af/task-catalog.toml"));
    assert!(own >= 10, "AF's catalog publishes {own} pins");

    assert_eq!(
        assert_reproduces(
            &root.join("fixtures/native-catalog-contracts"),
            &root.join("fixtures/native-catalog-contracts/.af/task-catalog.toml")
        ),
        24
    );
    for catalog in [
        "fixtures/issue-workflows/catalog.toml",
        "fixtures/issue-triage/catalog.toml",
    ] {
        assert_reproduces(&root, &root.join(catalog));
    }
    let fixtures = root.join("fixtures/task-runtime");
    let mut checked = 0;
    for entry in std::fs::read_dir(&fixtures).unwrap() {
        let repo = entry.unwrap().path();
        let catalog = repo.join(".af/task-catalog.toml");
        if repo.ends_with("bound-inputs") || repo.ends_with("continue") || !catalog.is_file() {
            continue;
        }
        checked += assert_reproduces(&repo, &catalog);
    }
    assert!(checked >= 28, "fixture catalogs held {checked} pins");
    let mut telemetry = 0;
    for entry in std::fs::read_dir(root.join("fixtures/telemetry-bench")).unwrap() {
        let repo = entry.unwrap().path().join("repo");
        let catalog = repo.join(".af/task-catalog.toml");
        if catalog.is_file() {
            telemetry += assert_reproduces(&repo, &catalog);
        }
    }
    assert_eq!(
        telemetry, 20,
        "every current telemetry fixture package is pinned"
    );
}

const RAW_CODEX_MODELS: &[u8] =
    include_bytes!("../fixtures/provider-models/codex-provider-models-raw-20261107.json");
const PROJECTED_CODEX_MODELS: &[u8] =
    include_bytes!("../fixtures/provider-models/codex-provider-models-recorded-20261107.json");

fn codex_observation_provenance() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../fixtures/provider-models/codex-provider-models-recorded-20261107.provenance.json"
    ))
    .unwrap()
}

fn response_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    format!(
        "sha256:{}",
        af_contracts::hex::encode(&Sha256::digest(bytes))
    )
}

/// Validate both recorded byte streams before reproducing the projection. Filtering the raw
/// array preserves model objects, their order and every field outside that array.
fn validate_recorded_codex_projection(
    raw_bytes: &[u8],
    bytes: &[u8],
    provenance: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    static SCHEMAS: std::sync::OnceLock<(jsonschema::Validator, jsonschema::Validator)> =
        std::sync::OnceLock::new();
    let (schema, provider_schema) = SCHEMAS.get_or_init(|| {
        (
            crate::schemas::validator("codex-model-catalog-observation-v1.json"),
            crate::schemas::validator("provider-models-v2.json"),
        )
    });
    if !schema.is_valid(provenance) {
        return Err("invalid observation provenance schema".into());
    }
    for (label, content, digest) in [
        ("raw", raw_bytes, "raw_response_sha256"),
        ("projected", bytes, "recorded_projection_sha256"),
    ] {
        let document: serde_json::Value =
            serde_json::from_slice(content).map_err(|error| error.to_string())?;
        if !provider_schema.is_valid(&document) {
            return Err(format!("invalid {label} provider-models schema"));
        }
        if provenance[digest] != response_digest(content) {
            return Err(format!("{label} response digest mismatch"));
        }
    }
    let mut projected: serde_json::Value = serde_json::from_slice(raw_bytes).unwrap();
    let providers = projected["providers"].as_array_mut().unwrap();
    if providers.len() != 1 {
        return Err("expected exactly one raw Provider".into());
    }
    providers[0]["models"]
        .as_array_mut()
        .unwrap()
        .retain(|model| matches!(model["id"].as_str(), Some("gpt-6.1-sol" | "gpt-6.1-terra")));
    let observation: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    if projected != observation {
        return Err("recorded projection differs from filtered raw response".into());
    }
    Ok(observation)
}

/// The coordinator's real token-free observation is separate from the synthetic parser
/// fixtures. It proves harness-advertised support only; inference access needs admission.
fn recorded_codex_models() -> Vec<serde_json::Value> {
    let provenance = codex_observation_provenance();
    let observation =
        validate_recorded_codex_projection(RAW_CODEX_MODELS, PROJECTED_CODEX_MODELS, &provenance)
            .unwrap();
    let schema = crate::schemas::validator("codex-model-catalog-observation-v1.json");
    let mut old = provenance.clone();
    old["schema"] = "af.codex-model-catalog-observation/0".into();
    assert!(!schema.is_valid(&old));
    assert_eq!(
        provenance["raw_response_sha256"],
        "sha256:840e259bf064b579ff4d9362d38ef6bde3c9592e11b893b9ce5f3beb9f5d3ffe"
    );
    assert_eq!(observation["exit_code"], 0);
    assert_eq!(observation["token_free"], true);
    let providers = observation["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 1);
    let provider = &providers[0];
    assert_eq!(provider["kind"], "codex");
    assert_eq!(provider["state"], "listed");
    for field in ["source", "harness_version", "freshness"] {
        assert_eq!(provider[field], provenance[field], "{field}");
    }
    let models = provider["models"].as_array().unwrap();
    assert_eq!(
        models
            .iter()
            .map(|model| model["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["gpt-6.1-sol", "gpt-6.1-terra"]
    );
    // The historical projection selects only the two 6.1 models. Audit replacement roles
    // against the complete authenticated raw observation, which also lists Sol and Astra.
    let raw: serde_json::Value = serde_json::from_slice(RAW_CODEX_MODELS).unwrap();
    raw["providers"][0]["models"].as_array().unwrap().clone()
}

#[test]
fn recorded_codex_projection_rejects_changed_selected_models_and_other_fields() {
    let provenance = codex_observation_provenance();
    validate_recorded_codex_projection(RAW_CODEX_MODELS, PROJECTED_CODEX_MODELS, &provenance)
        .unwrap();
    let mut reformatted = PROJECTED_CODEX_MODELS.to_vec();
    reformatted.push(b'\n');
    assert_eq!(
        validate_recorded_codex_projection(RAW_CODEX_MODELS, &reformatted, &provenance)
            .unwrap_err(),
        "projected response digest mismatch"
    );
    // Each mutation remains schema-valid. Update its digest too: the equality check must
    // detect a rewritten selected model or unrelated field, independently of byte hashing.
    for (pointer, replacement) in [
        (
            "/providers/0/models/0/display_name",
            serde_json::json!("changed Sol"),
        ),
        (
            "/providers/0/models/1/effort_levels",
            serde_json::json!(["high"]),
        ),
        ("/proves", serde_json::json!("changed scope")),
        ("/providers/0/skipped_entries", serde_json::json!(1)),
        (
            "/providers/0/freshness/fetched_at",
            serde_json::json!("2026-11-06T00:00:00Z"),
        ),
        (
            "/providers/0/static_rules/effort_levels",
            serde_json::json!(["high"]),
        ),
    ] {
        let mut altered: serde_json::Value =
            serde_json::from_slice(PROJECTED_CODEX_MODELS).unwrap();
        *altered.pointer_mut(pointer).unwrap() = replacement;
        let bytes = serde_json::to_vec(&altered).unwrap();
        let mut updated = provenance.clone();
        updated["recorded_projection_sha256"] = response_digest(&bytes).into();
        let error =
            validate_recorded_codex_projection(RAW_CODEX_MODELS, &bytes, &updated).unwrap_err();
        assert_eq!(
            error, "recorded projection differs from filtered raw response",
            "{pointer}"
        );
    }

    let mut raw: serde_json::Value = serde_json::from_slice(RAW_CODEX_MODELS).unwrap();
    raw["providers"][0]["models"][2]["default_effort"] = "high".into();
    let bytes = serde_json::to_vec(&raw).unwrap();
    assert_eq!(
        validate_recorded_codex_projection(&bytes, PROJECTED_CODEX_MODELS, &provenance)
            .unwrap_err(),
        "raw response digest mismatch"
    );
    let mut updated = provenance.clone();
    updated["raw_response_sha256"] = response_digest(&bytes).into();
    assert_eq!(
        validate_recorded_codex_projection(&bytes, PROJECTED_CODEX_MODELS, &updated).unwrap_err(),
        "recorded projection differs from filtered raw response"
    );
    raw["token_free"] = false.into();
    let bytes = serde_json::to_vec(&raw).unwrap();
    updated["raw_response_sha256"] = response_digest(&bytes).into();
    assert_eq!(
        validate_recorded_codex_projection(&bytes, PROJECTED_CODEX_MODELS, &updated).unwrap_err(),
        "invalid raw provider-models schema"
    );
}

fn assert_recorded_codex_support(
    models: &[serde_json::Value],
    name: &str,
    model: &str,
    effort: &str,
) {
    let listed = models
        .iter()
        .find(|row| row["id"].as_str() == Some(model))
        .unwrap_or_else(|| panic!("{name}: {model} absent from recorded observation"));
    assert!(
        listed["effort_levels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|level| level.as_str() == Some(effort)),
        "{name}: {model}/{effort} absent from recorded observation"
    );
}

/// Exact approved replacement Workers, including personal accounts and independent roles.
#[test]
fn every_shipped_model_worker_uses_supported_61_models_and_independent_judges() {
    use af_pipeline_config::task::catalog::{TaskWorkerManifest, TaskWorkerRunner};
    let root = workspace();
    let catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(root.join(".af/task-catalog.toml")).unwrap())
            .unwrap();
    let expected = BTreeMap::from([
        ("backlog/triage-author", ("gpt-6.1-sol", "medium", "author")),
        (
            "backlog/triage-verifier",
            ("gpt-6.1-terra", "medium", "verify"),
        ),
        (
            "issue-internal/implementer",
            ("gpt-6.1-sol", "medium", "implement"),
        ),
        (
            "issue-internal/researched-implementer",
            ("gpt-6.1-sol", "medium", "implement"),
        ),
        ("issue-internal/kimi", ("k3.1-pro", "high", "implement")),
        ("issue-internal/bugs", ("gpt-6.1-terra", "high", "review")),
        (
            "issue-internal/correctness",
            ("gpt-6-sol", "high", "review"),
        ),
        (
            "issue-internal/evaluator",
            ("gpt-6-astra", "medium", "evaluate"),
        ),
        (
            "issue-internal/light-evaluator",
            ("gpt-6-sol", "medium", "evaluate"),
        ),
        (
            "issue-internal/experiment-evaluator",
            ("gpt-6-astra", "medium", "evaluate"),
        ),
        (
            "issue-internal/investigator",
            ("gpt-6.1-sol", "high", "author"),
        ),
        (
            "issue-internal/designer",
            ("gpt-6-astra", "medium", "author"),
        ),
        (
            "issue-internal/report-verifier",
            ("gpt-6.1-terra", "high", "verify"),
        ),
    ]);
    let models = recorded_codex_models();
    let mut checked = std::collections::BTreeSet::new();
    for (name, pin) in catalog["packages"].as_table().unwrap() {
        let package = root.join(pin["path"].as_str().unwrap());
        let path = package.join("worker.toml");
        if !path.is_file() {
            continue;
        }
        let worker: TaskWorkerManifest =
            toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(&worker.name, name);
        assert_eq!(
            pin["version"].as_str(),
            Some(worker.version.as_str()),
            "{name}"
        );
        assert_eq!(
            pin["digest"].as_str().unwrap(),
            af_pipeline_config::lock::package_digest(name, &package).unwrap(),
            "{name}"
        );
        let &(model, effort, role) = expected
            .get(name.as_str())
            .unwrap_or_else(|| panic!("unapproved Worker {name}"));
        assert_eq!(
            worker.signature.roles,
            std::collections::BTreeSet::from([role.to_owned()]),
            "{name}"
        );
        let pi = name == "issue-internal/kimi";
        assert_eq!(
            worker.runner,
            TaskWorkerRunner::Model {
                provider_kind: if pi { "pi" } else { "codex" }.into(),
                model: model.into(),
                effort: effort.into(),
            },
            "{name}"
        );
        assert_eq!(
            catalog["providers"][name].as_str(),
            Some(if pi { "pi-personal" } else { "codex-personal" }),
            "{name}"
        );
        if !pi {
            assert_recorded_codex_support(&models, name, model, effort);
        }
        checked.insert(name.as_str());
    }
    assert_eq!(checked, expected.keys().copied().collect());
    assert_eq!(
        catalog["providers"]
            .as_table()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        checked
    );
    assert_eq!(
        catalog["independence"]["distinct_models"].as_bool(),
        Some(true)
    );
}

const LIGHT_ROLES: [(&str, &str, &str); 3] = [
    ("implementer", "implement", "Implementer"),
    ("correctness", "review", "correctness reviewer"),
    ("evaluator", "evaluate", "evaluator"),
];

fn selected_light_workers() -> (
    af_contracts::task::pipeline::PipelineDefinition,
    BTreeMap<String, af_pipeline_config::task::catalog::TaskWorkerManifest>,
) {
    let root = workspace();
    let catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(root.join(".af/task-catalog.toml")).unwrap())
            .unwrap();
    let package = |name: &str| root.join(catalog["packages"][name]["path"].as_str().unwrap());
    let pipeline = af_pipeline_config::task::parse_task_pipeline(
        &std::fs::read_to_string(package("bugfix/light-codex").join("pipeline.toml")).unwrap(),
    )
    .unwrap();
    let workers = LIGHT_ROLES
        .into_iter()
        .map(|(slot, _, _)| {
            let worker = toml::from_str(
                &std::fs::read_to_string(package(&pipeline.slots[slot].worker).join("worker.toml"))
                    .unwrap(),
            )
            .unwrap();
            (pipeline.slots[slot].worker.clone(), worker)
        })
        .collect();
    (pipeline, workers)
}

/// Shared by the positive cross-artifact audit and independent role mutations below. Display
/// labels are emitted only after the selected slot and Worker agree on the expected role.
fn validate_small_issue_rubrics(
    pipeline: &af_contracts::task::pipeline::PipelineDefinition,
    workers: &BTreeMap<String, af_pipeline_config::task::catalog::TaskWorkerManifest>,
) -> Result<(), String> {
    use af_pipeline_config::task::catalog::TaskWorkerRunner;

    let mut expected = Vec::new();
    for (slot_name, role, label) in LIGHT_ROLES {
        let slot = &pipeline.slots[slot_name];
        if slot.role != role {
            return Err(format!(
                "{slot_name}: slot.role must be {role}, got {}",
                slot.role
            ));
        }
        let worker = &workers[&slot.worker];
        if worker.signature.roles != std::collections::BTreeSet::from([role.to_owned()]) {
            return Err(format!(
                "{slot_name}: Worker.signature.roles must be [{role}], got {:?}",
                worker.signature.roles
            ));
        }
        let TaskWorkerRunner::Model {
            provider_kind,
            model,
            effort,
        } = &worker.runner
        else {
            return Err(format!("{slot_name}: must select a model Worker"));
        };
        let (expected_model, expected_effort) = match role {
            "implement" => ("gpt-6.1-sol", "medium"),
            "review" => ("gpt-6.1-terra", "high"),
            "evaluate" => ("gpt-6-sol", "medium"),
            _ => unreachable!(),
        };
        if provider_kind != "codex" || model != expected_model || effort != expected_effort {
            return Err(format!(
                "{slot_name}: expected codex {expected_model}/{expected_effort}, got {provider_kind} {model}/{effort}"
            ));
        }
        expected.push(format!("{label}: `{model}` {effort}"));
    }
    let expected = expected.join("; ");
    for path in [
        "fixtures/issue-workflows/task-packages/backlog/triage-author/instructions.md",
        "fixtures/issue-workflows/task-packages/backlog/triage-verifier/instructions.md",
        "fixtures/issue-triage/task-packages/backlog/triage-author/instructions.md",
        "fixtures/issue-triage/task-packages/backlog/triage-verifier/instructions.md",
        "fixtures/custom-tasks/triage/worker/instructions.md",
        "docs/design/issue-triage.md",
        "fixtures/issue-triage/example/task.json",
    ] {
        let text = std::fs::read_to_string(workspace().join(path)).unwrap();
        let text = if path.ends_with(".json") {
            let task: serde_json::Value = serde_json::from_str(&text).unwrap();
            task["requirements"]["rubric"].as_str().unwrap().to_owned()
        } else {
            text
        };
        let rows: Vec<_> = text
            .lines()
            .filter(|line| line.starts_with("| `S` |"))
            .collect();
        if rows.len() != 1 {
            return Err(format!("{path}: expected exactly one size/S rubric"));
        }
        let cells: Vec<_> = rows[0].split('|').map(str::trim).collect();
        if cells.len() != 6 {
            return Err(format!("{path}: expected size/S rubric columns"));
        }
        if cells[3] != format!("`light` (`{}`)", pipeline.name) {
            return Err(format!("{path}: selected pipeline differs"));
        }
        if cells[4] != expected {
            return Err(format!(
                "{path}: size/S Worker guidance differs: expected {expected}, got {}",
                cells[4]
            ));
        }
    }
    Ok(())
}

/// All staged size/S rubrics, active docs and the example agree with actual selected roles,
/// models and efforts, rather than merely matching hard-coded display labels.
#[test]
fn every_small_issue_rubric_matches_the_selected_light_worker_models_efforts_and_roles() {
    let (pipeline, workers) = selected_light_workers();
    validate_small_issue_rubrics(&pipeline, &workers).unwrap();
}

#[test]
fn small_issue_rubric_audit_rejects_slot_and_worker_role_mutations_independently() {
    let (pipeline, workers) = selected_light_workers();
    validate_small_issue_rubrics(&pipeline, &workers).unwrap();
    for (slot_name, role, _) in LIGHT_ROLES {
        let wrong_role = if role == "implement" {
            "review"
        } else {
            "implement"
        };
        let mut wrong_pipeline = pipeline.clone();
        wrong_pipeline.slots.get_mut(slot_name).unwrap().role = wrong_role.into();
        let error = validate_small_issue_rubrics(&wrong_pipeline, &workers).unwrap_err();
        assert!(
            error.starts_with(&format!("{slot_name}: slot.role")),
            "{error}"
        );

        let mut wrong_workers = workers.clone();
        wrong_workers
            .get_mut(&pipeline.slots[slot_name].worker)
            .unwrap()
            .signature
            .roles = std::collections::BTreeSet::from([wrong_role.into()]);
        let error = validate_small_issue_rubrics(&pipeline, &wrong_workers).unwrap_err();
        assert!(
            error.starts_with(&format!("{slot_name}: Worker.signature.roles")),
            "{error}"
        );
    }
}

/// A copy of a read-only package is writable, so the mutations below never need the checkout
/// to be writable, and never touch it.
#[test]
fn private_copies_of_a_read_only_package_are_writable() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let sealed = dir.path().join("sealed");
    std::fs::create_dir_all(sealed.join("outputs")).unwrap();
    for file in ["worker.toml", "outputs/report.json"] {
        let path = sealed.join(file);
        std::fs::write(&path, "x").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    }
    let private = dir.path().join("private");
    copy(&sealed, &private);
    for file in ["worker.toml", "outputs/report.json"] {
        std::fs::write(private.join(file), "y").unwrap();
        assert_eq!(std::fs::read(sealed.join(file)).unwrap(), b"x");
    }
}

/// The digest follows every digest-relevant byte and path, and only those; the command changes
/// no file of the package it reads.
#[test]
fn digest_relevant_mutations_change_the_digest_and_the_package_is_left_unchanged() {
    let root = workspace();
    let (_, pin, path) = pins(&root.join("fixtures/issue-workflows/catalog.toml"))
        .into_iter()
        .find(|(name, ..)| name == "issue-internal/bugs")
        .unwrap();
    let source = root.join(path);
    let original_source = tree(&source);
    let dir = tempfile::tempdir().unwrap();
    let package = dir.path().join("bugs");
    copy(&source, &package);
    let printed = |package: &Path| {
        let output = digest(dir.path(), package);
        assert!(output.status.success(), "{}", err(&output));
        out(&output).trim_end().to_owned()
    };

    let before = tree(&package);
    assert_eq!(printed(&package), pin, "a copy elsewhere keeps its pin");
    assert_eq!(tree(&package), before, "the command wrote to the package");

    let instructions = package.join("instructions.md");
    let original = std::fs::read(&instructions).unwrap();
    std::fs::write(&instructions, [original.as_slice(), b"\n"].concat()).unwrap();
    assert_ne!(printed(&package), pin, "instructions bytes are digested");
    std::fs::write(&instructions, &original).unwrap();
    assert_eq!(printed(&package), pin);

    std::fs::write(package.join("outputs/extra.json"), "{}").unwrap();
    assert_ne!(printed(&package), pin, "an added nested file is digested");
    std::fs::remove_file(package.join("outputs/extra.json")).unwrap();

    let schema = package.join("input.schema.json");
    std::fs::rename(&schema, package.join("input.json")).unwrap();
    assert_ne!(printed(&package), pin, "file paths are digested");
    std::fs::rename(package.join("input.json"), &schema).unwrap();

    let manifest = package.join("worker.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, text.replacen("\n", "\n# reviewed\n", 1)).unwrap();
    assert_ne!(printed(&package), pin, "manifest bytes are digested");
    std::fs::write(&manifest, &text).unwrap();
    assert_eq!(printed(&package), pin);

    assert_eq!(tree(&source), original_source, "the checkout was changed");
}

/// An invalid or incomplete package prints nothing on stdout, names the reason and exits 1.
#[test]
fn invalid_or_incomplete_packages_fail_closed() {
    let root = workspace();
    let dir = tempfile::tempdir().unwrap();
    let source = root.join("fixtures/issue-workflows/task-packages/issue-internal/bugs");
    let refuses = |package: &Path, needle: &str| {
        let before = tree(dir.path());
        let output = digest(dir.path(), package);
        assert_eq!(output.status.code(), Some(1), "{needle}: {}", err(&output));
        assert!(output.stdout.is_empty(), "{needle}: {}", out(&output));
        let message = err(&output);
        assert!(message.starts_with("af catalog digest: "), "{message}");
        assert!(message.contains(needle), "`{needle}` missing: {message}");
        assert_eq!(tree(dir.path()), before, "{needle}: a refusal wrote a file");
    };

    refuses(&dir.path().join("absent"), "No Task package directory");
    let empty = dir.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    refuses(&empty, "has no files");

    let headless = dir.path().join("headless");
    copy(&source, &headless);
    std::fs::remove_file(headless.join("worker.toml")).unwrap();
    refuses(&headless, "found none");

    let twice = dir.path().join("twice");
    copy(&source, &twice);
    std::fs::write(
        twice.join("pipeline.toml"),
        std::fs::read(root.join(
            "fixtures/issue-workflows/task-packages/issue-internal/light-review/pipeline.toml",
        ))
        .unwrap(),
    )
    .unwrap();
    refuses(&twice, "found pipeline.toml, worker.toml");

    let broken = dir.path().join("broken");
    copy(&source, &broken);
    let manifest = broken.join("worker.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, text.replace("[\"signature\"]", "[\"signature\"")).unwrap();
    refuses(&broken, "worker.toml");

    let floating = dir.path().join("floating");
    copy(&source, &floating);
    let manifest = floating.join("worker.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    // Whatever release the package is at, its version cut to `major.minor`.
    let mut value: toml::Value = toml::from_str(&text).unwrap();
    let version = value["version"].as_str().unwrap();
    let floating_version = version.rsplit_once('.').unwrap().0.to_owned();
    value["version"] = toml::Value::String(floating_version);
    std::fs::write(&manifest, toml::to_string(&value).unwrap()).unwrap();
    refuses(&floating, "is not one exact major.minor.patch release");

    let invalid = dir.path().join("invalid");
    copy(&source, &invalid);
    let manifest = invalid.join("worker.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, text.replace("af.worker/1", "x")).unwrap();
    refuses(
        &invalid,
        "Worker manifest has incompatible identity or protocol",
    );

    let linked = dir.path().join("linked");
    copy(&source, &linked);
    std::os::unix::fs::symlink(linked.join("instructions.md"), linked.join("alias.md")).unwrap();
    refuses(&linked, "contains a symlink at");

    let file = dir.path().join("file");
    std::fs::write(&file, "").unwrap();
    refuses(&file, "is not a directory");

    // A symlinked root is refused however its last component is spelled; the same spellings of
    // the package directory itself print its digest.
    let package = dir.path().join("package");
    copy(&source, &package);
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&package, &link).unwrap();
    let expected = out(&digest(dir.path(), &package));
    for suffix in ["", "/", "/.", "//", "/./", "/.//./."] {
        let mut spelled = link.clone().into_os_string();
        spelled.push(suffix);
        refuses(Path::new(&spelled), "is a symlink");
        let relative = format!("link{suffix}");
        refuses(Path::new(&relative), "is a symlink");
        let mut directory = package.clone().into_os_string();
        directory.push(suffix);
        let output = digest(dir.path(), Path::new(&directory));
        assert_eq!(
            output.status.code(),
            Some(0),
            "package{suffix}: {}",
            err(&output)
        );
        assert_eq!(out(&output), expected, "package{suffix}");
    }

    let missing = af().args(["catalog", "digest"]).output().unwrap();
    assert_eq!(
        missing.status.code(),
        Some(2),
        "a package directory is required"
    );
}

/// `--help` states the output, what contributes to the digest and what the command never does.
#[test]
fn help_describes_the_output_the_contributing_files_and_the_boundary() {
    for help in [
        af().args(["catalog", "digest", "--help"]).output().unwrap(),
        af().args(["help", "catalog", "digest"]).output().unwrap(),
    ] {
        assert!(help.status.success(), "{}", err(&help));
        let text = out(&help);
        for needle in [
            "Usage: af catalog digest <PACKAGE_DIR>",
            "sha256:",
            "contributes its relative path and its bytes",
            "exits 1",
            "Never:",
            "af catalog digest .af/task-packages/team/implementer",
        ] {
            assert!(text.contains(needle), "`{needle}` missing:\n{text}");
        }
    }
}

/// Every file under `root`, or none when it is absent: what af's own machine state holds.
fn state_files(root: &Path) -> Vec<PathBuf> {
    if root.exists() {
        tree(root).into_keys().collect()
    } else {
        Vec::new()
    }
}

/// The command is read-only for af itself too: in a project pinned to a release this machine
/// has not installed, with an update check due and `auto_update = "always"`, it neither
/// dispatches to nor installs the pinned release, schedules no check, activates nothing and
/// writes no self-management state; a malformed invocation of it does none of that either.
#[test]
fn the_digest_never_dispatches_installs_or_schedules_an_update_check() {
    let current = semver::Version::parse(VERSION).unwrap();
    let newer = semver::Version::new(current.major, current.minor + 1, 0).to_string();
    let keys = tempfile::tempdir().unwrap();
    let signer = Signer::new(keys.path());
    let sandbox = Sandbox::new().with_key(&signer);
    let digest = sandbox.publish("0.8.1", false);
    sandbox.publish(&newer, false);
    let real = sandbox.adopt_real_binary();
    write(
        &sandbox.path("config/af/config.toml"),
        "[self]\nauto_update = \"always\"\n",
    );
    let repo = sandbox.pinned_repo("repo", &Sandbox::lock("0.8.1", Some(&digest)));
    let package =
        workspace().join("fixtures/issue-workflows/task-packages/issue-internal/implementer");
    let expected = out(&digest_of(&package));
    let log = sandbox.path("checks.log");
    let before = (
        state_files(&sandbox.path("state")),
        state_files(&sandbox.path("cache")),
        state_files(&sandbox.versions()),
    );

    for binary in [af_binary(), real.as_path()] {
        let run = |args: &[&std::ffi::OsStr]| {
            sandbox
                .command(binary)
                .args(args)
                .current_dir(&repo)
                .env("AF_TEST_SELF_CHECK_LOG", &log)
                .output()
                .unwrap()
        };
        let output = run(&["catalog".as_ref(), "digest".as_ref(), package.as_os_str()]);
        assert!(output.status.success(), "{}", err(&output));
        assert_eq!(out(&output), expected, "the running af printed the digest");
        assert!(output.stderr.is_empty(), "{}", err(&output));

        let malformed = run(&["catalog".as_ref(), "digest".as_ref()]);
        assert_eq!(malformed.status.code(), Some(2), "{}", err(&malformed));
        assert!(!out(&malformed).contains("fake af"), "{}", out(&malformed));
    }
    assert!(!log.exists(), "no update check was scheduled");
    assert!(
        !sandbox.versions().join("0.8.1").exists(),
        "the pinned release was not installed"
    );
    assert!(!sandbox.versions().join(&newer).exists());
    assert!(sandbox.default_target().is_none(), "nothing was activated");
    assert_eq!(
        (
            state_files(&sandbox.path("state")),
            state_files(&sandbox.path("cache")),
            state_files(&sandbox.versions()),
        ),
        before,
        "af's own state is unchanged"
    );
}

/// The digest of `package` from the test's own af, with self-management off.
fn digest_of(package: &Path) -> Output {
    let output = af()
        .args(["catalog", "digest"])
        .arg(package)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", err(&output));
    output
}

/// A package that declares the typed `diff_subject` input tells its Worker to read that input,
/// never the `../diff-subject.json` sidecar ADR-0254 retired: the kernel materializes only
/// `../base` beside a judge's tree and injects the Diff Subject on the port.
#[test]
fn every_package_declaring_diff_subject_tells_its_worker_to_read_the_input() {
    let root = workspace();
    let mut declaring = std::collections::BTreeSet::new();
    for (name, _, path) in pins(&root.join(".af/task-catalog.toml")) {
        let package = root.join(path);
        let worker = package.join("worker.toml");
        if !worker.is_file() {
            continue;
        }
        let manifest: af_pipeline_config::task::catalog::TaskWorkerManifest =
            toml::from_str(&std::fs::read_to_string(worker).unwrap()).unwrap();
        if !manifest
            .signature
            .contract
            .inputs
            .contains_key("diff_subject")
        {
            continue;
        }
        declaring.insert(name);
        let instructions = std::fs::read_to_string(package.join("instructions.md")).unwrap();
        assert!(
            instructions.contains("`diff_subject` input"),
            "{}",
            package.display()
        );
        assert!(
            !instructions.contains("diff-subject.json"),
            "{}",
            package.display()
        );
    }
    assert_eq!(
        declaring,
        [
            "issue-internal/bugs",
            "issue-internal/correctness",
            "issue-internal/evaluator",
            "issue-internal/light-evaluator"
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );
}

/// Check the whole installed catalog, including every public and internal Pipeline, rather
/// than only the kernel pipelines selected by the performance Scenario.
#[test]
fn complete_catalog_captures_current_contracts_and_consistent_child_slots() {
    use af_contracts::task::pipeline::TaskOperatorV1;
    use af_pipeline_config::task::catalog::TaskPlanCompiler;

    let root = workspace();
    let temporary = tempfile::tempdir().unwrap();
    let cas = af_store::Cas::open(temporary.path().join("cas")).unwrap();
    let policy = cas.put(b"catalog-contract-test").unwrap();
    let mut compiler = TaskPlanCompiler::new(
        policy.clone(),
        policy,
        BTreeMap::new(),
        BTreeMap::new(),
        Default::default(),
    )
    .unwrap();
    let catalog: toml::Value =
        toml::from_str(&std::fs::read_to_string(root.join(".af/task-catalog.toml")).unwrap())
            .unwrap();
    for (name, value) in catalog["packages"].as_table().unwrap() {
        let pin: af_pipeline_config::task::catalog::TaskPackagePin =
            value.clone().try_into().unwrap();
        let package = root.join(&pin.path);
        let mut files = BTreeMap::new();
        for (path, (bytes, _)) in tree(&package) {
            let relative = path
                .strip_prefix(&root)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            let text = String::from_utf8_lossy(&bytes);
            for old in [
                "af/ReviewInput@1",
                "af/ImplementationInput@1",
                "af/TaskCheckReceipt@1",
            ] {
                assert!(!text.contains(old), "{name}: {relative} still names {old}");
            }
            files.insert(relative, bytes);
        }
        compiler
            .capture_package(&cas, name, &pin, &files)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    compiler.validate_dependency_closure().unwrap();
    for (name, pipeline) in compiler.pipelines() {
        for (slot_name, slot) in &pipeline.slots {
            let worker = compiler.worker(&slot.worker).unwrap();
            assert_eq!(
                worker.signature.worker_input_type.as_deref(),
                Some(slot.input_type.as_str()),
                "{name}: {slot_name}"
            );
            assert_eq!(
                worker.signature.worker_output_type.as_deref(),
                Some(slot.output_type.as_str()),
                "{name}: {slot_name}"
            );
        }
        for node in &pipeline.nodes {
            if let TaskOperatorV1::Call {
                pipeline: child,
                bindings,
                ..
            } = &node.operator
            {
                let child = &compiler.pipelines()[child];
                for (local, parent) in bindings {
                    assert_eq!(
                        pipeline.slots[parent].input_type, child.slots[local].input_type,
                        "{name}: {}: {local}",
                        node.id
                    );
                    assert_eq!(
                        pipeline.slots[parent].output_type, child.slots[local].output_type,
                        "{name}: {}: {local}",
                        node.id
                    );
                }
            }
        }
    }
}

/// Regenerate with AF_WRITE_CATALOG_CONTRACT_FIXTURES=1; the normal gate only compares.
#[test]
fn staged_catalog_contract_fixtures_match_native_interfaces() {
    use af_pipeline_config::task::catalog::TaskPlanCompiler;
    use af_pipeline_config::task::catalog::export::CatalogContractFixtures;

    let root = workspace();
    let temporary = tempfile::tempdir().unwrap();
    let cas = af_store::Cas::open(temporary.path().join("cas")).unwrap();
    let policy = cas.put(b"staged-catalog-contract-test").unwrap();
    for directory in ["fixtures/kernel-experiment", "fixtures/kernel-report"] {
        let catalog: af_pipeline_config::task::shared::SharedTaskCatalog = toml::from_str(
            &std::fs::read_to_string(root.join(directory).join("catalog.toml")).unwrap(),
        )
        .unwrap();
        let mut compiler = TaskPlanCompiler::new(
            policy.clone(),
            policy.clone(),
            BTreeMap::new(),
            BTreeMap::new(),
            Default::default(),
        )
        .unwrap();
        for (name, pin) in &catalog.packages {
            let files = tree(&root.join(&pin.path))
                .into_iter()
                .map(|(path, (bytes, _))| {
                    (
                        path.strip_prefix(&root)
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .to_owned(),
                        bytes,
                    )
                })
                .collect();
            compiler
                .capture_package(&cas, name, pin, &files)
                .unwrap_or_else(|e| panic!("{directory}: {name}: {e}"));
        }
        compiler.validate_dependency_closure().unwrap();
        let expected = CatalogContractFixtures {
            schema: "af.catalog-contract-fixtures/2".into(),
            pipelines: compiler
                .pipelines()
                .iter()
                .map(|(name, p)| (name.clone(), p.into()))
                .collect(),
            workers: catalog
                .packages
                .keys()
                .filter_map(|name| {
                    compiler
                        .worker(name)
                        .map(|w| (name.clone(), w.signature.clone()))
                })
                .collect(),
            kinds: catalog
                .packages
                .keys()
                .filter_map(|name| compiler.task_kind(name).map(|k| (name.clone(), k.clone())))
                .collect(),
        };
        let path = root.join(directory).join("contracts.json");
        if std::env::var_os("AF_WRITE_CATALOG_CONTRACT_FIXTURES").is_some() {
            let mut bytes = serde_json::to_vec_pretty(&expected).unwrap();
            bytes.push(b'\n');
            std::fs::write(&path, bytes).unwrap();
        }
        let actual: CatalogContractFixtures =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        compiler
            .check_contract_fixtures(&actual)
            .unwrap_or_else(|e| panic!("{directory}: {e}"));
        assert_eq!(actual, expected, "{directory}");
    }
}

/// Shipped Pi schemas must accept native predecessor envelopes and omitted optional ports,
/// while retaining closed input objects, exact artifact types and Snapshot requirements.
#[test]
fn fixture_pi_input_schemas_match_contracts_and_preserve_predecessor_snapshots() {
    use serde_json::{Value, json};

    for name in ["implementer", "correctness", "evaluator"] {
        let package = workspace()
            .join("fixtures/native-catalog-contracts/.af/task-packages/pi-light")
            .join(name);
        let worker: af_pipeline_config::task::catalog::TaskWorkerManifest =
            toml::from_str(&std::fs::read_to_string(package.join("worker.toml")).unwrap()).unwrap();
        let schema: Value =
            serde_json::from_slice(&std::fs::read(package.join("input.schema.json")).unwrap())
                .unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let ports = &worker.signature.contract.inputs;
        let properties = schema["properties"].as_object().unwrap();
        assert_eq!(properties.len(), ports.len(), "{name}");
        let required = schema["required"].as_array().unwrap();
        assert_eq!(
            required.len(),
            ports.values().filter(|p| !p.optional).count()
        );
        let mut inputs = json!({});
        for (port, contract) in ports {
            assert_eq!(required.contains(&json!(port)), !contract.optional);
            assert_eq!(properties[port]["items"]["additionalProperties"], false);
            let value = json!([{
                "artifact_id": format!("sha256:{}", "1".repeat(64)),
                "artifact_type": contract.artifact_type,
                "snapshot_id": format!("sha256:{}", "2".repeat(64)),
                "payload": {}
            }]);
            inputs[port] = value;
        }
        assert!(validator.is_valid(&inputs), "{name}: {inputs}");
        for port in ports.keys() {
            let mut wrong_type = inputs.clone();
            wrong_type[port][0]["artifact_type"] = json!("af/Unexpected@1");
            assert!(!validator.is_valid(&wrong_type), "{name}/{port}: type");
            let mut extra = inputs.clone();
            extra[port][0]["unexpected"] = json!(true);
            assert!(
                !validator.is_valid(&extra),
                "{name}/{port}: closed envelope"
            );
            let mut bad_snapshot = inputs.clone();
            bad_snapshot[port][0]["snapshot_id"] = json!("not-a-digest");
            assert!(
                !validator.is_valid(&bad_snapshot),
                "{name}/{port}: Snapshot"
            );
        }
        for (port, contract) in ports {
            if contract.optional {
                inputs.as_object_mut().unwrap().remove(port);
            } else if contract.artifact_type != "af/SourceTree@1"
                && matches!(
                    contract.affinity,
                    af_contracts::task::pipeline::PortAffinityV1::Unbound {}
                )
            {
                inputs[port][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("snapshot_id");
            }
        }
        assert!(validator.is_valid(&inputs), "{name}: optional inputs");
        let mut missing_snapshot = inputs.clone();
        missing_snapshot["source"][0]
            .as_object_mut()
            .unwrap()
            .remove("snapshot_id");
        assert!(
            !validator.is_valid(&missing_snapshot),
            "{name}: source Snapshot"
        );
        inputs["unexpected"] = json!([]);
        assert!(!validator.is_valid(&inputs), "{name}: closed inputs");
    }
}
