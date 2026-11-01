- Performance Lab CPU captures and their capability proofs decode every recording with
  function-level `perf script --no-inline`, so a CLI capture of hundreds of fresh-process
  recordings no longer spends its diagnostic budget expanding inline frames (4.7 s against
  0.09 s per real recording). Samples, losses, unknown frames, sampling settings and the
  retained recordings are unchanged; the diagnostics page says the stacks are function-level,
  and the kept recordings and symbols can be decoded again with inline expansion.
