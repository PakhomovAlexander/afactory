# ADR-2026-11-10-inspect-task-attempts: Task inspection

**Status:** accepted (2026-11-10). Its absence of a live channel is superseded by
[bounded Task activity](2026-11-10-publish-safe-task-activity.md).

## Context

The Task browser's flat progress view makes parallel work and retries difficult to distinguish.
Provider output can contain credentials and must pass the existing admission checks before it is
released. Silence in that output is not evidence that a Worker is stalled.

## Decision

Opening a Task shows navigation beside the selected Task's recorded state. Attempt selection uses
its recorded identity, not its position in a list, and polling reads the existing Task Store on
its background reader. The inspector displays only recorded execution metadata. It explicitly
says live output is unavailable where there is no approved activity channel; it does not read
unadmitted provider output. Terminal text uses the existing printable ASCII renderer.

## Consequences

The inspector can navigate the recorded attempts even when a runner exposes no activity stream.
A future live channel needs bounded storage, exact Attempt attribution and credential-safe
metadata before it can be displayed. This decision grants no authority to publish raw output.
