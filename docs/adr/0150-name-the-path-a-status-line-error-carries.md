# ADR-0150: Name the path a status-line error carries

**Status:** accepted (2026-10-18).

## Context

[Issue #232](https://github.com/PakhomovAlexander/afactory/issues/232), a follow-up to #229.
At 80 columns the browser's status line keeps an error's reason visible by shortening the path
the error names (`paint::fit_named`). Only the `:cd` refusal `<path>: not a directory` gave its
path's byte span. Every other error went through `paint::fit_path`, which guesses the path by
splitting the text at spaces. Two messages lost their reason that way:

- `:scope project` from a long start outside any repository
  (`<start> is not inside a git repository`), when the start holds spaces;
- `:cd` into a repository whose configuration file does not parse, which `Scope::resolve`
  reports as `<file>: <parse error>` (or `reading <file>: <error>`), when the repository path
  holds spaces.

The guessed path was its first word. Shortening it saved too little, the frame cut the line
from the right, and the reason was gone.

## Decision

An error that names a path carries the exact byte span of that path in its text, as a
`config::Refusal` (`text`, `path: Option<Range<usize>>`). The span is never found by searching
the text. `config::load_rooted` refuses an unreadable or unparsable layer with the span of that
layer's file. `config::load_naming` and `config::load_machine_naming` return it.
`Scope::resolve` and `Scope::user` pass it on. The browser's `:scope project` refuses a start
outside any repository with the start's span. The `:cd` and `:scope` handlers put the
refusal's text and span on the status line. The status line then shortens that one path with
`fit_named`, as it already did for `:cd`'s `not a directory`.

The text of every error is unchanged. `config::load` and `config::load_machine` still return
it as a `String`, so `af config` and every other caller see the same errors as before. A
message without a span still goes through `fit_path`.

## Consequences

At 80 columns these errors keep their reason. Leading path components give way to `...` first.
When the final component alone is too wide, its leading columns go next, even if that
component has spaces. A message that fits is shown unchanged and is still made printable. A
reason wider than the whole line is still cut by the frame, since no layout can show it.
Tests in `crates/af/src/tui/tests.rs` drive the real `:scope project` and `:cd` handlers and
check the 80-column frame's status line. They cover a long start with spaces whose final
component has several words and a non-ASCII character, a start whose trailing components fit
whole, a short unchanged start, and a repository path with spaces whose `.af/af.toml` does not
parse. A unit test in `crates/af/src/config.rs` checks that a refused layer's span covers
exactly its file and that the text is what `load` always gave.
