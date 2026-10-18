- `af` browser: `:scope project` from a long start outside any repository and `:cd` into a
  repository whose configuration file does not parse now keep their reason visible on the
  status line at 80 columns. Before, a path with spaces could hide the reason. The error carries
  the exact span of the path it names, so a path with spaces is shortened from the left as one
  path. The error text is unchanged, and a message that fits is unchanged (#232,
  [ADR-0150](docs/adr/0150-name-the-path-a-status-line-error-carries.md)).
