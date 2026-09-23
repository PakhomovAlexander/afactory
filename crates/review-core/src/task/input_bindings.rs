//! The durable record of a Task file's `inputs` table.
//!
//! One `af/TaskInputBindings@1` artifact lists, per bound root port, the recorded Task output or
//! the exact artifact the port resolved to at plan time. It is provenance and display data: no
//! acceptance, verification, plan approval, delivery or budget authority crosses a Task boundary
//! through it (ADR-0117). The executed inputs are the ordinary `ArtifactInputV1` ports of
//! `af/TaskRevision@1`, which gains no field.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{TaskAcceptanceV1, is_name, present_option, require};
use crate::is_digest;

pub const TASK_INPUT_BINDINGS_V1: &str = "af/TaskInputBindings@1";

/// The referenced Task, as it stood when the binding resolved. `acceptance` and
/// `domain_conclusion` are copied so an operator approving the plan sees them; neither is an
/// obligation of the referencing Task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferencedTaskV1 {
    pub task_id: String,
    pub task_revision_id: String,
    pub result_id: String,
    pub port: String,
    pub acceptance: TaskAcceptanceV1,
    pub domain_conclusion: String,
}

impl ReferencedTaskV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_name(&self.task_id)
                && is_digest(&self.task_revision_id)
                && is_digest(&self.result_id)
                && is_name(&self.port),
            "A referenced Task needs a valid Task ID, exact revision and result, and a named port",
        )?;
        require(
            !self.domain_conclusion.trim().is_empty()
                && self.domain_conclusion.chars().count() <= 256,
            "A referenced Task needs a bounded domain conclusion",
        )
    }
}

/// One bound root port. `artifact_id` is always the artifact the reference named;
/// `resolved_artifact_id` is present only when the adapter had to publish a different artifact
/// for the port — a re-rooted `source`, whose `af/SourceTree@1` envelope must name the
/// re-rooted Snapshot rather than the derived one it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskInputBindingV1 {
    pub artifact_id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub resolved_artifact_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub snapshot_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub rerooted_snapshot_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_option"
    )]
    pub task: Option<ReferencedTaskV1>,
}

impl TaskInputBindingV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            is_digest(&self.artifact_id)
                && self.resolved_artifact_id.as_deref().is_none_or(is_digest)
                && self.snapshot_id.as_deref().is_none_or(is_digest)
                && self.rerooted_snapshot_id.as_deref().is_none_or(is_digest),
            "A Task input binding requires exact artifact and Snapshot identities",
        )?;
        require(
            self.rerooted_snapshot_id.is_none()
                || (self.snapshot_id.is_some() && self.resolved_artifact_id.is_some()),
            "A re-rooted binding retains both the referenced Snapshot and the republished artifact",
        )?;
        match &self.task {
            Some(task) => task.validate(),
            None => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskInputBindingsV1 {
    pub schema: String,
    pub bindings: BTreeMap<String, TaskInputBindingV1>,
}

impl TaskInputBindingsV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            self.schema == "af.task-input-bindings/1",
            "Unsupported Task input bindings generation",
        )?;
        require(
            !self.bindings.is_empty() && self.bindings.len() <= 16,
            "A Task input bindings record describes one to sixteen bound ports",
        )?;
        for (port, binding) in &self.bindings {
            require(is_name(port), "Invalid bound Task input port name")?;
            binding.validate()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn task() -> ReferencedTaskV1 {
        ReferencedTaskV1 {
            task_id: "layout-l3b".into(),
            task_revision_id: digest('1'),
            result_id: digest('2'),
            port: "snapshot".into(),
            acceptance: TaskAcceptanceV1::Unsatisfied,
            domain_conclusion: "changes_requested".into(),
        }
    }

    fn record() -> TaskInputBindingsV1 {
        TaskInputBindingsV1 {
            schema: "af.task-input-bindings/1".into(),
            bindings: BTreeMap::from([(
                "source".into(),
                TaskInputBindingV1 {
                    artifact_id: digest('a'),
                    resolved_artifact_id: Some(digest('b')),
                    snapshot_id: Some(digest('c')),
                    rerooted_snapshot_id: Some(digest('d')),
                    task: Some(task()),
                },
            )]),
        }
    }

    #[test]
    fn bindings_require_exact_identities_and_reject_an_empty_map() {
        record().validate().unwrap();

        let mut invalid = record();
        invalid.bindings.get_mut("source").unwrap().artifact_id = "l3b/snapshot".into();
        assert!(
            invalid.validate().is_err(),
            "a bound port names an exact artifact"
        );

        let mut invalid = record();
        invalid.bindings.clear();
        assert!(
            invalid.validate().is_err(),
            "a written record names at least one bound port"
        );

        let mut invalid = record();
        invalid.schema = "af.task-input-bindings/2".into();
        assert!(invalid.validate().is_err());

        let mut invalid = record();
        invalid
            .bindings
            .get_mut("source")
            .unwrap()
            .rerooted_snapshot_id = Some(digest('d'));
        invalid.bindings.get_mut("source").unwrap().snapshot_id = None;
        assert!(
            invalid.validate().is_err(),
            "a re-rooted binding keeps the Snapshot it came from"
        );
    }

    #[test]
    fn every_binding_contract_is_closed() {
        for pointer in ["", "/bindings/source", "/bindings/source/task"] {
            let mut value = serde_json::to_value(record()).unwrap();
            value.pointer_mut(pointer).unwrap()["approved"] = json!(true);
            assert!(
                serde_json::from_value::<TaskInputBindingsV1>(value).is_err(),
                "{pointer} accepted an unknown field"
            );
        }
    }

    #[test]
    fn a_binding_with_no_task_round_trips() {
        let exact = TaskInputBindingsV1 {
            schema: "af.task-input-bindings/1".into(),
            bindings: BTreeMap::from([(
                "history".into(),
                TaskInputBindingV1 {
                    artifact_id: digest('a'),
                    resolved_artifact_id: None,
                    snapshot_id: None,
                    rerooted_snapshot_id: None,
                    task: None,
                },
            )]),
        };
        exact.validate().unwrap();
        let value = serde_json::to_value(&exact).unwrap();
        let written = json!({"schema":"af.task-input-bindings/1",
            "bindings":{"history":{"artifact_id":digest('a')}}});
        assert_eq!(value, written, "no absent member is written");
        assert_eq!(
            serde_json::from_value::<TaskInputBindingsV1>(value).unwrap(),
            exact
        );
    }
}
