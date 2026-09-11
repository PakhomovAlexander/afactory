# ADR-0052: Capture local bindings and compose Review acceptance

Status: accepted for the Task increment implementation; unreleased.

## Context

Reusable Pipelines need developer-specific Workers while retaining their public contract and
mandatory verification. An implementation Pipeline must be able to call the same Review
package used standalone. A completed Review with Findings is not implementation acceptance.

## Decision

Capture explicit local bindings with the Task's authority. Add only pinned `local/*` packages;
reject namespace shadowing. Replace permitted qualified slots after validating their default
and effective interfaces, payload schemas, effects, evidence, retention and all mapped child
constraints. Preserve every constraining default in the dependency closure. Infer mandatory
independence between verification and source-writing Workers in addition to authored slot
annotations. Local aliases do not replace canonical Provider identity checks.

Keep typed Call expansion in the existing compiler and scheduler. Retain each Call's public
coverage in the compiled inspection hierarchy. The installed `review_accept` operator requires
a child's public `reviewed` output, exact current checks and source. It emits a separately typed
`af/ReviewedImplementation@1` receipt retaining its complete invocation. The Review domain
validates the original canonical reduction and derives implementation acceptance from the
Review and required checks; it cannot reinterpret missing execution as approval.

Task-file input may request `verification = "review"`; its default evaluator profile remains
explicitly distinct. Both profiles resolve their mandatory artifact types and verifier policy
before compilation. Source, checks, child Review, acceptance and local delivery use one Task
history and allowance. Generated plans continue to require the common approval guard.

## Evidence and limits

CLI fixtures execute one pinned Pipeline with Alice/Bob replacements and edited local files
after capture. Composition fixtures exercise implementation, embedded Review, local delivery
and standalone use of the same locked Review package. Negative cases cover weakened schemas,
forbidden overrides, changed effects, missing public coverage, stale checks, Findings, missing
reviewers and failed checks. Git import/sync, repair and legacy Review entry-point migration
remain separate unfinished acceptance items in PR 2.
