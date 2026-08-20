//! The composition layer: the graph driving the real nodes.
//!
//! Everything below this crate was built to be provable in isolation — capture without a
//! scheduler, scheduling without models, checks without a graph. This is where they meet, and
//! the only thing it adds is wiring. That is deliberate: if composing them required new rules,
//! the boundaries underneath would be wrong.
//!
//! One review therefore looks like this, end to end:
//!
//! ```text
//!   capture ── snapshot ──┐
//!                         v
//!            gate (checks in a sandbox) ──decision──┐
//!                                                   v
//!                    architecture ┐  performance ┐  tdd ┐   (each sandboxed, gated)
//!                                 └──────────────┴──────┴──> gather
//!                                                              │
//!                                                              v
//!                                                           ledger ──> convergence
//! ```
//!
//! A blocked gate makes every node after it unreachable, so a review that could not build
//! produces no reviewer artifacts at all — not reviewer artifacts nobody reads.

use std::collections::BTreeMap;
use std::sync::Mutex;

use review_attempt::{
    AttemptId, AttemptLedger, Budget, BudgetLedger, Receipt, Reservation, Scope, Selection,
};
use review_check::{CheckDefinition, CheckRunner, Command, GateDecision, check_event};
use review_core::{
    EventType, LegacyStageOutput, MissingNodeV2, NodeInvocationPayloadV1,
    NodeOutputReceiptPayloadV1, PortArtifactsV1, RunFailureReasonV2, RunNodeOutcomeV2,
    RunNodeReportV2, RunReportPayloadV2, RunSuppressionReasonV2, RunVerdictV2, SnapshotAffinity,
};
use review_graph::{ArtifactMap, Dispatch, Node, NodeKind, NodeOutcome, PortContract, RunReport};
use review_runner::{ReviewerAdapter, ReviewerInputs, RunnerError};
use review_sandbox::{Mode, Sandbox};
use review_source_git::Manifest;
use review_store::{
    Cas, Convergence, ConvergencePolicy, EventStore, Ingest, Ledger, NewEvent, Verdict,
};

/// The input port a reviewer receives the campaign's prior findings on.
const PRIOR_FINDINGS_PORT: &str = "prior_findings";

/// The artifact ids a node's resolved inputs carry, dropping the port labels — for reducers
/// (gather, ledger) that consume artifacts regardless of which port delivered them.
fn artifact_ids(inputs: &ArtifactMap) -> Vec<String> {
    inputs.values().flatten().cloned().collect()
}

fn bind_single_output(node: &Node, artifacts: Vec<String>) -> Result<ArtifactMap, String> {
    let [port] = node.outputs.as_slice() else {
        return Err(format!(
            "node {} has {} output ports, but its built-in dispatcher produces one port",
            node.id,
            node.outputs.len()
        ));
    };
    Ok(BTreeMap::from([(port.name.clone(), artifacts)]))
}

fn port_artifacts(
    contracts: &[PortContract],
    artifacts: &ArtifactMap,
    subject_snapshot_id: &str,
) -> Vec<PortArtifactsV1> {
    contracts
        .iter()
        .map(|port| PortArtifactsV1 {
            port: port.name.clone(),
            artifact_type: port.artifact_type.clone(),
            cardinality: port.cardinality,
            optional: port.optional,
            snapshot_affinity: port.snapshot_affinity,
            artifact_ids: artifacts.get(&port.name).cloned().unwrap_or_default(),
            subject_snapshot_id: (port.snapshot_affinity == SnapshotAffinity::SameSubject)
                .then(|| subject_snapshot_id.to_string()),
        })
        .collect()
}

/// The run's own budget accounts, alongside the caps that opened them.
struct Budgets {
    attempt_cap: u64,
    ledger: Mutex<BudgetLedger>,
}

struct PreparedReviewerAttempt {
    attempt: AttemptId,
    reservation: Option<Reservation>,
}

/// What one whole run amounts to.
///
/// `Incomplete` exists because a partial review must never pass on the strength of the part
/// that ran: a run where any node failed or was suppressed — a blocked gate, a refused budget
/// reservation, a crashed reviewer — reports *which* nodes never contributed and cannot pass
/// the gate, whatever the ledger's findings would have said. This is the owner's exhaustion
/// policy (2026-08-18): finish in-flight work, dispatch nothing new, and fail closed at the
/// verdict rather than by discarding paid work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunVerdict {
    Pass,
    Fail(Verdict),
    Incomplete { missing: Vec<(String, String)> },
}

impl RunVerdict {
    pub fn passed(&self) -> bool {
        matches!(self, RunVerdict::Pass)
    }
}

