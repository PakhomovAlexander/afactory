//! Closed native Task journal codecs.
use super::*;
pub fn read_task_transition(event: &RunEvent) -> Result<TaskTransitionV1, StoreError> {
    if event.event_type != EventType::TaskTransitionV6 {
        return Err(conflict("Expected a typed Task transition"));
    }
    let value: TaskTransitionV1 = serde_json::from_value(event.payload.clone())?;
    value.validate().map_err(conflict)?;
    Ok(value)
}
/// A lease's execution marker, the Task log's one event kind beside its transitions
/// (`TaskExecutionStarted@1`, ADR-0153).
pub fn read_execution_started(
    event: &RunEvent,
) -> Result<af_contracts::task::execution_started::TaskExecutionStartedV1, StoreError> {
    if event.event_type != EventType::TaskExecutionStartedV1 {
        return Err(conflict("Expected a Task execution marker"));
    }
    let value: af_contracts::task::execution_started::TaskExecutionStartedV1 =
        serde_json::from_value(event.payload.clone())?;
    value.validate().map_err(conflict)?;
    Ok(value)
}
/// Whether `event` of a Task log is a lease's execution marker rather than a transition.
pub fn is_execution_started(event: &RunEvent) -> bool {
    event.event_type == EventType::TaskExecutionStartedV1
}
/// Whether `event` of a Task log is one of its event kinds beside its transitions: a lease's
/// execution marker (`TaskExecutionStarted@1`), a gate retry (`TaskGateRetried@1`) or what an
/// Attempt did about its Provider (`TaskProviderEvent@1`).
pub fn is_beside_transitions(event: &RunEvent) -> bool {
    is_execution_started(event)
        || super::gate_retry::is_gate_retried(event)
        || is_provider_event(event)
}
/// Whether `event` of a Task log is a transition, not one of the log's other event kinds (an
/// execution marker, a gate retry or a Provider event).
pub fn is_transition(event: &RunEvent) -> bool {
    event.event_type == EventType::TaskTransitionV6
}
/// Whether `event` of a Task log is what an Attempt did about its Provider (ADR-0208).
pub fn is_provider_event(event: &RunEvent) -> bool {
    event.event_type == EventType::TaskProviderEventV1
}
pub fn read_provider_event(
    event: &RunEvent,
) -> Result<af_contracts::task::provider_event::TaskProviderEventV1, StoreError> {
    if event.event_type != EventType::TaskProviderEventV1 {
        return Err(conflict("Expected a Task Provider event"));
    }
    let value: af_contracts::task::provider_event::TaskProviderEventV1 =
        serde_json::from_value(event.payload.clone())?;
    value.validate().map_err(conflict)?;
    Ok(value)
}
pub(super) fn encode_provider_event(
    value: &af_contracts::task::provider_event::TaskProviderEventV1,
) -> Result<(EventType, serde_json::Value), StoreError> {
    value.validate().map_err(conflict)?;
    Ok((EventType::TaskProviderEventV1, serde_json::to_value(value)?))
}
pub(super) fn encode_execution_started(
    value: &af_contracts::task::execution_started::TaskExecutionStartedV1,
) -> Result<(EventType, serde_json::Value), StoreError> {
    value.validate().map_err(conflict)?;
    Ok((
        EventType::TaskExecutionStartedV1,
        serde_json::to_value(value)?,
    ))
}
pub(super) fn encode_transition(
    value: &TaskTransitionV1,
) -> Result<(EventType, serde_json::Value), StoreError> {
    value.validate().map_err(conflict)?;
    Ok((EventType::TaskTransitionV6, serde_json::to_value(value)?))
}
