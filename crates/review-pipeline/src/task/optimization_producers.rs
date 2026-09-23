//! Source-side constructors for optimization evidence. Callers are installed domain operations:
//! policy, engine and producer identities must come from the admitted Task, never Worker output.
//! These constructors read verified CAS manifests; they do not execute candidate files or grant
//! acceptance. A later independent check/evaluation must bind the returned final Snapshot.

use std::collections::{BTreeMap, BTreeSet};

use review_core::Producer;
use review_core::task::optimization_experiment::*;
use review_source_git::task::{capture_snapshot, descends_from, read_snapshot};
use review_source_git::{Entry, EntryKind, Manifest, decode_path};
use review_store::Cas;

const CATALOG: &str = ".af/task-catalog.toml";
const LOCK: &str = ".af/af.lock";
const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PACKAGE_BYTES: u64 = 32 * 1024 * 1024;

fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

fn beneath(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn entries(manifest: &Manifest) -> Result<BTreeMap<String, &Entry>, String> {
    let mut result = BTreeMap::new();
    for entry in &manifest.entries {
        let path = String::from_utf8(decode_path(&entry.path))
            .map_err(|_| "Optimization constructors require UTF-8 paths")?;
        if !safe_path(&path) || entry.kind == EntryKind::Symlink {
            return Err("Optimization constructors refuse ambiguous paths and symlinks".into());
        }
        if result.insert(path, entry).is_some() {
            return Err("Optimization manifest repeats a decoded path".into());
        }
    }
    Ok(result)
}

fn changed_paths(before: &Manifest, after: &Manifest) -> Result<BTreeSet<String>, String> {
    let before = entries(before)?;
    let after = entries(after)?;
    Ok(before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .cloned()
        .collect())
}

/// Validate the whole proposed tree, including additions, deletions and executable-bit changes.
/// Unresolvable symlink authority is refused explicitly, rather than treated as protected bytes.
pub fn validate_configuration_diff(
    cas: &Cas,
    source_id: &str,
    candidate_id: &str,
    harness: &OptimizationHarnessV1,
) -> Result<BTreeSet<String>, String> {
    harness.validate()?;
    if source_id == candidate_id || !descends_from(cas, candidate_id, source_id)? {
        return Err("Optimization candidate must descend from the exact source".into());
    }
    let (_, before) = read_snapshot(cas, source_id)?;
    let (_, after) = read_snapshot(cas, candidate_id)?;
    let before_paths = entries(&before)?;
    let after_paths = entries(&after)?;
    for protected in &harness.transitive_dependencies {
        if !before_paths.keys().any(|path| beneath(path, protected)) {
            return Err(format!(
                "Protected oracle dependency is absent: {protected}"
            ));
        }
    }
    let changed = changed_paths(&before, &after)?;
    if changed.is_empty() {
        return Err("Optimization candidate has no source change".into());
    }
    for path in &changed {
        if !harness
            .candidate_writable_paths
            .iter()
            .any(|root| beneath(path, root))
            || harness
                .transitive_dependencies
                .iter()
                .any(|root| beneath(path, root) || beneath(root, path))
            || matches!(path.as_str(), CATALOG | LOCK | ".af/code-policy.toml")
        {
            return Err(format!(
                "Candidate changed protected or undeclared bytes: {path}"
            ));
        }
    }
    // These files are compiler/engine authority. Even an overly broad writable declaration
    // cannot let the candidate replace their contents before trusted finalization.
    for path in [CATALOG, LOCK, ".af/code-policy.toml"] {
        if before_paths.get(path) != after_paths.get(path) {
            return Err(format!("Candidate changed fixed authority: {path}"));
        }
    }
    Ok(changed)
}

fn bytes(cas: &Cas, entry: &Entry, bound: u64) -> Result<Vec<u8>, String> {
    cas.get_bounded(&entry.content, bound)
        .map_err(|error| error.to_string())
}

fn package_files(
    cas: &Cas,
    tree: &Manifest,
    root: &str,
) -> Result<BTreeMap<String, Vec<u8>>, String> {
    if !safe_path(root) {
        return Err("Optimization repinning requires a local relative package path".into());
    }
    let mut remaining = MAX_PACKAGE_BYTES;
    let mut files = BTreeMap::new();
    for (path, entry) in entries(tree)? {
        if let Some(relative) = path.strip_prefix(&format!("{root}/")) {
            let data = bytes(cas, entry, remaining)?;
            remaining = remaining
                .checked_sub(data.len() as u64)
                .ok_or("Package exceeds captured read bound")?;
            files.insert(relative.to_owned(), data);
        }
    }
    if files.is_empty() {
        return Err(format!("Captured package has no files: {root}"));
    }
    Ok(files)
}

fn validate_package_header(
    files: &BTreeMap<String, Vec<u8>>,
    name: &str,
    version: &str,
) -> Result<(), String> {
    let manifests: Vec<_> = ["pipeline.toml", "worker.toml", "kind.toml"]
        .into_iter()
        .filter_map(|path| files.get(path))
        .collect();
    if manifests.len() != 1 {
        return Err("Repinning requires one unambiguous Task package manifest".into());
    }
    let text = std::str::from_utf8(manifests[0]).map_err(|error| error.to_string())?;
    let value: toml::Value = toml::from_str(text).map_err(|error| error.to_string())?;
    if value.get("name").and_then(toml::Value::as_str) != Some(name)
        || value.get("version").and_then(toml::Value::as_str) != Some(version)
    {
        return Err("Candidate package changed its captured name or version".into());
    }
    Ok(())
}

fn publish<T: serde::Serialize>(
    cas: &Cas,
    ty: &str,
    producer: Producer,
    refs: Vec<String>,
    subject: &str,
    value: &T,
) -> Result<String, String> {
    if !matches!(producer, Producer::KernelOperation { .. }) {
        return Err("Optimization source evidence requires an installed kernel producer".into());
    }
    cas.put_artifact(
        ty,
        producer,
        refs,
        Some(subject.into()),
        serde_json::to_value(value).map_err(|error| error.to_string())?,
    )
    .map(|(id, _)| id)
    .map_err(|error| error.to_string())
}

pub struct FinalizedConfiguration {
    pub snapshot_id: String,
    pub repin_id: String,
}

/// Compute only package pins entailed by actual changed package bytes. The candidate cannot
/// supply a replacement catalog. An unchanged lock is a valid, explicitly evidenced harness-only
/// finalization; it is not invented package churn. The result still needs fresh verification.
pub fn finalize_configuration(
    cas: &Cas,
    source_id: &str,
    candidate_id: &str,
    harness_id: &str,
    engine_id: &str,
    producer: Producer,
) -> Result<FinalizedConfiguration, String> {
    cas.verify(engine_id).map_err(|error| error.to_string())?;
    let envelope = cas
        .get_artifact(harness_id)
        .map_err(|error| error.to_string())?;
    if envelope.artifact_type != OPTIMIZATION_HARNESS_V1 {
        return Err("Finalization requires the captured protected harness".into());
    }
    let harness: OptimizationHarnessV1 =
        serde_json::from_value(envelope.payload).map_err(|error| error.to_string())?;
    let changed = validate_configuration_diff(cas, source_id, candidate_id, &harness)?;
    let (_, baseline) = read_snapshot(cas, source_id)?;
    let (candidate, mut final_tree) = read_snapshot(cas, candidate_id)?;
    let baseline_entries = entries(&baseline)?;
    let catalog_entry = baseline_entries
        .get(CATALOG)
        .ok_or("Source has no captured Task catalog")?;
    let catalog_bytes = bytes(cas, catalog_entry, MAX_CONFIG_BYTES)?;
    let catalog: toml::Value =
        toml::from_str(std::str::from_utf8(&catalog_bytes).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let packages = catalog
        .get("packages")
        .and_then(toml::Value::as_table)
        .ok_or("Task catalog has no package pins")?;
    let mut entailed = BTreeMap::new();
    let mut protected_pins = BTreeMap::new();
    let mut package_roots = Vec::new();
    for (name, pin) in packages {
        let root = pin
            .get("path")
            .and_then(toml::Value::as_str)
            .ok_or("Only captured local Task packages can be repinned")?
            .to_owned();
        let version = pin
            .get("version")
            .and_then(toml::Value::as_str)
            .ok_or("Package lacks exact version")?
            .to_owned();
        let before_digest = pin
            .get("digest")
            .and_then(toml::Value::as_str)
            .ok_or("Package lacks digest")?
            .to_owned();
        let before_files = package_files(cas, &baseline, &root)?;
        let after_files = package_files(cas, &final_tree, &root)?;
        validate_package_header(&before_files, name, &version)?;
        validate_package_header(&after_files, name, &version)?;
        let before_actual = review_config::lock::package_digest_from_files(&before_files);
        if before_actual != before_digest {
            return Err(format!(
                "Baseline package does not match trusted pin: {name}"
            ));
        }
        let after_digest = review_config::lock::package_digest_from_files(&after_files);
        if before_actual == after_digest {
            protected_pins.insert(name.clone(), before_actual);
        } else {
            entailed.insert(
                name.clone(),
                PackagePinChangeV1 {
                    before: before_actual,
                    after: after_digest.clone(),
                },
            );
        }
        package_roots.push(root);
    }
    if !entailed.is_empty()
        && changed
            .iter()
            .any(|path| !package_roots.iter().any(|root| beneath(path, root)))
    {
        return Err("Mixed package and harness changes require separate experiments".into());
    }
    let before_lock_id = catalog_entry.content.clone();
    let (snapshot_id, after_lock_id) = if entailed.is_empty() {
        (candidate_id.to_owned(), before_lock_id.clone())
    } else {
        let mut document = std::str::from_utf8(&catalog_bytes)
            .map_err(|error| error.to_string())?
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| error.to_string())?;
        for (name, change) in &entailed {
            document["packages"][name]["digest"] = toml_edit::value(&change.after);
        }
        let serialized = document.to_string().into_bytes();
        let content = cas.put(&serialized).map_err(|error| error.to_string())?;
        let entry = final_tree
            .entries
            .iter_mut()
            .find(|entry| decode_path(&entry.path) == CATALOG.as_bytes())
            .ok_or("Candidate catalog is absent")?;
        entry.content = content.clone();
        entry.size = serialized.len() as u64;
        let final_id =
            capture_snapshot(cas, &final_tree, &candidate.origin_id, Some(candidate_id))?;
        (final_id, content)
    };
    let value = OptimizationPackageRepinV1 {
        schema: "af.optimization-package-repin/1".into(),
        source_snapshot_id: source_id.into(),
        candidate_snapshot_id: snapshot_id.clone(),
        before_lock_id,
        after_lock_id,
        engine_release_id: engine_id.into(),
        entailed,
        protected_pins,
    };
    value.validate()?;
    let repin_id = publish(
        cas,
        OPTIMIZATION_PACKAGE_REPIN_V1,
        producer,
        vec![
            source_id.into(),
            candidate_id.into(),
            snapshot_id.clone(),
            harness_id.into(),
            engine_id.into(),
            value.before_lock_id.clone(),
            value.after_lock_id.clone(),
        ],
        &snapshot_id,
        &value,
    )?;
    Ok(FinalizedConfiguration {
        snapshot_id,
        repin_id,
    })
}

pub struct HarnessFixtureRequest<'a> {
    pub fixture_snapshot_id: &'a str,
    pub product_source_id: &'a str,
    pub configuration_source_id: &'a str,
    pub product_candidate_id: &'a str,
    pub harness_path: &'a str,
    pub requirements_id: &'a str,
    pub harness_id: &'a str,
    pub engine_id: &'a str,
    pub environment_id: &'a str,
}

