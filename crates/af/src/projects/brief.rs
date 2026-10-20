//! Project Briefs (ADR-0159): the bounded selection of one Survey a pipeline's declared
//! `project` port carries into the Workers it is wired to.
//!
//! A Task's initial capture, before any pipeline is selected, pins the newest complete Survey of
//! the checkout's Project as `af/ProjectSurveyPin@1` in the Task Store and records an
//! `af/ProjectBriefCapture@1` in the revision's provenance: the pin, the project's
//! `[brief.<name>]` variants and the ancestry of every Survey head against the source commit.
//! Every `normalize_root_inputs` site then renders a `project_brief` port from that record alone
//! ([`Resolver`]), so a refresh, a removed Project Store or a deleted checkout between capture and
//! execution changes nothing, and the compiled plan pins the Brief's artifact ID. Each revision
//! records one `af/ProjectBriefReceipt@1` per Brief port: what it carries and how far its Survey
//! head is behind the source commit, read from the capture's observation, never from live git.
//!
//! Rendering drops whole sections, last-declared first, until the Brief's exact canonical bytes
//! fit the variant's `max_bytes`; a Brief whose mandatory fields alone do not fit is an error.
//! `af project brief` prints the bytes a plan would bind and records nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use review_config::task::catalog::{ProjectBriefResolver, TaskPlanCompiler};
use review_core::project::SurveyCompletenessV1;
use review_core::task::pipeline::{PipelineContractV1, TaskOperatorV1};
use review_core::task::project_brief::{
    NOT_KERNEL_CONSTRUCTED, ORIENT, PORT_REQUIRED_BY_PREFIX, PROJECT_BRIEF_CAPTURE_V1,
    PROJECT_BRIEF_PREVIEW_V1, PROJECT_BRIEF_RECEIPT_V1, PROJECT_BRIEF_V1, PROJECT_SURVEY_PIN_V1,
    ProjectBriefAncestryV1, ProjectBriefCaptureV1, ProjectBriefDropReasonV1, ProjectBriefDropV1,
    ProjectBriefHeadV1, ProjectBriefManifestEntryV1, ProjectBriefNoneReasonV1,
    ProjectBriefOutcomeV1, ProjectBriefPreviewOutcomeV1, ProjectBriefPreviewV1,
    ProjectBriefReceiptV1, ProjectBriefSectionsV1, ProjectBriefUnknownV1, ProjectBriefV1,
    ProjectBriefVariantV1, ProjectSurveyPinV1, RequestedBriefV1, newest_decisions,
    requested_variant,
};
use review_core::task::{ArtifactInputV1, TaskRevisionV1};
use review_core::{ArtifactEnvelope, PortCardinality, Producer};
use review_graph::task::{CompiledOperator, CompiledTask};
use review_store::Cas;
use serde::Deserialize;

use crate::projects::{Checkout, Located};

/// The project's variants: `[brief.<name>]` merged over the built-in `orient`; `none` is never
/// among them.
pub(crate) type Variants = BTreeMap<String, ProjectBriefVariantV1>;

/// The port name `af project brief` previews: the one the design and the docs declare.
const PREVIEW_PORT: &str = "project";
/// The bound of each git command of the ancestry observation.
const ANCESTRY_DEADLINE: Duration = Duration::from_secs(10);

/// One `[brief.<name>]` table as a layer writes it: every key optional, unknown keys refused.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BriefTable {
    sections: Option<Vec<String>>,
    // Read as written, so a value that is no count is refused naming its key.
    max_bytes: Option<toml::Value>,
    decisions: Option<toml::Value>,
    hot_paths: Option<toml::Value>,
}

/// The effective `[brief]` policy of the checkout at `toplevel`, from the configuration ladder.
pub(crate) fn load_policy(toplevel: &Path) -> Result<Variants, String> {
    let config = crate::config::load(Some(toplevel)).map_err(|_| {
        "the configuration does not load; `af config show --origin` names the file".to_string()
    })?;
    policy_of(config.effective.get("brief"))
}

