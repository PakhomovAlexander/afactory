Measure telemetry ownership-fence and final-settlement stalls from their selected I/O
boundaries, preserving real 80 ms interruption, one-second assertions and retained recovery
checks. Independent read/write cases and delayed preparation/deletion regressions prevent
setup load and earlier failures from hiding the target. Production enforcement entry/order
guards and three bypass mutation controls prevent injected timers from masking missing
production enforcement. Uniform bootstrap observations now
use their mathematically identical median directly; general inputs keep all 10,000 seeded
resamples, with exact serialized-equivalence and RNG-draw regressions. Bounded local timings
show reduced uniform computation, without claiming a hosted or whole-suite speedup.
