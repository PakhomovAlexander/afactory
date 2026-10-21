//! A real admitted Task over a compiled two-node graph, created through public writers.
//!
//! The Task revision, the three captured packages, the compiled plan and every event come from
//! the product's own compiler and protected Store writers: no SQL, no private test module, no
//! relaxed validation. Leases are the production 15-second lease, renewed or freshly taken
//! (never extended) immediately before a timed region by whoever measures.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use review_config::lock::package_digest_from_files;
use review_config::task::catalog::{AdmittedWorkerSettings, TaskPackagePin, TaskPlanCompiler};
use review_core::task::execution::{TaskInvocationV1, TaskOutputV1};
use review_core::task::plan::{ExecutionPlanV1, IndependencePolicyV1, WorkerExecutionV1};
use review_core::task::{EXECUTION_PLAN_V1, TASK_REVISION_V1, TaskResultV1, TaskRevisionV1};
use review_graph::task::{CompiledNode, CompiledTask};
use review_pipeline::task::host::{CapturedTaskAuthority, TaskDomain};
use review_pipeline::task::{TaskOperatorHost, TaskWorkOutput};
use review_store::store::task::execution::{PreparedTaskAttempt, ReservedTaskAttempt};
use review_store::store::task::{TaskLease, TaskProjection};
use review_store::{Cas, EventStore};
use serde_json::{Value, json};

use crate::fixtures;
use crate::protocol::{Failure, FailureKind};

pub const TASK_ID: &str = "perf-probe-task";
pub const WRITER: &str = "perf-probe";

/// The admitted Task's Store, CAS and lease, which live under the sample's scratch.
pub struct AuthorityFixture {
    pub cas: Cas,
    pub store: EventStore,
    pub lease: TaskLease,
}

pub fn now_ms() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())
        .map(|elapsed| elapsed.as_millis() as u64)
}

fn capture(
    compiler: &mut TaskPlanCompiler,
    cas: &Cas,
    name: &str,
    files: BTreeMap<String, Vec<u8>>,
) -> Result<String, String> {
    let pin = TaskPackagePin {
        version: fixtures::PACKAGE_VERSION.into(),
        digest: package_digest_from_files(&files),
        path: "package".into(),
    };
    let project = files
        .into_iter()
        .map(|(path, bytes)| (format!("package/{path}"), bytes))
        .collect();
    compiler.capture_package(cas, name, &pin, &project)
}

/// Capture the `perf/author` package alone into `cas`, as the compiler does; the typed-envelope
/// fixtures of the canonical and CAS cases are exactly these envelopes.
pub fn capture_author_package(cas: &Cas, padding: Option<Vec<u8>>) -> Result<String, String> {
    let mut compiler = compiler(cas)?;
    capture(
        &mut compiler,
        cas,
        fixtures::AUTHOR_PACKAGE,
        fixtures::author_files(padding),
    )
}

fn compiler(cas: &Cas) -> Result<TaskPlanCompiler, String> {
    let policy = cas
        .put_json(&fixtures::policy_document())
        .map_err(|error| error.to_string())?;
    if policy != fixtures::POLICY_ID {
        return Err(format!(
            "fixture policy is {policy}, not the declared {}",
            fixtures::POLICY_ID
        ));
    }
    TaskPlanCompiler::new(
        policy.clone(),
        policy,
        BTreeMap::new(),
        BTreeMap::from([("checked".into(), "document".into())]),
        IndependencePolicyV1::default(),
    )
}

