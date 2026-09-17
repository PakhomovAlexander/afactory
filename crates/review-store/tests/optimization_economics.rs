use review_core::task::optimization::{OptimizationHistoryV1, OptimizationSpanKindV1};
use review_store::optimization::project_economics;
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    captures: Vec<Capture>,
    expected: Expected,
}
#[derive(Deserialize)]
struct Capture {
    artifact_id: String,
    payload: OptimizationHistoryV1,
}
#[derive(Deserialize)]
struct Expected {
    af_chargeable_tokens: String,
    outer_chargeable_tokens: String,
    elapsed_ms: String,
    active_ms: String,
    summed_work_ms: String,
    repeated_failures: u32,
    missing_fields: Vec<String>,
}

#[test]
fn website_and_concurrent_work_fixture_has_independent_expected_totals() {
    let fixture: Fixture = serde_json::from_slice(include_bytes!(
        "../../../fixtures/self-optimizer/m1-economics.json"
    ))
    .unwrap();
    let captures: Vec<_> = fixture
        .captures
        .into_iter()
        .map(|capture| (capture.artifact_id, capture.payload))
        .collect();
    let actual = project_economics(&captures).unwrap();
    assert_eq!(
        actual.af_usage.chargeable_tokens.get().to_string(),
        fixture.expected.af_chargeable_tokens
    );
    assert_eq!(
        actual
            .outer_session_usage
            .chargeable_tokens
            .get()
            .to_string(),
        fixture.expected.outer_chargeable_tokens
    );
    assert_eq!(
        actual.elapsed_ms.get().to_string(),
        fixture.expected.elapsed_ms
    );
    assert_eq!(
        actual.active_ms.get().to_string(),
        fixture.expected.active_ms
    );
    assert_eq!(
        actual.summed_work_ms.get().to_string(),
        fixture.expected.summed_work_ms
    );
    assert_eq!(actual.repeated_failures, fixture.expected.repeated_failures);
    assert_eq!(
        actual.span_ms[&OptimizationSpanKindV1::Check].get(),
        40,
        "check receipts must be projected rather than inferred from field presence"
    );
    assert_eq!(
        actual.rows[0].span_ms[&OptimizationSpanKindV1::Active].get(),
        70
    );
    let build = &actual.cache_economics["build"];
    assert_eq!((build.hits, build.warm), (1, 1));
    assert_eq!(build.bytes_reused.unwrap().get(), 4096);
    assert_eq!(build.lookup_ms.unwrap().get(), 3);
    assert_eq!(
        actual.missing_fields.into_iter().collect::<Vec<_>>(),
        fixture.expected.missing_fields
    );
}

fn minimal_capture() -> (String, OptimizationHistoryV1) {
    let fixture: Fixture = serde_json::from_slice(include_bytes!(
        "../../../fixtures/self-optimizer/m1-economics.json"
    ))
    .unwrap();
    let mut capture = fixture.captures.into_iter().next().unwrap();
    capture.payload.observations.truncate(1);
    (capture.artifact_id, capture.payload)
}

#[test]
fn uncertain_tokens_are_not_promoted_to_exact_charges() {
    use review_core::task::optimization::MeasurementStatusV1;
    for (status, label) in [
        (MeasurementStatusV1::Estimated, "token_usage_estimated"),
        (MeasurementStatusV1::LowerBound, "token_usage_lower_bound"),
        (MeasurementStatusV1::Unknown, "token_usage_unknown"),
    ] {
        let (id, mut history) = minimal_capture();
        let mut uncertain = history.observations[0].clone();
        uncertain.observation_id = format!("sha256:{}", "9".repeat(64));
        uncertain.attribution.execution_id = "other".into();
        uncertain.tokens.as_mut().unwrap().status = status;
        history.observations.push(uncertain);
        let actual = project_economics(&[(id, history)]).unwrap();
        assert_eq!(actual.af_usage.chargeable_tokens.get(), 100);
        assert_eq!(actual.af_usage.input_tokens.unwrap().get(), 80);
        assert!(actual.missing_fields.contains(label));
    }
}

#[test]
fn rows_without_af_invocations_do_not_erase_native_components() {
    let (id, mut history) = minimal_capture();
    let mut metadata = history.observations[0].clone();
    metadata.observation_id = format!("sha256:{}", "9".repeat(64));
    metadata.attribution.execution_id = "metadata-only".into();
    metadata.tokens = None;
    history.observations.push(metadata);
    let actual = project_economics(&[(id, history)]).unwrap();
    assert_eq!(actual.af_usage.input_tokens.unwrap().get(), 80);
    assert_eq!(actual.af_usage.output_tokens.unwrap().get(), 20);
}

#[test]
fn repeated_failures_count_only_failed_occurrences_even_after_success() {
    use review_core::task::optimization::OptimizationOutcomeV1::{Failed, Verified};
    for (outcomes, expected) in [
        (vec![Verified, Failed], 0),
        (vec![Failed, Verified], 0),
        (vec![Failed, Failed, Verified], 1),
    ] {
        let (id, mut history) = minimal_capture();
        let template = history.observations[0].clone();
        history.observations = outcomes
            .into_iter()
            .enumerate()
            .map(|(index, outcome)| {
                let mut observation = template.clone();
                observation.observation_id = format!("sha256:{:064x}", index + 10);
                observation.outcome.as_mut().unwrap().outcome = outcome;
                observation
            })
            .collect();
        let actual = project_economics(&[(id, history)]).unwrap();
        assert_eq!(actual.repeated_failures, expected);
    }
}
