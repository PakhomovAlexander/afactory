//! Rebuildable project-economics projection over immutable History Captures.
//!
//! The reducer has no mutable cursor database. Callers discover captured artifacts from the
//! common Task log and replay them here. Exact observation IDs deduplicate overlapping imports;
//! distinct failures remain distinct occurrences across captures.

use std::collections::{BTreeMap, BTreeSet};

use review_core::task::optimization::*;
use review_core::task::usage::{DecimalU128, TaskTokenUsageV3};

#[derive(Default)]
struct Row {
    case_family: String,
    usage: BTreeMap<String, TaskTokenUsageV3>,
    outer_usage: BTreeMap<String, TaskTokenUsageV3>,
    context_tokens: BTreeMap<String, Option<DecimalU128>>,
    retrieval_tokens: BTreeMap<String, Option<DecimalU128>>,
    repeated_context_tokens: BTreeMap<String, Option<DecimalU128>>,
    spans: BTreeMap<String, OptimizationSpanV1>,
    outcome: Option<OptimizationOutcomeV1>,
    occurrences: u32,
    failure_occurrences: u32,
    missing: BTreeSet<String>,
}

fn retain_cumulative_measurement(
    values: &mut BTreeMap<String, Option<DecimalU128>>,
    key: &str,
    value: Option<DecimalU128>,
) {
    values
        .entry(key.into())
        .and_modify(|current| {
            *current = match (*current, value) {
                (Some(current), Some(value)) => Some(current.max(value)),
                _ => None,
            }
        })
        .or_insert(value);
}

fn summed_measurement(
    values: BTreeMap<String, Option<DecimalU128>>,
) -> Result<Option<DecimalU128>, String> {
    if values.is_empty() {
        return Ok(None);
    }
    let mut total = Some(0u128);
    for value in values.into_values() {
        total = match (total, value) {
            (Some(total), Some(value)) => Some(
                total
                    .checked_add(value.get())
                    .ok_or("Optimization measured token total overflow")?,
            ),
            _ => None,
        };
    }
    Ok(total.map(Into::into))
}

fn add_project_measurement(
    total: &mut Option<DecimalU128>,
    seen: &mut bool,
    value: Option<DecimalU128>,
) -> Result<(), String> {
    if !*seen {
        *total = value;
        *seen = true;
        return Ok(());
    }
    *total = match (*total, value) {
        (Some(total), Some(value)) => Some(
            total
                .get()
                .checked_add(value.get())
                .map(Into::into)
                .ok_or("Optimization project token volume overflow")?,
        ),
        _ => None,
    };
    Ok(())
}

struct CacheAggregate {
    value: OptimizationCacheEconomicsV1,
    bytes_reused: Option<u128>,
    tokens_reused: Option<u128>,
    lookup_ms: Option<u128>,
    warmup_ms: Option<u128>,
}

impl Default for CacheAggregate {
    fn default() -> Self {
        Self {
            value: OptimizationCacheEconomicsV1::default(),
            bytes_reused: Some(0),
            tokens_reused: Some(0),
            lookup_ms: Some(0),
            warmup_ms: Some(0),
        }
    }
}

fn add_cache_measurement(total: &mut Option<u128>, value: Option<u128>) -> Result<(), String> {
    *total = match (*total, value) {
        (Some(total), Some(value)) => Some(
            total
                .checked_add(value)
                .ok_or("Optimization cache total overflow")?,
        ),
        _ => None,
    };
    Ok(())
}

fn max_usage(target: &mut TaskTokenUsageV3, value: &TaskTokenUsageV3) {
    target.input_tokens = target.input_tokens.max(value.input_tokens);
    target.output_tokens = target.output_tokens.max(value.output_tokens);
    target.cache_read_tokens = target.cache_read_tokens.max(value.cache_read_tokens);
    target.cache_write_tokens = target.cache_write_tokens.max(value.cache_write_tokens);
    target.reasoning_tokens = target.reasoning_tokens.max(value.reasoning_tokens);
    target.chargeable_tokens = target.chargeable_tokens.max(value.chargeable_tokens);
}

