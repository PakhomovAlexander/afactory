//! Durable Run records and deterministic ordered-step transitions.
use super::contract::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ready,
    Running,
    Waiting,
    Succeeded,
    Failed,
    Cancelled,
}
impl Status {
    pub fn active(self) -> bool {
        matches!(self, Self::Ready | Self::Running | Self::Waiting)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Waiting {
    Decision {
        step: String,
        request: DecisionRequest,
    },
    Condition {
        step: String,
        deadline_unix_ms: u64,
        evidence: String,
    },
    Reconcile {
        step: String,
        intent_id: String,
        diagnostic: String,
    },
    Task {
        step: String,
        task_id: String,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Prepared,
    Uncertain,
    Done,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    pub id: String,
    pub input_digest: String,
    pub phase: Phase,
    pub evidence: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    pub step: String,
    pub started_unix_ms: u64,
    pub deadline_unix_ms: u64,
    pub status: Status,
    pub skipped: Option<String>,
    pub intents: Vec<Intent>,
    pub outputs: BTreeMap<String, Datum>,
    pub child: Option<String>,
    pub decision: Option<DecisionReceipt>,
    pub diagnostic: Option<String>,
}
impl Progress {
    pub fn complete(&self) -> bool {
        self.status == Status::Succeeded
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    pub scenario: String,
    pub inputs: BTreeMap<String, Datum>,
    pub started_unix_ms: u64,
    pub steps: Vec<Progress>,
    pub dispatches: u32,
    pub outputs: Option<BTreeMap<String, Datum>>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCharge {
    pub maximum: u64,
    pub charged: Option<u64>,
    pub unknown_usage: bool,
    pub step: String,
}
impl TaskCharge {
    pub fn reserved(&self) -> u64 {
        if self.unknown_usage || self.charged.is_none() {
            self.maximum.saturating_sub(self.charged.unwrap_or(0))
        } else {
            0
        }
    }
    pub fn committed(&self) -> u64 {
        self.charged.unwrap_or(0).saturating_add(self.reserved())
    }
    pub fn uncertain(&self) -> bool {
        self.charged.is_none() || self.unknown_usage
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationState {
    Requested,
    Confirmed,
    Unconfirmed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildCancellation {
    pub run: Option<u64>,
    pub state: CancellationState,
    pub evidence: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cancellation {
    pub requested_unix_ms: u64,
    pub reason: String,
    pub child_requests: BTreeMap<String, ChildCancellation>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub schema: String,
    pub id: String,
    pub execution_id: String,
    pub pin: Pin,
    pub limits: Limits,
    pub created_unix_ms: u64,
    pub updated_unix_ms: u64,
    pub status: Status,
    pub waiting: Option<Waiting>,
    pub frames: BTreeMap<String, Frame>,
    pub task_charges: BTreeMap<String, TaskCharge>,
    pub dispatches: u32,
    pub failure: Option<String>,
    pub cancellation: Option<Cancellation>,
    pub ant: Option<String>,
    pub goal_claim: Option<super::roadmap::Claim>,
    pub fresh_tasks: BTreeMap<String, TaskTarget>,
}
#[derive(Debug, Clone)]
pub struct Dispatch {
    pub frame: String,
    pub step: String,
    pub definition: Step,
    pub inputs: BTreeMap<String, Datum>,
    pub intent: Intent,
    pub timeout_ms: u64,
    pub step_started_unix_ms: u64,
    pub target: Option<TaskTarget>,
}
pub(crate) enum TaskReconciliation {
    Clear,
    Cancelled,
    Blocked {
        frame: String,
        task_id: String,
        diagnostic: String,
        missing: bool,
    },
}

impl TaskReconciliation {
    pub(crate) fn cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

impl Run {
    pub fn new(
        host: &dyn crate::Host,
        id: String,
        pin: Pin,
        inputs: BTreeMap<String, Datum>,
        limits: Limits,
        now: u64,
        ant: Option<String>,
    ) -> Result<Self> {
        name(&id)?;
        pin.validate(host)?;
        if pin.policy.implementation_startup.is_some() {
            crate::driver::validate_run_schedule(&pin, &limits, crate::driver::DEFAULT_POLL_MS)?;
        }
        super::implementation::preflight(host, &pin)?;
        let now = if pin.policy.implementation_startup.is_some() {
            host.authorization_time()?
        } else {
            now
        };
        limits.validate()?;
        require(
            now > 0
                && now
                    .checked_add(limits.max_wall_ms)
                    .is_some_and(|n| n <= af_contracts::json::SAFE_INTEGER_MAX as u64),
            "Invalid Run time",
        )?;
        let def = &pin.bundle.definitions[&pin.bundle.root];
        require(limits.within(&def.limits), "Run widens Scenario limits")?;
        input_values(&def.inputs, &inputs)?;
        require(
            inputs.values().all(Datum::scalar),
            "Root Run inputs cannot manufacture receipts",
        )?;
        let frame = Frame {
            scenario: pin.bundle.root.clone(),
            inputs,
            started_unix_ms: now,
            steps: Vec::new(),
            dispatches: 0,
            outputs: None,
        };
        Ok(Self {
            schema: "af/scenario-run@12".into(),
            id,
            execution_id: execution_id()?,
            pin,
            limits,
            created_unix_ms: now,
            updated_unix_ms: now,
            status: Status::Ready,
            waiting: None,
            frames: BTreeMap::from([("root".into(), frame)]),
            task_charges: BTreeMap::new(),
            dispatches: 0,
            failure: None,
            cancellation: None,
            ant,
            goal_claim: None,
            fresh_tasks: BTreeMap::new(),
        })
    }
    /// Exact committed usage (recorded charges plus unresolved reservations).
    /// Native overruns can exceed u64 across Tasks; durable individual charges stay u64.
    pub fn tokens(&self) -> Result<u128> {
        self.task_charges.values().try_fold(0_u128, |n, c| {
            n.checked_add(u128::from(c.committed()))
                .ok_or_else(|| "Scenario accounting overflow".into())
        })
    }
    pub fn token_allowance(&self) -> u64 {
        self.goal_claim
            .as_ref()
            .map_or(self.limits.max_tokens, |c| c.allowance)
    }
    pub fn accounted_tokens(&self) -> Result<u128> {
        let used = self.tokens()?;
        Ok(
            if self.goal_claim.is_some() && self.task_charges.values().any(TaskCharge::uncertain) {
                used.max(u128::from(self.token_allowance()))
            } else {
                used
            },
        )
    }
    pub fn validate(&self, host: &dyn crate::Host) -> Result<()> {
        require(
            self.schema == "af/scenario-run@12" && af_contracts::is_digest(&self.execution_id),
            "Unsupported Run version",
        )?;
        name(&self.id)?;
        self.pin.validate(host)?;
        self.limits.validate()?;
        require(
            self.goal_claim
                .as_ref()
                .is_none_or(|c| c.allowance > 0 && c.allowance <= self.limits.max_tokens),
            "Goal claim widens per-run budget",
        )?;
        require(
            self.limits
                .within(&self.pin.bundle.definitions[&self.pin.bundle.root].limits)
                && self.created_unix_ms > 0
                && self.updated_unix_ms >= self.created_unix_ms
                && self.updated_unix_ms <= af_contracts::json::SAFE_INTEGER_MAX as u64
                && self
                    .created_unix_ms
                    .checked_add(self.limits.max_wall_ms)
                    .is_some_and(|n| n <= af_contracts::json::SAFE_INTEGER_MAX as u64)
                && self.dispatches <= self.limits.max_dispatches
                && (self.tokens()? <= u128::from(self.token_allowance())
                    || matches!(self.status, Status::Failed | Status::Cancelled)),
            "Corrupt Run limits, time or accounting",
        )?;
        require(
            self.frames.get("root").is_some_and(|f| {
                f.scenario == self.pin.bundle.root && f.dispatches == self.dispatches
            }),
            "Missing Run root",
        )?;
        require(
            (self.status == Status::Waiting) == self.waiting.is_some()
                && (self.status == Status::Cancelled) == self.cancellation.is_some()
                && (self.status == Status::Failed) == self.failure.is_some(),
            "Corrupt Run lifecycle",
        )?;
        require(
            self.frames["root"].inputs.values().all(Datum::scalar),
            "Root inputs cannot manufacture receipts",
        )?;
        if let Some(cancel) = &self.cancellation {
            text(&cancel.reason)?;
            require(
                cancel.requested_unix_ms >= self.created_unix_ms
                    && cancel.requested_unix_ms <= self.updated_unix_ms,
                "Invalid cancellation time",
            )?;
            for (path, request) in &cancel.child_requests {
                text(&request.evidence)?;
                require(
                    request.run.is_none_or(|n| n > 0),
                    "Invalid recorded Task run",
                )?;
                let (frame, step) = path
                    .rsplit_once('/')
                    .ok_or("Invalid child cancellation path")?;
                let frame = self
                    .frames
                    .get(frame)
                    .ok_or("Unknown cancelled child frame")?;
                require(
                    self.pin.bundle.definitions[&frame.scenario]
                        .steps
                        .iter()
                        .any(|s| {
                            s.id == step
                                && matches!(
                                    s.operation,
                                    Kind::Task { .. }
                                        | Kind::CiDiagnosis { .. }
                                        | Kind::FreshTask { .. }
                                )
                        }),
                    "Cancellation targets a non-Task step",
                )?;
            }
        }
        let mut paths = std::collections::BTreeSet::new();
        let mut intent_ids = std::collections::BTreeSet::new();
        for (path, frame) in &self.frames {
            require(
                path == "root"
                    || self
                        .frames
                        .values()
                        .flat_map(|f| &f.steps)
                        .any(|s| s.child.as_deref() == Some(path)),
                "Orphan child frame",
            )?;
            let def = self
                .pin
                .bundle
                .definitions
                .get(&frame.scenario)
                .ok_or("Unknown pinned frame")?;
            input_values(&def.inputs, &frame.inputs)?;
            require(
                frame.started_unix_ms >= self.created_unix_ms
                    && frame.started_unix_ms <= self.updated_unix_ms
                    && frame
                        .started_unix_ms
                        .checked_add(def.limits.max_wall_ms)
                        .is_some_and(|n| n <= af_contracts::json::SAFE_INTEGER_MAX as u64)
                    && frame.steps.len() <= def.steps.len()
                    && frame.dispatches <= def.limits.max_dispatches,
                "Corrupt frame progress",
            )?;
            for (i, p) in frame.steps.iter().enumerate() {
                let step = &def.steps[i];
                let full = format!("{path}/{}", step.id);
                paths.insert(full.clone());
                self.validate_progress(path, p, step)?;
                require(
                    p.step == step.id
                        && p.started_unix_ms >= frame.started_unix_ms
                        && p.started_unix_ms <= self.updated_unix_ms
                        && p.deadline_unix_ms <= af_contracts::json::SAFE_INTEGER_MAX as u64
                        && p.deadline_unix_ms
                            == p.started_unix_ms
                                .checked_add(step.timeout_ms)
                                .ok_or("Step deadline overflow")?
                        && p.intents.len() <= usize::from(step.max_attempts),
                    "Corrupt step or retry bound",
                )?;
                require(
                    i + 1 == frame.steps.len() || p.complete(),
                    "Progress after incomplete step",
                )?;
                if p.complete() && p.skipped.is_none() {
                    input_values(&step.outputs, &p.outputs)?;
                    if matches!(
                        step.operation,
                        Kind::Task { .. } | Kind::CiDiagnosis { .. } | Kind::FreshTask { .. }
                    ) {
                        for value in p.outputs.values() {
                            if let Some(receipt) = super::task_output::accepted_receipt(value)? {
                                super::task_output::validate(p, receipt)?;
                            }
                        }
                    }
                }
                if p.skipped.is_some() {
                    require(
                        p.complete() && p.outputs.is_empty(),
                        "Skipped step has fabricated outputs",
                    )?;
                }
                for (attempt, intent) in p.intents.iter().enumerate() {
                    require(
                        intent.id
                            == hash(&(&self.execution_id, &self.pin.revision, &full, attempt + 1))?
                            && intent_ids.insert(intent.id.clone())
                            && af_contracts::is_digest(&intent.input_digest),
                        "Corrupt or duplicate intent",
                    )?;
                    for evidence in &intent.evidence {
                        text(evidence)?;
                    }
                }
                if let Some(child) = &p.child {
                    let Kind::Scenario { scenario } = &step.operation else {
                        return Err("Non-child step has a child".into());
                    };
                    require(
                        child == &full
                            && self
                                .frames
                                .get(child)
                                .is_some_and(|f| &f.scenario == scenario),
                        "Corrupt child binding",
                    )?;
                }
                if let Some(decision) = &p.decision {
                    decision.verify(&self.pin.policy)?;
                    require(
                        decision.request.run_id == self.id
                            && decision.request.execution_id == self.execution_id
                            && decision.request.pin_id == self.pin.revision
                            && decision.request.step == full,
                        "Decision bound to another Run or step",
                    )?;
                }
            }
            if let Some(outputs) = &frame.outputs {
                require(
                    frame.steps.len() == def.steps.len()
                        && frame.steps.iter().all(Progress::complete),
                    "Premature frame completion",
                )?;
                input_values(&def.outputs, outputs)?;
                for condition in &def.completion {
                    require(
                        self.resolve(path, condition)? == Datum::Boolean(true),
                        "False completion evidence",
                    )?;
                }
                for (port, binding) in &def.result {
                    require(
                        outputs.get(port) == Some(&self.resolve(path, binding)?),
                        "Changed final evidence",
                    )?;
                }
            }
        }
        for (intent_id, target) in &self.fresh_tasks {
            let mut found = false;
            for frame in self.frames.values() {
                let def = &self.pin.bundle.definitions[&frame.scenario];
                for (p, step) in frame.steps.iter().zip(&def.steps) {
                    if p.intents.iter().any(|i| &i.id == intent_id)
                        && let Kind::FreshTask { template, .. } = &step.operation
                    {
                        require(
                            target.task_id == super::template::task_id(intent_id)
                                && target.repo == template.repo
                                && target.state == template.state
                                && target.max_tokens == template.max_tokens
                                && af_contracts::is_digest(&target.plan_id)
                                && af_contracts::is_digest(&target.revision_id),
                            "Fresh Task plan widens recorded template",
                        )?;
                        found = true;
                    }
                }
            }
            require(found, "Fresh Task plan has no durable intent")?;
        }
        let mut implementation_reservations = 0_u64;
        for (key, charge) in &self.task_charges {
            require(
                paths.contains(&charge.step)
                    && charge.charged.is_none_or(|n| {
                        n <= charge.maximum
                            || matches!(self.status, Status::Failed | Status::Cancelled)
                    }),
                "Corrupt Task charge",
            )?;
            let (path, step_id) = charge.step.rsplit_once('/').ok_or("Invalid charge path")?;
            let frame = &self.frames[path];
            let step = self.pin.bundle.definitions[&frame.scenario]
                .steps
                .iter()
                .find(|s| s.id == step_id)
                .ok_or("Unknown charge step")?;
            match &step.operation {
                Kind::Task { target } | Kind::CiDiagnosis { target } => require(
                    *key == hash(&(&target.state, &target.task_id, &target.revision_id))?
                        && charge.maximum == target.max_tokens,
                    "Task charge identity differs",
                )?,
                Kind::FreshTask { template, .. } => {
                    if template.document["kind"] == "implement" {
                        implementation_reservations += 1;
                    }
                    let progress = frame
                        .steps
                        .iter()
                        .find(|p| p.step == step.id)
                        .ok_or("Missing fresh Task progress")?;
                    require(
                        progress.intents.iter().any(|i| {
                            hash(&(&template.state, super::template::task_id(&i.id))).as_ref()
                                == Ok(key)
                        }) && charge.maximum == template.max_tokens,
                        "Fresh Task reservation identity differs",
                    )?;
                }
                _ => return Err("Charge belongs to a non-Task step".into()),
            }
        }
        if let Some(scope) = &self.pin.policy.implementation_startup {
            require(
                implementation_reservations <= u64::from(scope.max_repairs) + 1,
                "Shared implementation repair allowance exhausted",
            )?;
        }
        require(
            self.status != Status::Succeeded || self.frames["root"].outputs.is_some(),
            "Success lacks Scenario completion evidence",
        )
    }
    fn validate_progress(&self, path: &str, p: &Progress, step: &Step) -> Result<()> {
        let full = format!("{path}/{}", step.id);
        if let Some(why) = &p.skipped {
            if why.starts_with("condition ") {
                let condition = step
                    .when
                    .as_ref()
                    .map(|b| self.resolve(path, b))
                    .transpose()?;
                require(
                    condition == Some(Datum::Boolean(false))
                        && *why == format!("condition {} is false", hash(&condition)?),
                    "Skipped step has no false predicate evidence",
                )?;
            } else {
                require(
                    *why == "deadline reached; explicit skip disposition"
                        && step.on_timeout == Timeout::Skip
                        && self.updated_unix_ms >= p.deadline_unix_ms
                        && matches!(step.operation, Kind::Wait { .. } | Kind::Decision { .. }),
                    "Invalid timeout skip disposition",
                )?;
            }
            return Ok(());
        }
        let inputs: BTreeMap<String, Datum> = step
            .inputs
            .iter()
            .map(|(n, b)| Ok((n.clone(), self.resolve(path, b)?)))
            .collect::<Result<_>>()?;
        for intent in &p.intents {
            require(
                intent.input_digest == hash(&(step, &inputs))?,
                "Intent input authority changed",
            )?;
        }
        if let Some(decision) = &p.decision {
            let Kind::Decision { owner } = &step.operation else {
                return Err("Decision recorded on another step kind".into());
            };
            require(
                decision.request.owner == *owner
                    && decision.request.subject == inputs["subject"].as_text()?
                    && decision.request.revision == inputs["revision"].as_text()?
                    && decision.request.expires_unix_ms
                        == p.deadline_unix_ms
                            .min(self.created_unix_ms + self.limits.max_wall_ms),
                "Decision object or deadline differs from request",
            )?;
        }
        if !p.complete() {
            return Ok(());
        }
        match &step.operation {
            Kind::CiDiagnosis { target } => {
                let Some(Datum::Observation(o)) = p.outputs.get("result") else {
                    return Err("CI diagnosis completion lacks native observation".into());
                };
                let record = crate::ci_diagnosis::Record::parse(o)?;
                record.target(target)?;
                require(
                    (record.receipt.charged_tokens <= target.max_tokens
                        || matches!(self.status, Status::Failed | Status::Cancelled))
                        && p.intents.last().is_some_and(|i| i.phase == Phase::Done),
                    "CI diagnosis lacks settled intent or exceeds reservation",
                )
            }
            Kind::Scenario { scenario } => {
                let child = self
                    .frames
                    .get(&full)
                    .ok_or("Completed child has no frame")?;
                require(
                    child.scenario == *scenario
                        && child.inputs == inputs
                        && child.outputs.as_ref() == Some(&p.outputs),
                    "Child evidence differs from its pinned invocation",
                )
            }
            Kind::Decision { .. } => {
                let decision = p
                    .decision
                    .as_ref()
                    .ok_or("Completed decision lacks signature")?;
                require(
                    decision.request.approve
                        && p.outputs.get("result") == Some(&Datum::Decision(decision.clone())),
                    "Completed decision lacks matching approval",
                )
            }
            Kind::FreshTask { .. } | Kind::Task { .. } => {
                let target = super::template::targets(self, p, step)
                    .ok_or("Completed fresh Task lacks its recorded native plan")?;
                let receipt = match p.outputs.get("result") {
                    Some(Datum::Task(receipt)) => receipt,
                    Some(Datum::TaskOutcome(outcome)) => &outcome.receipt,
                    _ => return Err("Task completion has no typed receipt".into()),
                };
                require(
                    receipt.task_id == target.task_id
                        && receipt.plan_id == target.plan_id
                        && receipt.revision_id == target.revision_id
                        && (receipt.charged_tokens <= target.max_tokens
                            || matches!(self.status, Status::Failed | Status::Cancelled)),
                    "Task receipt belongs to another pinned invocation",
                )?;
                require(
                    p.intents.last().is_some_and(|i| i.phase == Phase::Done),
                    "Task completion has no settled intent",
                )
            }
            Kind::Wait { condition, .. } => {
                let Some(Datum::Observation(o)) = p.outputs.get("result") else {
                    return Err("Wait completion lacks observation".into());
                };
                require(
                    o.satisfied
                        && o.subject == inputs["subject"].as_text()?
                        && o.revision == inputs["revision"].as_text()?
                        && o.condition == *condition
                        && p.intents.last().is_some_and(|i| i.phase == Phase::Done),
                    "Stale Wait completion evidence",
                )
            }
            Kind::Action { grant, .. } => {
                let intent = p.intents.last().ok_or("Action completion has no intent")?;
                require(
                    intent.phase == Phase::Done,
                    "Action completion intent is not settled",
                )?;
                if grant.operation == Operation::WriteOwnedFile {
                    let Some(Datum::OwnedFile(owned)) = p.outputs.get("result") else {
                        return Err("File action lacks ownership receipt".into());
                    };
                    require(
                        owned.owner == intent.id
                            && owned.root == std::path::Path::new(&grant.destination)
                            && owned.file == inputs["file"].as_text()?,
                        "File ownership differs from prepared intent",
                    )
                } else {
                    let Some(Datum::Action(receipt)) = p.outputs.get("result") else {
                        return Err("Action completion lacks receipt".into());
                    };
                    require(
                        receipt.intent_id == intent.id
                            && receipt.operation == grant.operation
                            && receipt.destination == grant.destination,
                        "Action receipt differs from prepared authority",
                    )?;
                    if grant.operation != Operation::CleanupOwnedFile {
                        require(
                            receipt.subject == inputs["subject"].as_text()?
                                && receipt.revision == inputs["revision"].as_text()?,
                            "Action receipt has stale object identity",
                        )?;
                    }
                    if grant.operation.backlog() {
                        require(
                            af_contracts::is_digest(&receipt.evidence),
                            "Backlog action lacks a Project receipt identity",
                        )?;
                    }
                    if grant.operation == Operation::PrepareCandidateV1 {
                        super::workspace::validate_preparation(receipt, &inputs)?;
                    }
                    Ok(())
                }
            }
        }
    }
    pub fn resolve(&self, path: &str, binding: &Binding) -> Result<Datum> {
        let frame = self.frames.get(path).ok_or("Unknown frame")?;
        let value = match binding {
            Binding::Project { value, projection } => {
                let value = self.resolve(path, value)?;
                if matches!(
                    projection,
                    Projection::CiDiagnosisField { .. } | Projection::CiDiagnosisRefused
                ) {
                    let Datum::Observation(observation) = &value else {
                        return Err("CI diagnosis projection requires a native observation".into());
                    };
                    crate::ci_diagnosis::native(self, observation)?;
                }
                if let Projection::TaskOutputField { port, .. } = projection {
                    let Datum::Task(receipt) = value else {
                        return Err("Task output field requires an accepted Task receipt".into());
                    };
                    return projection
                        .task_field(&receipt, super::task_output::read(self, &receipt, port)?);
                }
                return projection.apply(value);
            }
            Binding::Select {
                condition,
                when_true,
                when_false,
            } => {
                // Only the selected arm is resolved: a skipped step has no receipt to read.
                let Datum::Boolean(choice) = self.resolve(path, condition)? else {
                    return Err("Selection requires a boolean condition".into());
                };
                return self.resolve(path, if choice { when_true } else { when_false });
            }
            Binding::Equals { left, right } => {
                return Ok(Datum::Boolean(
                    self.resolve(path, left)? == self.resolve(path, right)?,
                ));
            }
            Binding::Literal { value } => Some(value.as_ref()),
            Binding::Input { port } => frame.inputs.get(port),
            Binding::Step { step, port } => frame
                .steps
                .iter()
                .find(|p| &p.step == step && p.complete() && p.skipped.is_none())
                .and_then(|p| p.outputs.get(port)),
        }
        .ok_or_else(|| match binding {
            Binding::Step { step, port } => format!(
                "Binding refers to missing, skipped or incomplete evidence: {path}/{step}.{port}"
            ),
            _ => "Binding refers to missing, skipped or incomplete evidence".into(),
        })?;
        Ok(value.clone())
    }
    /// Resolve the producing Decision invocation, following only the pinned port bindings.
    /// Bundle validation gives this invocation at most one publication consumer; the signed
    /// pin therefore binds its exact Action step and grant, including through child ports.
    fn decision_source(&self, path: &str, binding: &Binding) -> Result<String> {
        let definition = &self.pin.bundle.definitions[&self.frames[path].scenario];
        match binding {
            Binding::Input { port } => {
                let (parent, invocation) =
                    path.rsplit_once('/').ok_or("No root Decision source")?;
                let parent_definition = &self.pin.bundle.definitions[&self.frames[parent].scenario];
                let step = parent_definition
                    .steps
                    .iter()
                    .find(|s| s.id == invocation)
                    .ok_or("Missing child invocation")?;
                self.decision_source(parent, &step.inputs[port])
            }
            Binding::Step { step, port } => {
                let source = definition
                    .steps
                    .iter()
                    .find(|s| s.id == *step)
                    .ok_or("Missing Decision source")?;
                let full = format!("{path}/{step}");
                match &source.operation {
                    Kind::Decision { .. } => Ok(full),
                    Kind::Scenario { scenario } => self.decision_source(
                        &full,
                        &self.pin.bundle.definitions[scenario].result[port],
                    ),
                    _ => Err("Invalid Decision source".into()),
                }
            }
            _ => Err("Invalid Decision binding".into()),
        }
    }
    pub fn fail(&mut self, why: String) {
        // Step status describes Scenario progress. Intent phases and Task reservations
        // independently retain what is known about effects and spend.
        for frame in self.frames.values_mut() {
            if let Some(step) = frame.steps.last_mut().filter(|step| step.status.active()) {
                step.status = Status::Failed;
            }
        }
        self.status = Status::Failed;
        self.failure = Some(why);
        self.waiting = None;
    }

    /// Reconcile the active native intent while holding the Scenario transaction, then
    /// keep the Task Store fence through commit. Task cancellation releases that fence
    /// before signalling or waiting for the writer.
    pub(crate) fn reconcile_task_cancellation(
        &mut self,
        host: &dyn crate::Host,
        now: u64,
        cancellation_guard: &mut Option<Box<dyn Send>>,
    ) -> Result<TaskReconciliation> {
        use crate::host::TaskCancellationObservation;
        if !self.status.active() {
            return Ok(TaskReconciliation::Clear);
        }
        for (path, frame) in &self.frames {
            let Some(progress) = frame.steps.last().filter(|p| p.status.active()) else {
                continue;
            };
            let Some(intent) = progress
                .intents
                .last()
                .filter(|i| matches!(i.phase, Phase::Prepared | Phase::Uncertain))
            else {
                continue;
            };
            let step = &self.pin.bundle.definitions[&frame.scenario].steps[frame.steps.len() - 1];
            let Some(target) = super::template::targets(self, progress, step) else {
                continue;
            };
            // Steps run serially, including nested Scenarios. Only the active leaf
            // can hold a native Task intent.
            let (observation, guard) =
                host.task_cancellation_for_settlement(&target, &intent.id)?;
            *cancellation_guard = guard;
            let missing = matches!(&observation, TaskCancellationObservation::Missing(_));
            match observation {
                TaskCancellationObservation::Observed(Some(stop)) => {
                    let key = match step.operation {
                        Kind::FreshTask { .. } => hash(&(&target.state, &target.task_id))?,
                        _ => hash(&(&target.state, &target.task_id, &target.revision_id))?,
                    };
                    let path = path.clone();
                    self.updated_unix_ms = self.updated_unix_ms.max(now);
                    self.settle_task_cancellation(&path, &key, stop)?;
                    return Ok(TaskReconciliation::Cancelled);
                }
                TaskCancellationObservation::Observed(None) => break,
                TaskCancellationObservation::Missing(diagnostic)
                | TaskCancellationObservation::Unavailable(diagnostic) => {
                    let frame = path.clone();
                    self.record_task_read_failure(&frame, &diagnostic, !missing)?;
                    return Ok(TaskReconciliation::Blocked {
                        frame,
                        task_id: target.task_id,
                        diagnostic,
                        missing,
                    });
                }
            }
        }
        Ok(TaskReconciliation::Clear)
    }

    pub(crate) fn wait_for_template_capacity(
        &mut self,
        dispatch: &Dispatch,
        diagnostic: String,
        at: u64,
    ) -> Result<()> {
        require(
            at >= self.updated_unix_ms,
            "Preparation clock moved backwards",
        )?;
        let progress = self
            .frames
            .get_mut(&dispatch.frame)
            .and_then(|f| f.steps.last_mut())
            .filter(|p| p.step == dispatch.step)
            .ok_or("Fresh Task step disappeared")?;
        let intent = progress
            .intents
            .last_mut()
            .filter(|i| i.id == dispatch.intent.id && i.phase == Phase::Prepared)
            .ok_or("Fresh Task lost its prepared intent")?;
        // Keep the first native failure even if another retry hits a different CAS object.
        let diagnostic = intent.evidence.first().cloned().unwrap_or(diagnostic);
        text(&diagnostic)?;
        if intent.evidence.is_empty() {
            intent.evidence.push(diagnostic.clone());
        }
        progress.diagnostic = Some(diagnostic.clone());
        progress.status = Status::Waiting;
        self.updated_unix_ms = at;
        self.status = Status::Waiting;
        self.waiting = Some(Waiting::Reconcile {
            step: format!("{}/{}", dispatch.frame, dispatch.step),
            intent_id: intent.id.clone(),
            diagnostic,
        });
        Ok(())
    }

    pub(crate) fn record_task_read_failure(
        &mut self,
        frame: &str,
        diagnostic: &str,
        uncertain: bool,
    ) -> Result<()> {
        text(diagnostic)?;
        let progress = self
            .frames
            .get_mut(frame)
            .and_then(|f| f.steps.last_mut())
            .ok_or("Unreadable Task step disappeared")?;
        let intent = progress
            .intents
            .last_mut()
            .ok_or("Unreadable Task intent disappeared")?;
        if !intent.evidence.iter().any(|e| e == diagnostic) {
            intent.evidence.push(diagnostic.into());
        }
        if uncertain {
            intent.phase = Phase::Uncertain;
        }
        progress.diagnostic = Some(diagnostic.into());
        Ok(())
    }

    pub(crate) fn wait_for_task_read(&mut self, frame: &str, task_id: String) {
        let progress = self
            .frames
            .get_mut(frame)
            .unwrap()
            .steps
            .last_mut()
            .unwrap();
        progress.status = Status::Waiting;
        if self.status.active() {
            self.status = Status::Waiting;
            self.waiting = Some(Waiting::Task {
                step: format!("{frame}/{}", progress.step),
                task_id,
            });
        }
    }

    pub(crate) fn settle_task_cancellation(
        &mut self,
        path: &str,
        charge_key: &str,
        stop: super::adapter::TaskCancellation,
    ) -> Result<()> {
        text(&stop.evidence)?;
        let charge = self
            .task_charges
            .get_mut(charge_key)
            .ok_or("Cancelled native Task has no reservation")?;
        charge.charged = Some(charge.charged.unwrap_or(0).max(stop.charged_tokens));
        charge.unknown_usage = stop.unknown_usage;
        let progress = self
            .frames
            .get_mut(path)
            .and_then(|frame| frame.steps.last_mut())
            .ok_or("Cancelled Task step disappeared")?;
        let intent = progress
            .intents
            .last_mut()
            .ok_or("Cancelled intent disappeared")?;
        intent.phase = Phase::Failed;
        intent.evidence.push(stop.evidence.clone());
        progress.diagnostic = Some(stop.evidence.clone());
        if self.status.active() {
            self.fail(stop.evidence);
        }
        Ok(())
    }

    pub fn prepare(&mut self, host: &dyn crate::Host, now: u64) -> Result<Option<Dispatch>> {
        self.prepare_observed(host, now, TaskReconciliation::Clear)
    }

    pub(crate) fn prepare_observed(
        &mut self,
        host: &dyn crate::Host,
        now: u64,
        task: TaskReconciliation,
    ) -> Result<Option<Dispatch>> {
        if !self.status.active() {
            return Ok(None);
        }
        require(now >= self.updated_unix_ms, "Run clock moved backwards")?;
        self.updated_unix_ms = now;
        if now >= self.created_unix_ms + self.limits.max_wall_ms {
            self.fail("Scenario elapsed-time allowance exhausted, including waits".into());
            return Ok(None);
        }
        self.status = Status::Running;
        self.waiting = None;
        self.prepare_frame(host, "root", now, &task)
    }
    fn prepare_frame(
        &mut self,
        host: &dyn crate::Host,
        path: &str,
        now: u64,
        task: &TaskReconciliation,
    ) -> Result<Option<Dispatch>> {
        let frame = self.frames[path].clone();
        let def = self.pin.bundle.definitions[&frame.scenario].clone();
        if now >= frame.started_unix_ms + def.limits.max_wall_ms {
            self.fail(format!("Child elapsed-time allowance exhausted at {path}"));
            return Ok(None);
        }
        let i = frame.steps.iter().take_while(|p| p.complete()).count();
        if i == def.steps.len() {
            for condition in &def.completion {
                require(
                    self.resolve(path, condition)? == Datum::Boolean(true),
                    "Scenario completion condition is false",
                )?;
            }
            let outputs = def
                .result
                .iter()
                .map(|(p, b)| Ok((p.clone(), self.resolve(path, b)?)))
                .collect::<Result<_>>()?;
            self.frames.get_mut(path).unwrap().outputs = Some(outputs);
            if path == "root" {
                self.status = Status::Succeeded;
            }
            return Ok(None);
        }
        let step = def.steps[i].clone();
        let full = format!("{path}/{}", step.id);
        if frame.steps.len() == i {
            let condition = step
                .when
                .as_ref()
                .map(|b| self.resolve(path, b))
                .transpose()?;
            let skipped = if condition == Some(Datum::Boolean(false)) {
                Some(format!("condition {} is false", hash(&condition)?))
            } else {
                None
            };
            let progress = Progress {
                step: step.id.clone(),
                started_unix_ms: now,
                deadline_unix_ms: now
                    .checked_add(step.timeout_ms)
                    .ok_or("Step deadline overflow")?,
                status: if skipped.is_some() {
                    Status::Succeeded
                } else {
                    Status::Ready
                },
                skipped,
                intents: Vec::new(),
                outputs: BTreeMap::new(),
                child: None,
                decision: None,
                diagnostic: None,
            };
            let skipped = progress.skipped.is_some();
            self.frames.get_mut(path).unwrap().steps.push(progress);
            if skipped {
                return Ok(None);
            }
        }
        let progress = self.frames[path].steps[i].clone();
        if now >= progress.deadline_unix_ms {
            match step.on_timeout {
                Timeout::Skip
                    if matches!(step.operation, Kind::Wait { .. } | Kind::Decision { .. }) =>
                {
                    let p = &mut self.frames.get_mut(path).unwrap().steps[i];
                    p.status = Status::Succeeded;
                    p.skipped = Some("deadline reached; explicit skip disposition".into());
                }
                _ => self.fail(format!(
                    "Step {full} deadline reached; existing effects were not rolled back"
                )),
            }
            return Ok(None);
        }
        if let TaskReconciliation::Blocked {
            frame,
            task_id,
            diagnostic,
            missing,
        } = task
            && frame == path
        {
            if *missing {
                self.frames.get_mut(path).unwrap().steps[i]
                    .intents
                    .last_mut()
                    .unwrap()
                    .phase = Phase::Failed;
                self.fail(diagnostic.clone());
            } else {
                self.wait_for_task_read(path, task_id.clone());
            }
            return Ok(None);
        }
        let inputs: BTreeMap<_, _> = step
            .inputs
            .iter()
            .map(|(n, b)| Ok((n.clone(), self.resolve(path, b)?)))
            .collect::<Result<_>>()?;
        match &step.operation {
            Kind::Decision { owner } => {
                if let Some(decision) = &progress.decision {
                    require(
                        decision.request.expires_unix_ms > now,
                        "Stale human decision",
                    )?;
                    if decision.request.approve {
                        self.complete(
                            host,
                            path,
                            &step.id,
                            BTreeMap::from([("result".into(), Datum::Decision(decision.clone()))]),
                        )?;
                    } else {
                        self.fail(format!("Owner rejected {full}"));
                    }
                } else {
                    let request = DecisionRequest {
                        schema: "af/scenario-decision@1".into(),
                        run_id: self.id.clone(),
                        execution_id: self.execution_id.clone(),
                        pin_id: self.pin.revision.clone(),
                        step: full.clone(),
                        subject: inputs["subject"].as_text()?.into(),
                        revision: inputs["revision"].as_text()?.into(),
                        owner: owner.clone(),
                        approve: true,
                        expires_unix_ms: progress
                            .deadline_unix_ms
                            .min(self.created_unix_ms + self.limits.max_wall_ms),
                    };
                    self.status = Status::Waiting;
                    self.waiting = Some(Waiting::Decision {
                        step: full,
                        request,
                    });
                    self.frames.get_mut(path).unwrap().steps[i].status = Status::Waiting;
                }
                return Ok(None);
            }
            Kind::Scenario { scenario } => {
                if !self.frames.contains_key(&full) {
                    self.reserve_dispatch(&full)?;
                    self.frames.insert(
                        full.clone(),
                        Frame {
                            scenario: scenario.clone(),
                            inputs,
                            started_unix_ms: now,
                            steps: Vec::new(),
                            dispatches: 0,
                            outputs: None,
                        },
                    );
                    self.frames.get_mut(path).unwrap().steps[i].child = Some(full.clone());
                }
                if let Some(outputs) = self.frames[&full].outputs.clone() {
                    self.complete(host, path, &step.id, outputs)?;
                    return Ok(None);
                }
                return self.prepare_frame(host, &full, now, task);
            }
            Kind::Action {
                requires_decision,
                required_condition,
                ..
            } => {
                let subject = inputs
                    .get("subject")
                    .map(Datum::as_text)
                    .transpose()?
                    .unwrap_or("");
                let revision = inputs
                    .get("revision")
                    .map(Datum::as_text)
                    .transpose()?
                    .unwrap_or("");
                if *requires_decision {
                    let Some(Datum::Decision(d)) = inputs.get("decision") else {
                        return Err("Action requires signed human evidence".into());
                    };
                    d.verify(&self.pin.policy)?;
                    require(
                        d.request.run_id == self.id
                            && d.request.execution_id == self.execution_id
                            && d.request.pin_id == self.pin.revision
                            && d.request.step
                                == self.decision_source(path, &step.inputs["decision"])?
                            && d.request.approve
                            && d.request.subject == subject
                            && d.request.revision == revision
                            && d.request.expires_unix_ms > now,
                        "Stale or unrelated human decision",
                    )?;
                }
                if let Some(condition) = required_condition {
                    let Some(Datum::Observation(o)) = inputs.get("evidence") else {
                        return Err("Action requires observed evidence".into());
                    };
                    require(
                        o.satisfied
                            && o.subject == subject
                            && o.revision == revision
                            && o.condition == *condition,
                        "Stale checks or unrelated evidence",
                    )?;
                }
            }
            _ => {}
        }
        let input_digest = hash(&(&step, &inputs))?;
        let mut intents = progress.intents;
        let previous = intents.last();
        // An unbound prepared FreshTask already reserved its dispatch, even when the
        // scheduler stopped before it could record any preparation evidence.
        let retrying_preparation = matches!(step.operation, Kind::FreshTask { .. })
            && previous.is_some_and(|intent| {
                intent.phase == Phase::Prepared && !self.fresh_tasks.contains_key(&intent.id)
            });
        let polling_scoped_wait = self.pin.policy.implementation_startup.is_some()
            && matches!(step.operation, Kind::Wait { .. })
            && previous.is_some_and(|intent| intent.phase == Phase::Uncertain);
        let spends_dispatch = !retrying_preparation && !polling_scoped_wait;
        if spends_dispatch {
            self.check_dispatch(&full)?;
        }
        let new_intent = intents
            .last()
            .is_none_or(|intent| intent.phase == Phase::Failed);
        if new_intent {
            require(
                intents.len() < usize::from(step.max_attempts),
                "Step exhausted its bounded retries",
            )?;
            if let Kind::Task { target } | Kind::CiDiagnosis { target } = &step.operation {
                self.reserve_task(&full, target)?;
            }
            let intent_id = hash(&(
                &self.execution_id,
                &self.pin.revision,
                &full,
                intents.len() + 1,
            ))?;
            if let Kind::FreshTask { template, .. } = &step.operation {
                let key = hash(&(&template.state, super::template::task_id(&intent_id)))?;
                self.reserve_charge(&full, key, template.max_tokens)?;
            }
            intents.push(Intent {
                id: intent_id,
                input_digest: input_digest.clone(),
                phase: Phase::Prepared,
                evidence: Vec::new(),
            });
        }
        let intent = intents.last().unwrap().clone();
        require(
            intent.input_digest == input_digest,
            "Intent inputs changed across recovery",
        )?;
        if spends_dispatch {
            self.record_dispatch(&full);
        }
        let p = &mut self.frames.get_mut(path).unwrap().steps[i];
        p.intents = intents;
        p.status = Status::Running;
        if new_intent && matches!(step.operation, Kind::FreshTask { .. }) {
            p.diagnostic = None;
        }
        let step_started_unix_ms = p.started_unix_ms;
        let mut deadline = self.created_unix_ms + self.limits.max_wall_ms;
        for (frame_path, frame) in &self.frames {
            if full.starts_with(&format!("{frame_path}/")) {
                deadline = deadline.min(
                    frame.started_unix_ms
                        + self.pin.bundle.definitions[&frame.scenario]
                            .limits
                            .max_wall_ms,
                );
                if let Some(progress) = frame.steps.last() {
                    deadline = deadline.min(progress.deadline_unix_ms);
                }
            }
        }
        Ok(Some(Dispatch {
            frame: path.into(),
            step: step.id.clone(),
            definition: step,
            inputs,
            intent,
            timeout_ms: deadline - now,
            step_started_unix_ms,
            target: None,
        }))
    }
    fn reserve_dispatch(&mut self, path: &str) -> Result<()> {
        self.check_dispatch(path)?;
        self.record_dispatch(path);
        Ok(())
    }
    fn check_dispatch(&self, path: &str) -> Result<()> {
        require(
            self.dispatches < self.limits.max_dispatches,
            "Scenario dispatch allowance exhausted",
        )?;
        for (frame_path, frame) in &self.frames {
            if path.starts_with(&format!("{frame_path}/")) {
                require(
                    frame.dispatches
                        < self.pin.bundle.definitions[&frame.scenario]
                            .limits
                            .max_dispatches,
                    "Child dispatch allowance exhausted",
                )?;
            }
        }
        Ok(())
    }
    fn record_dispatch(&mut self, path: &str) {
        self.dispatches += 1;
        for (frame_path, frame) in &mut self.frames {
            if path.starts_with(&format!("{frame_path}/")) {
                frame.dispatches += 1;
            }
        }
    }
    fn reserve_task(&mut self, path: &str, target: &TaskTarget) -> Result<()> {
        let key = hash(&(target.state.clone(), &target.task_id, &target.revision_id))?;
        self.reserve_charge(path, key, target.max_tokens)
    }
    fn reserve_charge(&mut self, path: &str, key: String, maximum: u64) -> Result<()> {
        if !self.task_charges.contains_key(&key) {
            if let Some(scope) = &self.pin.policy.implementation_startup {
                let repair_step = |full: &str| -> bool {
                    full.rsplit_once('/')
                        .and_then(|(frame, id)| {
                            self.frames.get(frame).and_then(|f| {
                                self.pin.bundle.definitions[&f.scenario]
                                    .steps
                                    .iter()
                                    .find(|s| s.id == id)
                            })
                        })
                        .is_some_and(|s| {
                            matches!(&s.operation, Kind::FreshTask { template, .. } if template.document["kind"] == "implement")
                        })
                };
                if repair_step(path) {
                    require(
                        self.task_charges
                            .values()
                            .filter(|c| repair_step(&c.step))
                            .count()
                            <= scope.max_repairs as usize,
                        "Shared implementation repair allowance exhausted",
                    )?;
                }
            }
            require(
                self.tokens()?
                    .checked_add(u128::from(maximum))
                    .is_some_and(|n| n <= u128::from(self.token_allowance())),
                "Scenario token allowance exhausted",
            )?;
            // Check every containing Scenario's own allowance; root accounting is shared once.
            for (frame_path, frame) in &self.frames {
                if path.starts_with(&format!("{frame_path}/")) {
                    let used = self
                        .task_charges
                        .values()
                        .filter(|c| c.step.starts_with(&format!("{frame_path}/")))
                        .try_fold(0_u128, |n, c| {
                            n.checked_add(u128::from(c.committed()))
                                .ok_or("Child accounting overflow")
                        })?;
                    require(
                        used.checked_add(u128::from(maximum)).is_some_and(|n| {
                            n <= u128::from(
                                self.pin.bundle.definitions[&frame.scenario]
                                    .limits
                                    .max_tokens,
                            )
                        }),
                        "Child token allowance exhausted",
                    )?;
                }
            }
            self.task_charges.insert(
                key,
                TaskCharge {
                    maximum,
                    charged: None,
                    unknown_usage: false,
                    step: path.into(),
                },
            );
        }
        Ok(())
    }
    pub fn complete(
        &mut self,
        host: &dyn crate::Host,
        path: &str,
        step_id: &str,
        outputs: BTreeMap<String, Datum>,
    ) -> Result<()> {
        let frame = self.frames.get(path).ok_or("Unknown completion frame")?;
        let step = self.pin.bundle.definitions[&frame.scenario]
            .steps
            .iter()
            .find(|s| s.id == step_id)
            .ok_or("Unknown completion step")?;
        let mut captured = Vec::new();
        if matches!(
            step.operation,
            Kind::Task { .. } | Kind::CiDiagnosis { .. } | Kind::FreshTask { .. }
        ) {
            for value in outputs.values() {
                if let Some(receipt) = super::task_output::accepted_receipt(value)? {
                    captured.extend(super::task_output::capture(host, self, receipt)?);
                }
            }
        }
        let frame = self.frames.get_mut(path).ok_or("Missing frame")?;
        let p = frame.steps.last_mut().ok_or("Missing step")?;
        require(p.step == step_id && !p.complete(), "Stale step completion")?;
        if !captured.is_empty() {
            p.intents
                .last_mut()
                .ok_or("Task output lacks native dispatch intent")?
                .evidence
                .extend(captured);
        }
        p.outputs = outputs;
        p.status = Status::Succeeded;
        if let Some(intent) = p.intents.last_mut() {
            intent.phase = Phase::Done;
        }
        Ok(())
    }
    pub fn apply_decision(&mut self, receipt: DecisionReceipt, now: u64) -> Result<()> {
        receipt.verify(&self.pin.policy)?;
        let Some(Waiting::Decision { request, .. }) = &self.waiting else {
            return Err("Run is not waiting for this decision".into());
        };
        let mut expected = request.clone();
        expected.approve = receipt.request.approve;
        require(
            receipt.request == expected
                && receipt.request.expires_unix_ms > now
                && self.status == Status::Waiting,
            "Stale decision or wrong exact object",
        )?;
        let (path, step) = receipt
            .request
            .step
            .rsplit_once('/')
            .ok_or("Invalid decision path")?;
        let progress = self
            .frames
            .get_mut(path)
            .and_then(|f| f.steps.last_mut())
            .ok_or("Missing decision step")?;
        require(
            progress.step == step && progress.decision.is_none(),
            "Decision was already recorded",
        )?;
        progress.decision = Some(receipt);
        self.status = Status::Ready;
        self.waiting = None;
        self.updated_unix_ms = now;
        Ok(())
    }
}