/// The variants `[brief]` of the merged configuration declares, each validated under its closed
/// schema: the refusal names the variant and the field.
pub(crate) fn policy_of(table: Option<&toml::Value>) -> Result<Variants, String> {
    let refused = |error: String| format!("the [brief] policy does not load: {error}");
    let Some(table) = table else {
        return Ok(BTreeMap::new());
    };
    let table = table
        .as_table()
        .ok_or_else(|| refused("[brief] holds one `[brief.<name>]` table per variant".into()))?;
    let mut variants = BTreeMap::new();
    for (name, value) in table {
        let written: BriefTable = value.clone().try_into().map_err(|error: toml::de::Error| {
            refused(format!("[brief.{name}]: {}", error.message().trim()))
        })?;
        let base = (name == ORIENT).then(ProjectBriefVariantV1::orient);
        let sections = match (written.sections, &base) {
            (Some(sections), _) => sections,
            (None, Some(base)) => base.sections.clone(),
            (None, None) => {
                return Err(refused(format!(
                    "[brief.{name}] sections: a variant of the project's own lists its sections"
                )));
            }
        };
        let count = |field: &str, value: Option<toml::Value>, default: u64| match value {
            None => Ok(default),
            Some(value) => value
                .as_integer()
                .and_then(|value| u64::try_from(value).ok())
                .ok_or_else(|| refused(format!("[brief.{name}] {field}: a non-negative integer"))),
        };
        let orient = ProjectBriefVariantV1::orient();
        let variant = ProjectBriefVariantV1 {
            sections,
            max_bytes: count("max_bytes", written.max_bytes, orient.max_bytes.get())?.into(),
            decisions: count("decisions", written.decisions, orient.decisions.get())?.into(),
            hot_paths: count("hot_paths", written.hot_paths, orient.hot_paths.get())?.into(),
        };
        variant.validate(name).map_err(refused)?;
        variants.insert(name.clone(), variant);
    }
    Ok(variants)
}

fn producer(operation: &str) -> Producer {
    Producer::KernelOperation {
        run_id: "project-brief".into(),
        node_id: None,
        operation_id: operation.into(),
    }
}

/// The constructor every Brief names: what binding validation recognizes as the kernel's.
fn brief_producer() -> Producer {
    producer("render-project-brief@1")
}

fn pin_producer() -> Producer {
    producer("pin-project-survey@1")
}

/// The artifact ID `cas.put_artifact` gives `payload`, computed without writing anything, so
/// `af project brief` names the exact ID a plan binds.
fn artifact_id(
    artifact_type: &str,
    producer: Producer,
    input_artifacts: Vec<String>,
    payload: serde_json::Value,
) -> Result<String, String> {
    let bytes = review_store::canonicalize(&payload).map_err(|error| error.to_string())?;
    let envelope = ArtifactEnvelope {
        artifact_type: artifact_type.into(),
        artifact_id: String::new(),
        content_id: review_store::canonical::blob_content_id(&bytes),
        producer,
        input_artifacts,
        subject_snapshot_id: None,
        payload,
    };
    review_store::artifact_id(&envelope).map_err(|error| error.to_string())
}

fn value_of<T: serde::Serialize>(record: &T) -> Result<serde_json::Value, String> {
    serde_json::to_value(record).map_err(|error| error.to_string())
}

/// The Brief `name` renders from `pin`, and its exact canonical bytes. Sections the variant lists
/// that the pinned Survey does not hold are recorded as dropped first; then whole sections go,
/// last-declared first, until the bytes, `dropped` and `rendered_bytes` included, are at or under
/// `max_bytes`. A Brief that cannot fit once every section is gone is an error naming the variant
/// and both byte counts, never an oversized Brief and never an empty port.
pub(crate) fn render(
    pin: &ProjectSurveyPinV1,
    name: &str,
    variant: &ProjectBriefVariantV1,
) -> Result<(ProjectBriefV1, Vec<u8>), String> {
    let identity = pin.identity()?;
    let mut sections = ProjectBriefSectionsV1::default();
    let mut rendered = Vec::new();
    let mut dropped = Vec::new();
    for section in &variant.sections {
        sections.render(
            pin,
            section,
            variant.decisions.get(),
            variant.hot_paths.get(),
        );
        if sections.has(section) {
            rendered.push(section.clone());
        } else {
            dropped.push(ProjectBriefDropV1 {
                section: section.clone(),
                reason: ProjectBriefDropReasonV1::NotInSurvey,
            });
        }
    }
    let mut brief = ProjectBriefV1 {
        schema: PROJECT_BRIEF_V1.into(),
        survey_id: pin.survey_id.clone(),
        project_id: identity.project_id.clone(),
        variant: name.into(),
        head: ProjectBriefHeadV1 {
            commit: identity.head.clone(),
            tree_digest: identity.tree_digest.clone(),
        },
        sections,
        dropped,
        rendered_bytes: 0.into(),
    };
    let max_bytes = variant.max_bytes.get();
    loop {
        let bytes = sized(&mut brief)?;
        if bytes.len() as u64 <= max_bytes {
            brief.validate()?;
            return Ok((brief, bytes));
        }
        let Some(last) = rendered.pop() else {
            return Err(format!(
                "variant {name} cannot fit: with every section dropped its Brief is still {} \
                 bytes, over its max_bytes of {max_bytes}; raise [brief.{name}] max_bytes",
                bytes.len()
            ));
        };
        brief.sections.remove(&last);
        brief.dropped.push(ProjectBriefDropV1 {
            section: last,
            reason: ProjectBriefDropReasonV1::MaxBytes,
        });
    }
}

