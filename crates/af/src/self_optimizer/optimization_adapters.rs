//! Allowlisted native history adapters.
//!
//! These parsers intentionally do not deserialize provider transcript bodies. They select a
//! small set of identity, lifecycle, usage, cache and outcome fields into the normalized
//! economics contract. Unknown provider fields are ignored; reasoning/thinking and message
//! content are never copied into an Observation.

use super::*;
use review_core::task::usage::TaskTokenUsageV3;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AdapterVersion {
    Normalized,
    Native,
}

#[derive(Default)]
struct AttemptState {
    started_unix_ms: Option<u64>,
}

pub(super) struct AdapterState {
    pub(super) attest_project: bool,
    adapter: String,
    project_id: String,
    declared_execution_id: String,
    project_root: PathBuf,
    cutoff: u64,
    session_id: Option<String>,
    project_member: Option<bool>,
    model: Option<String>,
    effort: Option<String>,
    node: Option<String>,
    worker: Option<String>,
    sequence: u64,
    attempts: BTreeMap<String, AttemptState>,
    seen_usage_keys: BTreeSet<String>,
    gaps: BTreeSet<String>,
    version: Option<AdapterVersion>,
}

impl AdapterState {
    pub(super) fn new(
        adapter: &str,
        project_id: &str,
        execution_id: &str,
        project_root: &Path,
        cutoff: u64,
        trusted_continuation: bool,
    ) -> Self {
        Self {
            attest_project: false,
            adapter: adapter.into(),
            project_id: project_id.into(),
            declared_execution_id: execution_id.into(),
            project_root: std::fs::canonicalize(project_root)
                .unwrap_or_else(|_| project_root.to_path_buf()),
            cutoff,
            session_id: None,
            // A nonzero range continues a source whose preceding prefix was already retained and
            // identity-checked. Any new native session metadata below can still reset this.
            project_member: trusted_continuation.then_some(true),
            model: None,
            effort: None,
            node: None,
            worker: None,
            sequence: 0,
            attempts: BTreeMap::new(),
            seen_usage_keys: BTreeSet::new(),
            gaps: BTreeSet::new(),
            version: None,
        }
    }

    pub(super) fn version(&self) -> AdapterVersion {
        self.version.unwrap_or(AdapterVersion::Native)
    }

    pub(super) fn has_coverage_gap(&self) -> bool {
        self.gaps
            .iter()
            .any(|gap| gap != "excluded_foreign_project")
    }

    pub(super) fn take_gaps(&mut self) -> BTreeSet<String> {
        std::mem::take(&mut self.gaps)
    }

    fn mark_native(&mut self) {
        self.version.get_or_insert(AdapterVersion::Native);
    }

    fn observed_time(&mut self, value: &Value) -> u64 {
        for candidate in [
            value.get("observed_unix_ms"),
            value.get("timestamp_unix_ms"),
            value.get("now_unix_ms"),
            value.pointer("/payload/observed_unix_ms"),
            value.pointer("/payload/timestamp_unix_ms"),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(number) = decimal_u64(candidate) {
                return number;
            }
        }
        if let Some(timestamp) = value
            .get("timestamp")
            .or_else(|| value.pointer("/payload/timestamp"))
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_millis)
        {
            return timestamp;
        }
        self.gaps.insert("observed_time_unknown".into());
        self.cutoff
    }

    fn observe_identity(&mut self, value: &Value) {
        let session = string_at(
            value,
            &[
                "/session_id",
                "/sessionId",
                "/payload/id",
                "/payload/session_id",
            ],
        );
        if let Some(session) = session {
            if self.session_id.as_deref() != Some(session) {
                self.session_id = Some(bounded(session, &self.declared_execution_id));
                self.project_member = None;
                self.model = None;
                self.effort = None;
            }
        }
        if let Some(record_project) = string_at(value, &["/project_id", "/payload/project_id"]) {
            self.project_member = Some(record_project == self.project_id);
        }
        if let Some(cwd) = string_at(value, &["/cwd", "/payload/cwd", "/workspace/cwd"]) {
            let path = Path::new(cwd);
            let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            self.project_member = Some(canonical == self.project_root);
        }
        if let Some(model) = string_at(value, &["/model", "/payload/model", "/message/model"]) {
            self.model = Some(bounded(model, "unknown-model"));
        }
        if let Some(effort) = string_at(value, &["/effort", "/payload/effort"]) {
            self.effort = Some(bounded(effort, "unknown-effort"));
        }
    }

    fn project_member(&mut self) -> bool {
        match self.project_member {
            Some(true) => true,
            Some(false) => {
                self.gaps.insert("excluded_foreign_project".into());
                false
            }
            None => {
                self.gaps.insert("project_identity_unknown".into());
                false
            }
        }
    }

    fn attribution(
        &self,
        case_family: &str,
        execution_id: Option<&str>,
    ) -> OptimizationAttributionV1 {
        OptimizationAttributionV1 {
            project_id: self.project_id.clone(),
            case_family: bounded(case_family, "native-session"),
            execution_id: bounded(
                execution_id.unwrap_or(&self.declared_execution_id),
                &self.declared_execution_id,
            ),
            outer_execution_id: None,
            task_id: None,
            attempt_id: None,
            pipeline: None,
            node: self.node.clone(),
            worker: self.worker.clone(),
            model: self.model.clone(),
            effort: self.effort.clone(),
            configuration_id: None,
            environment_id: None,
        }
    }
}