/// Combine what ran with what converged. Completeness is checked first: convergence is a
/// statement about the findings that exist, and says nothing about the reviewers that never
/// produced any.
pub fn run_verdict(report: &RunReport, convergence: &Convergence) -> RunVerdict {
    let missing: Vec<(String, String)> = report
        .outcomes
        .iter()
        .filter_map(|(id, outcome)| match outcome {
            NodeOutcome::Completed { .. } => None,
            NodeOutcome::Failed { error } => Some((id.clone(), error.clone())),
            NodeOutcome::Suppressed { reason } => Some((id.clone(), format!("{reason:?}"))),
        })
        .collect();
    if !missing.is_empty() {
        return RunVerdict::Incomplete { missing };
    }
    // A gate can be intentionally observational and gate no downstream node. Its blocked
    // decision still prevents a pass even though there is then no suppressed outcome to make
    // the run incomplete.
    if !report.blocked_gates.is_empty() {
        return RunVerdict::Fail(Verdict::NotConverged);
    }
    match convergence.verdict {
        Verdict::Converged => RunVerdict::Pass,
        other => RunVerdict::Fail(other),
    }
}

/// What a pipeline needs to run one generation.
pub struct Kernel<'a> {
    cas: &'a Cas,
    store: Mutex<&'a mut EventStore>,
    run_id: String,
    /// The immutable subject. Every node is materialized from this, so they all inspect the
    /// same content by construction rather than by discipline.
    snapshot: Manifest,
    checks: Vec<CheckDefinition>,
    reviewers: BTreeMap<String, Box<dyn ReviewerAdapter>>,
    attempts: Mutex<AttemptLedger>,
    budgets: Option<Budgets>,
    /// Retries per node, spent only on timeouts. A retry is a new attempt: it fences its
    /// predecessor and reserves its own budget.
    timeout_retries: u32,
    /// Gate decisions by gate node. Keyed, so two gates in one pipeline never share a verdict.
    gates: Mutex<BTreeMap<String, GateDecision>>,
    /// The campaign's prior findings, as a CAS artifact every reviewer attempt receives —
    /// labelled data resolved by the kernel, which is what makes round N+1 a re-examination
    /// of round N's claims instead of a fresh look that happens to share a repository.
    prior_findings: Option<String>,
    /// Reviewer-thread events, held until a barrier flushes them in canonical order. Reviewers
    /// run concurrently, and appending from a worker thread would assign sequences — and the
    /// event IDs derived from them — in whatever order the threads happened to reach the log,
    /// so two identical runs would produce different, incomparable logs. Each `(node, seq)`
    /// keeps a node's own events in the order it emitted them; the flush sorts by that key.
    reviewer_events: Mutex<Vec<((String, u64), NewEvent)>>,
    reviewer_event_seq: Mutex<u64>,
    /// First attempts are reserved, assigned, and durably dispatched by the scheduler thread in
    /// plan order before any external model call starts. The worker removes its prepared entry.
    prepared_attempts: Mutex<BTreeMap<String, PreparedReviewerAttempt>>,
    /// The snapshot materialized once, cloned per sandbox. Built lazily on the first sandbox
    /// request — the gate's — so a run that never reaches a sandbox never pays for it.
    template: Mutex<Option<std::sync::Arc<review_sandbox::SandboxTemplate>>>,
}

