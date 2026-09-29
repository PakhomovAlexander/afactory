//! A resumed Attempt sends the delta prompt and nothing else: the forked session already holds
//! the package instructions and the Change Set the previous Attempt read, and re-sending them
//! would pay the prefix this layer exists to stop paying. What changed still arrives, and both
//! the transcript and the delta are listed in the Attempt's context manifest.

use review_runner::{
    NotesRequest, ReviewerInputArtifact, ReviewerInputs, SessionResume, compose_model_prompt,
};

const INSTRUCTIONS: &str = "You are the correctness reviewer. Read the whole change.";

fn digest(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn head_delta() -> serde_json::Value {
    serde_json::json!({
        "node": "correctness",
        "from_snapshot_id": digest('1'),
        "to_snapshot_id": digest('3'),
        "diff_policy_version": "review.kernel/git-tree-diff@test",
        "changed_paths": ["src/retry.rs"],
        "marks": [{"path": "src/retry.rs", "mark": "changed"}],
    })
}

fn change_set() -> ReviewerInputArtifact {
    let change_set = review_core::ChangeSetV1::new(
        digest('1'),
        digest('3'),
        vec!["src/retry.rs".to_string()],
        Vec::new(),
        b"diff --git a/src/retry.rs b/src/retry.rs\n--- a/src/retry.rs\n+++ b/src/retry.rs\n",
        "2.44.0",
        "review.kernel/git-diff@test",
    )
    .expect("a valid Change Set");
    let encoded = serde_json::to_vec(&change_set).expect("change set encodes");
    let artifact_id = review_store::canonical::blob_content_id(&encoded);
    ReviewerInputArtifact::change_set_from_encoded(artifact_id, &encoded)
        .expect("an admitted Change Set")
}

fn resumed() -> ReviewerInputs {
    ReviewerInputs {
        head_delta: Some(head_delta()),
        head_delta_artifact_id: Some(digest('5')),
        warm_set_artifact_id: Some(digest('6')),
        notes_request: Some(NotesRequest { max_bytes: 16384 }),
        session_id: review_core::session_id_for_attempt(&"b".repeat(26)),
        session_resume: Some(SessionResume {
            session_id: review_core::session_id_for_attempt(&"a".repeat(26)).unwrap(),
            artifact_id: digest('9'),
            transcript_bytes: 262_144,
            estimated_tokens: 65_536,
        }),
        ..ReviewerInputs::default()
    }
}

#[test]
fn a_resumed_attempt_sends_the_delta_and_not_the_prefix() {
    let mut cold = resumed();
    cold.session_resume = None;
    cold.artifacts
        .insert("change_set".into(), vec![change_set()]);
    let (cold_prompt, cold_manifest) = compose_model_prompt(INSTRUCTIONS, &cold).unwrap();
    assert!(cold_prompt.contains(INSTRUCTIONS));
    assert!(cold_prompt.contains("Canonical patch:"));

    let mut warm = resumed();
    warm.artifacts
        .insert("change_set".into(), vec![change_set()]);
    let (prompt, manifest) = compose_model_prompt(INSTRUCTIONS, &warm).unwrap();
    assert!(
        !prompt.contains(INSTRUCTIONS),
        "the fork already holds the package instructions"
    );
    assert!(
        !prompt.contains("Canonical patch:"),
        "the fork already read the Change Set; Delta Marking says what moved since"
    );
    assert!(prompt.contains("## Continuing your previous session (kernel data)"));
    assert!(prompt.contains("Delta Marking since the previous Round's head"));
    assert!(prompt.contains("## Notes for your next Attempt (optional output)"));
    assert!(
        prompt.contains("## Output contract"),
        "the output contract is restated in full: it is what the kernel parses"
    );
    assert!(
        prompt.len() < cold_prompt.len(),
        "a delta prompt of {} bytes is not smaller than the cold prompt of {} bytes",
        prompt.len(),
        cold_prompt.len()
    );
    assert!(cold_manifest.rendered_bytes > manifest.rendered_bytes);
}

#[test]
fn the_manifest_lists_the_transcript_and_the_delta() {
    let inputs = resumed();
    let (_, manifest) = compose_model_prompt(INSTRUCTIONS, &inputs).unwrap();
    let names: Vec<&str> = manifest
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "worker_instructions",
            "role_scoped_inputs",
            "warm_set",
            "warm_head_delta",
            "warm_notes_request",
            "warm_session",
            "warm_session_delta",
        ]
    );
    let transcript = manifest
        .entries
        .iter()
        .find(|entry| entry.name == "warm_session")
        .expect("the transcript is declared");
    assert_eq!(
        transcript.artifact_type.as_deref(),
        Some("review.kernel/SessionSnapshot@1")
    );
    assert_eq!(
        transcript.artifact_id.as_deref(),
        Some(digest('9').as_str())
    );
    assert_eq!(
        (transcript.rendered_bytes, transcript.estimated_tokens),
        (262_144, 65_536),
        "the transcript's cost is declared even though it is not prompt bytes"
    );
    let delta = manifest
        .entries
        .iter()
        .find(|entry| entry.name == "warm_session_delta")
        .expect("the delta prompt is declared");
    assert!(delta.rendered_bytes > 0 && delta.estimated_tokens > 0);
    assert!(
        delta.rendered_bytes < transcript.rendered_bytes,
        "the delta is what a resume sends instead of the whole input"
    );
}

#[test]
fn a_resumed_attempt_never_re_sends_its_own_notes() {
    // The fork holds this node's own reasoning already; Notes exist for the Attempt that starts
    // cold. Delta Marking, which says where that reasoning went stale, is still sent.
    let mut inputs = resumed();
    inputs.notes = Some(serde_json::json!({
        "node": "correctness",
        "attempt_id": "a".repeat(26),
        "head_snapshot_id": digest('1'),
        "inspected": [],
        "model_of_change": "the retry loop gained a cap",
        "open_questions": [],
        "hints": [],
    }));
    inputs.notes_artifact_id = Some(digest('4'));
    let (prompt, manifest) = compose_model_prompt(INSTRUCTIONS, &inputs).unwrap();
    assert!(!prompt.contains("the retry loop gained a cap"));
    assert!(
        manifest
            .entries
            .iter()
            .all(|entry| entry.name != "warm_notes")
    );
    assert!(prompt.contains("Delta Marking since the previous Round's head"));
}
