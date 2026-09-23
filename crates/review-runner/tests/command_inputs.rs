use review_runner::{
    CommandAdapter, MAX_PRIOR_FINDINGS_BYTES, ReviewerAdapter, ReviewerInputs, RunnerError,
};

#[test]
fn a_command_refuses_oversized_refusal_history_before_it_renders() {
    let inputs = ReviewerInputs {
        refused_attempts: vec!["x".repeat(MAX_PRIOR_FINDINGS_BYTES)],
        ..ReviewerInputs::default()
    };

    let error = CommandAdapter.render_input(&inputs).unwrap_err();
    assert!(matches!(error, RunnerError::Refused(_)));
    assert!(error.to_string().contains("refused attempt history"));
}

/// A command Worker's rendered input is the typed document written to its stdin, refusal
/// history included even when the Attempt has no other input.
#[test]
fn a_command_renders_exactly_the_document_it_would_receive() {
    let inputs = ReviewerInputs {
        refused_attempts: vec!["retry-feedback".into()],
        ..ReviewerInputs::default()
    };
    let rendered = CommandAdapter.render_input(&inputs).unwrap();
    assert_eq!(rendered.transport, review_runner::InputTransport::Json);
    assert_eq!(rendered.bytes, serde_json::to_vec(&inputs).unwrap());
    assert!(
        String::from_utf8(rendered.bytes.clone())
            .unwrap()
            .contains("retry-feedback")
    );
    assert_eq!(
        rendered.manifest.rendered_bytes,
        rendered.bytes.len() as u64
    );
    assert_eq!(rendered.manifest.entries[0].name, "worker_input");
}
