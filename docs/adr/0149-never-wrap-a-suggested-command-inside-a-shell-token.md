# ADR-0149: Never wrap a suggested command inside a shell token

**Status:** accepted (2026-10-17). Amends
[ADR-0104](0104-preview-captured-task-plans-before-first-execution.md): its bounded line width
holds for every preview row except the shell commands the preview suggests.

## Context

[Issue #157](https://github.com/PakhomovAlexander/afactory/issues/157). The Task plan preview
wrapped every row at 96 bytes. A Task ID may be 128 ASCII characters
(`review_core::task::is_name`), so the indented `af task run <ID> \` row came to 143 bytes and
was cut after 82 ID characters. The first line then had no shell continuation, and the second
held the remaining 46 characters and the `\`.
Pasting the action ran a different command. The `Details: af task explain <ID>` hint split the
same way. The persisted Task ID and the JSON inspection were never truncated.

## Decision

A row of the preview that suggests a shell command (the two-line `af task run <ID> \` /
`--confirm-plan <PLAN>` action, and the `Details: af task explain <ID>` hint) is written whole
on its own line. The `\` continuation sits only between arguments. The width bound wraps prose
rows and never cuts a command. Command rows still go through the same sanitizer as every other
row, so terminal controls, newlines and non-ASCII characters still show as `?`. The action
still names the exact plan ID that `--confirm-plan` checks, and is still followed by the
reminder to use the same `--repo` and `--state` selectors. Task ID limits, plan approval and
the rest of the layout are unchanged.

## Consequences

With a long Task ID, a command row can be wider than 96 columns, and a narrow terminal may wrap
it visually. A terminal soft wrap does not add a newline when the text is copied. A hard
newline inside a token did. A unit test in `crates/af/src/task_execution/preview.rs` and the
`task_preview` CLI regression check, for short IDs and for a 128-character ID, that the action
and the Details hint keep every shell token. The CLI regression also runs the copied
`af task explain` and `af task run --confirm-plan` with the preview's state selector, using the
fixture's command Workers and no real Provider.
