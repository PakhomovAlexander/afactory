//! Terminal native evidence is distinct from an accepted Task receipt.
use crate::contract::*;
use af_contracts::task::state::{TaskCauseV1, TaskOutcomeV1};
use af_contracts::task::{TaskAcceptanceV1, TaskExecutionV1};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepairEvidence {
    pub findings_id: String,
    pub round_id: String,
    pub snapshot_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskOutcome {
    pub schema: String,
    pub receipt: TaskReceipt,
    pub outcome: TaskOutcomeV1,
    #[serde(deserialize_with = "Option::deserialize")]
    pub cause: Option<TaskCauseV1>,
    pub unknown_usage: bool,
    #[serde(deserialize_with = "Option::deserialize")]
    pub checks_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub repair: Option<RepairEvidence>,
}
impl TaskOutcome {
    pub fn accepted(&self) -> bool {
        self.outcome == TaskOutcomeV1::Verified
            && self.receipt.result.execution == TaskExecutionV1::Completed
            && self.receipt.result.acceptance == TaskAcceptanceV1::Satisfied
    }
    pub fn repairable(&self) -> bool {
        !self.unknown_usage
            && self.receipt.result.execution != TaskExecutionV1::Exhausted
            && self.repair.is_some()
            && matches!(
                (self.outcome, self.cause),
                (
                    TaskOutcomeV1::GateFailed,
                    Some(TaskCauseV1::CheckFailed | TaskCauseV1::CheckFlakyThenFailed)
                ) | (
                    TaskOutcomeV1::ChangesRequested,
                    Some(TaskCauseV1::ReviewerFindings | TaskCauseV1::EvaluatorRejected)
                )
            )
            && (self.outcome != TaskOutcomeV1::GateFailed || self.checks_id.is_some())
    }
    pub fn validate(&self) -> Result<()> {
        require(
            self.schema == "af/scenario-task-outcome@1",
            "Unsupported native Task outcome",
        )?;
        let r = &self.receipt;
        name(&r.task_id)?;
        require(
            [&r.plan_id, &r.revision_id, &r.result_id]
                .iter()
                .all(|s| af_contracts::is_digest(s)),
            "Invalid native outcome identity",
        )?;
        r.result.validate()?;
        require(
            r.result.task_revision_id == r.revision_id,
            "Native outcome revision differs",
        )?;
        require(
            (self.outcome == TaskOutcomeV1::Verified) == (r.result.acceptance == TaskAcceptanceV1::Satisfied),
            "Native outcome acceptance differs",
        )?;
        require(
            self.cause.is_none_or(|c| c.outcome() == Some(self.outcome)),
            "Native outcome cause differs",
        )?;
        if let Some(checks) = &self.checks_id {
            require(af_contracts::is_digest(checks), "Invalid check evidence")?;
        }
        if let Some(repair) = &self.repair {
            require(
                [&repair.findings_id, &repair.round_id, &repair.snapshot_id]
                    .iter()
                    .all(|s| af_contracts::is_digest(s)),
                "Invalid native repair evidence",
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use af_contracts::task::state::{TaskCauseV1 as Cause, TaskOutcomeV1 as Outcome};
    use serde_json::json;

    // Contract fixtures, not executed native Task or Provider evidence.
    fn terminal() -> TaskOutcome {
        serde_json::from_value(json!({
            "schema":"af/scenario-task-outcome@1",
            "receipt":{"task_id":"failed","plan_id":digest(b"plan"),"revision_id":digest(b"revision"),
                "result_id":digest(b"result"),"charged_tokens":17,
                "result":{"task_revision_id":digest(b"revision"),"execution":"incomplete",
                    "acceptance":"unsatisfied","domain_conclusion":"gate_failed",
                    "outputs":{},"evidence":[digest(b"check")],"missing_obligations":["goal"]}},
            "outcome":"gate_failed","cause":"check_failed","unknown_usage":false,
            "checks_id":digest(b"check"),
            "repair":{"findings_id":digest(b"findings"),"round_id":digest(b"round"),"snapshot_id":digest(b"candidate")}
        })).unwrap()
    }
    #[test]
    fn terminal_evidence_roundtrips_without_satisfying_accepted_task_contract() {
        let value = terminal();
        value.validate().unwrap();
        crate::tests::valid("scenario-v7.json#/$defs/TaskOutcome", &value);
        let wire = serde_json::to_value(&value).unwrap();
        assert_eq!(
            serde_json::from_value::<TaskOutcome>(wire.clone()).unwrap(),
            value
        );
        assert!(value.repairable());
        assert!(!value.accepted());
        assert!(Datum::Task(value.receipt.clone()).validate().is_err());
        assert!(
            Projection::AcceptedTask
                .apply(Datum::TaskOutcome(value.clone()))
                .is_err()
        );
        assert_eq!(
            Projection::TaskRepairable
                .apply(Datum::TaskOutcome(value))
                .unwrap(),
            Datum::Boolean(true)
        );
        for field in ["schema", "extra"] {
            let mut bad = wire.clone();
            bad[field] = json!("unknown");
            assert!(!crate::tests::schema("scenario-v7.json#/$defs/TaskOutcome").is_valid(&bad));
            assert!(
                serde_json::from_value::<TaskOutcome>(bad).map_or(true, |v| v.validate().is_err())
            );
        }
    }
    #[test]
    fn terminal_boundaries_and_missing_or_unknown_evidence_never_authorize_repair() {
        for (outcome, cause) in [
            (Outcome::Cancelled, Cause::HostCancelled),
            (Outcome::Exhausted, Cause::Budget),
            (Outcome::Expired, Cause::Deadline),
            (Outcome::ProviderFailed, Cause::ModelRefused),
            (Outcome::ProviderFailed, Cause::IdentityChanged),
            (Outcome::GateFailed, Cause::PreflightFailed),
        ] {
            let mut value = terminal();
            value.outcome = outcome;
            value.cause = Some(cause);
            value.validate().unwrap();
            assert!(!value.repairable(), "{outcome:?}/{cause:?}");
            assert!(!value.accepted());
        }
        for fault in 0..5 {
            let mut value = terminal();
            match fault {
                0 => value.unknown_usage = true,
                1 => value.repair = None,
                2 => value.checks_id = None,
                3 => value.receipt.result.execution = TaskExecutionV1::Exhausted,
                _ => value.cause = None,
            }
            value.validate().unwrap();
            assert!(!value.repairable());
            assert_eq!(value.receipt.charged_tokens, 17);
        }
    }
}
