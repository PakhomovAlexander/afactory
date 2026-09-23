//! Every refusal here runs while the Task revision is still being built, which is why the
//! acceptance obligation "before any Worker or Provider admission" is structural rather than
//! something a test has to observe.

use super::*;
use review_core::task::{TaskAcceptanceV1, TaskExecutionV1};
use review_source_git::task::{capture_snapshot, derive_source_tree, source_tree};
use review_source_git::{Entry, EntryKind, Manifest};
use serde_json::json;

type Ports = BTreeMap<String, ArtifactInputV1>;
type Refs = BTreeMap<String, TaskInputRefV1>;

const IMPLEMENT: TaskKindProfile = TaskKindProfile::ReviewedImplementation;

/// The only part of the Store resolution reads. Results, ports and artifacts come from the
/// CAS exactly as they do in production.
#[derive(Default)]
struct Recorded(BTreeMap<String, TaskPhaseV1>);

impl RecordedTasks for Recorded {
    fn phase(&self, _cas: &Cas, task_id: &str) -> Result<Option<TaskPhaseV1>, String> {
        Ok(self.0.get(task_id).cloned())
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    cas: Cas,
    tasks: Recorded,
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    Fixture {
        _directory: directory,
        cas,
        tasks: Recorded::default(),
    }
}

impl Fixture {
    fn put(&self, artifact_type: &str, payload: serde_json::Value) -> String {
        let empty = Vec::new();
        let put = self
            .cas
            .put_artifact(artifact_type, producer(), empty, None, payload);
        put.unwrap().0
    }

    /// Record a finished Task whose result carries exactly these output ports.
    fn finish(&mut self, task_id: &str, outputs: Ports) {
        let fixture = json!({ "fixture": task_id });
        let result = TaskResultV1 {
            task_revision_id: self.cas.put_json(&fixture).unwrap(),
            execution: TaskExecutionV1::Completed,
            acceptance: TaskAcceptanceV1::Unsatisfied,
            domain_conclusion: "changes_requested".into(),
            outputs,
            evidence: Default::default(),
            missing_obligations: Default::default(),
        };
        result.validate().unwrap();
        let payload = serde_json::to_value(&result).unwrap();
        let result_id = self.put(TASK_RESULT_V1, payload);
        let phase = TaskPhaseV1::Finished { result_id };
        self.tasks.0.insert(task_id.into(), phase);
    }

    fn manifest(&self) -> Manifest {
        let content = self.cas.put(b"fixture\n").unwrap();
        let entry = Entry {
            path: "a.txt".into(),
            kind: EntryKind::File,
            content,
            size: 8,
        };
        Manifest::new(vec![entry]).unwrap()
    }

    fn origin(&self, payload: serde_json::Value) -> String {
        self.cas.put_json(&payload).unwrap()
    }

    fn with(&self, profile: TaskKindProfile, table: Refs) -> Result<BoundInputs, String> {
        resolve(&self.cas, &self.tasks, profile, &table)
    }

    fn bind(&self, table: Refs) -> Result<BoundInputs, String> {
        self.with(IMPLEMENT, table)
    }