/// The canonical bytes of `brief` once `rendered_bytes` is their own length: the length counts
/// its own digits, so it is set until it holds still, which it does within two rounds.
fn sized(brief: &mut ProjectBriefV1) -> Result<Vec<u8>, String> {
    loop {
        let bytes = crate::projects::survey::canonical_bytes(&value_of(&*brief)?)?;
        if brief.rendered_bytes.get() == bytes.len() as u64 {
            return Ok(bytes);
        }
        brief.rendered_bytes = (bytes.len() as u64).into();
    }
}

/// The pin's artifact ID, and the ID the Brief rendered from it takes.
fn identities(
    pin: &ProjectSurveyPinV1,
    brief: &ProjectBriefV1,
) -> Result<(String, String), String> {
    let pin_id = artifact_id(
        PROJECT_SURVEY_PIN_V1,
        pin_producer(),
        vec![],
        value_of(pin)?,
    )?;
    let brief_id = artifact_id(
        PROJECT_BRIEF_V1,
        brief_producer(),
        vec![pin_id.clone()],
        value_of(brief)?,
    )?;
    Ok((pin_id, brief_id))
}

/// The Project Store of `checkout`, when one exists: opening it otherwise would create it.
fn existing_store(checkout: &Checkout) -> Result<Option<crate::projects::ProjectStore>, String> {
    let state_home = crate::xdg_state_root()?;
    let directory = crate::projects::store_directory(&state_home, &checkout.project_id);
    if std::fs::symlink_metadata(directory.join("events.sqlite")).is_err() {
        return Ok(None);
    }
    crate::projects::ProjectStore::open(&state_home, &checkout.project_id).map(Some)
}

/// The pin of the newest complete, uncollected Survey of `checkout`'s Project whose
/// `repository_id` is the checkout's, or of the Survey `named`; `None` without a Project Store or
/// such a Survey. Every record is read through the Project Store's own replay checks.
fn pin_of(checkout: &Checkout, named: Option<&str>) -> Result<Option<ProjectSurveyPinV1>, String> {
    let Some(store) = existing_store(checkout)? else {
        return Ok(None);
    };
    let log = store.log()?;
    for recorded in log.chain() {
        if named.is_some_and(|id| id != recorded.survey_id) {
            continue;
        }
        let complete = recorded.completeness == SurveyCompletenessV1::Complete;
        if named.is_some() && !complete {
            return Err(format!(
                "Survey {} is {}; a Brief is rendered only from a complete Survey",
                recorded.survey_id,
                recorded.completeness.as_str()
            ));
        }
        if !complete {
            continue;
        }
        if store.collected(recorded)?.is_some() {
            if named.is_some() {
                return Err(format!(
                    "Survey {} was collected; `af project history` lists the Surveys there are",
                    recorded.survey_id
                ));
            }
            continue;
        }
        let read = store.survey(recorded)?;
        let ours = read
            .survey
            .identity
            .body
            .as_ref()
            .is_some_and(|identity| identity.repository_id == checkout.repository_id);
        if ours {
            return ProjectSurveyPinV1::of(&recorded.survey_id, &read.survey).map(Some);
        }
    }
    match named {
        Some(id) => Err(format!(
            "this Project records no Survey {id}; `af project history` lists the Surveys there are"
        )),
        None => Ok(None),
    }
}

/// One Survey head against `source`, by `git merge-base --is-ancestor` and `git rev-list
/// --count` in the planning checkout; a typed `unknown` when either cannot answer.
fn ancestry(
    checkout: Option<&Checkout>,
    head: &str,
    source: Option<&str>,
) -> ProjectBriefAncestryV1 {
    let unknown = |reason| ProjectBriefAncestryV1::Unknown { reason };
    let Some(source) = source else {
        return unknown(ProjectBriefUnknownV1::NoCommittedRevision);
    };
    let Some(git) =
        checkout.and_then(|checkout| crate::projects::Git::open(&checkout.toplevel).ok())
    else {
        return unknown(ProjectBriefUnknownV1::NoCheckout);
    };
    let limit = crate::projects::Git::within(ANCESTRY_DEADLINE);
    let run = |args: &[&str]| match git.run_within(args, None, limit) {
        Ok(output) => Ok(output),
        Err(stopped) if stopped.deadline => Err(ProjectBriefUnknownV1::Deadline),
        Err(_) => Err(ProjectBriefUnknownV1::NoCheckout),
    };
    let observed = (|| {
        for commit in [head, source] {
            let object = format!("{commit}^{{commit}}");
            if !run(&["cat-file", "-e", &object])?.status.success() {
                return Err(ProjectBriefUnknownV1::MissingObjects);
            }
        }
        let ancestor = run(&["merge-base", "--is-ancestor", head, source])?;
        match ancestor.status.code() {
            Some(0) => {}
            Some(1) => return Err(ProjectBriefUnknownV1::NotAncestor),
            _ => return Err(ProjectBriefUnknownV1::MissingObjects),
        }
        let range = format!("{head}..{source}");
        let counted = run(&["rev-list", "--count", &range, "--"])?;
        String::from_utf8_lossy(&counted.stdout)
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|_| counted.status.success())
            .ok_or(ProjectBriefUnknownV1::MissingObjects)
    })();
    match observed {
        Ok(behind) => ProjectBriefAncestryV1::Behind {
            behind_commits: behind.into(),
        },
        Err(reason) => unknown(reason),
    }
}

