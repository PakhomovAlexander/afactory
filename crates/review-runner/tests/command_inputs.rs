use review_core::{Arg, Command};
use review_runner::{ReviewerAdapter, ReviewerInputs};
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
