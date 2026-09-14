//! Fixed Provider readiness context. No business inputs, package prompt or credentials.
use super::TaskInvocationV1;
use crate::ContextManifest;
use review_core::task::provider::TaskProviderAdmissionV2;
use serde::{Deserialize, Serialize};

pub const TASK_PROVIDER_CONTEXT_V2: &str = "af/TaskProviderContext@2";
pub const PROBE_INPUT: &[u8] = b"Reply with exactly: OK\n";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskProviderContextV2 {
    pub invocation: TaskInvocationV1,
    pub capability: TaskProviderAdmissionV2,
    pub rendered_id: String,
    pub manifest: ContextManifest,
}

impl TaskProviderContextV2 {
    pub fn validate(&self) -> Result<(), String> {
        self.invocation.validate()?;
        self.capability.validate()?;
        let mut manifest = ContextManifest::default();
        manifest.record(
            "capability_probe",
            "installed Provider admission",
            Some(self.rendered_id.clone()),
            None,
            PROBE_INPUT.len(),
        );
        manifest.finish(PROBE_INPUT.len());
        if !review_core::is_digest(&self.rendered_id)
            || !self.invocation.inputs.is_empty()
            || self.invocation.plan_id != self.capability.plan_id
            || self.manifest != manifest
        {
            return Err(
                "Provider context must retain the exact fixed probe and captured plan".into(),
            );
        }
        Ok(())
    }

    pub fn artifact_refs(&self) -> Vec<String> {
        vec![
            self.invocation.plan_id.clone(),
            self.capability.probe_policy_id.clone(),
            self.rendered_id.clone(),
        ]
    }
}