fn add_usage(target: &mut TaskTokenUsageV3, value: &TaskTokenUsageV3) -> Result<(), String> {
    let add = |left: Option<DecimalU128>, right: Option<DecimalU128>| -> Result<_, String> {
        match (left, right) {
            (Some(left), Some(right)) => left
                .get()
                .checked_add(right.get())
                .map(DecimalU128::from)
                .map(Some)
                .ok_or_else(|| "Optimization token total overflow".to_owned()),
            // A native component is unknown unless every contributing invocation reported it.
            _ => Ok(None),
        }
    };
    target.input_tokens = add(target.input_tokens, value.input_tokens)?;
    target.output_tokens = add(target.output_tokens, value.output_tokens)?;
    target.cache_read_tokens = add(target.cache_read_tokens, value.cache_read_tokens)?;
    target.cache_write_tokens = add(target.cache_write_tokens, value.cache_write_tokens)?;
    target.reasoning_tokens = add(target.reasoning_tokens, value.reasoning_tokens)?;
    target.chargeable_tokens = target
        .chargeable_tokens
        .get()
        .checked_add(value.chargeable_tokens.get())
        .ok_or("Optimization charge total overflow")?
        .into();
    Ok(())
}

fn summed_usage(values: BTreeMap<String, TaskTokenUsageV3>) -> Result<TaskTokenUsageV3, String> {
    let mut values = values.into_values();
    let Some(mut total) = values.next() else {
        return Ok(TaskTokenUsageV3::default());
    };
    for value in values {
        add_usage(&mut total, &value)?;
    }
    Ok(total)
}

fn interval_union(spans: impl Iterator<Item = (u64, u64)>) -> Result<u128, String> {
    let mut spans: Vec<_> = spans.collect();
    spans.sort_unstable();
    let mut total = 0u128;
    let mut current: Option<(u64, u64)> = None;
    for (start, end) in spans {
        let Some((old_start, old_end)) = current else {
            current = Some((start, end));
            continue;
        };
        if start <= old_end {
            current = Some((old_start, old_end.max(end)));
        } else {
            total = total
                .checked_add(u128::from(old_end - old_start))
                .ok_or("Optimization duration overflow")?;
            current = Some((start, end));
        }
    }
    if let Some((start, end)) = current {
        total = total
            .checked_add(u128::from(end - start))
            .ok_or("Optimization duration overflow")?;
    }
    Ok(total)
}

fn span_totals(spans: &BTreeMap<String, OptimizationSpanV1>) -> Result<(u128, u128, u128), String> {
    let exact = |span: &&OptimizationSpanV1| span.status == MeasurementStatusV1::Exact;
    let active = interval_union(
        spans
            .values()
            .filter(exact)
            .filter(|span| span.kind == OptimizationSpanKindV1::Active)
            .map(|span| (span.start_unix_ms.get(), span.end_unix_ms.get())),
    )?;
    let elapsed = interval_union(
        spans
            .values()
            .filter(exact)
            .filter(|span| span.kind == OptimizationSpanKindV1::EndToEnd)
            .map(|span| (span.start_unix_ms.get(), span.end_unix_ms.get())),
    )?;
    let mut summed = 0u128;
    for span in spans.values().filter(exact).filter(|span| {
        !matches!(
            span.kind,
            OptimizationSpanKindV1::EndToEnd
                | OptimizationSpanKindV1::Queue
                | OptimizationSpanKindV1::ApprovalWaiting
                | OptimizationSpanKindV1::UserWaiting
        )
    }) {
        summed = summed
            .checked_add(u128::from(
                span.end_unix_ms.get() - span.start_unix_ms.get(),
            ))
            .ok_or("Optimization summed work overflow")?;
    }
    Ok((active, elapsed, summed))
}