fn revision(requirements: &str, deadline: u64) -> Result<TaskRevisionV1, String> {
    let policy = fixtures::POLICY_ID;
    serde_json::from_value(json!({
        "task_id": TASK_ID, "revision": 1, "kind": "document",
        "goal": "Hold a compiled two-node graph for the performance laboratory.",
        "inputs": {"requirements": {"artifact_ids": [requirements],
            "artifact_type": fixtures::REQUIREMENTS_V1, "cardinality": "one"}},
        "required_outputs": {"document": {"artifact_type": "af/CheckedDocument@1",
            "cardinality": "one"}},
        "acceptance": {"checked": {"evidence_type": "af/CheckedDocument@1",
            "verifier_policy": policy}},
        "provenance": {"adapter_id": policy, "input_artifact_ids": [requirements]},
        "authority": {"policy_id": policy, "allowed_effects": [], "data_destinations": []},
        "limits": {"tokens": 1000, "max_attempts": 4, "deadline_unix_ms": deadline,
            "verification": {"tokens": 200, "attempts": 2, "wall_ms": 10000}},
        "strategy": "conservative",
        "pipeline": {"name": fixtures::PIPELINE_PACKAGE, "fallback": "refuse"},
        "facts": {}
    }))
    .map_err(|error| error.to_string())
}

/// Build, propose and admit the Task in a fresh Store and CAS under `root`. `lease_ms` is the
/// production lease for every probe request; only the expiry tests pass a shorter one.
pub fn build(
    root: &Path,
    padding: Option<Vec<u8>>,
    lease_ms: u64,
) -> Result<AuthorityFixture, String> {
    build_then(
        root,
        padding,
        lease_ms,
        Setup::default(),
        |_, _, _, _, _| (),
    )
    .map(|(fixture, ())| fixture)
    .map_err(|failure| failure.detail)
}

/// A setup hook given the Store and a setup lease; only a test passes one that does anything.
pub type LeaseHook<'a> = Box<dyn FnOnce(&EventStore, &TaskLease) -> Result<(), String> + 'a>;

/// How [`build_then`] owns the Task while it sets it up.
pub struct Setup<'a> {
    /// Give each setup write a production lease of its own. The opening lease covers the
    /// proposal and is released; a lease taken at the next epoch covers the admission and is
    /// released too, so the measuring writer takes a whole lease of its own immediately before
    /// its region ([`take_fresh_lease`]). Every lease is released while it is live: one the
    /// Store reports lapsed is the side's `lease_expired` failure, and no later lease replaces
    /// it. An authority sample does this; the plain fixture [`build`] does not, because its
    /// event log is exactly opened, proposed and admitted, and the expiry tests keep its lease
    /// to let it lapse.
    pub hand_over: bool,
    /// Runs between the capture and the lease's opening, where the capture's own cost lies;
    /// only a test passes one that does anything.
    pub after_capture: Box<dyn FnOnce() -> Result<(), String> + 'a>,
    /// Runs after the opening lease is taken and before the proposal is written, with the Store
    /// and that lease; only a test passes one that does anything, to wait until the Store
    /// itself reports a short lease lapsed, so the ordering of expiry and the write is
    /// established rather than assumed.
    pub after_open: LeaseHook<'a>,
    /// Runs after the proposal's lease is released and before the admission's is taken, with
    /// [`Setup::hand_over`]; only a test passes one that does anything.
    pub between_writes: Box<dyn FnOnce() -> Result<(), String> + 'a>,
}

impl Default for Setup<'_> {
    fn default() -> Self {
        Setup {
            hand_over: false,
            after_capture: Box::new(|| Ok(())),
            after_open: Box::new(|_: &EventStore, _: &TaskLease| Ok(())),
            between_writes: Box::new(|| Ok(())),
        }
    }
}

