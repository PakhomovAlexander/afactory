# Native preflight fixtures

The helper reads current `af/task-inspection@20` and `af/task-diff@1` advisory documents.
The bound-source example is `../bound-inputs/inspection-bound.json`; its optional evaluation
input and conditional verifier remain unknowns in the advisory report. Tests consume that
current captured document and actual CLI/script output, with exact refusal and schema checks.

For a captured native Review Task, export its real inspection with
`af review inspect TASK_ID --json`. Export a native diff with `af task diff TASK_ID --json`,
using explicit recorded Snapshot IDs when inspecting a planned Task with no candidate output.
These documents describe recorded data; they grant no execution, promotion or publication authority.
Native Review planning and execution both refuse an empty Diff. The helper also refuses an empty
Diff advisory document and the retired `af/review-plan@1` identity. No successful empty Review
plan is fabricated. See the [preflight guide](../../../docs/task-execution/preflight.md).