/// Whether a Task whose captured catalog holds `compiler`'s pipelines can carry a constructed
/// Brief: one of them declares a `project_brief` root port, or a Planner may generate one.
pub(crate) fn constructs(compiler: &TaskPlanCompiler, planner: bool) -> bool {
    use review_core::task::pipeline::RootDefaultV1;
    planner
        || compiler.pipelines().values().any(|pipeline| {
            pipeline
                .contract
                .inputs
                .values()
                .any(|port| port.root_default == Some(RootDefaultV1::ProjectBrief))
        })
}

/// The initial capture: the pin, the project's variants and the ancestry of every Survey head a
/// Brief of this Task describes (the pin's, each Brief the Task file binds), recorded as one
/// `af/ProjectBriefCapture@1` whose ID the caller keeps in the revision's provenance. `None` when
/// the record would say nothing, so a Task that can carry no Brief keeps its revision identity.
/// A Project Store that cannot be read is reported on stderr and pins nothing: a Survey never
/// fails a Task.
pub(crate) fn capture(
    cas: &Cas,
    repo: &Path,
    source_commit: Option<&str>,
    constructs: bool,
    bound: &BTreeMap<String, ArtifactInputV1>,
) -> Result<Option<String>, String> {
    let bound_briefs: Vec<&String> = bound
        .values()
        .filter(|port| port.artifact_type == PROJECT_BRIEF_V1)
        .flat_map(|port| &port.artifact_ids)
        .collect();
    if !constructs && bound_briefs.is_empty() {
        return Ok(None);
    }
    let checkout = match crate::projects::locate(repo) {
        Ok(Located::Checkout(checkout)) => Some(checkout),
        _ => None,
    };
    let toplevel = checkout
        .as_ref()
        .map_or_else(|| repo.to_path_buf(), |checkout| checkout.toplevel.clone());
    let mut record = ProjectBriefCaptureV1 {
        schema: PROJECT_BRIEF_CAPTURE_V1.into(),
        source_commit: source_commit.map(str::to_string),
        variants: load_policy(&toplevel)?,
        ..Default::default()
    };
    let mut heads = Vec::new();
    if constructs && let Some(checkout) = &checkout {
        match pin_of(checkout, None) {
            Ok(Some(pin)) => {
                let (pin_id, _) = cas
                    .put_artifact(
                        PROJECT_SURVEY_PIN_V1,
                        pin_producer(),
                        vec![],
                        None,
                        value_of(&pin)?,
                    )
                    .map_err(|error| error.to_string())?;
                heads.push(pin.identity()?.head.clone());
                record.pin_id = Some(pin_id);
            }
            Ok(None) => {}
            Err(error) => {
                eprintln!("af: no Project Brief can be constructed for this Task: {error}");
            }
        }
    }
    for id in bound_briefs {
        heads.push(read_brief(cas, id)?.head.commit);
    }
    for head in heads {
        let observed = ancestry(checkout.as_ref(), &head, source_commit);
        record.ancestry.insert(head, observed);
    }
    if record.is_empty() {
        return Ok(None);
    }
    record.validate()?;
    let refs = record.pin_id.iter().cloned().collect();
    cas.put_artifact(
        PROJECT_BRIEF_CAPTURE_V1,
        producer("capture-project-brief@1"),
        refs,
        None,
        value_of(&record)?,
    )
    .map(|(id, _)| Some(id))
    .map_err(|error| error.to_string())
}

fn typed<T: serde::de::DeserializeOwned>(
    cas: &Cas,
    id: &str,
    kind: &str,
) -> Result<(ArtifactEnvelope, T), String> {
    let envelope = cas.get_artifact(id).map_err(|error| error.to_string())?;
    if envelope.artifact_type != kind || envelope.artifact_id != id {
        return Err(format!("{id} is no {kind}"));
    }
    let value = serde_json::from_value(envelope.payload.clone())
        .map_err(|error| format!("{id} is no valid {kind}: {error}"))?;
    Ok((envelope, value))
}