/// [`build`], then `then` over the admitted Task and what its region captures from.
///
/// Capturing the host and authority validates the compiled plan. The setup writes' capture
/// happens once, before the Task's lease is opened, so no capture runs while a setup lease is
/// live; it serves only proposal and admission and is dropped before `then` runs. Proposing and
/// admitting validate the plan as well and need a lease; on a loaded host one of those writes
/// alone can take most of a 15-second lease, so with [`Setup::hand_over`] each gets its own.
/// The region captures a fresh host and authority through [`Region::capture`]. A write that
/// runs past its lease still fails, through the Store; nothing is retried, renewed in a region
/// or extended.
pub fn build_then<T>(
    root: &Path,
    padding: Option<Vec<u8>>,
    lease_ms: u64,
    setup: Setup<'_>,
    then: impl FnOnce(&mut EventStore, &Cas, &TaskLease, &BTreeMap<String, String>, &Region<'_>) -> T,
) -> Result<(AuthorityFixture, T), Failure> {
    let fixture = |error: String| Failure::new(FailureKind::FixtureFailed, error);
    let cas = Cas::open(root.join("cas")).map_err(|error| fixture(error.to_string()))?;
    let mut store =
        EventStore::open(root.join("events.sqlite")).map_err(|error| fixture(error.to_string()))?;
    let mut compiler = compiler(&cas).map_err(fixture)?;
    let mut packages = BTreeMap::new();
    for (name, files) in [
        (fixtures::PIPELINE_PACKAGE, fixtures::pipeline_files()),
        (fixtures::AUTHOR_PACKAGE, fixtures::author_files(padding)),
        (fixtures::CHECKER_PACKAGE, fixtures::checker_files()),
    ] {
        let id = capture(&mut compiler, &cas, name, files).map_err(fixture)?;
        packages.insert(name.to_string(), id);
    }
    for name in [fixtures::AUTHOR_PACKAGE, fixtures::CHECKER_PACKAGE] {
        compiler
            .bind_worker(
                name,
                AdmittedWorkerSettings {
                    execution: WorkerExecutionV1::Command {},
                    invocation_policy_id: fixtures::POLICY_ID.into(),
                },
            )
            .map_err(fixture)?;
    }
    let (requirements, _) = cas
        .put_artifact(
            fixtures::REQUIREMENTS_V1,
            fixtures::producer(),
            vec![],
            None,
            fixtures::requirements_payload(64),
        )
        .map_err(|error| fixture(error.to_string()))?;
    let task = revision(&requirements, now_ms().map_err(fixture)? + 3_600_000).map_err(fixture)?;
    let (revision_id, _) = cas
        .put_artifact(
            TASK_REVISION_V1,
            fixtures::producer(),
            vec![],
            None,
            serde_json::to_value(&task).map_err(|error| fixture(error.to_string()))?,
        )
        .map_err(|error| fixture(error.to_string()))?;
    let (plan, graph) = compiler
        .compile(&cas, &revision_id, fixtures::PIPELINE_PACKAGE)
        .map_err(fixture)?;
    let (plan_id, _) = cas
        .put_artifact(
            EXECUTION_PLAN_V1,
            fixtures::producer(),
            vec![revision_id.clone()],
            None,
            serde_json::to_value(&plan).map_err(|error| fixture(error.to_string()))?,
        )
        .map_err(|error| fixture(error.to_string()))?;
    let lease = captured(
        &cas,
        &compiler,
        &task,
        &plan,
        graph.clone(),
        |authority, _| {
            (setup.after_capture)().map_err(fixture)?;
            let lease = store
                .open_task(&cas, &revision_id, WRITER, lease_ms)
                .map_err(|error| fixture(error.to_string()))?;
            (setup.after_open)(&store, &lease).map_err(fixture)?;
            let proposed = store.propose_task_plan(&cas, &lease, &plan_id, authority);
            written(&store, &lease, proposed)?;
            let lease = if setup.hand_over {
                hand_over(&mut store, &cas, &lease)?;
                (setup.between_writes)().map_err(fixture)?;
                store
                    .take_task_lease(&cas, TASK_ID, WRITER, lease_ms)
                    .map_err(|error| {
                        fixture(format!("taking the admission's lease failed: {error}"))
                    })?
            } else {
                lease
            };
            let admitted = store.admit_task_plan(&cas, &lease, authority);
            written(&store, &lease, admitted)?;
            if setup.hand_over {
                hand_over(&mut store, &cas, &lease)?;
            }
            Ok::<_, Failure>(lease)
        },
    )
    .map_err(fixture)??;
    let region = Region {
        cas: &cas,
        compiler: &compiler,
        task: &task,
        plan: &plan,
        graph: &graph,
    };
    let value = then(&mut store, &cas, &lease, &packages, &region);
    Ok((AuthorityFixture { cas, store, lease }, value))
}

/// A setup write's result: a failure is `lease_expired` only when the Store reports its lease
/// lapsed or fenced, and the fixture's otherwise.
fn written<T>(
    store: &EventStore,
    lease: &TaskLease,
    result: Result<T, review_store::StoreError>,
) -> Result<T, Failure> {
    result.map_err(|error| {
        crate::cases::classify(store, lease, error.to_string(), FailureKind::FixtureFailed)
    })
}

/// End a setup writer's lease after its last write by releasing it through the public writer,
/// while it is live. A lease the Store reports lapsed is `lease_expired`: nothing is written for
/// it, and the setup that needed it fails rather than continuing under a replacement.
pub fn hand_over(store: &mut EventStore, cas: &Cas, lease: &TaskLease) -> Result<(), Failure> {
    let released = store.release_task_lease(cas, lease).map(|_| ());
    written(store, lease, released).map_err(|failure| {
        Failure::new(
            failure.kind,
            format!("releasing a setup lease failed: {}", failure.detail),
        )
    })
}

/// A fresh production lease of the measuring writer, taken through the public writer
/// immediately before a timed region, after [`Setup::hand_over`] released the setup writer's.
pub fn take_fresh_lease(store: &mut EventStore, cas: &Cas) -> Result<TaskLease, String> {
    store
        .take_task_lease(cas, TASK_ID, WRITER, fixtures::TASK_LEASE_MS)
        .map_err(|error| format!("taking a fresh lease before the region failed: {error}"))
}

/// What a timed region captures its own host and authority from, after the setup writes and
/// their capture are done.
pub struct Region<'a> {
    cas: &'a Cas,
    compiler: &'a TaskPlanCompiler,
    task: &'a TaskRevisionV1,
    plan: &'a ExecutionPlanV1,
    graph: &'a CompiledTask,
}

