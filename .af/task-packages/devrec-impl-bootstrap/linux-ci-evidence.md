## What I saw
PR689 Linux end-to-end CI fails reproducibly in two new workspace installation tests; each also fails alone. Mac local gate and combined focused tests passed.

## Evidence
Run https://github.com/PakhomovAlexander/afactory/actions/runs/42157522480, head716a36f035f08661a572181905723b9e9b4b66c5, e2e job127695170854. 1578 tests:1576 passed,2 failed. Both isolated reruns fail. scenario::workspace_lifecycle::production_workspace_scenario_repairs_same_pr_and_cleans_only_after_merge panics at workspace_lifecycle.rs498; task_delivery::workspace::privately_installed_workspace_adapter_prepares_native_accepted_candidate_without_service panics at workspace.rs194. Installer stderr for both: Installation refused; retain private state. No specific underlying refusal is exposed. Base/functional/uix/container jobs pass.

## Repro
Run these two native tests on the hosted Linux debug build with the pinned Rust1.88 toolchain and normal production installer. The current evidence establishes repeatable installation refusal, not its exact cause. Potential executable-size or environment differences need diagnosis; do not weaken production bounds or path ownership checks.