/// Whether `id` is a typed record of `kind` in `cas`: an absent, unreadable or raw entry never
/// claims to be one.
fn is_record(cas: &Cas, id: &str, kind: &str) -> bool {
    let found = cas.get_optional_artifact(id).ok().flatten();
    found.is_some_and(|envelope| envelope.artifact_type == kind)
}

fn read_brief(cas: &Cas, id: &str) -> Result<ProjectBriefV1, String> {
    let (_, brief): (_, ProjectBriefV1) = typed(cas, id, PROJECT_BRIEF_V1)?;
    brief.validate()?;
    Ok(brief)
}

fn read_pin(cas: &Cas, id: &str) -> Result<ProjectSurveyPinV1, String> {
    let (_, pin): (_, ProjectSurveyPinV1) = typed(cas, id, PROJECT_SURVEY_PIN_V1)?;
    pin.validate()?;
    Ok(pin)
}

/// The capture `task`'s provenance holds; the empty one when it holds none.
fn recorded_capture(cas: &Cas, task: &TaskRevisionV1) -> Result<ProjectBriefCaptureV1, String> {
    let mut found = None;
    for id in &task.provenance.input_artifact_ids {
        if is_record(cas, id, PROJECT_BRIEF_CAPTURE_V1) {
            if found.is_some() {
                return Err("A Task revision records one Project Brief capture".into());
            }
            let (_, capture): (_, ProjectBriefCaptureV1) =
                typed(cas, id, PROJECT_BRIEF_CAPTURE_V1)?;
            capture.validate()?;
            found = Some(capture);
        }
    }
    Ok(found.unwrap_or_else(|| ProjectBriefCaptureV1 {
        schema: PROJECT_BRIEF_CAPTURE_V1.into(),
        ..Default::default()
    }))
}

/// The `project_brief` root constructor the compiler calls wherever it normalizes root inputs:
/// a pure function of the revision's capture and the CAS, never of a Survey, a policy file or
/// git.
#[derive(Debug)]
pub(crate) struct Resolver;

impl ProjectBriefResolver for Resolver {
    fn resolve(
        &self,
        cas: &Cas,
        task: &TaskRevisionV1,
        port: &str,
        variant: &str,
        nodes: &[String],
    ) -> Result<Option<ArtifactInputV1>, String> {
        let capture = recorded_capture(cas, task)?;
        let definition = match requested_variant(&capture.variants, variant) {
            None => {
                let wired = match nodes {
                    [] => "no node".to_string(),
                    nodes => format!("node {}", nodes.join(", ")),
                };
                return Err(format!(
                    "Project Brief port {port}, wired to {wired}, requests variant {variant}, \
                     which neither a [brief.{variant}] table nor a built-in variant defines"
                ));
            }
            Some(RequestedBriefV1::Nothing) => return Ok(None),
            Some(RequestedBriefV1::Render(definition)) => definition,
        };
        let Some(pin_id) = capture.pin_id.as_deref() else {
            return Ok(None);
        };
        let pin = read_pin(cas, pin_id)?;
        let (brief, _) = render(&pin, variant, &definition)
            .map_err(|error| format!("Project Brief port {port}: {error}"))?;
        let (id, _) = cas
            .put_artifact(
                PROJECT_BRIEF_V1,
                brief_producer(),
                vec![pin_id.to_string()],
                None,
                value_of(&brief)?,
            )
            .map_err(|error| error.to_string())?;
        Ok(Some(ArtifactInputV1 {
            artifact_ids: vec![id],
            artifact_type: PROJECT_BRIEF_V1.into(),
            cardinality: PortCardinality::One,
            snapshot_id: None,
        }))
    }
}

/// Admit a Brief a Task file binds: the kernel constructed it from a pinned Survey, and its
/// payload is that Survey's rendering. A Worker-authored one, or one whose sections are not the
/// pin's, is refused with the typed reason `not_kernel_constructed` before the binding
/// suppresses construction (ADR-0134 grants provenance, not verification).
pub(crate) fn admit_bound(cas: &Cas, input: &ArtifactInputV1) -> Result<(), String> {
    for id in &input.artifact_ids {
        let refused = |why: &str| format!("{NOT_KERNEL_CONSTRUCTED}: Project Brief {id} {why}");
        let (envelope, brief): (_, ProjectBriefV1) = typed(cas, id, PROJECT_BRIEF_V1)?;
        if envelope.producer != brief_producer() {
            return Err(refused(
                "was not rendered by the kernel's Brief constructor; a Worker never authors one",
            ));
        }
        let [pin_id] = envelope.input_artifacts.as_slice() else {
            return Err(refused(
                "names no single pinned Survey it was rendered from",
            ));
        };
        let pin = read_pin(cas, pin_id)
            .map_err(|error| refused(&format!("has no valid pin: {error}")))?;
        brief.validate().map_err(|error| refused(&error))?;
        if !rendered_from(&pin, &brief) {
            return Err(refused("is not a rendering of the Survey it names"));
        }
        let bytes = crate::projects::survey::canonical_bytes(&envelope.payload)?;
        if bytes.len() as u64 != brief.rendered_bytes.get() {
            return Err(refused("does not measure its own bytes"));
        }
    }
    Ok(())
}