pub(super) fn parse_adapter_record(
    state: &mut AdapterState,
    line: &[u8],
) -> Result<Vec<NormalizedRecord>, String> {
    // `external` is the public normalized import/fixture adapter; native adapters never read
    // the normalized record contract.
    if state.adapter == "external" {
        let record = serde_json::from_slice::<NormalizedRecord>(line)
            .map_err(|_| "external adapter requires the normalized record contract")?;
        if record.attribution.project_id != state.project_id {
            return Err("normalized record has a foreign project identity".into());
        }
        state.version = Some(AdapterVersion::Normalized);
        return Ok(vec![record]);
    }
    let value: Value = serde_json::from_slice(line).map_err(|error| error.to_string())?;
    state.mark_native();
    state.sequence = state.sequence.saturating_add(1);
    match state.adapter.as_str() {
        "af" => parse_af(state, &value),
        "codex" => parse_codex(state, &value),
        "claude" => parse_claude(state, &value),
        _ => Err("unsupported history adapter".into()),
    }
}

fn parse_af(state: &mut AdapterState, value: &Value) -> Result<Vec<NormalizedRecord>, String> {
    if value
        .get("schema")
        .and_then(Value::as_str)
        .is_some_and(|schema| schema.starts_with("af/task-inspection@"))
    {
        return parse_af_inspection(state, value);
    }
    Err(
        "af adapter accepts af/task-inspection receipts from `af task show --json`; use external for normalized records"
            .into(),
    )
}

