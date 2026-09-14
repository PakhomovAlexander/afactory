//! An optional host interruption request. It grants no execution or resource authority.
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) fn check(cancellation: Option<&AtomicBool>) -> Result<(), String> {
    if cancellation.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        Err("Task execution was cancelled by its host".into())
    } else {
        Ok(())
    }
}
pub(crate) fn refused(message: impl Into<String>) -> TaskWorkOutput {
    TaskWorkOutput {
        usage_observation: None,
        usage: None,
        outputs: Err(message.into()),
        charged_tokens: Some(0),
        raw_artifact_ids: vec![],
        usage_id: None,
        feedback_id: None,
    }
}
