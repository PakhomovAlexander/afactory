Telemetry measurements now disable Docker daemon logging and writable shared memory and
reuse the build image's volume admission check before any candidate execution. Mandatory
Docker smoke coverage adds legitimate measurements, denied writes, output flooding,
aggregate and unlinked allocation, volume refusal and confirmed cleanup beside the existing
eight build/admission cases.

The telemetry workflow carries private, bounded scratch ownership through every execution
and final cleanup. Pending or uncertain launches and missing, corrupt or foreign evidence
retain the exact image, mount and control paths without unmounting or unlinking them. Clean
completion and a proved pre-launch refusal still clean up. Result JSON cannot authorize
cleanup, including after cancellation before publication.

Gate-owner test readiness markers publish atomically; production supervision and all
existing fixture assertions are unchanged. Calibration SHA acceptance is unchanged:
[GitHub documents case-insensitive string comparisons](https://docs.github.com/en/actions/reference/workflows-and-actions/expressions#operators).
The executable run-name test now models that rule without lowercasing the dispatch input.
These repairs make no performance, calibration readiness or rollout claim.
