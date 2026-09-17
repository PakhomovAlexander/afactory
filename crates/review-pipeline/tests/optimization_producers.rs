use std::collections::{BTreeMap, BTreeSet};

use review_core::Producer;
use review_core::task::optimization_experiment::*;
use review_pipeline::task::optimization_producers::*;
use review_source_git::task::{capture_snapshot, read_snapshot};
use review_source_git::{Entry, EntryKind, Manifest};
use review_store::Cas;

fn producer() -> Producer {
    Producer::KernelOperation {
        run_id: "optimization-producer-test".into(),
        node_id: None,
        operation_id: "finalize@1".into(),
    }
}

fn tree(
    cas: &Cas,
    origin: &str,
    parent: Option<&str>,
    files: &BTreeMap<String, Vec<u8>>,
) -> String {
    let entries = files
        .iter()
        .map(|(path, bytes)| Entry {
            path: path.clone(),
            kind: EntryKind::File,
            content: cas.put(bytes).unwrap(),
            size: bytes.len() as u64,
        })
        .collect();
    capture_snapshot(cas, &Manifest::new(entries).unwrap(), origin, parent).unwrap()
}

fn files(pairs: &[(&str, &str)]) -> BTreeMap<String, Vec<u8>> {
    pairs
        .iter()
        .map(|(path, text)| ((*path).into(), text.as_bytes().to_vec()))
        .collect()
}

fn policy(cas: &Cas, writable: &str, protected: &str) -> (String, OptimizationHarnessV1) {
    let value = OptimizationHarnessV1 {
        schema: "af.optimization-harness/1".into(),
        oracle_id: cas.put(b"protected-oracle-v1").unwrap(),
        checks: BTreeSet::from(["correctness".into()]),
        transitive_dependencies: BTreeSet::from([protected.into()]),
        candidate_writable_paths: BTreeSet::from([writable.into()]),
        cache_read_scopes: BTreeSet::new(),
        cache_write_scopes: BTreeSet::new(),
    };
    value.validate().unwrap();
    let id = cas
        .put_artifact(
            OPTIMIZATION_HARNESS_V1,
            producer(),
            vec![],
            None,
            serde_json::to_value(&value).unwrap(),
        )
        .unwrap()
        .0;
    (id, value)
}

fn package_source() -> BTreeMap<String, Vec<u8>> {
    let package = files(&[
        ("worker.toml", "name='project/author'\nversion='1.0.0'\n"),
        ("prompt.txt", "baseline"),
    ]);
    let protected = files(&[
        ("worker.toml", "name='project/verifier'\nversion='1.0.0'\n"),
        ("prompt.txt", "fixed oracle"),
    ]);
    let mut result = files(&[
        ("checks/check.py", "assert True"),
        (".af/af.lock", "fixed engine pin"),
        ("harness.sh", "broken"),
    ]);
    for (path, bytes) in &package {
        result.insert(format!("packages/author/{path}"), bytes.clone());
    }
    for (path, bytes) in &protected {
        result.insert(format!("packages/verifier/{path}"), bytes.clone());
    }
    let text = format!(
        "# retain this catalog comment\nschema='af.task-catalog/2'\n[packages.'project/author']\nversion='1.0.0'\npath='packages/author'\ndigest='{}'\n[packages.'project/verifier']\nversion='1.0.0'\npath='packages/verifier'\ndigest='{}'\n",
        review_config::lock::package_digest_from_files(&package),
        review_config::lock::package_digest_from_files(&protected)
    );
    result.insert(".af/task-catalog.toml".into(), text.into_bytes());
    result
}

