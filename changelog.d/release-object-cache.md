- Release initialization automatically seeds safe same-repository local Git objects through an
  isolated bundle, while requiring authoritative remote main/tag acquisition. Stderr reports cache
  and fetch progress, and read-only status distinguishes active initialization from a missing run.
