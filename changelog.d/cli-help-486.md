- Avoid repeated CLI parsing and help rendering at startup, and skip usage-document command
  lookup for help/version display results, preserving help text, errors and dispatch behavior.
  Claw performance validation is pending; this is not yet a verified regression resolution.