impl Region<'_> {
    /// Capture a host and authority that no setup write used and hand them to `operation`. The
    /// authority's plan validator is new, so the region's first check recompiles the plan, as
    /// a first check after admission does in a fresh process. The capture validates the plan
    /// through the compiler, not through that validator; callers run it before taking the
    /// region's lease.
    pub fn capture<T>(
        &self,
        operation: impl FnOnce(&CapturedTaskAuthority<'_>, &dyn TaskOperatorHost) -> T,
    ) -> Result<T, String> {
        captured(
            self.cas,
            self.compiler,
            self.task,
            self.plan,
            self.graph.clone(),
            operation,
        )
    }
}

/// Refuse to time a check through an authority whose validator already holds a validated
/// plan: that check would reuse it instead of recompiling, and the region would not be cold.
pub fn require_cold(authority: &CapturedTaskAuthority<'_>) -> Result<(), String> {
    if authority.holds_validated_plan() {
        return Err(
            "the region's authority already holds a validated plan, so its first \
                    check would not be cold"
                .into(),
        );
    }
    Ok(())
}

/// Capture the host and the authority over `compiler` and hand them to `operation`. Capturing
/// validates the plan again; callers keep it outside any timed region and before any lease
/// they hold has to last.
fn captured<T>(
    cas: &Cas,
    compiler: &TaskPlanCompiler,
    task: &TaskRevisionV1,
    plan: &ExecutionPlanV1,
    graph: CompiledTask,
    operation: impl FnOnce(&CapturedTaskAuthority<'_>, &dyn TaskOperatorHost) -> T,
) -> Result<T, String> {
    let host = review_pipeline::task::host::CapturedTaskHost::capture_with_models(
        cas,
        compiler,
        task,
        plan,
        graph,
        &review_pipeline::task::host::EmptyTaskEnvironment,
        &ProbeDomain,
        &BTreeMap::new(),
    )?;
    let authority = CapturedTaskAuthority::new(
        compiler,
        &host,
        &review_pipeline::task::host::NoTaskDeveloper,
    );
    Ok(operation(&authority, &host))
}

impl AuthorityFixture {
    /// Renew the production lease; called immediately before a timed region.
    pub fn renew(&mut self) -> Result<(), String> {
        self.store
            .renew_task_lease(&self.cas, &self.lease, fixtures::TASK_LEASE_MS)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub fn run_id(&self) -> Result<String, String> {
        review_store::store::task::task_run_id(TASK_ID).map_err(|error| error.to_string())
    }
}

/// The stable semantic projection of an admitted Task over the two-node graph: phase,
/// admission, graph order and slots, and every captured package's identity and envelope size.
/// Lease, clock and revision metadata (the revision carries a deadline) stay outside it.
pub fn semantic_projection(
    cas: &Cas,
    state: &TaskProjection,
    plan: &ExecutionPlanV1,
    graph: &CompiledTask,
) -> Result<Value, String> {
    let mut packages = serde_json::Map::new();
    for (name, dependency) in &plan.dependencies {
        let envelope_bytes = cas
            .stored_len(&dependency.artifact_id)
            .map_err(|error| error.to_string())?;
        packages.insert(
            name.clone(),
            json!({"artifact_id": dependency.artifact_id,
                   "content_id": dependency.content_digest,
                   "envelope_bytes": envelope_bytes}),
        );
    }
    let slots: serde_json::Map<String, Value> = graph
        .slots
        .iter()
        .map(|(slot, declaration)| (slot.clone(), json!(declaration.worker)))
        .collect();
    Ok(json!({
        "task_id": state.task_id,
        "phase": serde_json::to_value(&state.phase).map_err(|error| error.to_string())?,
        "admitted": state.admitted,
        "plan_recorded": state.plan_id.is_some(),
        "graph_order": graph.order,
        "graph_slots": slots,
        "packages": packages,
    }))
}

/// A domain that admits nothing: the measured constructors never execute a node, and an
/// accidental execution must fail rather than succeed.
pub struct ProbeDomain;

impl TaskOperatorHost for ProbeDomain {
    fn prepare_context(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _: &CompiledNode,
        _: &ReservedTaskAttempt,
    ) -> Result<String, String> {
        Err("the performance probe executes no Worker".into())
    }

    fn execute(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _: &CompiledNode,
        _: Option<&PreparedTaskAttempt>,
        _: Option<&std::sync::atomic::AtomicBool>,
    ) -> TaskWorkOutput {
        // These workloads only validate a plan and construct its runtime. Any execution is
        // outside the workload contract and must fail the sample, never dispatch a Worker.
        panic!("the performance probe must not execute a node")
    }
}

impl TaskDomain for ProbeDomain {
    fn validate_context(
        &self,
        _: &Cas,
        _: &TaskInvocationV1,
        _: &ReservedTaskAttempt,
        _: &str,
    ) -> Result<(), String> {
        Err("the performance probe admits no context".into())
    }

    fn validate_output(
        &self,
        _: &Cas,
        _: &TaskRevisionV1,
        _: &ExecutionPlanV1,
        _: &TaskInvocationV1,
        _: &TaskOutputV1,
        _: &CompiledNode,
    ) -> Result<(), String> {
        Err("the performance probe admits no output".into())
    }

    fn validate_result(&self, _: &Cas, _: &TaskRevisionV1, _: &TaskResultV1) -> Result<(), String> {
        Err("the performance probe admits no result".into())
    }
}
