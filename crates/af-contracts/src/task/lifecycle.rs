//! Pure lifecycle precedence. Callers obtain these facts from checked history, not Worker text.
use super::{TaskPhaseV1, state::*};

pub fn phase(
    recorded: &TaskPhaseV1,
    lease_live: bool,
    admitted: bool,
    abandoned_cause: TaskCauseV1,
    failures: impl Iterator<Item = TaskCauseV1>,
) -> TaskStatePhaseV1 {
    match recorded {
        TaskPhaseV1::Finished { .. } => TaskStatePhaseV1::Finished,
        TaskPhaseV1::Waiting { .. } => TaskStatePhaseV1::Waiting,
        _ if lease_live => TaskStatePhaseV1::Running,
        _ if !admitted => TaskStatePhaseV1::Planned,
        _ if abandoned_cause != TaskCauseV1::HostCancelled
            && failures.into_iter().any(|cause| {
                !matches!(cause, TaskCauseV1::WriterLost | TaskCauseV1::HostCancelled)
            }) =>
        {
            TaskStatePhaseV1::Waiting
        }
        _ => TaskStatePhaseV1::Abandoned,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lifecycle_phase_preserves_derived_waiting_and_cancellation_precedence() {
        let phase_of = |live, admitted, cause, failures: Vec<TaskCauseV1>| {
            phase(
                &TaskPhaseV1::Running {},
                live,
                admitted,
                cause,
                failures.into_iter(),
            )
        };
        assert_eq!(
            phase_of(
                false,
                true,
                TaskCauseV1::WriterLost,
                vec![TaskCauseV1::Budget]
            ),
            TaskStatePhaseV1::Waiting
        );
        assert_eq!(
            phase_of(
                false,
                true,
                TaskCauseV1::HostCancelled,
                vec![TaskCauseV1::Budget]
            ),
            TaskStatePhaseV1::Abandoned
        );
        assert_eq!(
            phase_of(false, false, TaskCauseV1::WriterLost, vec![]),
            TaskStatePhaseV1::Planned
        );
        assert_eq!(
            phase_of(
                true,
                true,
                TaskCauseV1::WriterLost,
                vec![TaskCauseV1::Budget]
            ),
            TaskStatePhaseV1::Running
        );
        assert_eq!(
            phase(
                &TaskPhaseV1::Waiting {
                    reason: super::super::TaskWaitingReasonV1::NeedsHuman
                },
                true,
                true,
                TaskCauseV1::WriterLost,
                std::iter::empty()
            ),
            TaskStatePhaseV1::Waiting
        );
    }
}
