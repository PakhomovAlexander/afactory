# ADR-0248: Hand review findings to the agent that will fix them

**Status:** accepted (2026-10-28). Changes the printed `af task findings --json` document to
`af/task-findings@2` and the printed `af review report --json` document to
`af/review-report@5`, and adds the SARIF profile `af/findings-sarif@1`
([ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md)).
Extends [ADR-0211](0211-give-the-agent-loop-its-verbs-and-deliver-a-verified-verification.md)
and [ADR-0244](0244-version-review-inspection-views.md).

## Context

An agent outside af that fixes review findings had to parse `af task findings` or `af review
report` and assemble its own instructions (issue
[#375](https://github.com/PakhomovAlexander/afactory/issues/375)). Review tools converge on three
handoffs: a self-contained prompt per finding, explicit coverage states, and SARIF for
code-scanning interfaces. They also show the trap: a tool that marks a comment addressed when a
later commit touches the file reports a fix nobody verified.

af printed neither a prompt nor coverage. Worse, `af task findings` printed a reviewer whose
result was missing as `no findings (its result is missing)`, and `af review report` said nothing
about reviewer nodes that failed in the last run beyond the verdict's reasons, so "no findings"
and "not reviewed" read alike. af already has the stronger primitive for fixes: a Task's
`af/TaskFixReceipt@1` and a Campaign's `FixVerification@1` are verification records tied to a
Subject and its Snapshot.

## Decision

1. **A prompt per finding.** Every finding of `af/task-findings@2` and `af/review-report@5`
   carries `agent_prompt` (`review_core::task::handoff::FindingPromptV1`): `location` (`file`,
   `line`, both null for a change-wide finding, and the `snapshot_id` of the reviewed Snapshot),
   `evidence`, `requested_change` (the reviewer's fix, or a fixed request when it proposed none),
   `verification` (how the fix will be judged, below) and `text`, all of it as one prompt an
   agent can be given as it is. A Task finding's Snapshot is its round's; a Campaign finding's
   is the head Snapshot of the Round that last saw it.
2. **Explicit coverage.** Each round of `af/task-findings@2` carries `coverage`
   (`CoverageV1`), and `af/review-report@5` carries `coverage` for its last recorded run, with
   that run's number, Round and head Snapshot, or `null` when no run is recorded or its Round's
   Subject is unreadable:
   - per reviewer: `reviewed`, or `not_run` with its `reason`. A Task reviewer is `not_run` when
     the round recorded no result from it (`missing_reviewers`); a Campaign `reviewer` or
     `scatter` node of the pinned plan is `not_run` when the run's outcome is failed (its error)
     or suppressed, or the run recorded none.
   - per path: each path a diff round changed, `reviewed` when every reviewer ran,
     `partially_reviewed` when some did, `not_reviewed` when none did; then each path a finding
     names outside the round, `outside_round`. A whole-tree round lists the paths its findings
     name.

   Text and Markdown say the same: a reviewer that did not run prints `not reviewed: it did not
   run`, never `no findings`, and a `coverage` line follows the reviewers.
3. **"Verified fixed" stays reserved for a verification record tied to a Snapshot.** A Task
   round's `verified_fixed` lists a finding only when the round's Finding Set marks it `fixed`
   and the repair assessment the round continued names it with a positive
   `af/TaskFixReceipt@1` on the round's Snapshot. A Campaign finding's `verified_fix` is
   non-null only when its fixed Resolution names a positive `FixVerification@1` for the same
   finding: the record and the head Snapshot of its Subject. A `fixed` status alone, a later
   commit or a touched file is never reported as a verified fix. The prompt's `verification`
   says so in words.
4. **SARIF on the existing `--format` surface.** `af review report` already chose its output
   with `--format md|text|json` and `--json` as the shorthand for `--format json`, the two
   refused together (ADR-0244). It gains `--format sarif`. `af task findings` gains the same
   flag, `--format text|json|sarif`, with its existing `--json` as the shorthand and the same
   refusal (exit 2). Both print one SARIF 2.1.0 log: `version` `2.1.0`, `$schema` the OASIS
   SARIF 2.1.0 schema, one run whose `tool.driver` is `af` with one rule per reviewer
   (`af-review/<reviewer>`); each finding one result with that `ruleId` and `ruleIndex`, a
   `level` from its severity (blocker `error`, major `warning`, minor `note`), the prompt
   `text` as its `message`, a physical location (the percent-encoded path and `startLine`) or,
   for a change-wide finding, a logical one, its Finding ID as a partial fingerprint, its
   prompt in its properties, and an accepted external suppression when it was declined. The
   coverage is one invocation whose `executionSuccessful` is false when a reviewer did not run
   or nothing was reviewed, with a notification per reviewer that did not run. The run's
   `properties.schema` is the profile `af/findings-sarif@1` (`schemas/findings-sarif-v1.json`),
   beside the Task or Campaign, round, Snapshot, coverage and `verified_fixed`. A Campaign
   finding with a verified fix is listed there, not as a result. Before printing, af checks the
   log against the SARIF 2.1.0 required structure and refuses one that breaks it.
5. **Versions.** `af/task-findings@1` and `af/review-report@4` are no longer printed and their
   schemas are deleted; nothing reads either. The Rust types are `TaskFindingsV2` and its parts
   in `review_core::task::agent_loop`, beside the shared handoff types in
   `review_core::task::handoff`. The continuation findings a continued Task's implementer
   receives (`af.task-review-findings/1`, ADR-0212) are unchanged.

## Considered options

- **A separate `af task findings --sarif` flag or an `af task export-sarif` command.** Rejected:
  af already selects a printed shape with `--format`, and one more spelling would be one more
  thing to learn and keep consistent.
- **Mark a finding fixed when a later round no longer reports it.** Rejected: absence from a
  later round is not proof; it is the trap the issue names. Only a verification record counts.
- **Add the prompt and coverage to the existing versions.** Rejected: a changed printed shape is
  a new version (ADR-0145); a reader of the old version fails closed instead of guessing.
- **Validate against the official SARIF schema in the test suite.** Not possible offline: the
  sandbox and the gate have no network, and af adds no dependency. The test checks the
  structure the official schema requires of what af writes, through `sarif::check` and the
  profile schema, and the check also runs before every print.

## Consequences

- An agent hands a finding to a fixer with `agent_prompt.text`, and tells "not reviewed" from
  "nothing found" by `coverage`, without parsing prose.
- Code-scanning tools take `--format sarif` as it is; GitHub code scanning shows a change-wide
  finding without a file location.
- Consumers of `af task findings --json` read `af/task-findings@2`, and of `af review report
  --json` `af/review-report@5`.
- Follow-up: vendor the official OASIS SARIF 2.1.0 schema under `schemas/` and validate the
  printed log against it in the suite.

Tests: `review-core` `task::handoff` unit tests (prompt fields, coverage states, a reviewer that
never ran) and `schema_parity::task_agent_loop::findings_name_each_reviewers_findings_and_the_evaluators_verdict`;
af's `sarif::tests::a_sarif_log_has_the_sarif_2_1_0_required_structure` and
`the_structural_check_refuses_a_log_missing_a_required_part`;
`task_execution::findings::tests::a_reviewer_that_did_not_run_is_not_reviewed_never_no_findings`;
`report_tasks::tests::a_campaign_without_a_task_reports_empty_task_accounting`; the integration
tests `task_agent_loop::findings_print_each_reviewers_findings_by_severity_and_the_evaluators_verdict`
(the binary's `--json` and `--format sarif`),
`campaign_loop::a_campaign_converges_after_a_scoped_nonfixed_resolution` (the report's prompts,
coverage and SARIF log), `campaign_loop::missing_disposition_coverage_makes_the_round_structurally_incomplete`
(a reviewer that did not run is `not_run` in the JSON, `not reviewed` in the text and a failed
invocation in the SARIF log) and `cli_surface`.
