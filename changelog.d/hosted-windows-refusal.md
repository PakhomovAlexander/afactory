- A `github_actions` performance executor configuration that declares `[core.windows]` is now
  refused when it is admitted, before any original is reserved or dispatched: `performance.yml`
  has no windows input, so the hosted run would silently have measured the tier's own windows.
  Without windows, hosted dispatch is unchanged, and local declared windows still work.