/// Whether every section of `brief` is `pin`'s, verbatim but for the cuts a variant makes.
fn rendered_from(pin: &ProjectSurveyPinV1, brief: &ProjectBriefV1) -> bool {
    let Ok(identity) = pin.identity() else {
        return false;
    };
    fn same<T: PartialEq>(rendered: &Option<T>, pinned: &T) -> bool {
        rendered.as_ref().is_none_or(|rendered| rendered == pinned)
    }
    let sections = &brief.sections;
    let decisions = sections.decisions.as_ref().is_none_or(|rendered| {
        let mut expected = pin.decisions.clone();
        let count = rendered.body.as_ref().map_or(0, |body| body.records.len());
        if let Some(body) = expected.body.as_mut() {
            body.records = newest_decisions(&body.records, count);
        }
        *rendered == expected
    });
    let hot_paths = sections.hot_paths.as_ref().is_none_or(|rendered| {
        pin.history
            .body
            .as_ref()
            .is_some_and(|history| history.hot_paths.starts_with(rendered))
    });
    brief.survey_id == pin.survey_id
        && brief.project_id == identity.project_id
        && brief.head.commit == identity.head
        && brief.head.tree_digest == identity.tree_digest
        && same(&sections.identity, &pin.identity)
        && same(&sections.tree, &pin.tree)
        && same(&sections.history, &pin.history)
        && same(&sections.contributors, &pin.contributors)
        && same(&sections.vocabulary, &pin.vocabulary)
        && same(&sections.checks, &pin.checks)
        && decisions
        && hot_paths
}

/// Record one `af/ProjectBriefReceipt@1` per Brief port of `contract` in `revision`'s
/// provenance, replacing those of an earlier normalization: the Brief and its staleness, keyed
/// by the Brief ID and the source commit, or why the port carries none. Staleness comes from the
/// capture's ancestry observation alone, for a constructed and a bound Brief alike.
pub(crate) fn record_receipts(
    cas: &Cas,
    contract: &PipelineContractV1,
    revision: &mut TaskRevisionV1,
) -> Result<(), String> {
    let mut kept = Vec::new();
    for id in &revision.provenance.input_artifact_ids {
        if !is_record(cas, id, PROJECT_BRIEF_RECEIPT_V1) {
            kept.push(id.clone());
        }
    }
    revision.provenance.input_artifact_ids = kept;
    let capture = recorded_capture(cas, revision)?;
    let bound: BTreeSet<String> = crate::task_execution::recorded_input_bindings(cas, revision)?
        .map(|record| record.bindings.into_keys().collect())
        .unwrap_or_default();
    for (port, declared) in &contract.inputs {
        if declared.artifact_type != PROJECT_BRIEF_V1 {
            continue;
        }
        let receipt = match revision.inputs.get(port) {
            Some(input) => {
                let [id] = input.artifact_ids.as_slice() else {
                    return Err(format!("Project Brief port {port} carries one Brief"));
                };
                let brief = read_brief(cas, id)?;
                let staleness = capture.ancestry.get(&brief.head.commit).cloned().unwrap_or(
                    ProjectBriefAncestryV1::Unknown {
                        reason: ProjectBriefUnknownV1::NoCheckout,
                    },
                );
                ProjectBriefReceiptV1 {
                    schema: PROJECT_BRIEF_RECEIPT_V1.into(),
                    port: port.clone(),
                    outcome: if bound.contains(port) {
                        ProjectBriefOutcomeV1::Bound
                    } else {
                        ProjectBriefOutcomeV1::Constructed
                    },
                    variant: brief.variant,
                    reason: None,
                    brief_id: Some(id.clone()),
                    source_commit: capture.source_commit.clone(),
                    staleness: Some(staleness),
                }
            }
            None => {
                // A Brief port no constructor fills and nothing bound carries nothing to say.
                let Some(variant) = declared.brief.clone() else {
                    continue;
                };
                let reason = match requested_variant(&capture.variants, &variant) {
                    Some(RequestedBriefV1::Nothing) => ProjectBriefNoneReasonV1::VariantNone,
                    _ => ProjectBriefNoneReasonV1::NoSurvey,
                };
                ProjectBriefReceiptV1 {
                    schema: PROJECT_BRIEF_RECEIPT_V1.into(),
                    port: port.clone(),
                    outcome: ProjectBriefOutcomeV1::None,
                    variant,
                    reason: Some(reason),
                    brief_id: None,
                    source_commit: capture.source_commit.clone(),
                    staleness: None,
                }
            }
        };
        receipt.validate()?;
        let refs = receipt.brief_id.iter().cloned().collect();
        let (id, _) = cas
            .put_artifact(
                PROJECT_BRIEF_RECEIPT_V1,
                producer("receipt-project-brief@1"),
                refs,
                None,
                value_of(&receipt)?,
            )
            .map_err(|error| error.to_string())?;
        revision.provenance.input_artifact_ids.push(id);
    }
    Ok(())
}