#[test]
fn finalizer_recomputes_actual_package_bytes_and_preserves_other_authority() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path()).unwrap();
    let origin = cas.put(b"repository").unwrap();
    let engine = cas.put(b"engine").unwrap();
    let before = package_source();
    let source = tree(&cas, &origin, None, &before);
    let mut proposed = before.clone();
    proposed.insert("packages/author/prompt.txt".into(), b"candidate".to_vec());
    let candidate = tree(&cas, &origin, Some(&source), &proposed);
    let (harness, _) = policy(&cas, "packages/author", "checks");
    let finalized =
        finalize_configuration(&cas, &source, &candidate, &harness, &engine, producer()).unwrap();
    assert_ne!(finalized.snapshot_id, candidate);
    let (snapshot, final_tree) = read_snapshot(&cas, &finalized.snapshot_id).unwrap();
    assert_eq!(
        snapshot.parent_snapshot_id.as_deref(),
        Some(candidate.as_str())
    );
    let envelope = cas.get_artifact(&finalized.repin_id).unwrap();
    let receipt: OptimizationPackageRepinV1 = serde_json::from_value(envelope.payload).unwrap();
    receipt.validate().unwrap();
    assert_eq!(receipt.entailed.len(), 1);
    assert!(receipt.entailed.contains_key("project/author"));
    assert!(receipt.protected_pins.contains_key("project/verifier"));
    let expected = review_config::lock::package_digest_from_files(&files(&[
        ("worker.toml", "name='project/author'\nversion='1.0.0'\n"),
        ("prompt.txt", "candidate"),
    ]));
    assert_eq!(receipt.entailed["project/author"].after, expected);
    let catalog = final_tree
        .entries
        .iter()
        .find(|entry| entry.path == ".af/task-catalog.toml")
        .unwrap();
    let text = String::from_utf8(cas.get(&catalog.content).unwrap()).unwrap();
    assert!(text.starts_with("# retain this catalog comment\n"));
    let document: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(
        document["packages"]["project/author"]["digest"].as_str(),
        Some(expected.as_str())
    );
    let lock = final_tree
        .entries
        .iter()
        .find(|entry| entry.path == ".af/af.lock")
        .unwrap();
    assert_eq!(cas.get(&lock.content).unwrap(), before[".af/af.lock"]);
}

#[test]
fn finalizer_refuses_candidate_control_of_oracle_catalog_engine_or_unrelated_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path()).unwrap();
    let origin = cas.put(b"repository").unwrap();
    let engine = cas.put(b"engine").unwrap();
    let before = package_source();
    let source = tree(&cas, &origin, None, &before);
    let (harness_id, harness) = policy(&cas, "packages/author", "checks");
    for target in [
        "checks/check.py",
        ".af/task-catalog.toml",
        ".af/af.lock",
        "unrelated.txt",
    ] {
        let mut proposed = before.clone();
        proposed.insert(target.into(), b"tampered".to_vec());
        let candidate = tree(&cas, &origin, Some(&source), &proposed);
        assert!(
            finalize_configuration(&cas, &source, &candidate, &harness_id, &engine, producer())
                .is_err(),
            "{target}"
        );
    }
    let mut proposed = before.clone();
    proposed.remove("checks/check.py");
    let candidate = tree(&cas, &origin, Some(&source), &proposed);
    assert!(validate_configuration_diff(&cas, &source, &candidate, &harness).is_err());
    let mut proposed = before.clone();
    proposed.insert(
        "packages/author/prompt.txt".into(),
        b"../checks/check.py".to_vec(),
    );
    let candidate = tree(&cas, &origin, Some(&source), &proposed);
    let (snapshot, mut manifest) = read_snapshot(&cas, &candidate).unwrap();
    manifest
        .entries
        .iter_mut()
        .find(|entry| entry.path == "packages/author/prompt.txt")
        .unwrap()
        .kind = EntryKind::Symlink;
    let symlinked = capture_snapshot(&cas, &manifest, &snapshot.origin_id, Some(&source)).unwrap();
    assert!(validate_configuration_diff(&cas, &source, &symlinked, &harness).is_err());
}