/// Read the actual public AF inspection receipt, preserving its cumulative accounting.
/// Historical exports have no project identity: importing them requires explicit source
/// attestation plus an exact Task ID match. That provenance limitation stays visible.
fn parse_af_inspection(
    state: &mut AdapterState,
    value: &Value,
) -> Result<Vec<NormalizedRecord>, String> {
    if value.get("schema").and_then(Value::as_str) != Some("af/task-inspection@11") {
        return Err("unsupported AF inspection generation".into());
    }
    let task = value
        .get("task_id")
        .and_then(Value::as_str)
        .ok_or("AF receipt lacks Task identity")?;
    if task != state.declared_execution_id {
        return Err("AF receipt does not match declared execution identity".into());
    }
    state.observe_identity(value);
    let native_project = value.get("project_id").and_then(Value::as_str);
    if native_project.is_some_and(|project| project != state.project_id) {
        return Err("AF receipt has a foreign project identity".into());
    }
    if native_project.is_none() && !state.attest_project {
        state.gaps.insert("project_identity_unknown".into());
        return Ok(vec![]);
    }
    let total = value
        .get("chargeable_tokens")
        .and_then(decimal_u128)
        .ok_or("AF receipt lacks exact total")?;
    let mut times = BTreeMap::new();
    let mut opened = None;
    let mut finished = None;
    let mut approval_started = None;
    let mut waiting_started: Option<(u64, OptimizationSpanKindV1)> = None;
    let mut lifecycle_intervals = Vec::new();
    let mut latest = 0;
    for entry in value
        .get("history")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(transition) = entry.get("transition") else {
            continue;
        };
        let Some(time) = transition.get("now_unix_ms").and_then(decimal_u64) else {
            continue;
        };
        latest = latest.max(time);
        let change = &transition["change"];
        match change["kind"].as_str() {
            Some("opened") => opened = Some(time),
            Some("finished") => finished = Some(time),
            Some("planning_completed") => approval_started = Some(time),
            Some("plan_decided") => {
                if let Some(start) = approval_started.take() {
                    lifecycle_intervals.push((
                        OptimizationSpanKindV1::ApprovalWaiting,
                        "approval",
                        start,
                        time,
                    ));
                }
            }
            Some("waiting") => {
                let kind = if change["reason"] == "needs_plan_review" {
                    OptimizationSpanKindV1::ApprovalWaiting
                } else {
                    OptimizationSpanKindV1::UserWaiting
                };
                waiting_started = Some((time, kind));
            }
            Some("resumed") => {
                if let Some((start, kind)) = waiting_started.take() {
                    lifecycle_intervals.push((kind, "task-wait", start, time));
                }
            }
            Some("execution_recorded") => {
                if let Some(id) = change["record_id"].as_str() {
                    times.insert(id.to_owned(), time);
                }
            }
            _ => {}
        }
    }
    let mut charges: BTreeMap<String, u128> = BTreeMap::new();
    let mut starts = BTreeMap::new();
    let mut ends = BTreeMap::new();
    let mut reserved = BTreeMap::new();
    let mut attempt_outcomes = BTreeMap::new();
    let walls = value
        .get("attempt_walls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|wall| {
            wall.get("attempt_id")
                .and_then(Value::as_str)
                .map(|attempt| (attempt.to_owned(), wall))
        })
        .collect::<BTreeMap<_, _>>();
    let attempts_with_cache_evidence = value
        .get("runtime_observations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| {
            entry
                .pointer("/record/caches")
                .and_then(Value::as_array)
                .is_some_and(|caches| !caches.is_empty())
        })
        .filter_map(|entry| {
            entry
                .pointer("/record/attempt_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect::<BTreeSet<_>>();
    for entry in value
        .get("execution_records")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let record = &entry["record"];
        let Some(attempt) = record["attempt_id"].as_str() else {
            continue;
        };
        let time = entry["artifact_id"]
            .as_str()
            .and_then(|id| times.get(id))
            .copied();
        match record["kind"].as_str() {
            Some("reserved") => {
                if let Some(t) = time {
                    reserved.insert(attempt.to_owned(), t);
                }
            }
            Some("started") => {
                if let Some(t) = time {
                    starts.insert(attempt.to_owned(), t);
                }
            }
            Some("settled") | Some("usage_observed") => {
                if let Some(charge) = record.get("charged_tokens").and_then(decimal_u128) {
                    charges
                        .entry(attempt.to_owned())
                        .and_modify(|old| *old = (*old).max(charge))
                        .or_insert(charge);
                }
                if record["kind"] == "settled" {
                    if let Some(t) = time {
                        ends.insert(attempt.to_owned(), t);
                    }
                    match record.pointer("/result/kind").and_then(Value::as_str) {
                        Some("failed") => {
                            attempt_outcomes
                                .insert(attempt.to_owned(), OptimizationOutcomeV1::Failed);
                        }
                        Some("abandoned") => {
                            attempt_outcomes
                                .insert(attempt.to_owned(), OptimizationOutcomeV1::Abandoned);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    let counted = charges
        .values()
        .try_fold(0u128, |sum, n| sum.checked_add(*n))
        .ok_or("AF receipt charge overflow")?;
    if counted > total {
        return Err("AF receipt attempts exceed its cumulative total".into());
    }
    if total > counted {
        charges.insert("unattributed".into(), total - counted);
    }
    let mut observations = Vec::new();
    for (attempt, charge) in charges {
        let mut attribution = state.attribution("af-task", Some(task));
        attribution.task_id = Some(task.into());
        if attempt != "unattributed" {
            attribution.attempt_id = Some(attempt.clone());
        }
        if attempt_outcomes.contains_key(&attempt) {
            // A failed/abandoned child is its own observed execution outcome. Keeping it under
            // the parent Task's execution row would collapse several failed arms and retries
            // into the final parent verdict even though their charges remain distinct.
            attribution.execution_id = format!("{task}:{attempt}");
        }
        let mut spans = Vec::new();
        if let Some(wall) = walls.get(&attempt)
            && let (Some(start), Some(elapsed)) = (
                wall.get("started_unix_ms").and_then(decimal_u64),
                wall.get("elapsed_ms").and_then(decimal_u64),
            )
        {
            spans = runtime_spans(
                "af-wall",
                &format!("{task}-{attempt}"),
                start,
                start.saturating_add(elapsed),
            );
        } else if let (Some(start), Some(end)) = (starts.get(&attempt), ends.get(&attempt)) {
            spans = runtime_spans("af", &format!("{task}-{attempt}"), *start, *end);
        }
        if let (Some(start), Some(end)) = (reserved.get(&attempt), starts.get(&attempt)) {
            let id = content_id(&serde_json::json!([task, attempt, "queue", start, end]))
                .map_err(|e| e.to_string())?;
            spans.push(OptimizationSpanV1 {
                span_id: id,
                kind: OptimizationSpanKindV1::Queue,
                start_unix_ms: (*start).into(),
                end_unix_ms: (*end).into(),
                clock: "host".into(),
                status: MeasurementStatusV1::Exact,
            });
        }
        let usage = walls
            .get(&attempt)
            .and_then(|wall| wall.get("usage"))
            .cloned()
            .map(serde_json::from_value::<TaskTokenUsageV3>)
            .transpose()
            .map_err(|error| format!("invalid AF Attempt wall usage: {error}"))?
            .unwrap_or(TaskTokenUsageV3 {
                chargeable_tokens: charge.into(),
                ..Default::default()
            });
        if usage.chargeable_tokens.get() != charge {
            return Err("AF Attempt wall charge contradicts cumulative receipt".into());
        }
        let tokens = OptimizationTokenObservationV1 {
            cumulative_key: format!("af-{attempt}"),
            usage,
            status: MeasurementStatusV1::Exact,
            context_tokens: None,
            retrieval_tokens: None,
            repeated_context_tokens: None,
            outer_session: false,
        };
        let mut missing = missing_for_tokens(Some(&tokens));
        missing.insert("worker_attribution".into());
        if !attempts_with_cache_evidence.contains(&attempt) {
            missing.insert("cache_measurements".into());
        }
        if native_project.is_none() {
            missing.insert("project_identity_attested".into());
        }
        if spans.is_empty() {
            missing.insert("active_time".into());
        }
        let outcome = attempt_outcomes.get(&attempt).copied().map(|outcome| {
            missing.insert("requirements_identity".into());
            missing.insert("retry_classification".into());
            missing.insert("repair_classification".into());
            OptimizationOutcomeObservationV1 {
                outcome,
                requirements_id: None,
                verifier_id: None,
                retries: 0,
                repairs: 0,
                later_defects: 0,
            }
        });
        observations.push(NormalizedRecord {
            observed_unix_ms: latest.into(),
            attribution,
            tokens: Some(tokens),
            spans,
            caches: vec![],
            outcome,
            missing_fields: missing,
        });
    }
    for entry in value
        .get("runtime_observations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let evidence: review_core::task::runtime::TaskRuntimeEvidenceV1 =
            serde_json::from_value(entry["record"].clone())
                .map_err(|error| format!("invalid AF runtime evidence: {error}"))?;
        evidence.validate()?;
        if evidence.task_id != task {
            return Err("AF runtime evidence has another Task identity".into());
        }
        let mut attribution = state.attribution("af-task", Some(task));
        attribution.task_id = Some(task.into());
        attribution.attempt_id = Some(evidence.attempt_id.clone());
        attribution.node = Some(evidence.node.clone());
        attribution.configuration_id = Some(evidence.context_id.clone());
        let mut spans = Vec::new();
        for span in evidence.spans {
            let kind = match span.kind {
                review_core::task::runtime::TaskRuntimeSpanKindV1::Check => {
                    OptimizationSpanKindV1::Check
                }
                review_core::task::runtime::TaskRuntimeSpanKindV1::DependencyPreparation => {
                    OptimizationSpanKindV1::DependencyPreparation
                }
            };
            spans.push(OptimizationSpanV1 {
                span_id: span.span_id,
                kind,
                start_unix_ms: span.started_unix_ms.into(),
                end_unix_ms: span.started_unix_ms.saturating_add(span.elapsed_ms).into(),
                clock: "host".into(),
                status: MeasurementStatusV1::Exact,
            });
        }
        let mut missing = BTreeSet::new();
        let caches = evidence
            .caches
            .into_iter()
            .map(|cache| {
                // AF records dependency preparation only, never a tool or provider cache result.
                missing.insert("cache_internal_result".into());
                if cache.toolchain_id.is_none() {
                    missing.insert("cache_toolchain_identity".into());
                }
                OptimizationCacheObservationV1 {
                    kind: format!("preparation_{}", cache.kind),
                    eligible: cache.eligible,
                    result: CacheResultV1::Unknown,
                    temperature: CacheTemperatureV1::Unknown,
                    invalidation_id: Some(cache.source_digest),
                    // Preparation bytes were made available; they are not evidence of an
                    // internal cache hit and therefore are not reported as bytes reused.
                    bytes_reused: None,
                    tokens_reused: None,
                    lookup_ms: Some(cache.lookup_ms.into()),
                    warmup_ms: Some(cache.materialization_ms.into()),
                }
            })
            .collect::<Vec<_>>();
        let observed = spans
            .iter()
            .map(|span| span.end_unix_ms.get())
            .max()
            .unwrap_or(latest);
        observations.push(NormalizedRecord {
            observed_unix_ms: observed.into(),
            attribution,
            tokens: None,
            spans,
            caches,
            outcome: None,
            missing_fields: missing,
        });
    }
    let mut attribution = state.attribution("af-task", Some(task));
    attribution.task_id = Some(task.into());
    let mut spans = Vec::new();
    if let (Some(start), Some(end)) = (opened, finished) {
        spans = runtime_spans("af-task", task, start, end);
        spans.retain(|span| span.kind == OptimizationSpanKindV1::EndToEnd);
    }
    for (kind, label, start, end) in lifecycle_intervals {
        let span_id = content_id(&serde_json::json!([task, label, start, end]))
            .map_err(|error| error.to_string())?;
        spans.push(OptimizationSpanV1 {
            span_id,
            kind,
            start_unix_ms: start.into(),
            end_unix_ms: end.into(),
            clock: "host".into(),
            status: MeasurementStatusV1::Exact,
        });
    }
    let result = &value["result"];
    let verifier = result
        .pointer("/outputs/verification/artifact_ids/0")
        .and_then(Value::as_str)
        .filter(|id| review_core::is_digest(id))
        .map(str::to_owned);
    let verified = result["acceptance"] == "satisfied" && verifier.is_some();
    observations.push(NormalizedRecord {
        observed_unix_ms: latest.into(),
        attribution,
        tokens: None,
        spans,
        caches: vec![],
        outcome: Some(OptimizationOutcomeObservationV1 {
            outcome: if verified && verifier.is_some() {
                OptimizationOutcomeV1::Verified
            } else if result["execution"] == "failed" {
                OptimizationOutcomeV1::Failed
            } else {
                OptimizationOutcomeV1::Incomplete
            },
            requirements_id: None,
            verifier_id: verifier,
            retries: 0,
            repairs: 0,
            later_defects: 0,
        }),
        missing_fields: [
            "requirements_identity".into(),
            "retry_classification".into(),
            "repair_classification".into(),
        ]
        .into(),
    });
    Ok(observations)
}

fn parse_codex(state: &mut AdapterState, value: &Value) -> Result<Vec<NormalizedRecord>, String> {
    state.observe_identity(value);
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
    let payload_kind = value
        .pointer("/payload/type")
        .and_then(Value::as_str)
        .unwrap_or("");
    if matches!(kind, "session_meta" | "turn_context") || payload_kind == "turn_context" {
        return Ok(Vec::new());
    }
    // Reasoning and visible message records are intentionally not inputs to economics.
    if matches!(kind, "response_item" | "item.completed")
        || matches!(payload_kind, "reasoning" | "agent_reasoning" | "message")
    {
        return Ok(Vec::new());
    }
    if !state.project_member() {
        return Ok(Vec::new());
    }
    let observed = state.observed_time(value);
    let session = state.declared_execution_id.clone();
    let turn = string_at(value, &["/turn_id", "/payload/turn_id"])
        .map(str::to_owned)
        .unwrap_or_else(|| format!("turn-{}", state.sequence));
    if matches!(kind, "turn.started" | "turn_started") || payload_kind == "turn_started" {
        state.attempts.entry(turn).or_default().started_unix_ms = Some(observed);
        return Ok(Vec::new());
    }
    let usage = if kind == "turn.completed" {
        value.get("usage")
    } else if payload_kind == "token_count" {
        value
            .pointer("/payload/info/total_token_usage")
            .or_else(|| value.pointer("/payload/info/last_token_usage"))
    } else {
        None
    };
    let failed =
        matches!(kind, "turn.failed" | "error") || matches!(payload_kind, "turn_failed" | "error");
    if usage.is_none() && !failed && kind != "turn.completed" {
        return Ok(Vec::new());
    }
    let cumulative_key = if payload_kind == "token_count" {
        session.clone()
    } else {
        format!("{session}-{turn}")
    };
    let tokens = usage.map(|usage| usage_from_codex(usage, &cumulative_key));
    let mut spans = Vec::new();
    if kind == "turn.completed" || failed {
        if let Some(start) = state
            .attempts
            .get(&turn)
            .and_then(|attempt| attempt.started_unix_ms)
        {
            spans.extend(runtime_spans("codex", &turn, start, observed));
        }
    }
    let mut missing = missing_for_tokens(tokens.as_ref());
    if spans.is_empty() {
        missing.insert("elapsed_time".into());
    }
    let outcome =
        (kind == "turn.completed" || failed).then_some(OptimizationOutcomeObservationV1 {
            outcome: if failed {
                OptimizationOutcomeV1::Failed
            } else {
                OptimizationOutcomeV1::Incomplete
            },
            requirements_id: None,
            verifier_id: None,
            retries: 0,
            repairs: 0,
            later_defects: 0,
        });
    if outcome.is_some() {
        missing.insert("outcome_verification".into());
    }
    Ok(vec![NormalizedRecord {
        observed_unix_ms: observed.into(),
        attribution: state.attribution("codex-session", None),
        tokens,
        spans,
        caches: (kind == "turn.completed")
            .then(|| usage.map(provider_cache))
            .flatten()
            .into_iter()
            .collect(),
        outcome,
        missing_fields: missing,
    }])
}

fn parse_claude(state: &mut AdapterState, value: &Value) -> Result<Vec<NormalizedRecord>, String> {
    state.observe_identity(value);
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
    // Thinking blocks and all message content are withheld. Usage metadata on an assistant
    // envelope is still safe because only its numeric allowlist is selected below.
    if kind == "assistant"
        && value
            .pointer("/message/content")
            .and_then(Value::as_array)
            .is_some_and(|blocks| {
                blocks.iter().all(|block| {
                    matches!(
                        block.get("type").and_then(Value::as_str),
                        Some("thinking" | "redacted_thinking")
                    )
                })
            })
        && value.pointer("/message/usage").is_none()
    {
        return Ok(Vec::new());
    }
    let usage = if kind == "result" {
        value.get("usage")
    } else if kind == "assistant" {
        value.pointer("/message/usage")
    } else {
        None
    };
    if usage.is_none() && kind != "result" {
        return Ok(Vec::new());
    }
    if !state.project_member() {
        return Ok(Vec::new());
    }
    let observed = state.observed_time(value);
    let session = state.declared_execution_id.clone();
    let cumulative_key = if kind == "result" {
        session.clone()
    } else {
        string_at(value, &["/message/id", "/uuid"])
            .map(|id| format!("{session}-{}", bounded(id, "message")))
            .unwrap_or_else(|| format!("{session}-message-{}", state.sequence))
    };
    let first_usage = state.seen_usage_keys.insert(cumulative_key.clone());
    let tokens = usage.map(|usage| usage_from_claude(usage, &cumulative_key));
    let mut spans = Vec::new();
    if kind == "result"
        && let Some(elapsed) = value.get("duration_ms").and_then(decimal_u64)
    {
        spans.extend(runtime_spans(
            "claude",
            &session,
            observed.saturating_sub(elapsed),
            observed,
        ));
    }
    let failed = value.get("is_error").and_then(Value::as_bool) == Some(true);
    let mut missing = missing_for_tokens(tokens.as_ref());
    missing.insert("reasoning_tokens".into());
    if kind == "result" {
        missing.insert("outcome_verification".into());
    }
    if spans.is_empty() {
        missing.insert("elapsed_time".into());
    }
    Ok(vec![NormalizedRecord {
        observed_unix_ms: observed.into(),
        attribution: state.attribution("claude-session", None),
        tokens,
        spans,
        caches: first_usage
            .then(|| usage.map(provider_cache))
            .flatten()
            .into_iter()
            .collect(),
        outcome: (kind == "result").then_some(OptimizationOutcomeObservationV1 {
            outcome: if failed {
                OptimizationOutcomeV1::Failed
            } else {
                OptimizationOutcomeV1::Incomplete
            },
            requirements_id: None,
            verifier_id: None,
            retries: 0,
            repairs: 0,
            later_defects: 0,
        }),
        missing_fields: missing,
    }])
}

fn usage_from_codex(value: &Value, cumulative_key: &str) -> OptimizationTokenObservationV1 {
    let input = counter(value, "input_tokens");
    let output = counter(value, "output_tokens");
    let read = counter(value, "cached_input_tokens");
    let write = counter(value, "cache_write_input_tokens");
    let reasoning = counter(value, "reasoning_output_tokens");
    let charge = input
        .zip(read)
        .and_then(|(input, read)| input.checked_sub(read))
        .and_then(|input| output.and_then(|output| input.checked_add(output)));
    OptimizationTokenObservationV1 {
        cumulative_key: bounded(cumulative_key, "codex-session"),
        usage: TaskTokenUsageV3 {
            input_tokens: input.map(Into::into),
            output_tokens: output.map(Into::into),
            cache_read_tokens: read.map(Into::into),
            cache_write_tokens: write.map(Into::into),
            reasoning_tokens: reasoning.map(Into::into),
            chargeable_tokens: charge.unwrap_or(0).into(),
        },
        status: if charge.is_some() {
            MeasurementStatusV1::Exact
        } else {
            MeasurementStatusV1::LowerBound
        },
        context_tokens: input.map(Into::into),
        retrieval_tokens: None,
        repeated_context_tokens: read.map(Into::into),
        outer_session: true,
    }
}

fn usage_from_claude(value: &Value, cumulative_key: &str) -> OptimizationTokenObservationV1 {
    let input = counter(value, "input_tokens");
    let output = counter(value, "output_tokens");
    let write = counter(value, "cache_creation_input_tokens");
    let read = counter(value, "cache_read_input_tokens");
    let charge = input
        .zip(output)
        .zip(write)
        .and_then(|((input, output), write)| input.checked_add(output)?.checked_add(write));
    OptimizationTokenObservationV1 {
        cumulative_key: bounded(cumulative_key, "claude-session"),
        usage: TaskTokenUsageV3 {
            input_tokens: input.map(Into::into),
            output_tokens: output.map(Into::into),
            cache_read_tokens: read.map(Into::into),
            cache_write_tokens: write.map(Into::into),
            reasoning_tokens: None,
            chargeable_tokens: charge.unwrap_or(0).into(),
        },
        status: if charge.is_some() {
            MeasurementStatusV1::Exact
        } else {
            MeasurementStatusV1::LowerBound
        },
        context_tokens: input.map(Into::into),
        retrieval_tokens: None,
        repeated_context_tokens: read.map(Into::into),
        outer_session: true,
    }
}

fn provider_cache(value: &Value) -> OptimizationCacheObservationV1 {
    let read =
        counter(value, "cached_input_tokens").or_else(|| counter(value, "cache_read_input_tokens"));
    OptimizationCacheObservationV1 {
        kind: "provider_prompt".into(),
        eligible: true,
        result: match read {
            Some(0) => CacheResultV1::Miss,
            Some(_) => CacheResultV1::Hit,
            None => CacheResultV1::Unknown,
        },
        temperature: match read {
            Some(0) => CacheTemperatureV1::Cold,
            Some(_) => CacheTemperatureV1::Warm,
            None => CacheTemperatureV1::Unknown,
        },
        invalidation_id: None,
        bytes_reused: None,
        tokens_reused: read.map(Into::into),
        lookup_ms: None,
        warmup_ms: None,
    }
}

fn runtime_spans(adapter: &str, id: &str, start: u64, end: u64) -> Vec<OptimizationSpanV1> {
    [
        OptimizationSpanKindV1::EndToEnd,
        OptimizationSpanKindV1::Active,
    ]
    .into_iter()
    .map(|kind| {
        let body = serde_json::json!({
            "adapter": adapter,
            "execution": id,
            "kind": kind,
            "start": start.to_string(),
            "end": end.to_string(),
        });
        OptimizationSpanV1 {
            span_id: content_id(&body).expect("span identity is canonical JSON"),
            kind,
            start_unix_ms: start.into(),
            end_unix_ms: end.max(start).into(),
            clock: "host".into(),
            status: MeasurementStatusV1::Exact,
        }
    })
    .collect()
}

fn missing_for_tokens(tokens: Option<&OptimizationTokenObservationV1>) -> BTreeSet<String> {
    let mut missing = BTreeSet::new();
    let Some(tokens) = tokens else {
        missing.insert("token_usage".into());
        return missing;
    };
    for (name, value) in [
        ("input_tokens", tokens.usage.input_tokens),
        ("output_tokens", tokens.usage.output_tokens),
        ("cache_read_tokens", tokens.usage.cache_read_tokens),
        ("cache_write_tokens", tokens.usage.cache_write_tokens),
        ("reasoning_tokens", tokens.usage.reasoning_tokens),
        ("context_tokens", tokens.context_tokens),
        ("retrieval_tokens", tokens.retrieval_tokens),
        ("repeated_context_tokens", tokens.repeated_context_tokens),
    ] {
        if value.is_none() {
            missing.insert(name.into());
        }
    }
    if tokens.status != MeasurementStatusV1::Exact {
        missing.insert("token_usage_non_exact".into());
    }
    missing
}

fn counter(value: &Value, key: &str) -> Option<u128> {
    value.get(key).and_then(decimal_u128)
}

fn decimal_u128(value: &Value) -> Option<u128> {
    value
        .as_u64()
        .map(u128::from)
        .or_else(|| value.as_str()?.parse().ok())
}

fn decimal_u64(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

fn string_at<'a>(value: &'a Value, paths: &[&str]) -> Option<&'a str> {
    paths.iter().find_map(|path| value.pointer(path)?.as_str())
}

fn bounded(value: &str, fallback: &str) -> String {
    if value.is_empty() || value.chars().any(char::is_control) {
        return fallback.into();
    }
    let mut output = String::new();
    for character in value.chars() {
        if output.len() + character.len_utf8() > 256 {
            break;
        }
        output.push(character);
    }
    output
}

// Minimal strict UTC RFC3339 parser for provider records. Offsets are deliberately refused:
// provider logs used by these adapters emit UTC, and guessing a local offset would corrupt a
// collection window.
fn parse_rfc3339_millis(value: &str) -> Option<u64> {
    let value = value.strip_suffix('Z')?;
    let (date, time) = value.split_once('T')?;
    let mut date = date.split('-').map(|part| part.parse::<i64>().ok());
    let (year, month, day) = (date.next()??, date.next()??, date.next()??);
    if date.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (clock, fraction) = time.split_once('.').map_or((time, ""), |parts| parts);
    let mut clock = clock.split(':').map(|part| part.parse::<u64>().ok());
    let (hour, minute, second) = (clock.next()??, clock.next()??, clock.next()??);
    if clock.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let millis = fraction.chars().take(3).collect::<String>();
    let millis = if millis.is_empty() {
        0
    } else {
        millis.parse::<u64>().ok()? * 10u64.pow(3u32.saturating_sub(millis.len() as u32))
    };
    // Howard Hinnant's civil-date transform, valid for the post-epoch dates in session logs.
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let yoe = adjusted_year - era * 400;
    let adjusted_month = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * adjusted_month + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let seconds = u64::try_from(days)
        .ok()?
        .checked_mul(86_400)?
        .checked_add(hour * 3_600 + minute * 60 + second)?;
    seconds.checked_mul(1_000)?.checked_add(millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(adapter: &str, root: &Path) -> AdapterState {
        AdapterState::new(
            adapter,
            &format!("sha256:{}", "1".repeat(64)),
            "declared-session",
            root,
            2_000_000_000_000,
            false,
        )
    }

    #[test]
    fn real_inspection_shape_requires_attestation_and_reconciles_cumulative_attempts() {
        let dir = tempfile::tempdir().unwrap();
        let mut adapter = state("af", dir.path());
        let receipt = serde_json::json!({"schema":"af/task-inspection@11","task_id":"declared-session","chargeable_tokens":"17",
            "history":[{"transition":{"now_unix_ms":1,"change":{"kind":"opened"}}},
                {"transition":{"now_unix_ms":2,"change":{"kind":"execution_recorded","record_id":"reserved"}}},
                {"transition":{"now_unix_ms":3,"change":{"kind":"execution_recorded","record_id":"started"}}},
                {"transition":{"now_unix_ms":8,"change":{"kind":"execution_recorded","record_id":"settled"}}},
                {"transition":{"now_unix_ms":10,"change":{"kind":"finished"}}}],
            "execution_records":[
                {"artifact_id":"reserved","record":{"kind":"reserved","attempt_id":"a"}},
                {"artifact_id":"started","record":{"kind":"started","attempt_id":"a"}},
                {"record":{"kind":"usage_observed","attempt_id":"a","charged_tokens":"4"}},
                {"artifact_id":"settled","record":{"kind":"settled","attempt_id":"a","charged_tokens":"12"}},
                {"record":{"kind":"settled","attempt_id":"b","charged_tokens":"5"}}],
            "result":{"execution":"completed","acceptance":"satisfied","domain_conclusion":"verified",
                "outputs":{"verification":{"artifact_ids":[format!("sha256:{}","2".repeat(64))]}}}});
        assert!(
            parse_af_inspection(&mut adapter, &receipt)
                .unwrap()
                .is_empty()
        );
        adapter.attest_project = true;
        let rows = parse_af_inspection(&mut adapter, &receipt).unwrap();
        assert_eq!(
            rows.iter()
                .filter_map(|row| row.tokens.as_ref())
                .map(|tokens| tokens.usage.chargeable_tokens.get())
                .sum::<u128>(),
            17
        );
        assert!(rows[0].missing_fields.contains("project_identity_attested"));
        assert!(
            rows[0]
                .tokens
                .as_ref()
                .unwrap()
                .usage
                .input_tokens
                .is_none()
        );
        assert!(
            rows.iter()
                .flat_map(|row| &row.spans)
                .any(|span| span.kind == OptimizationSpanKindV1::Queue
                    && span.start_unix_ms.get() == 2
                    && span.end_unix_ms.get() == 3)
        );
        assert_eq!(
            rows.last().unwrap().outcome.as_ref().unwrap().outcome,
            OptimizationOutcomeV1::Verified
        );
        let mut foreign = receipt.clone();
        foreign["task_id"] = serde_json::json!("other-task");
        assert!(parse_af_inspection(&mut adapter, &foreign).is_err());
        for earlier in ["af/task-inspection@3", "af/task-inspection@10"] {
            let mut old = receipt.clone();
            old["schema"] = serde_json::json!(earlier);
            assert_eq!(
                parse_af_inspection(&mut adapter, &old).unwrap_err(),
                "unsupported AF inspection generation"
            );
        }
        let mut corrupt = receipt;
        corrupt["chargeable_tokens"] = serde_json::json!("16");
        assert!(parse_af_inspection(&mut adapter, &corrupt).is_err());
    }

    #[test]
    fn experimental_inspection_counts_children_failed_arms_and_updates_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut adapter = state("af", dir.path());
        adapter.attest_project = true;
        let exact = |byte: char| format!("sha256:{}", byte.to_string().repeat(64));
        let receipt = serde_json::json!({
            "schema":"af/task-inspection@11",
            "task_id":"declared-session",
            "chargeable_tokens":"19",
            "history":[],
            "experiments":[{
                "kind":"prepared",
                "artifact_id":exact('1'),
                "artifact_type":"af/ExperimentPrepared@1",
                "record":{
                    "schema":"af.experiment-prepared/1",
                    "task_revision_id":exact('2'),
                    "outer_plan_id":exact('3'),
                    "slot_id":exact('4'),
                    "specification_id":exact('5'),
                    "compiled_child_plan_id":exact('6'),
                    "policy_id":exact('7'),
                    "spent_accounting_prefix_id":exact('8'),
                    "writer_epoch":1,
                    "children":[{
                        "node":"root.trial.baseline",
                        "arm":"baseline",
                        "case_id":exact('9'),
                        "repetition":1,
                        "task_kind":"trial",
                        "package":"trial/baseline",
                        "worker_package_id":exact('a'),
                        "effort":"command",
                        "effects":[],
                        "source_snapshot_id":exact('b'),
                        "requirements_id":exact('c'),
                        "authority_id":exact('d'),
                        "invocation_id":exact('e'),
                        "allowance":{"tokens":7,"attempts":1,"wall_ms":100}
                    }]
                }
            }],
            "execution_records":[
                {"record":{"kind":"usage_observed","attempt_id":"baseline","charged_tokens":"7"}},
                {"record":{"kind":"settled","attempt_id":"baseline","charged_tokens":"7"}},
                {"record":{"kind":"usage_observed","attempt_id":"candidate","charged_tokens":"9"}},
                {"record":{"kind":"settled","attempt_id":"candidate","charged_tokens":"12"}}
            ],
            "result":{"execution":"exhausted","acceptance":"inconclusive","domain_conclusion":"experiment_failed"}
        });
        let rows = parse_af_inspection(&mut adapter, &receipt).unwrap();
        let charges = rows
            .iter()
            .filter_map(|row| row.tokens.as_ref())
            .map(|tokens| tokens.usage.chargeable_tokens.get())
            .collect::<Vec<_>>();
        assert_eq!(charges, vec![7, 12]);
        assert_eq!(charges.into_iter().sum::<u128>(), 19);
        let outcomes = rows
            .iter()
            .filter_map(|row| row.outcome.as_ref())
            .collect::<Vec<_>>();
        assert!(!outcomes.is_empty());
        assert!(
            outcomes
                .iter()
                .all(|outcome| outcome.outcome != OptimizationOutcomeV1::Verified)
        );
    }

    #[test]
    fn only_the_external_adapter_reads_normalized_records() {
        let dir = tempfile::tempdir().unwrap();
        let mut external = state("external", dir.path());
        let line = serde_json::to_vec(&NormalizedRecord {
            observed_unix_ms: 1.into(),
            attribution: external.attribution("fixture", None),
            tokens: None,
            spans: Vec::new(),
            caches: Vec::new(),
            outcome: None,
            missing_fields: BTreeSet::new(),
        })
        .unwrap();
        assert_eq!(parse_adapter_record(&mut external, &line).unwrap().len(), 1);

        let mut af = state("af", dir.path());
        let error = parse_adapter_record(&mut af, &line).unwrap_err();
        assert!(error.contains("af/task-inspection"), "{error}");

        let mut codex = state("codex", dir.path());
        assert!(parse_adapter_record(&mut codex, &line).unwrap().is_empty());
        assert!(codex.gaps.contains("project_identity_unknown"));

        let mut claude = state("claude", dir.path());
        assert!(parse_adapter_record(&mut claude, &line).unwrap().is_empty());
    }

    #[test]
    fn non_usage_claude_metadata_does_not_create_a_missing_project_gap() {
        let dir = tempfile::tempdir().unwrap();
        let mut adapter = state("claude", dir.path());
        assert!(
            parse_adapter_record(
                &mut adapter,
                br#"{"type":"queue-operation","operation":"enqueue"}"#
            )
            .unwrap()
            .is_empty()
        );
        assert!(adapter.gaps.is_empty());
    }

    #[test]
    fn absent_native_components_remain_unknown() {
        let usage = usage_from_codex(
            &serde_json::json!({"input_tokens":10,"output_tokens":2}),
            "session",
        );
        assert_eq!(usage.usage.cache_read_tokens, None);
        assert_eq!(usage.usage.cache_write_tokens, None);
        assert_eq!(usage.usage.reasoning_tokens, None);
        assert_eq!(usage.status, MeasurementStatusV1::LowerBound);
        let usage = usage_from_claude(
            &serde_json::json!({"input_tokens":10,"output_tokens":2}),
            "message",
        );
        assert_eq!(usage.usage.cache_write_tokens, None);
        assert_eq!(usage.status, MeasurementStatusV1::LowerBound);
    }

    #[test]
    fn rfc3339_parser_is_exact_for_epoch_and_millis() {
        assert_eq!(parse_rfc3339_millis("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_millis("1970-01-01T00:00:01.250Z"),
            Some(1_250)
        );
    }

    #[test]
    fn native_codex_filters_foreign_and_reasoning_and_keeps_cumulative_usage() {
        let dir = tempfile::tempdir().unwrap();
        let mut adapter = state("codex", dir.path());
        let meta = serde_json::json!({"type":"session_meta","timestamp_unix_ms":"1",
            "payload":{"id":"session","cwd":dir.path()}});
        assert!(
            parse_adapter_record(&mut adapter, meta.to_string().as_bytes())
                .unwrap()
                .is_empty()
        );
        let reasoning = serde_json::json!({"type":"response_item","payload":{"type":"reasoning","text":"secret"}});
        assert!(
            parse_adapter_record(&mut adapter, reasoning.to_string().as_bytes())
                .unwrap()
                .is_empty()
        );
        let usage = serde_json::json!({"type":"event_msg","timestamp_unix_ms":"2","payload":{"type":"token_count","info":{"total_token_usage":{
            "input_tokens":100,"cached_input_tokens":40,"output_tokens":5,"reasoning_output_tokens":3}}}});
        let records = parse_adapter_record(&mut adapter, usage.to_string().as_bytes()).unwrap();
        assert_eq!(records.len(), 1);
        let tokens = records[0].tokens.as_ref().unwrap();
        assert_eq!(tokens.usage.chargeable_tokens.get(), 65);
        assert!(tokens.outer_session);
        assert!(!serde_json::to_string(&records).unwrap().contains("secret"));

        let mut foreign = state("codex", dir.path());
        let meta = serde_json::json!({"type":"session_meta","payload":{"id":"other","cwd":"/definitely/foreign"}});
        parse_adapter_record(&mut foreign, meta.to_string().as_bytes()).unwrap();
        assert!(
            parse_adapter_record(&mut foreign, usage.to_string().as_bytes())
                .unwrap()
                .is_empty()
        );
        assert!(foreign.gaps.contains("excluded_foreign_project"));
    }

    #[test]
    fn native_claude_keeps_allowlisted_usage_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut adapter = state("claude", dir.path());
        let value = serde_json::json!({"type":"result","sessionId":"s","cwd":dir.path(),
            "timestamp_unix_ms":"100","duration_ms":"20","is_error":false,
            "result":"visible but excluded","private_reasoning":"excluded",
            "usage":{"input_tokens":10,"output_tokens":2,"cache_creation_input_tokens":3,"cache_read_input_tokens":7}});
        let record = parse_adapter_record(&mut adapter, value.to_string().as_bytes())
            .unwrap()
            .remove(0);
        assert_eq!(
            record
                .tokens
                .as_ref()
                .unwrap()
                .usage
                .chargeable_tokens
                .get(),
            15
        );
        let encoded = serde_json::to_string(&record).unwrap();
        assert!(!encoded.contains("visible") && !encoded.contains("private"));
    }
}