/// The manifest entry the runner records for `brief` on port `port`: the bytes of the Worker
/// value it renders and its token estimate.
fn manifest_entry(
    port: &str,
    id: &str,
    payload: serde_json::Value,
) -> Result<ProjectBriefManifestEntryV1, String> {
    let value = review_runner::task::WorkerValue {
        artifact_id: id.into(),
        artifact_type: PROJECT_BRIEF_V1.into(),
        snapshot_id: None,
        payload,
    };
    let bytes = serde_json::to_vec(&value)
        .map_err(|error| error.to_string())?
        .len();
    Ok(ProjectBriefManifestEntryV1 {
        name: port.into(),
        required_by: format!("{PORT_REQUIRED_BY_PREFIX}{port}"),
        artifact_id: id.into(),
        artifact_type: PROJECT_BRIEF_V1.into(),
        rendered_bytes: (bytes as u64).into(),
        estimated_tokens: review_runner::estimate_tokens(bytes).into(),
    })
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(12)]
}

/// The preview line of a port that carries nothing.
fn none_line(port: &str, reason: ProjectBriefNoneReasonV1) -> String {
    match reason {
        ProjectBriefNoneReasonV1::VariantNone => format!("{port}: none (variant none)"),
        ProjectBriefNoneReasonV1::NoSurvey => {
            format!("{port}: none (no Survey recorded; run af project refresh)")
        }
    }
}

/// One line per Brief port of `revision`, from its receipts and the Briefs they name: what `af
/// task plan` and `af task explain` print beside the other inputs.
pub(crate) fn preview_lines(cas: &Cas, revision: &TaskRevisionV1) -> Result<Vec<String>, String> {
    let mut lines = BTreeMap::new();
    for id in &revision.provenance.input_artifact_ids {
        if !is_record(cas, id, PROJECT_BRIEF_RECEIPT_V1) {
            continue;
        }
        let (_, receipt): (_, ProjectBriefReceiptV1) = typed(cas, id, PROJECT_BRIEF_RECEIPT_V1)?;
        receipt.validate()?;
        let line = match (&receipt.brief_id, &receipt.staleness, receipt.reason) {
            (Some(brief_id), Some(staleness), _) => {
                let (envelope, brief): (_, ProjectBriefV1) =
                    typed(cas, brief_id, PROJECT_BRIEF_V1)?;
                let entry = manifest_entry(&receipt.port, brief_id, envelope.payload)?;
                let origin = match receipt.outcome {
                    ProjectBriefOutcomeV1::Bound => format!("bound, {}", brief.variant),
                    _ => brief.variant.clone(),
                };
                format!(
                    "{}: {PROJECT_BRIEF_V1} {brief_id} ({origin}, {} B, ~{} tokens, survey {}, behind {})",
                    receipt.port,
                    brief.rendered_bytes.get(),
                    entry.estimated_tokens.get(),
                    short(&brief.head.commit),
                    staleness.shown()
                )
            }
            (_, _, Some(reason)) => none_line(&receipt.port, reason),
            _ => return Err("A Brief receipt holds a Brief or a reason".into()),
        };
        lines.insert(receipt.port.clone(), line);
    }
    Ok(lines.into_values().collect())
}