fn span_kind_totals<'a>(
    spans: impl IntoIterator<Item = &'a OptimizationSpanV1>,
) -> Result<BTreeMap<OptimizationSpanKindV1, DecimalU128>, String> {
    let mut grouped: BTreeMap<OptimizationSpanKindV1, Vec<(u64, u64)>> = BTreeMap::new();
    for span in spans {
        if span.status == MeasurementStatusV1::Exact {
            grouped
                .entry(span.kind)
                .or_default()
                .push((span.start_unix_ms.get(), span.end_unix_ms.get()));
        }
    }
    grouped
        .into_iter()
        .map(|(kind, spans)| Ok((kind, interval_union(spans.into_iter())?.into())))
        .collect()
}

fn cache_label(value: CacheResultV1) -> &'static str {
    match value {
        CacheResultV1::Hit => "hit",
        CacheResultV1::Miss => "miss",
        CacheResultV1::Unknown => "unknown",
    }
}

/// Reduce a complete retained capture chain. Inputs are `(artifact_id, payload)` pairs in chain
/// order. Replaying the same bytes produces the same projection and performs no inference.
pub fn project_economics(
    captures: &[(String, OptimizationHistoryV1)],
) -> Result<OptimizationEconomicsV1, String> {
    if captures.is_empty() {
        return Err("Optimization economics requires at least one History Capture".into());
    }
    let project_id = captures[0].1.project_id.clone();
    let mut expected_previous: Option<&str> = None;
    let mut capture_ids = Vec::with_capacity(captures.len());
    let mut seen_captures = BTreeSet::new();
    let mut seen_observations = BTreeSet::new();
    let mut seen_native_content = BTreeSet::new();
    let mut seen_receipts = BTreeMap::<String, OptimizationSourceReceiptV1>::new();
    let mut partial_sources = BTreeMap::<(String, String, String), u64>::new();
    let mut unscoped_partial_gaps = BTreeSet::new();
    let mut rows = BTreeMap::<String, Row>::new();
    let mut missing = BTreeSet::new();
    let mut exposed = BTreeSet::new();
    let mut cache_results = BTreeMap::<String, BTreeMap<String, u64>>::new();
    let mut cache_economics = BTreeMap::<String, CacheAggregate>::new();
    let mut cutoff = 0u64;

    for (capture_id, capture) in captures {
        capture.validate()?;
        if !review_core::is_digest(capture_id)
            || !seen_captures.insert(capture_id.clone())
            || capture.project_id != project_id
            || capture.previous_capture_id.as_deref() != expected_previous
        {
            return Err("Optimization captures are not one exact retained project chain".into());
        }
        expected_previous = Some(capture_id);
        capture_ids.push(capture_id.clone());
        cutoff = cutoff.max(capture.cutoff_unix_ms.get());
        // Resolve a partial gap only after every affected source for that adapter is
        // covered. A complete receipt for an unrelated source proves nothing about it.
        for gap in &capture.gaps {
            if let Some(adapter) = gap.strip_prefix("partial_") {
                if !capture.receipts.iter().any(|receipt| {
                    receipt.adapter == adapter
                        && receipt.completeness == SourceCompletenessV1::Partial
                }) {
                    unscoped_partial_gaps.insert(gap.clone());
                }
            } else {
                missing.insert(gap.clone());
            }
        }
        for receipt in &capture.receipts {
            let key = (
                receipt.adapter.clone(),
                receipt.source_id.clone(),
                receipt.execution_id.clone(),
            );
            if receipt.completeness == SourceCompletenessV1::Partial {
                partial_sources
                    .entry(key)
                    .and_modify(|end| *end = (*end).max(receipt.byte_end.get()))
                    .or_insert(receipt.byte_end.get());
            } else if receipt.completeness == SourceCompletenessV1::Complete
                && receipt.byte_start.get() == 0
                && partial_sources
                    .get(&key)
                    .is_some_and(|end| receipt.byte_end.get() >= *end)
            {
                partial_sources.remove(&key);
            }
        }
        exposed.extend(capture.exposed_case_families.iter().cloned());
        for receipt in &capture.receipts {
            match seen_receipts.get(&receipt.receipt_id) {
                Some(previous) if previous != receipt => {
                    return Err("One source receipt identity has conflicting bytes".into());
                }
                _ => {
                    seen_receipts.insert(receipt.receipt_id.clone(), receipt.clone());
                }
            }
        }
        for observation in &capture.observations {
            if !seen_observations.insert(observation.observation_id.clone()) {
                continue;
            }
            // Native snapshots may be exported under new source labels or carry legacy
            // source-dependent IDs. Deduplicate their execution evidence without rewriting
            // retained artifact IDs. Normalized fixtures keep occurrence/range semantics.
            if let Some(receipt) = seen_receipts.get(&observation.source_receipt_id)
                && receipt.adapter_version == "native-v1"
            {
                let mut body =
                    serde_json::to_value(observation).map_err(|error| error.to_string())?;
                let fields = body
                    .as_object_mut()
                    .ok_or("Native observation must be an object")?;
                fields.remove("observation_id");
                fields.remove("source_receipt_id");
                let identity = crate::content_id(
                    &serde_json::json!({"adapter":receipt.adapter,"record":body}),
                )
                .map_err(|error| error.to_string())?;
                if !seen_native_content.insert(identity) {
                    continue;
                }
            }
            let row = rows
                .entry(observation.attribution.execution_id.clone())
                .or_default();
            if row.case_family.is_empty() {
                row.case_family = observation.attribution.case_family.clone();
            } else if row.case_family != observation.attribution.case_family {
                return Err("One execution was assigned to conflicting case families".into());
            }
            row.missing
                .extend(observation.missing_fields.iter().cloned());
            for span in &observation.spans {
                match row.spans.get(&span.span_id) {
                    Some(previous) if previous != span => {
                        return Err("One span identity has conflicting observations".into());
                    }
                    _ => {
                        row.spans.insert(span.span_id.clone(), span.clone());
                    }
                }
            }
            if let Some(tokens) = &observation.tokens {
                if tokens.status == MeasurementStatusV1::Exact {
                    let target = if tokens.outer_session {
                        &mut row.outer_usage
                    } else {
                        &mut row.usage
                    };
                    max_usage(
                        target.entry(tokens.cumulative_key.clone()).or_default(),
                        &tokens.usage,
                    );
                    for (name, value, values) in [
                        (
                            "context_tokens",
                            tokens.context_tokens,
                            &mut row.context_tokens,
                        ),
                        (
                            "retrieval_tokens",
                            tokens.retrieval_tokens,
                            &mut row.retrieval_tokens,
                        ),
                        (
                            "repeated_context_tokens",
                            tokens.repeated_context_tokens,
                            &mut row.repeated_context_tokens,
                        ),
                    ] {
                        retain_cumulative_measurement(values, &tokens.cumulative_key, value);
                        if value.is_none() {
                            row.missing.insert(name.into());
                        }
                    }
                } else {
                    row.missing.insert(
                        match tokens.status {
                            MeasurementStatusV1::Estimated => "token_usage_estimated",
                            MeasurementStatusV1::LowerBound => "token_usage_lower_bound",
                            _ => "token_usage_unknown",
                        }
                        .into(),
                    );
                }
            }

            for cache in &observation.caches {
                *cache_results
                    .entry(cache.kind.clone())
                    .or_default()
                    .entry(cache_label(cache.result).into())
                    .or_default() += 1;
                let aggregate = cache_economics.entry(cache.kind.clone()).or_default();
                if cache.eligible {
                    aggregate.value.eligible = aggregate.value.eligible.saturating_add(1);
                } else {
                    aggregate.value.ineligible = aggregate.value.ineligible.saturating_add(1);
                }
                match cache.result {
                    CacheResultV1::Hit => {
                        aggregate.value.hits = aggregate.value.hits.saturating_add(1)
                    }
                    CacheResultV1::Miss => {
                        aggregate.value.misses = aggregate.value.misses.saturating_add(1)
                    }
                    CacheResultV1::Unknown => {
                        aggregate.value.unknown_results =
                            aggregate.value.unknown_results.saturating_add(1)
                    }
                }
                match cache.temperature {
                    CacheTemperatureV1::Cold => {
                        aggregate.value.cold = aggregate.value.cold.saturating_add(1)
                    }
                    CacheTemperatureV1::Warm => {
                        aggregate.value.warm = aggregate.value.warm.saturating_add(1)
                    }
                    CacheTemperatureV1::Unknown => {
                        aggregate.value.unknown_temperature =
                            aggregate.value.unknown_temperature.saturating_add(1)
                    }
                }
                if let Some(id) = &cache.invalidation_id {
                    aggregate.value.invalidation_ids.insert(id.clone());
                } else {
                    aggregate
                        .value
                        .missing_fields
                        .insert("invalidation_identity".into());
                }
                for (name, present) in [
                    ("bytes_reused", cache.bytes_reused.is_some()),
                    ("tokens_reused", cache.tokens_reused.is_some()),
                    ("lookup_time", cache.lookup_ms.is_some()),
                    ("warmup_time", cache.warmup_ms.is_some()),
                ] {
                    if !present {
                        aggregate.value.missing_fields.insert(name.into());
                    }
                }
                add_cache_measurement(
                    &mut aggregate.bytes_reused,
                    cache.bytes_reused.map(DecimalU128::get),
                )?;
                add_cache_measurement(
                    &mut aggregate.tokens_reused,
                    cache.tokens_reused.map(DecimalU128::get),
                )?;
                add_cache_measurement(
                    &mut aggregate.lookup_ms,
                    cache.lookup_ms.map(|value| u128::from(value.get())),
                )?;
                add_cache_measurement(
                    &mut aggregate.warmup_ms,
                    cache.warmup_ms.map(|value| u128::from(value.get())),
                )?;
            }
            if let Some(outcome) = &observation.outcome {
                row.occurrences = row.occurrences.saturating_add(1);
                if outcome.outcome != OptimizationOutcomeV1::Verified {
                    row.failure_occurrences = row.failure_occurrences.saturating_add(1);
                }
                // Captures and records are folded in their recorded order.
                row.outcome = Some(outcome.outcome);
            }
        }
    }

    missing.extend(unscoped_partial_gaps);
    missing.extend(
        partial_sources
            .keys()
            .map(|(adapter, _, _)| format!("partial_{adapter}")),
    );

    // Project elapsed is the union on the shared host clock across every execution. Summing the
    // already-unioned rows would turn concurrency into fictitious wall time.
    let project_elapsed = interval_union(rows.values().flat_map(|row| {
        row.spans
            .values()
            .filter(|span| {
                span.status == MeasurementStatusV1::Exact
                    && span.kind == OptimizationSpanKindV1::EndToEnd
            })
            .map(|span| (span.start_unix_ms.get(), span.end_unix_ms.get()))
    }))?;
    let project_span_ms = span_kind_totals(rows.values().flat_map(|row| row.spans.values()))?;
    let mut result_rows = Vec::with_capacity(rows.len());
    let mut af_usage = TaskTokenUsageV3::default();
    let mut outer_usage = TaskTokenUsageV3::default();
    let mut total_active = 0u128;
    let mut total_summed = 0u128;
    let mut verified = 0u32;
    let mut failed = 0u32;
    let mut repeated = 0u32;
    let mut any_af = false;
    let mut any_outer = false;
    let mut context_tokens = None;
    let mut retrieval_tokens = None;
    let mut repeated_context_tokens = None;
    let mut saw_context = false;
    let mut saw_retrieval = false;
    let mut saw_repeated_context = false;
    for (execution_id, mut row) in rows {
        if row.usage.is_empty() && row.outer_usage.is_empty() {
            row.missing.insert("token_usage".into());
        }
        if row.outcome.is_none() {
            row.missing.insert("outcome".into());
        }
        let has_af = !row.usage.is_empty();
        let has_outer = !row.outer_usage.is_empty();
        let row_af = summed_usage(std::mem::take(&mut row.usage))?;
        let row_outer = summed_usage(std::mem::take(&mut row.outer_usage))?;
        let row_context = summed_measurement(std::mem::take(&mut row.context_tokens))?;
        let row_retrieval = summed_measurement(std::mem::take(&mut row.retrieval_tokens))?;
        let row_repeated_context =
            summed_measurement(std::mem::take(&mut row.repeated_context_tokens))?;
        let (active, elapsed, summed) = span_totals(&row.spans)?;
        let span_ms = span_kind_totals(row.spans.values())?;
        let outcome = row.outcome.unwrap_or(OptimizationOutcomeV1::Incomplete);
        match outcome {
            OptimizationOutcomeV1::Verified => verified = verified.saturating_add(1),
            _ => {
                failed = failed.saturating_add(1);
            }
        }
        repeated = repeated.saturating_add(row.failure_occurrences.saturating_sub(1));
        if !row
            .spans
            .values()
            .any(|span| span.kind == OptimizationSpanKindV1::EndToEnd)
        {
            row.missing.insert("elapsed_time".into());
        }
        missing.extend(row.missing.iter().cloned());
        if has_af && !any_af {
            af_usage = row_af.clone();
            any_af = true;
        } else if has_af {
            add_usage(&mut af_usage, &row_af)?;
        }
        if has_outer && !any_outer {
            outer_usage = row_outer.clone();
            any_outer = true;
        } else if has_outer {
            add_usage(&mut outer_usage, &row_outer)?;
        }
        if has_af || has_outer {
            add_project_measurement(&mut context_tokens, &mut saw_context, row_context)?;
            add_project_measurement(&mut retrieval_tokens, &mut saw_retrieval, row_retrieval)?;
            add_project_measurement(
                &mut repeated_context_tokens,
                &mut saw_repeated_context,
                row_repeated_context,
            )?;
        }
        total_active = total_active
            .checked_add(active)
            .ok_or("Active time overflow")?;
        total_summed = total_summed
            .checked_add(summed)
            .ok_or("Work time overflow")?;
        result_rows.push(OptimizationEconomicsRowV1 {
            execution_id,
            case_family: row.case_family,
            af_usage: row_af,
            outer_session_usage: row_outer,
            active_ms: active.into(),
            elapsed_ms: elapsed.into(),
            summed_work_ms: summed.into(),
            span_ms,
            context_tokens: row_context,
            retrieval_tokens: row_retrieval,
            repeated_context_tokens: row_repeated_context,
            outcome,
            occurrences: row.occurrences,
            missing_fields: row.missing,
        });
    }
    let cache_economics = cache_economics
        .into_iter()
        .map(|(kind, mut aggregate)| {
            aggregate.value.bytes_reused = aggregate.bytes_reused.map(Into::into);
            aggregate.value.tokens_reused = aggregate.tokens_reused.map(Into::into);
            aggregate.value.lookup_ms = aggregate.lookup_ms.map(Into::into);
            aggregate.value.warmup_ms = aggregate.warmup_ms.map(Into::into);
            missing.extend(
                aggregate
                    .value
                    .missing_fields
                    .iter()
                    .map(|field| format!("cache_{field}")),
            );
            (kind, aggregate.value)
        })
        .collect();
    let result = OptimizationEconomicsV1 {
        schema: "af.optimization-economics/1".into(),
        project_id,
        capture_ids,
        cutoff_unix_ms: cutoff.into(),
        rows: result_rows,
        af_usage,
        outer_session_usage: outer_usage,
        active_ms: total_active.into(),
        elapsed_ms: project_elapsed.into(),
        summed_work_ms: total_summed.into(),
        span_ms: project_span_ms,
        context_tokens,
        retrieval_tokens,
        repeated_context_tokens,
        verified,
        failed_or_incomplete: failed,
        repeated_failures: repeated,
        cache_results,
        cache_economics,
        missing_fields: missing,
        exposed_case_families: exposed,
    };
    result.validate()?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_core::task::usage::DecimalU64;

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }
    fn observation(
        id: char,
        execution: &str,
        charge: u128,
        start: u64,
        end: u64,
    ) -> OptimizationObservationV1 {
        OptimizationObservationV1 {
            observation_id: digest(id),
            source_receipt_id: digest('2'),
            attribution: OptimizationAttributionV1 {
                project_id: digest('1'),
                case_family: "harness".into(),
                execution_id: execution.into(),
                outer_execution_id: None,
                task_id: Some("task-1".into()),
                attempt_id: None,
                pipeline: None,
                node: None,
                worker: None,
                model: None,
                effort: None,
                configuration_id: None,
                environment_id: None,
            },
            tokens: Some(OptimizationTokenObservationV1 {
                cumulative_key: format!("invocation-{execution}"),
                usage: TaskTokenUsageV3 {
                    input_tokens: Some(charge.into()),
                    output_tokens: Some(0u128.into()),
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                    chargeable_tokens: charge.into(),
                },
                status: MeasurementStatusV1::Exact,
                context_tokens: None,
                retrieval_tokens: None,
                repeated_context_tokens: None,
                outer_session: false,
            }),
            spans: vec![OptimizationSpanV1 {
                span_id: digest(id.to_ascii_uppercase()),
                kind: OptimizationSpanKindV1::EndToEnd,
                start_unix_ms: start.into(),
                end_unix_ms: end.into(),
                clock: "host".into(),
                status: MeasurementStatusV1::Exact,
            }],
            caches: vec![],
            outcome: Some(OptimizationOutcomeObservationV1 {
                outcome: OptimizationOutcomeV1::Failed,
                requirements_id: None,
                verifier_id: None,
                retries: 0,
                repairs: 0,
                later_defects: 0,
            }),
            missing_fields: BTreeSet::new(),
        }
    }
    fn capture(
        previous: Option<String>,
        observations: Vec<OptimizationObservationV1>,
    ) -> OptimizationHistoryV1 {
        OptimizationHistoryV1 {
            schema: "af.optimization-history/1".into(),
            project_id: digest('1'),
            previous_capture_id: previous,
            cutoff_unix_ms: DecimalU64::from(100),
            receipts: vec![OptimizationSourceReceiptV1 {
                receipt_id: digest('2'),
                adapter: "fixture".into(),
                adapter_version: "v1".into(),
                project_id: digest('1'),
                source_id: "fixture.jsonl".into(),
                execution_id: "source".into(),
                byte_start: 0.into(),
                byte_end: 100.into(),
                prefix_digest: digest('3'),
                cutoff_unix_ms: 100.into(),
                redaction_version: "v1".into(),
                completeness: SourceCompletenessV1::Complete,
            }],
            observations,
            gaps: BTreeSet::new(),
            exposed_case_families: BTreeSet::from(["harness".into()]),
        }
    }

    #[test]
    fn cumulative_snapshots_and_overlapping_spans_are_not_summed() {
        let mut later = observation('5', "exec", 15, 5, 20);
        later.tokens.as_mut().unwrap().cumulative_key = "invocation-exec".into();
        let captures = vec![(
            digest('8'),
            capture(None, vec![observation('4', "exec", 10, 0, 10), later]),
        )];
        let result = project_economics(&captures).unwrap();
        assert_eq!(result.af_usage.chargeable_tokens.get(), 15);
        assert_eq!(result.elapsed_ms.get(), 20);
        assert_eq!(result.summed_work_ms.get(), 0);
        assert_eq!(result.repeated_failures, 1);
    }

    #[test]
    fn appended_capture_deduplicates_old_observations_but_retains_new_failures() {
        let first_id = digest('8');
        let first = observation('4', "exec", 10, 0, 10);
        let captures = vec![
            (first_id.clone(), capture(None, vec![first.clone()])),
            (
                digest('9'),
                capture(
                    Some(first_id),
                    vec![first, observation('5', "exec", 12, 10, 20)],
                ),
            ),
        ];
        let result = project_economics(&captures).unwrap();
        assert_eq!(result.af_usage.chargeable_tokens.get(), 12);
        assert_eq!(result.repeated_failures, 1);
        assert_eq!(result.capture_ids.len(), 2);
    }

    #[test]
    fn complete_zero_based_recapture_supersedes_a_covered_partial_gap() {
        let first_id = digest('8');
        let mut first = capture(None, vec![]);
        first.receipts[0].completeness = SourceCompletenessV1::Partial;
        first.receipts[0].byte_end = 50.into();
        first.gaps.insert("partial_fixture".into());
        let mut complete = capture(Some(first_id.clone()), vec![]);
        complete.receipts[0].receipt_id = digest('6');
        complete.receipts[0].prefix_digest = digest('7');
        complete.receipts[0].byte_start = 0.into();
        complete.receipts[0].byte_end = 100.into();
        complete.receipts[0].completeness = SourceCompletenessV1::Complete;
        let result = project_economics(&[(first_id, first), (digest('9'), complete)]).unwrap();
        assert!(!result.missing_fields.contains("partial_fixture"));
    }

    #[test]
    fn completing_one_source_cannot_hide_another_partial_source() {
        let first_id = digest('8');
        let mut first = capture(None, vec![]);
        first.receipts[0].completeness = SourceCompletenessV1::Partial;
        first.receipts[0].byte_end = 50.into();
        first.gaps.insert("partial_fixture".into());
        let mut other = first.receipts[0].clone();
        other.receipt_id = digest('5');
        other.source_id = "other-source".into();
        first.receipts.push(other);
        let mut second = capture(Some(first_id.clone()), vec![]);
        second.receipts[0].receipt_id = digest('6');
        second.receipts[0].prefix_digest = digest('7');
        second.receipts[0].byte_start = 0.into();
        second.receipts[0].byte_end = 100.into();
        second.receipts[0].completeness = SourceCompletenessV1::Complete;
        let result = project_economics(&[(first_id, first), (digest('9'), second)]).unwrap();
        assert!(result.missing_fields.contains("partial_fixture"));
    }

    #[test]
    fn native_replay_deduplicates_legacy_source_dependent_observation_ids() {
        let first_id = digest('8');
        let first_observation = observation('4', "exec", 10, 0, 10);
        let mut first = capture(None, vec![first_observation.clone()]);
        first.receipts[0].adapter_version = "native-v1".into();
        let mut duplicate = first_observation;
        duplicate.observation_id = digest('5');
        let mut second = capture(Some(first_id.clone()), vec![duplicate]);
        second.receipts[0].adapter_version = "native-v1".into();
        let result = project_economics(&[(first_id, first), (digest('9'), second)]).unwrap();
        assert_eq!(result.af_usage.chargeable_tokens.get(), 10);
        assert_eq!(result.repeated_failures, 0);
        assert_eq!(result.rows[0].occurrences, 1);
    }

    #[test]
    fn one_execution_without_context_leaves_the_project_context_unknown() {
        let measured = |id: char, execution: &str, context: Option<u128>| {
            let mut observation = observation(id, execution, 10, 0, 10);
            observation.tokens.as_mut().unwrap().context_tokens = context.map(Into::into);
            observation
        };
        let economics = |observations| {
            project_economics(&[(digest('8'), capture(None, observations))]).unwrap()
        };
        let both = economics(vec![
            measured('4', "first", Some(80)),
            measured('5', "second", Some(20)),
        ]);
        assert_eq!(both.context_tokens.unwrap().get(), 100);
        let one = economics(vec![
            measured('4', "first", Some(80)),
            measured('5', "second", None),
        ]);
        assert!(one.context_tokens.is_none());
        assert_eq!(
            one.rows
                .iter()
                .find(|row| row.execution_id == "first")
                .unwrap()
                .context_tokens
                .unwrap()
                .get(),
            80
        );
    }

    #[test]
    fn concurrent_executions_do_not_sum_overlapping_elapsed_time() {
        let captures = vec![(
            digest('8'),
            capture(
                None,
                vec![
                    observation('4', "first", 10, 0, 10),
                    observation('5', "second", 10, 5, 15),
                ],
            ),
        )];
        let result = project_economics(&captures).unwrap();
        assert_eq!(result.elapsed_ms.get(), 15);
        assert_eq!(
            result
                .rows
                .iter()
                .map(|row| row.elapsed_ms.get())
                .sum::<u128>(),
            20
        );
    }
}
