- Pi now retains a bounded, sanitized terminal SDK diagnostic, including assistant errors after a
  completed tool call. A failed Attempt with finalized native usage and non-aborted SDK settlement
  retains complete accounting and its exact AF charge, without a misleading incomplete-billing or
  malformed-reply failure. Successful final replies still require successful settlement. The host
  protocol is now `af.pi-host/7`; version 6 and earlier fail closed.
- Pi classifies original terminal errors before withholding private diagnostic text, preserving
  quota, authentication and network failures. Bounded structured error `type` and `code` fields
  preserve rate-limit and unavailable-model classifications even with neutral messages; these
  fields stay out of diagnostic text. Diagnostics also withhold configured header values named
  `type` and secrets from captured credential files. Real-host regressions cover selected OAuth
  credentials and linked credential files as terminal errors, checking operator output, retained
  artifacts and failure categories alongside the structured-error cases.
