# ADR-0093: Derive CodeTask acceptance from execution and evidence

Date: 2026-09-13
Status: Proposed

## Context

A CodeTask can retain passed public verification while another independently declared Worker
fails. Deriving Satisfied first and changing only execution to Exhausted constructs an invalid
TaskResult and prevents terminal publication. Result validation repeats that mismatch.
The Pipeline permits an independent executable node outside the public acceptance chain;
its failure must remain part of the Task outcome.

## Decision

Determine CodeTask execution status before assessing acceptance. A failed planned node keeps
Exhausted execution and yields Inconclusive acceptance with an incomplete domain conclusion.
Use that same rule in result validation. Retain the exact public outputs, passed verification,
failed Attempt, diagnostic and usage. A passed obligation remains answered: an unrelated failure
does not invent a missing obligation. Complete execution retains the existing receipt-derived
Satisfied or Unsatisfied behavior. No TaskResult wire change or new accounting path is needed.

## Alternatives and verification

Dropping the independent failed node or claiming verified execution would lose admitted work.
Changing acceptance only in the constructor would still fail independent result validation.
Rejecting every Pipeline with independent work would narrow the existing public contract merely
to avoid handling its actual terminal outcome.

The implementation/Store regression passes the implementation/check/evaluator chain while an
independent Worker fails, then publishes a coherent terminal result. It checks the failed node
in the common RunReport, retained passed evidence with no missing obligation, exact outputs and
usage after reopening, and refusal to dispatch a finished Task. Existing positive, failed-check,
unavailable-check and timeout controls remain intact. No model inference is used.