/// Installed same-path fixture constructor. It changes exactly the declared harness file in
/// two new derived fixture trees. Neither arm is relabelled as the historical product Snapshot.
pub fn materialize_harness_fixture(
    cas: &Cas,
    request: &HarnessFixtureRequest<'_>,
    producer: Producer,
) -> Result<String, String> {
    for id in [
        request.requirements_id,
        request.engine_id,
        request.environment_id,
    ] {
        cas.verify(id).map_err(|error| error.to_string())?;
    }
    let envelope = cas
        .get_artifact(request.harness_id)
        .map_err(|error| error.to_string())?;
    if envelope.artifact_type != OPTIMIZATION_HARNESS_V1 {
        return Err("Fixture requires captured protected harness policy".into());
    }
    let harness: OptimizationHarnessV1 =
        serde_json::from_value(envelope.payload).map_err(|error| error.to_string())?;
    let changed = validate_configuration_diff(
        cas,
        request.configuration_source_id,
        request.product_candidate_id,
        &harness,
    )?;
    if changed != BTreeSet::from([request.harness_path.to_owned()]) {
        return Err(
            "Harness fixture construction requires exactly one declared harness change".into(),
        );
    }
    let (_, source) = read_snapshot(cas, request.configuration_source_id)?;
    let (_, historical_case) = read_snapshot(cas, request.product_source_id)?;
    let (_, candidate) = read_snapshot(cas, request.product_candidate_id)?;
    let (fixture, fixture_tree) = read_snapshot(cas, request.fixture_snapshot_id)?;
    let fixture_entries = entries(&fixture_tree)?;
    for path in &harness.transitive_dependencies {
        if !fixture_entries.keys().any(|entry| beneath(entry, path)) {
            return Err("Fixed fixture is missing a protected oracle dependency".into());
        }
    }
    let target = fixture_entries
        .get(request.harness_path)
        .ok_or("Fixed fixture lacks the declared harness path")?;
    let source_entries = entries(&source)?;
    let candidate_entries = entries(&candidate)?;
    let baseline_harness = source_entries
        .get(request.harness_path)
        .ok_or("Baseline harness is absent")?;
    let candidate_harness = candidate_entries
        .get(request.harness_path)
        .ok_or("Candidate harness is absent")?;
    let case_entries = entries(&historical_case)?;
    let case_harness = case_entries
        .get(request.harness_path)
        .ok_or("Historical case lacks the baseline harness")?;
    if case_harness.content != baseline_harness.content
        || case_harness.kind != baseline_harness.kind
    {
        return Err("Historical case baseline harness differs from selected configuration".into());
    }
    if baseline_harness.content == candidate_harness.content
        && baseline_harness.kind == candidate_harness.kind
    {
        return Err("Harness correction requires changed harness bytes or executable mode".into());
    }
    let derive = |replacement: &Entry| -> Result<String, String> {
        let mut tree = fixture_tree.clone();
        let mut replacement = replacement.clone();
        replacement.path = target.path.clone();
        *tree
            .entries
            .iter_mut()
            .find(|entry| entry.path == target.path)
            .ok_or("Fixture target disappeared")? = replacement;
        capture_snapshot(
            cas,
            &tree,
            &fixture.origin_id,
            Some(request.fixture_snapshot_id),
        )
    };
    let baseline_id = derive(baseline_harness)?;
    let candidate_id = derive(candidate_harness)?;
    let constructor = cas.put_json(&serde_json::json!({"schema":"af.harness-fixture-constructor/1","engine":request.engine_id,"fixture":request.fixture_snapshot_id,"configuration_source":request.configuration_source_id,"path":request.harness_path,"environment":request.environment_id})).map_err(|error| error.to_string())?;
    let harness_identity = |entry: &Entry| -> Result<String, String> {
        let manifest = Manifest::new(vec![entry.clone()]).map_err(|error| error.to_string())?;
        cas.put_json(&serde_json::to_value(manifest).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())
    };
    let baseline_harness_id = harness_identity(baseline_harness)?;
    let candidate_harness_id = harness_identity(candidate_harness)?;
    let value = HarnessMaterializationV1 {
        schema: "af.harness-materialization/1".into(),
        fixture_constructor_id: constructor.clone(),
        product_source_snapshot_id: request.product_source_id.into(),
        requirements_id: request.requirements_id.into(),
        protected_oracle_id: harness.oracle_id,
        baseline_harness_id: baseline_harness_id.clone(),
        candidate_harness_id: candidate_harness_id.clone(),
        baseline_derived_snapshot_id: baseline_id.clone(),
        candidate_derived_snapshot_id: candidate_id.clone(),
        environment_id: request.environment_id.into(),
    };
    value.validate()?;
    publish(
        cas,
        HARNESS_MATERIALIZATION_V1,
        producer,
        vec![
            request.fixture_snapshot_id.into(),
            request.product_source_id.into(),
            request.configuration_source_id.into(),
            request.product_candidate_id.into(),
            request.requirements_id.into(),
            request.harness_id.into(),
            request.engine_id.into(),
            request.environment_id.into(),
            constructor,
            baseline_harness_id,
            candidate_harness_id,
            baseline_id,
            candidate_id,
        ],
        request.product_candidate_id,
        &value,
    )
}