    /// The published record, read back from the CAS by the identity resolution returned.
    fn record(&self, bound: &BoundInputs) -> TaskInputBindingsV1 {
        let envelope = self.cas.get_artifact(&bound.record_id).unwrap();
        assert_eq!(envelope.artifact_type, TASK_INPUT_BINDINGS_V1);
        let record: TaskInputBindingsV1 = serde_json::from_value(envelope.payload).unwrap();
        record.validate().unwrap();
        record
    }
}

fn task_ref(task: &str, port: &str) -> TaskInputRefV1 {
    TaskInputRefV1::Task(TaskOutputRefV1 {
        task: task.into(),
        port: port.into(),
    })
}

fn exact_ref(artifact: &str) -> TaskInputRefV1 {
    TaskInputRefV1::Artifact(ExactArtifactRefV1 {
        artifact: artifact.into(),
    })
}

fn one(artifact_type: &str, id: &str) -> ArtifactInputV1 {
    ArtifactInputV1 {
        artifact_ids: vec![id.into()],
        artifact_type: artifact_type.into(),
        cardinality: PortCardinality::One,
        snapshot_id: None,
    }
}

fn table(port: &str, reference: TaskInputRefV1) -> Refs {
    Refs::from([(port.to_owned(), reference)])
}

fn ports(port: &str, input: ArtifactInputV1) -> Ports {
    Ports::from([(port.to_owned(), input)])
}

#[test]
fn only_the_ports_this_adapter_constructs_are_bindable() {
    let fixture = fixture();
    for port in NOT_BINDABLE {
        let reference = task_ref("prior", "x");
        let error = fixture.bind(table(port, reference)).unwrap_err();
        let named = format!("{port} <- task prior/x");
        assert!(error.contains(&named), "{error}");
        assert!(error.contains("is not bindable"), "{error}");
    }
    let reference = task_ref("prior", "snapshot");
    let error = fixture.bind(table("candidate", reference)).unwrap_err();
    assert!(error.contains("candidate <- task prior"), "{error}");
    assert!(error.contains("source, history, sources"), "{error}");
}

#[test]
fn an_unknown_or_unfinished_task_is_refused_by_name() {
    let mut fixture = fixture();
    let reference = task_ref("prior", "history");
    let again = reference.clone();
    let error = fixture.bind(table("history", reference)).unwrap_err();
    assert!(error.contains("history <- task prior/history"), "{error}");
    assert!(error.contains("no such Task is recorded"), "{error}");

    let running = TaskPhaseV1::Running {};
    fixture.tasks.0.insert("prior".into(), running);
    let error = fixture.bind(table("history", again)).unwrap_err();
    assert!(error.contains("history <- task prior/history"), "{error}");
    assert!(error.contains("is running, not finished"), "{error}");
}

#[test]
fn an_absent_port_lists_the_ports_the_result_does_carry() {
    let mut fixture = fixture();
    let history = fixture.put(REVIEW_HISTORY_V1, json!({"kind": "empty"}));
    let carried = one(REVIEW_HISTORY_V1, &history);
    fixture.finish("prior", ports("history", carried));
    let reference = task_ref("prior", "ledger");
    let error = fixture.bind(table("history", reference)).unwrap_err();
    assert!(error.contains("history <- task prior/ledger"), "{error}");
    assert!(error.contains("no output port ledger"), "{error}");
    assert!(error.contains("it carries history"), "{error}");
}

#[test]
fn a_type_mismatch_names_both_types_the_task_and_the_port() {
    let mut fixture = fixture();
    let payload = json!({"schema":"af.document-sources/1","sources":{}});
    let sources = fixture.put(DOCUMENT_SOURCES_V1, payload);
    let carried = one(DOCUMENT_SOURCES_V1, &sources);
    fixture.finish("prior", ports("history", carried));
    let reference = task_ref("prior", "history");
    let error = fixture.bind(table("history", reference)).unwrap_err();
    assert!(error.contains("history <- task prior/history"), "{error}");
    assert!(error.contains("af/DocumentSources@1 one"), "{error}");
    assert!(error.contains("af/ReviewHistory@1 one"), "{error}");

    // The same check reads an exact artifact's own envelope type.
    let reference = exact_ref(&sources);
    let error = fixture.bind(table("history", reference)).unwrap_err();
    assert!(error.contains("history <- artifact sha256:"), "{error}");
    assert!(error.contains("af/ReviewHistory@1 one"), "{error}");
}

#[test]
fn a_many_output_never_binds_a_one_port_however_many_it_holds() {
    for count in [1, 3] {
        let mut fixture = fixture();
        let mut ids = Vec::new();
        for index in 0..count {
            let payload = json!({"kind": "empty", "n": index});
            ids.push(fixture.put(REVIEW_HISTORY_V1, payload));
        }
        let carried = ArtifactInputV1 {
            artifact_ids: ids,
            artifact_type: REVIEW_HISTORY_V1.into(),
            cardinality: PortCardinality::Many,
            snapshot_id: None,
        };
        fixture.finish("prior", ports("history", carried));
        let reference = task_ref("prior", "history");
        let error = fixture.bind(table("history", reference)).unwrap_err();
        assert!(error.contains("af/ReviewHistory@1 many"), "{error}");
        let takes = "takes af/ReviewHistory@1 one";
        assert!(error.contains(takes), "{count}: {error}");
    }
}

#[test]
fn an_exact_artifact_history_binding_resolves_and_names_no_task() {
    let fixture = fixture();
    let history = fixture.put(REVIEW_HISTORY_V1, json!({"kind": "empty"}));
    let reference = exact_ref(&history);
    let bound = fixture.bind(table("history", reference)).unwrap();
    assert_eq!(bound.ports["history"], one(REVIEW_HISTORY_V1, &history));
    assert!(bound.source_manifest.is_none());
    let record = fixture.record(&bound);
    let binding = &record.bindings["history"];
    assert_eq!(binding.artifact_id, history);
    assert!(binding.task.is_none());
    assert!(binding.resolved_artifact_id.is_none());
}

#[test]
fn a_root_capture_is_verbatim_and_a_derived_tree_is_re_rooted() {
    let mut fixture = fixture();
    let manifest = fixture.manifest();
    let committed = fixture.origin(json!({"schema":"af.task-source-origin/1",
        "repository_id":"example/hub","source_revision":"bba24cb",
        "content_digest":manifest.content_digest()}));
    let cas = &fixture.cas;
    let root = capture_snapshot(cas, &manifest, &committed, None).unwrap();
    let refs = vec![committed];
    let root_port = source_tree(cas, producer(), &root, refs).unwrap();
    let carried = ports("snapshot", root_port.clone());
    fixture.finish("root-capture", carried);

    let reference = task_ref("root-capture", "snapshot");
    let bound = fixture.bind(table("source", reference)).unwrap();
    assert_eq!(bound.ports["source"], root_port, "carried verbatim");
    assert_eq!(bound.source_manifest.as_ref(), Some(&manifest));
    let record = fixture.record(&bound);
    let binding = &record.bindings["source"];
    assert_eq!(binding.snapshot_id.as_deref(), Some(root.as_str()));
    assert!(binding.rerooted_snapshot_id.is_none());

    // A `snapshot` output is derived, so it is republished as a root over the same Manifest.
    let encoded = serde_json::to_value(&manifest).unwrap();
    let manifest_id = fixture.cas.put_json(&encoded).unwrap();
    let cas = &fixture.cas;
    let empty = Vec::new();
    let made = derive_source_tree(cas, producer(), &manifest_id, &root, empty);
    let derived = made.unwrap();
    fixture.finish("derived", ports("snapshot", derived.clone()));
    let reference = task_ref("derived", "snapshot");
    let bound = fixture.bind(table("source", reference)).unwrap();
    let port = &bound.ports["source"];
    assert_ne!(port.artifact_ids, derived.artifact_ids);
    assert_ne!(port.snapshot_id, derived.snapshot_id);
    let record = fixture.record(&bound);
    let binding = &record.bindings["source"];
    assert_eq!(binding.artifact_id, derived.artifact_ids[0]);
    assert_eq!(binding.snapshot_id, derived.snapshot_id);
    assert_eq!(binding.rerooted_snapshot_id, port.snapshot_id);
    let resolved = binding.resolved_artifact_id.as_deref();
    assert_eq!(resolved, Some(port.artifact_ids[0].as_str()));
    let named = binding.task.as_ref().unwrap();
    assert_eq!(named.domain_conclusion, "changes_requested");
    assert_eq!(named.acceptance, TaskAcceptanceV1::Unsatisfied);

    // The re-rooted Snapshot is parentless, keeps the identical content, states where it came
    // from, and deliberately has no committed revision for delivery to compare.
    let rerooted = port.snapshot_id.clone().unwrap();
    let read = read_snapshot(&fixture.cas, &rerooted).unwrap();
    let (snapshot, rerooted_manifest) = read;
    assert!(snapshot.parent_snapshot_id.is_none());
    assert_eq!(rerooted_manifest, manifest);
    let origin = read_origin(&fixture.cas, &snapshot.origin_id).unwrap();
    assert_eq!(origin.repository_id(), "example/hub");
    assert_eq!(origin.source_revision(), None);
    let from = origin.bound_from().unwrap();
    let source = from.task.as_ref().unwrap();
    assert_eq!(source.task_id, "derived");
    assert_eq!(source.port, "snapshot");
}

#[test]
fn a_parentless_generation_two_source_is_carried_verbatim() {
    let fixture = fixture();
    let manifest = fixture.manifest();
    let from = json!({"artifact_id":format!("sha256:{}", "a".repeat(64)),
        "snapshot_id":format!("sha256:{}", "b".repeat(64))});
    let origin = fixture.origin(json!({"schema":"af.task-source-origin/2",
        "repository_id":"example/hub","content_digest":manifest.content_digest(),
        "bound_from":from}));
    let cas = &fixture.cas;
    let snapshot = capture_snapshot(cas, &manifest, &origin, None).unwrap();
    let refs = vec![origin];
    let port = source_tree(cas, producer(), &snapshot, refs).unwrap();
    let reference = exact_ref(&port.artifact_ids[0]);
    let bound = fixture.bind(table("source", reference)).unwrap();
    assert_eq!(bound.ports["source"], port, "nothing is re-rooted twice");
    let record = fixture.record(&bound);
    let binding = &record.bindings["source"];
    assert!(binding.rerooted_snapshot_id.is_none());
    assert_eq!(binding.snapshot_id.as_deref(), Some(snapshot.as_str()));
}

/// An `af/SourceTree@1` names one Snapshot in its payload and one in its envelope subject.
/// Admission requires them — and the recorded port — to be one identity before anything is
/// decided about the tree, so a forged envelope cannot carry one Snapshot's bytes into a port
/// that names another's.
#[test]
fn an_exact_source_envelope_that_names_two_snapshots_is_refused() {
    let fixture = fixture();
    let manifest = fixture.manifest();
    let committed = fixture.origin(json!({"schema":"af.task-source-origin/1",
        "repository_id":"example/hub","source_revision":"bba24cb",
        "content_digest":manifest.content_digest()}));
    let dirty = fixture.origin(json!({"schema":"af.task-source-origin/1",
        "repository_id":"example/hub","source_revision":null,
        "content_digest":manifest.content_digest()}));
    let cas = &fixture.cas;
    let root = capture_snapshot(cas, &manifest, &committed, None).unwrap();
    let other = capture_snapshot(cas, &manifest, &dirty, None).unwrap();
    assert_ne!(root, other, "two Snapshots over one tree");

    let payload = json!({ "snapshot_id": other });
    let refs = vec![root.clone(), other.clone()];
    let subject = Some(root.clone());
    let put = cas.put_artifact(SOURCE_TREE_V1, producer(), refs, subject, payload);
    let forged = put.unwrap().0;
    let error = fixture
        .bind(table("source", exact_ref(&forged)))
        .unwrap_err();
    assert!(error.contains("source <- artifact sha256:"), "{error}");
    assert!(error.contains("disagrees with the Snapshot"), "{error}");

    // The honest envelope for the same root capture still resolves, verbatim.
    let port = source_tree(cas, producer(), &root, vec![committed]).unwrap();
    let reference = exact_ref(&port.artifact_ids[0]);
    let bound = fixture.bind(table("source", reference)).unwrap();
    assert_eq!(bound.ports["source"], port);

    // A recorded port that names no Snapshot at all is refused as well: the payload, the
    // subject and the port must be one identity, and an absent one cannot be.
    let mut unnamed = port.clone();
    unnamed.snapshot_id = None;
    let error = super::admitted_source(cas, &unnamed).unwrap_err();
    assert!(error.contains("disagrees with the Snapshot"), "{error}");
}

/// Every label a refusal echoes comes from the Task file, so every one of them is sanitized:
/// the destination port and the map key that named it, the referenced Task ID and output port,
/// and the artifact spelling of an exact reference.
#[test]
fn every_refusal_sanitizes_the_task_file_text_it_echoes() {
    let fixture = fixture();
    let hostile = "l3b\nAPPROVED\u{1b}[2J\u{202e}\u{e9}";
    let sanitized = "l3b?APPROVED?[2J??";
    let refused = |port: &str, reference: TaskInputRefV1| {
        let error = fixture.bind(table(port, reference)).unwrap_err();
        let printable = error.is_ascii() && !error.chars().any(|c| c.is_ascii_control());
        assert!(printable, "{error:?}");
        error
    };
    for reference in [task_ref(hostile, hostile), exact_ref(hostile)] {
        // The map key is the destination port, and it is refused by name.
        let error = refused(hostile, reference.clone());
        assert!(error.contains(sanitized), "{error}");
        assert!(error.contains("not a bindable root input port"), "{error}");
        // A bindable port reaches the reference itself, which is echoed the same way.
        let error = refused("source", reference);
        assert!(
            error.starts_with("Task input binding source <- "),
            "{error}"
        );
    }
    let error = refused("requirements", task_ref(hostile, "snapshot"));
    assert!(error.contains(sanitized), "{error}");
    assert!(error.contains("is not bindable"), "{error}");
}

#[test]
fn an_optimization_history_port_expects_the_optimization_type() {
    let fixture = fixture();
    let history = fixture.put(REVIEW_HISTORY_V1, json!({"kind": "empty"}));
    let profile = TaskKindProfile::OptimizationAnalysis;
    let bound = table("history", exact_ref(&history));
    let error = fixture.with(profile, bound).unwrap_err();
    assert!(error.contains("af/OptimizationHistory@1 one"), "{error}");
}

#[test]
fn a_task_file_without_an_inputs_table_keeps_its_bytes() {
    // The `inputs` table is `present_option`: absent stays absent, and explicit null is
    // refused, so a Task file written before ADR-0117 compiles to the identical revision.
    let without = json!({"schema":"af.task-file/1","task_id":"pagination",
        "kind":"implement","goal":"Add offset pagination.","strategy":"standard",
        "facts":{},"limits":{"tokens":1000,"max_attempts":1,"wall_ms":1000,
            "verification":{"tokens":10,"attempts":1,"wall_ms":10}}});
    let read = serde_json::from_value::<super::super::TaskFile>(without.clone());
    let file = read.unwrap();
    assert!(file.inputs.is_none());
    assert_eq!(serde_json::to_value(&file).unwrap(), without);

    let mut null = without.clone();
    null["inputs"] = serde_json::Value::Null;
    let read = serde_json::from_value::<super::super::TaskFile>(null);
    assert!(read.is_err(), "an optional table is absent or a value");

    let mut bound = without;
    bound["inputs"] = json!({"history":{"task":"prior","port":"history"}});
    let read = serde_json::from_value::<super::super::TaskFile>(bound.clone());
    let file = read.unwrap();
    let declared = file.inputs.as_ref().unwrap();
    assert_eq!(declared["history"], task_ref("prior", "history"));
    assert_eq!(serde_json::to_value(&file).unwrap(), bound);

    let closed = [
        json!({"history":{"task":"prior"}}),
        json!({"history":{"task":"prior","port":"history","store":"x"}}),
        json!({"history":{"artifact":"sha256:x","task":"prior"}}),
    ];
    for invalid in closed {
        let mut value = bound.clone();
        value["inputs"] = invalid.clone();
        let read = serde_json::from_value::<super::super::TaskFile>(value);
        assert!(read.is_err(), "{invalid} was admitted");
    }
}