/// Refuse, before any dispatch, a Worker node wired to a Brief whose known request (its
/// instructions and every root artifact wired to it, the Brief included) is over the Worker
/// request renderer's cap.
pub(crate) fn check_request_cap(
    cas: &Cas,
    compiler: &TaskPlanCompiler,
    graph: &CompiledTask,
    revision: &TaskRevisionV1,
) -> Result<(), String> {
    use review_runner::task::MAX_WORKER_BYTES;
    let briefs: BTreeSet<&str> = revision
        .inputs
        .iter()
        .filter(|(_, input)| input.artifact_type == PROJECT_BRIEF_V1)
        .map(|(name, _)| name.as_str())
        .collect();
    if briefs.is_empty() {
        return Ok(());
    }
    for (id, node) in &graph.nodes {
        let CompiledOperator::Primitive {
            operator:
                TaskOperatorV1::Worker { slot }
                | TaskOperatorV1::Verify { slot }
                | TaskOperatorV1::FixVerify { slot },
            ..
        } = &node.operator
        else {
            continue;
        };
        let roots: Vec<&str> = node
            .inputs
            .values()
            .filter(|address| address.node == "root.inputs")
            .map(|address| address.port.as_str())
            .collect();
        if !roots.iter().any(|port| briefs.contains(port)) {
            continue;
        }
        let instructions = graph
            .slots
            .get(slot)
            .and_then(|declaration| compiler.package_files(&declaration.worker))
            .and_then(|files| files.get("instructions.md"))
            .map_or(0, Vec::len);
        let mut bytes = instructions;
        for port in roots {
            let Some(input) = revision.inputs.get(port) else {
                continue;
            };
            for artifact in &input.artifact_ids {
                let envelope = cas
                    .get_artifact(artifact)
                    .map_err(|error| error.to_string())?;
                let value = review_runner::task::WorkerValue {
                    artifact_id: artifact.clone(),
                    artifact_type: envelope.artifact_type,
                    snapshot_id: envelope.subject_snapshot_id,
                    payload: envelope.payload,
                };
                bytes += serde_json::to_vec(&value)
                    .map_err(|error| error.to_string())?
                    .len();
            }
        }
        if bytes > MAX_WORKER_BYTES {
            return Err(format!(
                "Worker node {id} would receive {bytes} bytes of instructions and root inputs \
                 with its Project Brief, over the Worker request cap of {MAX_WORKER_BYTES} bytes; \
                 lower the Brief's max_bytes or wire the port to fewer nodes"
            ));
        }
    }
    Ok(())
}

/// `af project brief`: the exact bytes the port would carry for `variant` (default `orient`) of
/// the newest complete Survey or the one `survey` names, token-free and recording nothing.
/// `--json` prints `af/project-brief-preview@1`, with `none` and its reason (exit 0) where no
/// Brief is rendered; the text form without a Survey exits 1 with the preview's sentence.
pub(crate) fn command(
    variant: Option<String>,
    survey: Option<String>,
    json: bool,
) -> Result<i32, String> {
    let checkout = crate::projects::require_checkout(Path::new("."))?;
    let variants = load_policy(&checkout.toplevel)?;
    let name = variant.unwrap_or_else(|| ORIENT.into());
    let none = |reason| {
        let preview = ProjectBriefPreviewV1 {
            schema: PROJECT_BRIEF_PREVIEW_V1.into(),
            variant: name.clone(),
            outcome: ProjectBriefPreviewOutcomeV1::None,
            reason: Some(reason),
            brief_id: None,
            brief: None,
            manifest: None,
        };
        if json {
            print_preview(&preview)?;
            return Ok(0);
        }
        match reason {
            ProjectBriefNoneReasonV1::VariantNone => {
                eprintln!("{}", none_line(PREVIEW_PORT, reason));
                Ok(0)
            }
            // `af project: none (…)`: the preview's sentence, after the command's own prefix.
            ProjectBriefNoneReasonV1::NoSurvey => Err(none_line(PREVIEW_PORT, reason)
                .trim_start_matches(&format!("{PREVIEW_PORT}: "))
                .to_string()),
        }
    };
    let definition = match requested_variant(&variants, &name) {
        None => {
            return Err(format!(
                "no variant {name}: neither a [brief.{name}] table nor a built-in variant \
                 (orient, none) defines it"
            ));
        }
        Some(RequestedBriefV1::Nothing) => return none(ProjectBriefNoneReasonV1::VariantNone),
        Some(RequestedBriefV1::Render(definition)) => definition,
    };
    let Some(pin) = pin_of(&checkout, survey.as_deref())? else {
        return none(ProjectBriefNoneReasonV1::NoSurvey);
    };
    let (brief, bytes) = render(&pin, &name, &definition)?;
    if !json {
        println!(
            "{}",
            std::str::from_utf8(&bytes).map_err(|error| error.to_string())?
        );
        return Ok(0);
    }
    let (_, brief_id) = identities(&pin, &brief)?;
    let payload = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let preview = ProjectBriefPreviewV1 {
        schema: PROJECT_BRIEF_PREVIEW_V1.into(),
        variant: name.clone(),
        outcome: ProjectBriefPreviewOutcomeV1::Brief,
        reason: None,
        manifest: Some(manifest_entry(PREVIEW_PORT, &brief_id, payload)?),
        brief_id: Some(brief_id),
        brief: Some(brief),
    };
    print_preview(&preview)?;
    Ok(0)
}

/// The preview's canonical bytes, so its `brief` member is byte for byte the text form.
fn print_preview(preview: &ProjectBriefPreviewV1) -> Result<(), String> {
    preview.validate()?;
    let bytes = crate::projects::survey::canonical_bytes(&value_of(preview)?)?;
    println!(
        "{}",
        std::str::from_utf8(&bytes).map_err(|error| error.to_string())?
    );
    Ok(())
}

#[cfg(test)]
#[path = "brief/tests.rs"]
mod tests;