impl<'a> Kernel<'a> {
    pub fn new(
        cas: &'a Cas,
        store: &'a mut EventStore,
        run_id: impl Into<String>,
        snapshot: Manifest,
    ) -> Kernel<'a> {
        Kernel {
            cas,
            store: Mutex::new(store),
            run_id: run_id.into(),
            snapshot,
            checks: Vec::new(),
            reviewers: BTreeMap::new(),
            attempts: Mutex::new(AttemptLedger::default()),
            budgets: None,
            timeout_retries: 1,
            gates: Mutex::new(BTreeMap::new()),
            prior_findings: None,
            reviewer_events: Mutex::new(Vec::new()),
            reviewer_event_seq: Mutex::new(0),
            prepared_attempts: Mutex::new(BTreeMap::new()),
            template: Mutex::new(None),
        }
    }

    pub fn with_checks(mut self, checks: Vec<CheckDefinition>) -> Self {
        self.checks = checks;
        self
    }

    pub fn with_reviewer(mut self, node_id: impl Into<String>, command: Command) -> Self {
        self.reviewers.insert(node_id.into(), Box::new(command));
        self
    }

    /// Bind a model-backed (or any other) adapter to a node, behind the same contract the
    /// `command` reviewers use.
    pub fn with_adapter(
        mut self,
        node_id: impl Into<String>,
        adapter: Box<dyn ReviewerAdapter>,
    ) -> Self {
        self.reviewers.insert(node_id.into(), adapter);
        self
    }

    /// Deliver a prior-findings artifact (already in the CAS) to every reviewer attempt.
    pub fn with_prior_findings(mut self, artifact: impl Into<String>) -> Self {
        self.prior_findings = Some(artifact.into());
        self
    }

    /// Cap the run. Reservation before every dispatch; a dispatch that cannot reserve does not
    /// happen, and the refusal names the scope that said no.
    pub fn with_budgets(mut self, attempt_cap: u64, run_cap: u64) -> Self {
        self.budgets = Some(Budgets {
            attempt_cap,
            ledger: Mutex::new(BudgetLedger::default().with_limit(Scope::Run, Budget::of(run_cap))),
        });
        self
    }

    /// Tokens committed so far, across every attempt including fenced ones. `None` when the
    /// run is uncapped.
    pub fn spent(&self) -> Option<u64> {
        self.budgets.as_ref().map(|b| {
            b.ledger
                .lock()
                .expect("budget ledger")
                .committed(&Scope::Run)
        })
    }

    /// Every attempt this run made, quarantines included — the operator's view.
    pub fn attempts(&self) -> AttemptLedger {
        self.attempts.lock().expect("attempt ledger").clone()
    }

    /// The decision a gate node reached, if it ran.
    pub fn gate_decision(&self, node_id: &str) -> Option<GateDecision> {
        self.gates.lock().expect("gates").get(node_id).cloned()
    }

    /// The ledger as it stands, rebuilt from the log rather than accumulated in memory.
    pub fn ledger(&self) -> Ledger {
        Ledger::rebuild(
            *self.store.lock().expect("event store"),
            self.cas,
            &self.run_id,
        )
        .expect("replay")
    }

    pub fn convergence(&self, policy: ConvergencePolicy) -> Convergence {
        self.ledger().convergence(policy)
    }

    /// Append one event to the run's log. Everything the kernel decides goes through here:
    /// the log is the authority a run is rebuilt from, so a decision it never saw is a
    /// decision that, on replay, never happened.
    fn append(&self, event: NewEvent) -> Result<(), String> {
        self.store
            .lock()
            .expect("event store")
            .append(&self.run_id, self.cas, event)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn append_batch(&self, events: &[NewEvent]) -> Result<(), String> {
        self.store
            .lock()
            .expect("event store")
            .append_batch(&self.run_id, self.cas, events)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn prepare_reviewer_attempt(
        &self,
        node_id: &str,
        prior_findings_artifact: Option<&String>,
        timeouts: &[String],
    ) -> Result<PreparedReviewerAttempt, String> {
        let reservation = match &self.budgets {
            Some(budgets) => Some(
                budgets
                    .ledger
                    .lock()
                    .expect("budget ledger")
                    .reserve(
                        &[Scope::Node(node_id.to_string()), Scope::Run],
                        budgets.attempt_cap,
                    )
                    .map_err(|error| {
                        if timeouts.is_empty() {
                            format!("never dispatched: {error}")
                        } else {
                            format!("{}; retry refused: {error}", timeouts.join("; "))
                        }
                    })?,
            ),
            None => None,
        };
        let attempt = self
            .attempts
            .lock()
            .expect("attempt ledger")
            .dispatch(node_id);
        let event = NewEvent::new(
            EventType::AttemptDispatchedV1,
            serde_json::json!({
                "reserved": reservation.as_ref().map(|reservation| reservation.amount),
                "prior_findings": prior_findings_artifact,
            }),
        )
        .node(node_id)
        .attempt(attempt.to_string())
        .referencing(prior_findings_artifact.cloned().into_iter().collect());
        if let Err(error) = self.append(event) {
            if let (Some(budgets), Some(reservation)) = (&self.budgets, &reservation) {
                budgets
                    .ledger
                    .lock()
                    .expect("budget ledger")
                    .release(reservation);
            }
            self.attempts.lock().expect("attempt ledger").fence(node_id);
            return Err(error);
        }
        Ok(PreparedReviewerAttempt {
            attempt,
            reservation,
        })
    }

    /// Hold a reviewer-thread event for the canonical-order flush. See `reviewer_events`.
    fn buffer_reviewer_event(&self, node_id: &str, event: NewEvent) {
        let mut seq = self.reviewer_event_seq.lock().expect("reviewer event seq");
        let key = (node_id.to_string(), *seq);
        *seq += 1;
        self.reviewer_events
            .lock()
            .expect("reviewer events")
            .push((key, event));
    }

    /// Append every buffered reviewer event, sorted by `(node, emission order)`, then clear the
    /// buffer. Called at the gather barrier — every reviewer has finished by then — so the log
    /// is a function of the pipeline, not of thread timing. Idempotent: a second call on an
    /// already-drained buffer is a no-op, which is why a gather-less pipeline can still flush
    /// from the run driver.
    pub fn flush_reviewer_events(&self) -> Result<(), String> {
        let mut pending = self.reviewer_events.lock().expect("reviewer events");
        pending.sort_by(|a, b| a.0.cmp(&b.0));
        let events: Vec<NewEvent> = pending.iter().map(|(_, event)| event.clone()).collect();
        self.append_batch(&events)?;
        pending.clear();
        Ok(())
    }

    /// Durably record what became of every node, and the verdict derived from that. Without
    /// this the log holds the attempts but not the run's conclusion — an operator resuming
    /// from the log alone could not say what the review decided.
    pub fn publish_report(&self, report: &RunReport, verdict: &RunVerdict) -> Result<(), String> {
        // The guaranteed flush point. `run_gather` flushes when it runs — the ordinary case,
        // and the one that keeps attempt events ahead of the findings — but a gather that was
        // suppressed (a failed reviewer upstream) or a pipeline with no gather node never
        // reaches it, and the buffered attempts, charges included, would be lost. Every run
        // ends with a report, so flushing here records the paid work no matter the graph.
        self.flush_reviewer_events()?;
        let outcomes: Vec<RunNodeReportV2> = report
            .outcomes
            .iter()
            .map(|(id, outcome)| {
                let outcome = match outcome {
                    NodeOutcome::Completed { outputs } => RunNodeOutcomeV2::Completed {
                        output_artifacts: artifact_ids(outputs),
                    },
                    NodeOutcome::Failed { error } => RunNodeOutcomeV2::Failed {
                        error: error.clone(),
                    },
                    NodeOutcome::Suppressed { reason } => RunNodeOutcomeV2::Suppressed {
                        reason: match reason {
                            review_graph::SuppressionReason::GateBlocked => {
                                RunSuppressionReasonV2::GateBlocked
                            }
                            review_graph::SuppressionReason::UpstreamMissing => {
                                RunSuppressionReasonV2::UpstreamMissing
                            }
                        },
                    },
                };
                RunNodeReportV2 {
                    node: id.clone(),
                    outcome,
                }
            })
            .collect();
        let verdict = match verdict {
            RunVerdict::Pass => RunVerdictV2::Pass,
            RunVerdict::Fail(Verdict::NotConverged) => RunVerdictV2::Fail {
                reason: RunFailureReasonV2::NotConverged,
            },
            RunVerdict::Fail(Verdict::Exhausted) => RunVerdictV2::Fail {
                reason: RunFailureReasonV2::Exhausted,
            },
            RunVerdict::Fail(Verdict::Converged) => {
                return Err("invalid run verdict: converged cannot be a failure".to_string());
            }
            RunVerdict::Incomplete { missing } => RunVerdictV2::Incomplete {
                missing_nodes: missing
                    .iter()
                    .map(|(node, reason)| MissingNodeV2 {
                        node: node.clone(),
                        reason: reason.clone(),
                    })
                    .collect(),
            },
        };
        let payload = RunReportPayloadV2 {
            outcomes,
            blocked_gates: report.blocked_gates.iter().cloned().collect(),
            verdict,
            spent_tokens: self.spent(),
        };
        self.append(NewEvent::new(
            EventType::RunReportV2,
            serde_json::to_value(payload).map_err(|e| e.to_string())?,
        ))
    }

    /// A sandbox in the requested mode, as a copy-on-write clone of the run's single
    /// materialized template. The template is built once, under the lock, on the first call
    /// (the gate's); every later sandbox — the reviewers' — clones it instead of walking the
    /// manifest and re-reading the whole tree from the CAS.
    fn sandbox(&self, mode: Mode) -> Result<Sandbox, String> {
        let template = {
            let mut guard = self.template.lock().expect("template");
            match guard.as_ref() {
                Some(template) => template.clone(),
                None => {
                    let template = std::sync::Arc::new(
                        review_sandbox::SandboxTemplate::materialize(&self.snapshot, self.cas)
                            .map_err(|e| e.to_string())?,
                    );
                    *guard = Some(template.clone());
                    template
                }
            }
        };
        Sandbox::from_template(&template, mode).map_err(|e| e.to_string())
    }

    /// Emit the run's generation state — the campaign's prior findings — as the artifact a
    /// reviewer receives on its `prior_findings` input edge. In the first round there is no
    /// prior state, so an empty finding set is emitted; the edge is satisfied either way, and
    /// nothing about delivery depends on ambient kernel state.
    fn run_generation(&self) -> Result<Vec<String>, String> {
        let artifact = match &self.prior_findings {
            Some(artifact) => artifact.clone(),
            None => self
                .cas
                .put_json(&serde_json::json!({ "round": 0, "prior_findings": [] }))
                .map_err(|e| e.to_string())?,
        };
        Ok(vec![artifact])
    }

    fn run_gate(&self, node_id: &str) -> Result<Vec<String>, String> {
        // The gate's checks run in a read-only sandbox: a check that mutates the tree would
        // change what every reviewer after it inspects, which is the same torn-input problem
        // capture solves one layer down.
        let sandbox = self.sandbox(Mode::ReadOnly)?;
        // Run the checks holding no lock: each is a build or a test, and the store lock is
        // shared with every other node, so holding it across a check would stall the whole
        // pipeline for the build's duration. The lock is taken only to append each result.
        let runner = CheckRunner::new(self.cas, sandbox.root());
        let mut results = Vec::with_capacity(self.checks.len());
        for check in &self.checks {
            let result = runner.run(check);
            self.append(check_event(&result, node_id))?;
            results.push(result);
        }

        let decision = GateDecision::evaluate(&results);
        let sealed = sandbox.seal().map_err(|e| e.to_string())?;
        if !sealed.unchanged() {
            // Not fatal, but never silent: a read-only gate that mutated its tree has broken an
            // assumption every downstream node is relying on. Bounded — a check that ran a
            // build could have touched thousands of paths, and this is an error string.
            let paths = sealed.mutations.paths();
            return Err(format!(
                "gate mutated its read-only sandbox: {} paths, e.g. {:?}",
                paths.len(),
                &paths[..paths.len().min(20)]
            ));
        }
        let artifact = self
            .cas
            .put_json(&serde_json::to_value(&decision).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        self.append(
            NewEvent::new(
                EventType::GateDecisionV1,
                serde_json::to_value(&decision).map_err(|e| e.to_string())?,
            )
            .node(node_id)
            .referencing(vec![artifact.clone()]),
        )?;
        self.gates
            .lock()
            .expect("gates")
            .insert(node_id.to_string(), decision);
        Ok(vec![artifact])
    }

    fn run_reviewer(
        &self,
        node_id: &str,
        node_inputs: &ArtifactMap,
    ) -> Result<Vec<String>, String> {
        let adapter = self
            .reviewers
            .get(node_id)
            .ok_or_else(|| format!("no reviewer bound to node {node_id}"))?;

        // Prior findings arrive through the wired `prior_findings` input port — a data artifact
        // the pipeline routed from the generation node — not from ambient kernel state. A
        // reviewer that declares no such input receives none; the plan is the delivery.
        let prior_findings_artifact = node_inputs
            .get(PRIOR_FINDINGS_PORT)
            .and_then(|artifacts| artifacts.first())
            .cloned();
        let mut inputs = ReviewerInputs::default();
        if let Some(artifact) = &prior_findings_artifact {
            let value = self.cas.get_json(artifact).map_err(|e| e.to_string())?;
            // The generation node emits an empty document in the first round; only a non-empty
            // finding set is worth rendering into the prompt.
            let has_findings = value
                .get("prior_findings")
                .and_then(|f| f.as_array())
                .is_some_and(|f| !f.is_empty());
            if has_findings {
                inputs.prior_findings = Some(value);
            }
        }

        let mut timeouts: Vec<String> = Vec::new();
        let mut prepared = self
            .prepared_attempts
            .lock()
            .expect("prepared attempts")
            .remove(node_id);
        for _ in 0..=self.timeout_retries {
            // The scheduler prepares the first attempt in plan order before spawning this
            // worker. Timeout retries are prepared here only after the predecessor is fenced.
            let PreparedReviewerAttempt {
                attempt,
                reservation,
            } = match prepared.take() {
                Some(prepared) => prepared,
                None => self.prepare_reviewer_attempt(
                    node_id,
                    prior_findings_artifact.as_ref(),
                    &timeouts,
                )?,
            };

            // Each attempt gets its own fresh sandbox. Reviewers may edit freely — a TDD
            // reviewer must — and nothing they do can reach a sibling, the source, the
            // snapshot, or a retry of themselves.
            let sandbox = self.sandbox(Mode::EphemeralWrite)?;

            match adapter.invoke(self.cas, sandbox.root(), &inputs) {
                Ok(returned) => {
                    let selection = self
                        .attempts
                        .lock()
                        .expect("attempt ledger")
                        .admit(&Receipt {
                            attempt: attempt.clone(),
                            output: returned.raw_artifact.clone(),
                            cost: returned.cost_tokens,
                        });
                    // The spend happened whatever the selection said; forgiving a fenced
                    // attempt's bill would make lateness cheaper than answering.
                    if let (Some(budgets), Some(reservation)) = (&self.budgets, &reservation) {
                        budgets
                            .ledger
                            .lock()
                            .expect("budget ledger")
                            .charge(reservation, returned.cost_tokens);
                    }
                    self.buffer_reviewer_event(
                        node_id,
                        NewEvent::new(
                            EventType::AttemptAdmittedV1,
                            serde_json::json!({
                                "selection": match selection {
                                    Selection::Selected => "selected",
                                    Selection::Quarantined => "quarantined",
                                },
                                "cost_tokens": returned.cost_tokens,
                            }),
                        )
                        .node(node_id)
                        .attempt(attempt.to_string())
                        .referencing(vec![returned.raw_artifact.clone()]),
                    );
                    if selection == Selection::Quarantined {
                        // Fenced while running: the receipt and its charge stand, the output
                        // is invisible downstream. This is `admit`'s entire enforcement point.
                        return Err(format!(
                            "attempt {attempt} was fenced; its late result is quarantined"
                        ));
                    }

                    let sealed = sandbox.seal().map_err(|e| e.to_string())?;
                    // The mutation set can be enormous — a reviewer that built to verify a
                    // claim leaves a whole target/ behind. The full list lives once in the CAS
                    // (content-addressed, so identical sets across attempts dedupe); the result
                    // artifact and the event carry a bounded summary that references it, never
                    // the whole list inlined into SQLite and re-parsed on every ledger rebuild.
                    let mutations_artifact = self
                        .cas
                        .put_json(&serde_json::json!({
                            "added": sealed.mutations.added,
                            "modified": sealed.mutations.modified,
                            "deleted": sealed.mutations.deleted,
                        }))
                        .map_err(|e| e.to_string())?;
                    let mutation_summary = mutation_summary(&sealed.mutations, &mutations_artifact);

                    // The artifact is the node's typed result, whole — it is what the edges
                    // deliver, so it must be sufficient for whatever consumes it.
                    let artifact = self
                        .cas
                        .put_json(&serde_json::json!({
                            "node": node_id,
                            "attempt": attempt.to_string(),
                            "output": serde_json::to_value(&returned.output)
                                .map_err(|e| e.to_string())?,
                            "cost_tokens": returned.cost_tokens,
                            "raw": returned.raw_artifact,
                            "sandbox_mutations": mutation_summary,
                        }))
                        .map_err(|e| e.to_string())?;
                    return Ok(vec![artifact]);
                }
                Err(RunnerError::TimedOut { after_ms }) => {
                    // Fence, charge, retry. The killed process's true spend is unreportable,
                    // so the full reservation is charged — the conservative reading of "a
                    // fenced attempt charges", and the one that keeps a hang from being a
                    // free retry.
                    self.attempts.lock().expect("attempt ledger").fence(node_id);
                    if let Some(reservation) = &reservation {
                        self.attempts
                            .lock()
                            .expect("attempt ledger")
                            .charge(&attempt, reservation.amount);
                    }
                    if let (Some(budgets), Some(reservation)) = (&self.budgets, &reservation) {
                        budgets
                            .ledger
                            .lock()
                            .expect("budget ledger")
                            .charge(reservation, reservation.amount);
                    }
                    self.buffer_reviewer_event(
                        node_id,
                        NewEvent::new(
                            EventType::AttemptFencedV1,
                            serde_json::json!({
                                "reason": format!("timed out after {after_ms}ms"),
                                "charged": reservation.as_ref().map(|r| r.amount),
                            }),
                        )
                        .node(node_id)
                        .attempt(attempt.to_string()),
                    );
                    timeouts.push(format!("attempt {attempt} timed out after {after_ms}ms"));
                }
                Err(error @ (RunnerError::Refused(_) | RunnerError::Unavailable(_))) => {
                    // Nothing executed, so nothing was spent: the reservation is released,
                    // not charged.
                    if let (Some(budgets), Some(reservation)) = (&self.budgets, &reservation) {
                        budgets
                            .ledger
                            .lock()
                            .expect("budget ledger")
                            .release(reservation);
                    }
                    self.buffer_reviewer_event(
                        node_id,
                        NewEvent::new(
                            EventType::AttemptReleasedV1,
                            serde_json::json!({
                                "error": error.to_string(),
                                "released": reservation.as_ref().map(|r| r.amount),
                            }),
                        )
                        .node(node_id)
                        .attempt(attempt.to_string()),
                    );
                    return Err(error.to_string());
                }
                Err(error) => {
                    // Failed or malformed: the reviewer did execute, its spend is unreported,
                    // and forgiving it would make crashing cheaper than answering. Full
                    // reservation, same rule as a timeout.
                    if let (Some(budgets), Some(reservation)) = (&self.budgets, &reservation) {
                        budgets
                            .ledger
                            .lock()
                            .expect("budget ledger")
                            .charge(reservation, reservation.amount);
                    }
                    if let Some(reservation) = &reservation {
                        self.attempts
                            .lock()
                            .expect("attempt ledger")
                            .charge(&attempt, reservation.amount);
                    }
                    self.buffer_reviewer_event(
                        node_id,
                        NewEvent::new(
                            EventType::AttemptFailedV1,
                            serde_json::json!({
                                "error": error.to_string(),
                                "charged": reservation.as_ref().map(|r| r.amount),
                            }),
                        )
                        .node(node_id)
                        .attempt(attempt.to_string()),
                    );
                    return Err(error.to_string());
                }
            }
        }
        Err(format!("every attempt timed out: {}", timeouts.join("; ")))
    }

    /// A real gather: one artifact holding exactly the report artifacts the edges delivered.
    /// A reviewer whose result port feeds no edge is absent here, and therefore absent from
    /// everything downstream — the plan is the data flow, not a suggestion about it.
    ///
    /// This is also the run's canonical barrier: every reviewer has finished, so the buffered
    /// reviewer events are flushed here in node order, giving the log a shape that is a
    /// function of the pipeline rather than of thread timing.
    fn run_gather(&self, inputs: &[String]) -> Result<Vec<String>, String> {
        self.flush_reviewer_events()?;
        let artifact = self
            .cas
            .put_json(&serde_json::json!(inputs))
            .map_err(|e| e.to_string())?;
        Ok(vec![artifact])
    }

    fn run_ledger(&self, inputs: &[String]) -> Result<Vec<String>, String> {
        // The ledger reduces what its edges delivered — never a global map of whatever happened
        // to run. Each input is one reviewer's result, or a gather manifest of result ids.
        let mut results: Vec<(String, LegacyStageOutput)> = Vec::new();
        let mut load = |id: &str| -> Result<(), String> {
            let value = self.cas.get_json(id).map_err(|e| e.to_string())?;
            let node = value
                .get("node")
                .and_then(|n| n.as_str())
                .ok_or_else(|| format!("artifact {id} is not a reviewer result"))?
                .to_string();
            let output: LegacyStageOutput =
                serde_json::from_value(value.get("output").cloned().unwrap_or_default())
                    .map_err(|e| format!("artifact {id}: {e}"))?;
            results.push((node, output));
            Ok(())
        };
        for input in inputs {
            match self.cas.get_json(input).map_err(|e| e.to_string())? {
                serde_json::Value::Array(ids) => {
                    for id in &ids {
                        let id = id
                            .as_str()
                            .ok_or_else(|| format!("gather manifest {input} holds a non-id"))?;
                        load(id)?;
                    }
                }
                _ => load(input)?,
            }
        }
        // Canonical gather order: node id — not completion order, not artifact digest order.
        results.sort_by(|a, b| a.0.cmp(&b.0));

        {
            let mut store = self.store.lock().expect("event store");
            let mut ingest =
                Ingest::new(*store, self.cas, self.run_id.clone()).map_err(|e| e.to_string())?;
            for (node_id, stage) in &results {
                ingest
                    .add_live_stage_output(node_id, stage)
                    .map_err(|e| e.to_string())?;
            }
        }
        // The `findings` port must carry a real artifact, not a label: the scheduler delivers
        // exactly this string to whatever consumes the port, and a downstream event referencing
        // a non-CAS string would be rejected as a dangling artifact far from its cause.
        let ledger = Ledger::rebuild(
            *self.store.lock().expect("event store"),
            self.cas,
            &self.run_id,
        )
        .map_err(|e| e.to_string())?;
        let artifact = self
            .cas
            .put_json(&serde_json::json!({
                "round": ledger.round,
                "sources": results.iter().map(|(n, _)| n).collect::<Vec<_>>(),
                "findings": ledger.len(),
            }))
            .map_err(|e| e.to_string())?;
        Ok(vec![artifact])
    }
}

/// A compact record of a sandbox's mutations: the counts, a bounded sample of paths, and the
/// CAS digest of the full set. Bounded on purpose — the full list is thousands of entries when
/// a reviewer built, and it must not be inlined into every event payload.
fn mutation_summary(
    mutations: &review_sandbox::MutationSet,
    full_artifact: &str,
) -> serde_json::Value {
    const SAMPLE: usize = 20;
    let paths = mutations.paths();
    serde_json::json!({
        "count": paths.len(),
        "added": mutations.added.len(),
        "modified": mutations.modified.len(),
        "deleted": mutations.deleted.len(),
        "sample": paths.iter().take(SAMPLE).collect::<Vec<_>>(),
        "truncated": paths.len() > SAMPLE,
        "artifact": full_artifact,
    })
}

impl Dispatch for Kernel<'_> {
    fn record_invocation(&self, node: &Node, inputs: &ArtifactMap) -> Result<(), String> {
        let payload = NodeInvocationPayloadV1 {
            node: node.id.clone(),
            inputs: port_artifacts(&node.inputs, inputs, &self.snapshot.content_digest()),
        };
        self.append(
            NewEvent::new(
                EventType::NodeInvocationV1,
                serde_json::to_value(payload).map_err(|e| e.to_string())?,
            )
            .node(&node.id)
            .referencing(artifact_ids(inputs)),
        )?;
        if node.kind == NodeKind::Reviewer {
            if !self.reviewers.contains_key(&node.id) {
                return Err(format!("no reviewer bound to node {}", node.id));
            }
            let prior_findings = inputs
                .get(PRIOR_FINDINGS_PORT)
                .and_then(|artifacts| artifacts.first());
            let prepared = self.prepare_reviewer_attempt(&node.id, prior_findings, &[])?;
            self.prepared_attempts
                .lock()
                .expect("prepared attempts")
                .insert(node.id.clone(), prepared);
        }
        Ok(())
    }

    fn run(&self, node: &Node, inputs: &ArtifactMap) -> Result<ArtifactMap, String> {
        // Routing is on the validated kind, never the id: an id is a name someone chose, and a
        // reviewer named `gather` must still be a reviewer that runs.
        let artifacts = match node.kind {
            NodeKind::Generation => self.run_generation(),
            NodeKind::Gate => self.run_gate(&node.id),
            // Gather and ledger reduce whatever artifacts their edges delivered; the port
            // labels are the reviewer's concern, not theirs.
            NodeKind::Gather => self.run_gather(&artifact_ids(inputs)),
            NodeKind::Ledger => self.run_ledger(&artifact_ids(inputs)),
            NodeKind::Reviewer => self.run_reviewer(&node.id, inputs),
        }?;
        bind_single_output(node, artifacts)
    }

    fn record_outputs(&self, node: &Node, outputs: &ArtifactMap) -> Result<(), String> {
        let payload = NodeOutputReceiptPayloadV1 {
            node: node.id.clone(),
            outputs: port_artifacts(&node.outputs, outputs, &self.snapshot.content_digest()),
        };
        let mut event = NewEvent::new(
            EventType::NodeOutputReceiptV1,
            serde_json::to_value(payload).map_err(|e| e.to_string())?,
        )
        .node(&node.id)
        .referencing(artifact_ids(outputs));
        if node.kind == NodeKind::Reviewer
            && let Some(artifact) = artifact_ids(outputs).first()
            && let Ok(value) = self.cas.get_json(artifact)
            && let Some(attempt) = value.get("attempt").and_then(serde_json::Value::as_str)
        {
            event = event.attempt(attempt);
        }
        if node.kind == NodeKind::Reviewer {
            // The scheduler publishes outputs as soon as this returns. Commit the attempt
            // lifecycle and receipt together before that publication point; retaining buffered
            // events until gather permits consumers to be selected from an unsealed output.
            let mut pending = self.reviewer_events.lock().expect("reviewer events");
            let mut events: Vec<NewEvent> = pending
                .iter()
                .filter(|((id, _), _)| id == &node.id)
                .map(|(_, event)| event.clone())
                .collect();
            events.push(event);
            self.append_batch(&events)?;
            pending.retain(|((id, _), _)| id != &node.id);
            Ok(())
        } else {
            self.append(event)
        }
    }

    fn gate_passed(&self, node_id: &str, _outputs: &ArtifactMap) -> bool {
        self.gates
            .lock()
            .expect("gates")
            .get(node_id)
            .map(GateDecision::passed)
            .unwrap_or(false)
    }
}