#[test]
fn harness_only_finalization_retains_an_explicit_unchanged_lock_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path()).unwrap();
    let origin = cas.put(b"repository").unwrap();
    let engine = cas.put(b"engine").unwrap();
    let before = package_source();
    let source = tree(&cas, &origin, None, &before);
    let mut proposed = before;
    proposed.insert("harness.sh".into(), b"fixed".to_vec());
    let candidate = tree(&cas, &origin, Some(&source), &proposed);
    let (harness, _) = policy(&cas, "harness.sh", "checks");
    let result =
        finalize_configuration(&cas, &source, &candidate, &harness, &engine, producer()).unwrap();
    assert_eq!(result.snapshot_id, candidate);
    let receipt: OptimizationPackageRepinV1 =
        serde_json::from_value(cas.get_artifact(&result.repin_id).unwrap().payload).unwrap();
    assert!(receipt.entailed.is_empty());
    assert_eq!(receipt.before_lock_id, receipt.after_lock_id);
    receipt.validate().unwrap();
    let mut forged = receipt;
    forged.after_lock_id = cas.put(b"changed lock without entailed pins").unwrap();
    assert!(forged.validate().is_err());
}

#[test]
fn harness_materialization_uses_distinct_derived_fixture_trees_and_fixed_oracle_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let cas = Cas::open(temp.path()).unwrap();
    let origin = cas.put(b"product").unwrap();
    let fixture_origin = cas.put(b"fixture").unwrap();
    let source_files = files(&[("harness.sh", "broken"), ("oracle.sh", "protected")]);
    let source = tree(&cas, &origin, None, &source_files);
    let mut configuration_files = source_files.clone();
    configuration_files.insert(
        "current-only.txt".into(),
        b"added after the historical case".to_vec(),
    );
    let configuration = tree(&cas, &origin, Some(&source), &configuration_files);
    let mut candidate_files = configuration_files;
    candidate_files.insert("harness.sh".into(), b"fixed".to_vec());
    let candidate = tree(&cas, &origin, Some(&configuration), &candidate_files);
    let fixture = tree(
        &cas,
        &fixture_origin,
        None,
        &files(&[
            ("harness.sh", "placeholder"),
            ("oracle.sh", "protected fixture oracle"),
        ]),
    );
    let (harness, _) = policy(&cas, "harness.sh", "oracle.sh");
    let requirements = cas.put(b"requirements").unwrap();
    let engine = cas.put(b"engine").unwrap();
    let environment = cas.put(b"environment").unwrap();
    let request = HarnessFixtureRequest {
        fixture_snapshot_id: &fixture,
        product_source_id: &source,
        configuration_source_id: &configuration,
        product_candidate_id: &candidate,
        harness_path: "harness.sh",
        requirements_id: &requirements,
        harness_id: &harness,
        engine_id: &engine,
        environment_id: &environment,
    };
    let id = materialize_harness_fixture(&cas, &request, producer()).unwrap();
    let receipt: HarnessMaterializationV1 =
        serde_json::from_value(cas.get_artifact(&id).unwrap().payload).unwrap();
    receipt.validate().unwrap();
    assert_eq!(receipt.product_source_snapshot_id, source);
    let mut arms = Vec::new();
    for id in [
        &receipt.baseline_derived_snapshot_id,
        &receipt.candidate_derived_snapshot_id,
    ] {
        assert_ne!(id, &source);
        assert_ne!(id, &candidate);
        assert_ne!(id, &fixture);
        let (snapshot, manifest) = read_snapshot(&cas, id).unwrap();
        assert_eq!(
            snapshot.parent_snapshot_id.as_deref(),
            Some(fixture.as_str())
        );
        let oracle = manifest
            .entries
            .iter()
            .find(|entry| entry.path == "oracle.sh")
            .unwrap();
        assert_eq!(
            cas.get(&oracle.content).unwrap(),
            b"protected fixture oracle"
        );
        let target = manifest
            .entries
            .iter()
            .find(|entry| entry.path == "harness.sh")
            .unwrap();
        arms.push(cas.get(&target.content).unwrap());
    }
    assert_eq!(arms, vec![b"broken".to_vec(), b"fixed".to_vec()]);
    let incompatible = tree(
        &cas,
        &origin,
        None,
        &files(&[
            ("harness.sh", "different baseline"),
            ("oracle.sh", "protected"),
        ]),
    );
    let mismatched = HarnessFixtureRequest {
        product_source_id: &incompatible,
        ..request
    };
    assert!(materialize_harness_fixture(&cas, &mismatched, producer()).is_err());
}
