- Allow quarantine confirmation and application to recover expired input entries on fresh
  evidence, while keeping normal expiry checks and strict validation of the full written catalog.
  Clean cross-platform evidence keeps original dates and cannot renew expired entries.
  Apply refuses reports more than 14 days old or predating an entry’s current window, including
  clean and always-failing results. Flaky renewal dates follow the confirmation report date,
  so delayed or repeated application cannot extend the evidence’s 14-day window.
