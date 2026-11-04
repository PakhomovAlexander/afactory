- Capture excerpts for bracketed and comma- or semicolon-separated canonical triage references,
  including filenames with colons, backslashes and spaces. Unsafe paths remain refused,
  including absolute, traversal or URL prefixes before opening brackets, even across control
  characters, line breaks, nested brackets or extra tokens inside the brackets. Every ASCII
  control before an opening bracket preserves the unsafe prefix and records missing evidence,
  even when ordinary text follows the control. Unsafe authority stays across text, whitespace
  and delimiters; only a completed canonical reference supplies a safe boundary. Completion
  uses the reference parser's numeric suffix and trailing punctuation rules and checks the
  entire saved path against the Manifest, so a punctuated reference followed by a quoted
  reference captures both excerpts without a missing-evidence refusal. Completion consumes
  only that reference and its punctuation: an attached absolute, traversal or URL prefix
  still refuses a later quoted reference while preserving the first excerpt. Dot runs retain
  the attached `../` prefix in full. An unsafe quoted canonical filename with numeric colons
  produces exactly one complete refusal, even when its final filename component is absent
  from the Manifest.
- Restore direct completed-fix evidence such as `Shipped fix for #432`; contextual, negated and
  partial mentions remain private owner-review evidence. Sweeps still deduplicate and never close.
- Retain [same-host full-suite fixture-cost evidence](../docs/design/issue-triage-fixture-cost.md)
  for the source before the in-memory split, the continuation source and this close-out candidate.
