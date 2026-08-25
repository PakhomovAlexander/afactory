use review_core::{Arg, Command};
use review_runner::{MAX_PRIOR_FINDINGS_BYTES, ReviewerAdapter, ReviewerInputs, RunnerError};
use review_store::Cas;

const EMPTY_RESULT: &str =
    r#"{"verdict":"approve","summary":null,"findings":[],"benchmark_demands":[],"disputes":[]}"#;

#[test]
fn a_command_retry_receives_refusal_history_even_without_other_inputs() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let command = Command::new(
        "/bin/sh",
        vec![
            Arg::literal("-c"),
            Arg::literal(format!(
                "input=$(cat); case \"$input\" in *retry-feedback*) ;; *) exit 9 ;; esac; printf '%s\\n' '{EMPTY_RESULT}'"
            )),
        ],
    );
    let inputs = ReviewerInputs {
        refused_attempts: vec!["retry-feedback".into()],
        ..ReviewerInputs::default()
    };

    let returned = ReviewerAdapter::invoke(&command, &cas, directory.path(), &inputs).unwrap();
    assert!(returned.output.findings.is_empty());
}

#[test]
fn a_command_retry_refuses_oversized_refusal_history_before_dispatch() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let command = Command::new("/bin/sh", vec![Arg::literal("-c"), Arg::literal("exit 99")]);
    let inputs = ReviewerInputs {
        refused_attempts: vec!["x".repeat(MAX_PRIOR_FINDINGS_BYTES)],
        ..ReviewerInputs::default()
    };

    let error = match ReviewerAdapter::invoke(&command, &cas, directory.path(), &inputs) {
        Ok(_) => panic!("oversized refusal history was dispatched"),
        Err(error) => error,
    };
    assert!(matches!(error, RunnerError::Refused(_)));
    assert!(error.to_string().contains("refused attempt history"));
}
